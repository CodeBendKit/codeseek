//! Phase P0 交付物 1 的**端到端**验证：图缓存版本门禁必须落在
//! "缓存失效 → 全量重建"这条既有降级链上，绝不把错误上抛成致命失败，也绝不 panic。
//!
//! 单元测试（`src/storage/petgraph_storage.rs` / `src/storage/persistence.rs` 的内联用例）
//! 只能证明"门禁函数本身返回 Err / Both 分支降级成 Ok(None)"；本文件证明的是
//! **真实消费方**（`PersistenceManager::load_graph` 与 `CodeParser::build_petgraph_code_graph`）
//! 在遇到旧格式 / 版本不符的缓存时，行为依然是"重建 + 重新落盘"。
//!
//! 注意：`PersistenceManager::project_dir()` 硬编码为 `~/.codeseek/projects/<project_id>`，
//! 无法注入临时目录，因此本文件用**带进程号的唯一 project_id** 写入，并在用例结束时删除；
//! 除这个自建的目录外不触碰 home 里的任何其它数据。

use std::path::{Path, PathBuf};

use codeseek::cli::args::StorageMode;
use codeseek::codegraph::parser::CodeParser;
use codeseek::codegraph::types::{FunctionInfo, PetCodeGraph};
use codeseek::storage::persistence::PersistenceManager;
use codeseek::storage::petgraph_storage::{PetGraphStorageManager, GRAPH_SCHEMA_VERSION};

/// 本测试专用的一次性项目 id（进程号 + 用途后缀，避免与真实项目重名）
fn unique_project_id(tag: &str) -> String {
    format!("codeseek_p0_gate_{tag}_{}", std::process::id())
}

/// `~/.codeseek/projects/<id>`（与 `PersistenceManager::project_dir` 同一形状）
fn project_dir_of(project_id: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join(".codeseek").join("projects").join(project_id))
}

fn make_graph(function_names: &[&str]) -> PetCodeGraph {
    let mut graph = PetCodeGraph::new();
    for (index, name) in function_names.iter().enumerate() {
        graph.add_function(FunctionInfo {
            id: uuid::Uuid::new_v4(),
            name: (*name).to_string(),
            file_path: PathBuf::from(format!("src/{name}.rs")),
            line_start: index + 1,
            line_end: index + 5,
            namespace: "crate".to_string(),
            language: "rust".to_string(),
            signature: Some((*name).to_string()),
            module_path: None,
            qualified_name: None,
        });
    }
    graph
}

/// 产出一个"裸 bincode、无版本文件头"的字节序列 —— 即 P0 之前遗留的 `graph.bin` 形状。
fn legacy_binary_payload(graph: &PetCodeGraph) -> Vec<u8> {
    let staging = tempfile::TempDir::new().expect("创建暂存目录失败");
    let path = staging.path().join("graph.bin");
    // 档位戳在这里**无关紧要**：本函数产出的是"没有 8 字节版本头"的旧格式载荷，
    // 读侧在解码载荷之前就被文件头拦下（`GRAPH_BINARY_HEADER_LEN`/magic 校验）。
    // 传当前档位只是为了满足 `save_to_binary` 的强制签名（戳不许缺）。
    PetGraphStorageManager::save_to_binary(graph, &path, "layered", 8).expect("暂存落盘失败");
    let bytes = std::fs::read(&path).expect("读回暂存文件失败");
    assert_eq!(&bytes[..4], b"CGSP", "新格式必须以 CGSP magic 开头");
    // 剥掉 8 字节文件头 = 旧格式（纯 bincode 载荷）
    bytes[8..].to_vec()
}

/// 产出一个"没有 schema_version 键"的 JSON —— 即 P0 之前遗留的 `graph.json` 形状。
fn legacy_json_payload(graph: &PetCodeGraph) -> String {
    // 同上：本函数随后要删掉 `schema_version` 键，载荷在版本门禁处就被判失效，
    // 档位戳取当前值仅为满足强制签名。
    let json = PetGraphStorageManager::save_to_json(graph, "layered", 8).expect("JSON 序列化失败");
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("save_to_json 必须产出合法 JSON");
    assert!(value
        .as_object_mut()
        .and_then(|obj| obj.remove("schema_version"))
        .is_some());
    serde_json::to_string_pretty(&value).expect("旧格式 JSON 序列化失败")
}

/// 1) 新格式缓存能被 `PersistenceManager` 正常读回，且磁盘上的 graph.bin 确实带版本头。
#[test]
fn test_current_version_cache_still_hits_through_persistence() {
    let project_id = unique_project_id("hit");
    let dir = project_dir_of(&project_id).expect("HOME 不可用");
    let base_dir = tempfile::TempDir::new().expect("创建 base_dir 失败");

    let manager = PersistenceManager::with_storage_mode(StorageMode::Both, base_dir.path().to_path_buf());
    let graph = make_graph(&["alpha", "beta"]);
    manager.save_graph(&project_id, &graph, "layered", 8).expect("Both 模式落盘应成功");

    // 磁盘形状：graph.bin 头部是 magic + 当前版本号；graph.json 里带同名键
    let bin = std::fs::read(dir.join("graph.bin")).expect("读 graph.bin 失败");
    assert_eq!(&bin[..4], b"CGSP");
    assert_eq!(
        u32::from_le_bytes([bin[4], bin[5], bin[6], bin[7]]),
        GRAPH_SCHEMA_VERSION,
        "graph.bin 文件头版本号必须是当前 GRAPH_SCHEMA_VERSION"
    );
    let json_text = std::fs::read_to_string(dir.join("graph.json")).expect("读 graph.json 失败");
    let json_value: serde_json::Value = serde_json::from_str(&json_text).expect("graph.json 应可解析");
    assert_eq!(
        json_value.get("schema_version").and_then(|v| v.as_u64()),
        Some(GRAPH_SCHEMA_VERSION as u64)
    );

    // 读回：Both 模式命中二进制缓存
    let loaded = manager.load_graph(&project_id).expect("当前版本缓存读取不该报错");
    let loaded = loaded.expect("当前版本缓存必须命中");
    assert_eq!(loaded.graph.node_count(), 2);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = base_dir.close();
}

/// 2) 关键链路：旧格式 graph.bin + 旧格式 graph.json 同时存在时，
///    `load_graph` 必须返回 `Ok(None)`（缓存 miss），而不是 `Err`，更不允许 panic。
#[test]
fn test_legacy_caches_degrade_to_cache_miss_instead_of_fatal_error() {
    let project_id = unique_project_id("legacy");
    let dir = project_dir_of(&project_id).expect("HOME 不可用");
    std::fs::create_dir_all(&dir).expect("创建项目缓存目录失败");
    let base_dir = tempfile::TempDir::new().expect("创建 base_dir 失败");

    let stale = make_graph(&["stale_marker"]);
    std::fs::write(dir.join("graph.bin"), legacy_binary_payload(&stale)).expect("写旧 graph.bin 失败");
    std::fs::write(dir.join("graph.json"), legacy_json_payload(&stale)).expect("写旧 graph.json 失败");

    let manager = PersistenceManager::with_storage_mode(StorageMode::Both, base_dir.path().to_path_buf());
    let result = manager.load_graph(&project_id);

    match result {
        // 版本门禁把两份缓存都判为不可用 → Both 分支降级成 Ok(None) → 上层据此全量重建
        Ok(None) => {}
        Ok(Some(graph)) => panic!(
            "旧格式缓存绝不该被读成有效数据（读到 {} 个节点），说明版本门禁/回退链失效",
            graph.graph.node_count()
        ),
        Err(e) => panic!("缓存不可用绝不该上抛成致命错误，实际 Err: {e}"),
    }

    // 单独 Binary 模式下，门禁的 Err 依然只是"读不到"，由 `_load_existing_graph` 降级；
    // 这里只断言"不 panic、且拿不到脏数据"。
    let binary_only = PersistenceManager::with_storage_mode(StorageMode::Binary, base_dir.path().to_path_buf());
    let raw = binary_only.load_graph(&project_id);
    assert!(
        matches!(raw, Err(_) | Ok(None)),
        "Binary-only 模式下旧缓存要么是 Err（由上层降级）要么是 Ok(None)，绝不能是 Some"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = base_dir.close();
}

/// 3) 端到端：把旧格式 graph.bin 预先种进项目缓存目录，再用**默认（非 force）**的
///    `CodeParser::build_petgraph_code_graph` 建图 —— 必须走
///    "Err → warn → Ok(None) → PetCodeGraph::new() 全量重建"，产出干净、完整的图。
#[test]
fn test_parser_rebuilds_from_scratch_when_cache_version_is_stale() {
    // 复制 fixture 到唯一临时目录，目录名同时作为 `_load_existing_graph` 的 project_id
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("resolution")
        .join("cross_file_same_name");
    let staging = tempfile::TempDir::new().expect("创建临时目录失败");
    let project_dir = staging.path().join(unique_project_id("parser"));
    copy_tree(&fixture_root, &project_dir);

    let project_id = project_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .expect("临时项目目录名不可用");
    let cache_dir = project_dir_of(&project_id).expect("HOME 不可用");
    std::fs::create_dir_all(&cache_dir).expect("创建项目缓存目录失败");
    let stale = make_graph(&["stale_marker"]);
    std::fs::write(cache_dir.join("graph.bin"), legacy_binary_payload(&stale)).expect("种入旧 graph.bin 失败");

    let md5_id = format!("{:x}", md5::compute(project_dir.to_string_lossy().as_bytes()));

    // 故意**不**调用 set_force_reindex：证明"版本门禁自身"就足以让旧缓存失效
    let mut parser = CodeParser::new();
    // Phase P3：本用例锁 legacy 的 `resolved_calls == 3` 基线，显式钉 Legacy，
    // 不依赖翻转后的 layered 默认值（门禁/缓存失效逻辑与档位无关，此处只是产边口径）。
    parser.set_resolver_mode(codeseek::cli::args::ResolverMode::Legacy);
    let graph = parser
        .build_petgraph_code_graph(&project_dir)
        .expect("旧缓存必须被降级为全量重建，而不是让建图失败");

    let names: Vec<String> = graph.get_all_functions().into_iter().map(|f| f.name.clone()).collect();
    assert_eq!(graph.graph.node_count(), 5, "全量重建应得到 fixture 的 5 个函数，实际 {names:?}");
    assert!(
        !names.iter().any(|name| name == "stale_marker"),
        "旧缓存里的内容绝不能被部分读进新图：{names:?}"
    );
    assert_eq!(graph.get_stats().resolved_calls, 3, "重建后的调用边规模应与基线一致");

    // 清理：旧缓存目录 + 本次建图写下的文件哈希目录
    let _ = std::fs::remove_dir_all(&cache_dir);
    if let Some(md5_dir) = project_dir_of(&md5_id) {
        let _ = std::fs::remove_dir_all(&md5_dir);
    }
    let _ = staging.close();
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("创建目标目录失败");
    for entry in std::fs::read_dir(src).expect("读取 fixture 目录失败").flatten() {
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("复制 fixture 文件失败");
        }
    }
}
