use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::codegraph::types::{PetCodeGraph, FunctionInfo, CallRelation, CodeGraphStats};
use tracing::warn;

/// 图缓存的 schema 版本，**任何改动序列化形状的提交都必须在同一个 commit 内把它 +1**。
///
/// 为什么必须有这个常量：`graph.bin` 用 bincode 编码，而 bincode **不是自描述格式** ——
/// 它只按字段声明顺序裸写"长度 + 前缀"，字段顺序 / 类型 / 数量的任何变化都会让旧文件的
/// 字节整体错位（更糟的是：错位的字节会被当成 `Vec` 的元素个数，从而触发巨量内存分配）。
/// 所以后续给 `FunctionInfo` / `CallRelation` / `PetGraphStorage` 增删字段（例如 P2 的
/// receiver / 限定名 / 多候选字段）时，绝不能只改结构体：必须同时递增本常量。
///
/// 版本不符（或文件缺少版本字段，视作 0）一律判定为**缓存失效**，走既有的
/// `Err(String)` → `persistence` → `parser::_load_existing_graph` 的
/// `Err → warn → Ok(None)` → `PetCodeGraph::new()` 全量重建路径；绝不 panic、绝不 unwrap。
///
/// **Phase P3 收口：2 → 3**。递增的唯一原因：本版本给 `PetGraphStorage` 在
/// `schema_version` 之后追加了两个"解析档位戳"字段（`resolver_mode` / `max_candidates`）。
/// bincode 按字段声明顺序编码，新字段插在载荷最前面 ⇒ 所有 v2 的 `graph.bin` 字节整体错位，
/// 必须由版本号一次性判失效；`graph.json` 虽然自描述（缺字段会被 `serde(default)` 补成
/// 空串/0），但空串档位同样过不了 `validate_resolution_stamp`，语义与"版本不符"一致。
/// 影响面：**现存 v2 缓存一律失效一次并触发全量重建**（已批准），没有任何路径会 panic。
pub const GRAPH_SCHEMA_VERSION: u32 = 3;

/// 把 `CodeParser` 的 `max_candidates: usize` 折算成戳字段用的 `u16`。
///
/// 为什么戳用 `u16` 而不是 `usize`：`usize` 的宽度随目标平台变化（32/64 位不同），
/// 而 bincode 是无自描述的定宽编码 —— 同一个缓存在 32 位机上写的字节，到 64 位机上
/// 就会被读成不同的数，进而把**同一档**的缓存误判成失效（白白全量重建）。`u16` 足够：
/// 引擎内部真正消费的上限被 `ResolutionConfig.max_candidates: u8` 饱和到 255
/// （见 `codegraph/parser.rs::resolution_config`），任何 >65535 的 CLI 取值都已经在
/// 更早的环节被截断成"等价行为"，因此把 >65535 的值饱和成 65535 不会误判。
///
/// ⚠ 写侧与校验侧**必须**都经过本函数，否则会出现"写入 70000、期望 65535"这类假失效。
pub fn clamp_max_candidates(value: usize) -> u16 {
    value.min(u16::MAX as usize) as u16
}

/// `graph.bin` 文件头 magic（4 字节 ASCII，"CodeGraph Storage"）。
///
/// 作用：让加载器在**碰 bincode 载荷之前**就认出"这不是当前格式的文件"，
/// 典型场景就是 P0 之前遗留的裸 bincode 旧缓存（没有文件头）。
/// 没有这道检查的话，旧文件的前 4 字节（原 `functions` 长度低位）会被当作
/// `schema_version`，紧接 8 字节会被当作 `Vec` 长度 → 可能 `with_capacity(数十亿)`。
const GRAPH_BINARY_MAGIC: &[u8; 4] = b"CGSP";

/// `graph.bin` 文件头总长度：magic(4 字节) + schema_version(4 字节，小端固定宽度)。
const GRAPH_BINARY_HEADER_LEN: usize = 8;

/// 把 `schema_version` 写成文件头里的小端固定宽度字节
fn binary_header_bytes(version: u32) -> [u8; GRAPH_BINARY_HEADER_LEN] {
    let mut header = [0u8; GRAPH_BINARY_HEADER_LEN];
    header[..4].copy_from_slice(GRAPH_BINARY_MAGIC);
    header[4..].copy_from_slice(&version.to_le_bytes());
    header
}

/// 同一进程内的临时文件序号，保证并发写同一目标时各自占用不同的临时文件名
static TEMP_WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **原子写**：先写同目录下的临时文件，再 `rename` 覆盖目标文件。
///
/// 为什么必须有：`graph.json` / `graph.bin` 的路径是 `~/.codeseek/projects/<id>/`，
/// 属于**进程级全局共享**位置 —— `init`、HTTP/MCP 服务、以及多个测试线程都可能同时对
/// 同一个 `<id>` 读写。原来的 `fs::write` 是"就地截断再写"，读者可能正好读到被截断的
/// 半截文件，从而得到"缓存损坏"的假阳性（这是 P0 之前就存在的偶发失败，与版本门禁无关）。
/// 改成"临时文件 + rename" 之后，读者要么看到旧的完整文件、要么看到新的完整文件。
///
/// 注：`rename` 只在同一文件系统内是原子的，所以临时文件必须与目标文件同目录。
fn write_bytes_atomically(file_path: &Path, bytes: &[u8]) -> Result<(), String> {
    let seq = TEMP_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file_name = file_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "graph".to_string());
    let temp_path = file_path.with_file_name(format!(
        "{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        seq
    ));

    // 目标文件的父目录可能尚未创建（保持与旧实现一致的容错：旧实现依赖调用方先建目录）
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
        }
    }

    if let Err(e) = fs::write(&temp_path, bytes) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Failed to write temp file {}: {}",
            temp_path.display(),
            e
        ));
    }

    if let Err(e) = fs::rename(&temp_path, file_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Failed to replace file {} with {}: {}",
            file_path.display(),
            temp_path.display(),
            e
        ));
    }

    Ok(())
}

/// petgraph代码图存储格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetGraphStorage {
    /// 图缓存 schema 版本 —— **必须是结构体的第一个字段**（bincode 按声明顺序编码，
    /// 放首位可保证任何旧文件在解码载荷前就会因字节错位而失败，属于第二重保险）。
    ///
    /// `#[serde(default)]` 只对 JSON 生效：JSON 是自描述格式，旧缓存缺少本字段时
    /// 会取 `u32` 默认值 0；0 恒不等于 `GRAPH_SCHEMA_VERSION`（>=1），因此旧 JSON
    /// 同样被门禁判为失效，而不是被当成合法数据读进来。
    #[serde(default)]
    pub schema_version: u32,
    /// **解析档位戳（1/2）**：这份图是用哪一档 callee 解析器建的 —— `"legacy"` / `"layered"`，
    /// 空串表示"未知/旧文件"（即 v2 及更早、没有戳的缓存），一律判失效。
    ///
    /// 为什么必须有它：`schema_version` 只能证明"字节形状兼容"，证明不了"语义档位一致"。
    /// 自 Phase P3 起全局默认档从 legacy 切到 layered，而**旧缓存的字节形状没变过**
    /// （v2 就带了 `resolution_level`/`confidence` 四个标量），于是上一轮用 legacy 建的
    /// `graph.bin` 在新默认下仍会通过了 `validate_schema_version()` 被
    /// `_load_existing_graph` 读回来；再叠加"文件 MD5 未变 ⇒ 整文件跳过重解析"的语义
    /// （`_should_skip_file`），被跳过文件里那些**由旧档产出的边**会原样留在图里，
    /// 得到一张 legacy/layered 混档图：同一文件集里 `resolution_level`/`confidence`
    /// 的含义不一致，而且没有任何自动检测（只能靠用户手工 `--force-reindex`）。
    /// 本字段把"档位"变成缓存的一部分身份，换档 ⇒ 失效 ⇒ 全量重建。
    ///
    /// `#[serde(default)]` 只对 JSON 生效（空串），bincode 侧因字段位置变化由版本号拦住。
    #[serde(default)]
    pub resolver_mode: String,
    /// **解析档位戳（2/2）**：建图时的 `--max-candidates` 上限（已经过
    /// [`clamp_max_candidates`] 折算成 `u16`）。0 表示"未知/旧文件"。
    ///
    /// 为什么上限也要进戳：它直接改变落盘边数 —— `emit_layered_resolution` 按
    /// `take(max_candidates)` 截断候选。若戳只有档位没有上限，用户把
    /// `--max-candidates=8` 改成 `=1` 之后旧缓存仍然"档位匹配"，于是新上限只作用于
    /// 本轮重解析的文件，其余文件继续留 8 条边 —— 与档位混档完全同形的缺陷。
    #[serde(default)]
    pub max_candidates: u16,
    /// 函数信息列表
    pub functions: Vec<FunctionInfo>,
    /// 调用关系列表
    pub call_relations: Vec<CallRelation>,
    /// 函数名映射
    pub function_names: HashMap<String, Vec<Uuid>>,
    /// 文件映射
    pub file_functions: HashMap<PathBuf, Vec<Uuid>>,
    /// 统计信息
    pub stats: CodeGraphStats,
}

impl PetGraphStorage {
    /// 从PetCodeGraph创建存储格式，并**同时写入解析档位戳**。
    ///
    /// 档位必须由调用方传入而不是在内部"猜"：`PetCodeGraph` 是纯图数据，本身不携带
    /// "这些边是哪档解析器产的"这一元信息（把它塞进 `PetCodeGraph` 会改变
    /// `to_json()` 的输出形状，而 `to_json` 是 `repository.rs::save_state` 的活跃链路，
    /// 本轮明确不许动）。因此戳沿保存链路逐层显式传递：
    /// `main.rs`（`cli.resolver` / `cli.max_candidates`）→ `PersistenceManager::save_graph*`
    /// → `PetGraphStorageManager::save_to_*` → 本函数。
    ///
    /// `resolver_mode` 只接受 [`crate::cli::args::ResolverMode::as_str`] 产出的稳定名，
    /// `max_candidates` 只接受 [`clamp_max_candidates`] 的折算结果。
    pub fn from_petgraph(code_graph: &PetCodeGraph, resolver_mode: &str, max_candidates: u16) -> Self {
        let functions: Vec<FunctionInfo> = code_graph.get_all_functions().into_iter().cloned().collect();
        let call_relations: Vec<CallRelation> = code_graph.get_all_call_relations().into_iter().cloned().collect();
        
        Self {
            schema_version: GRAPH_SCHEMA_VERSION,
            resolver_mode: resolver_mode.to_string(),
            max_candidates,
            functions,
            call_relations,
            function_names: code_graph.function_names.clone(),
            file_functions: code_graph.file_functions.clone(),
            stats: code_graph.stats.clone(),
        }
    }

    /// 版本门禁：校验本存储结构的 `schema_version` 是否与当前 `GRAPH_SCHEMA_VERSION` 一致。
    ///
    /// 为什么门禁落在这里（而不是 `to_petgraph`）：`to_petgraph` 的既有返回类型是
    /// `PetCodeGraph`（不是 `Result`），它同时被 `traits::GraphSerializer` 的转发实现依赖；
    /// 为了传播一个版本错误去改它的签名，会连带改动 trait 与全部调用点，改动面远大于收益。
    /// 而 `PetGraphStorageManager` 的 `load_from_file` / `load_from_json` / `load_from_binary`
    /// 是**全仓唯一的反序列化咽喉**（`persistence.rs` 的 `load_graph_json` /
    /// `load_graph_binary` 都只走这三个入口），在这里校验：
    /// - 一处生效，JSON / bincode / trait 转发三条读路径全覆盖；
    /// - 返回类型本来就是 `Result<PetCodeGraph, String>`，版本不符 → `Err(String)`，
    ///   语义与"文件损坏"完全一致，天然复用上层"缓存失效 → 全量重建"的降级链。
    pub fn validate_schema_version(&self) -> Result<(), String> {
        if self.schema_version != GRAPH_SCHEMA_VERSION {
            return Err(format!(
                "图缓存 schema 版本不匹配：文件版本={}，当前版本={}；判定为缓存失效，将触发全量重建",
                self.schema_version, GRAPH_SCHEMA_VERSION
            ));
        }
        Ok(())
    }

    /// **解析档位门禁**：校验这份缓存是不是"当前档位 + 当前候选上限"建的。
    ///
    /// 与 `validate_schema_version` 的分工：版本门禁由三个 `load_*` 反序列化咽喉自动执行
    /// （那里拿不到"当前档位"，档位是**消费者**的配置），而档位门禁必须由知道本轮配置的
    /// 调用方执行 —— 生产上只有两处：
    ///  - `codegraph/parser.rs::CodeParser::_load_existing_graph`（用 `self.resolver_mode()`
    ///    与 `self.max_candidates()`），不通过则丢弃已加载的图、走既有 `Err → warn → Ok(None)`
    ///    的降级链 ⇒ 全量重建；
    ///  - `main.rs` 的 `init` 直接读缓存处（用 `cli.resolver` / `cli.max_candidates`），
    ///    那里原本会把旧档的图并进本轮结果，同样必须拒收。
    ///
    /// 判定是**严格相等**（不做"layered 兼容 legacy"这类偏序）：两档在同一份源码上产出的
    /// 边集本就不同，任何"混用"都是要防的语义风险本身。空串档位（无戳的旧文件）必然失配。
    pub fn validate_resolution_stamp(&self, expected_mode: &str, expected_max_candidates: u16) -> Result<(), String> {
        if self.resolver_mode != expected_mode {
            let stored = if self.resolver_mode.is_empty() {
                "未知（无档位戳的旧缓存）".to_string()
            } else {
                self.resolver_mode.clone()
            };
            return Err(format!(
                "图缓存解析档位不一致：缓存档位={stored}，当前档位={expected_mode}（候选上限={expected_max_candidates}）；\
                 判定为缓存失效，将触发全量重建（换 --resolver 必须换一份干净的图）"
            ));
        }
        if self.max_candidates != expected_max_candidates {
            return Err(format!(
                "图缓存候选上限不一致：缓存 --max-candidates={}，当前={}（档位={expected_mode}）；\
                 判定为缓存失效，将触发全量重建（截断条数会改变落盘边数，旧边不能沿用）",
                self.max_candidates, expected_max_candidates
            ));
        }
        Ok(())
    }

    /// 转换为PetCodeGraph
    ///
    /// 注意：本方法**不做版本校验**（签名返回 `PetCodeGraph`，无法传播错误）；
    /// 校验在三个 `load_*` 反序列化入口完成，见 `validate_schema_version` 的注释。
    /// 若新增绕过 `PetGraphStorageManager` 的直接反序列化路径，必须自行调用门禁。
    pub fn to_petgraph(&self) -> PetCodeGraph {
        let mut code_graph = PetCodeGraph::new();
        
        // 添加所有函数
        for function in &self.functions {
            code_graph.add_function(function.clone());
        }
        
        // 添加所有调用关系
        for relation in &self.call_relations {
            if let Err(_e) = code_graph.add_call_relation(relation.clone()) {
                // Ignore errors for now
            }
        }
        
        // 恢复映射和统计信息
        code_graph.function_names = self.function_names.clone();
        code_graph.file_functions = self.file_functions.clone();
        code_graph.stats = self.stats.clone();
        
        code_graph
    }
}

/// petgraph代码图存储管理器
pub struct PetGraphStorageManager;

impl PetGraphStorageManager {
    /// 保存代码图到文件
    pub fn save_to_file(code_graph: &PetCodeGraph, file_path: &Path, resolver_mode: &str, max_candidates: u16) -> Result<(), String> {
        let storage = PetGraphStorage::from_petgraph(code_graph, resolver_mode, max_candidates);
        let json = serde_json::to_string_pretty(&storage)
            .map_err(|e| format!("Failed to serialize code graph: {}", e))?;
        
        // 原子写：内部错误信息已带完整路径，这里直接上抛，避免重复前缀
        write_bytes_atomically(file_path, json.as_bytes())?;
        
        Ok(())
    }

    /// 从文件读回**存储结构本身**（已做版本门禁）。
    ///
    /// 为什么额外要有这一层（而不是只保留 `load_from_file`）：解析档位戳在
    /// `PetGraphStorage` 上，`to_petgraph()` 之后就拿不到了（`PetCodeGraph` 不带戳，
    /// 见 `from_petgraph` 的注释）。而档位是否可用只有**消费者**知道（它才是"当前档位"
    /// 的来源），所以消费者需要的是存储结构 + `validate_resolution_stamp`，
    /// `load_from_*` 则继续作为"只要图、不关心档位"的既有入口（trait 转发、单测、
    /// `PersistenceManager::load_graph` 都走它）。
    /// 两条路径共用同一段反序列化代码，不存在第二份"读缓存"实现。
    pub fn read_storage_from_file(file_path: &Path) -> Result<PetGraphStorage, String> {
        let json = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;
        Self::read_storage_from_json(&json)
    }

    /// 从文件加载代码图
    pub fn load_from_file(file_path: &Path) -> Result<PetCodeGraph, String> {
        Ok(Self::read_storage_from_file(file_path)?.to_petgraph())
    }

    /// 保存代码图到JSON字符串
    pub fn save_to_json(code_graph: &PetCodeGraph, resolver_mode: &str, max_candidates: u16) -> Result<String, String> {
        let storage = PetGraphStorage::from_petgraph(code_graph, resolver_mode, max_candidates);
        serde_json::to_string_pretty(&storage)
            .map_err(|e| format!("Failed to serialize code graph: {}", e))
    }

    /// 从JSON字符串读回存储结构（版本门禁落在这里，理由同 `read_storage_from_file`）
    pub fn read_storage_from_json(json_str: &str) -> Result<PetGraphStorage, String> {
        let storage: PetGraphStorage = serde_json::from_str(json_str)
            .map_err(|e| format!("Failed to deserialize code graph: {}", e))?;
        // 版本门禁：schema_version 不符（含旧缓存缺字段被 `serde(default)` 补成 0）
        // 一律返回 Err(String)，与"文件损坏"完全同语义 —— 上层会把它降级为
        // "缓存 miss → 全量重建"，绝不 panic。
        storage.validate_schema_version()?;
        Ok(storage)
    }

    /// 从JSON字符串加载代码图
    pub fn load_from_json(json_str: &str) -> Result<PetCodeGraph, String> {
        Ok(Self::read_storage_from_json(json_str)?.to_petgraph())
    }

    /// 保存代码图为二进制格式
    ///
    /// 落盘布局：`magic("CGSP") | schema_version(u32 小端) | bincode(PetGraphStorage)`。
    /// 文件头是**第一重**版本门禁：加载器可以在不解析载荷的前提下判定格式是否可用。
    /// 载荷内的 `PetGraphStorage.schema_version`（结构体首字段）是**第二重**门禁。
    pub fn save_to_binary(code_graph: &PetCodeGraph, file_path: &Path, resolver_mode: &str, max_candidates: u16) -> Result<(), String> {
        let storage = PetGraphStorage::from_petgraph(code_graph, resolver_mode, max_candidates);
        let payload = bincode::serialize(&storage)
            .map_err(|e| format!("Failed to serialize code graph: {}", e))?;

        let mut bytes = binary_header_bytes(GRAPH_SCHEMA_VERSION).to_vec();
        bytes.extend_from_slice(&payload);

        // 原子写：内部错误信息已带完整路径，这里直接上抛，避免重复前缀
        write_bytes_atomically(file_path, &bytes)?;

        Ok(())
    }

    /// 从二进制文件读回存储结构（三层门禁，全部返回 `Err(String)` = 缓存失效）
    ///
    /// 三层判定加上"档位戳"一共四层，全部只做判定、不做任何 unwrap：
    /// 1. 文件头长度/magic 不符 —— P0 之前遗留的裸 bincode 旧缓存正是这一类；
    ///    **必须在读载荷之前拦掉**，否则错位的字节会被当成 `Vec` 长度而疯狂分配内存；
    /// 2. 文件头里的 `schema_version` 与当前常量不符（P3 的 2→3 正是在这一层被拦住）；
    /// 3. 载荷解码成功后，再校验载荷自身的 `schema_version`（防绕过文件头的构造数据）；
    /// 4. （由消费者执行）`validate_resolution_stamp` —— 见 `PetGraphStorage` 上的注释。
    pub fn read_storage_from_binary(file_path: &Path) -> Result<PetGraphStorage, String> {
        let binary = fs::read(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;

        if binary.len() < GRAPH_BINARY_HEADER_LEN {
            return Err(format!(
                "图缓存文件 {} 过短（{} 字节，不足 {} 字节文件头）；判定为缓存失效，将触发全量重建",
                file_path.display(), binary.len(), GRAPH_BINARY_HEADER_LEN
            ));
        }
        if &binary[..4] != GRAPH_BINARY_MAGIC {
            warn!("图缓存 {} 缺少版本文件头（旧格式或已损坏）；判定为缓存失效", file_path.display());
            return Err(format!(
                "图缓存文件 {} 不是受支持的格式（缺少 magic {:?}，可能是 schema_version 引入前的旧缓存）；判定为缓存失效，将触发全量重建",
                file_path.display(), GRAPH_BINARY_MAGIC
            ));
        }
        let header_version = u32::from_le_bytes([binary[0 + 4], binary[1 + 4], binary[2 + 4], binary[3 + 4]]);
        if header_version != GRAPH_SCHEMA_VERSION {
            return Err(format!(
                "图缓存 schema 版本不匹配：文件版本={}，当前版本={}；判定为缓存失效，将触发全量重建",
                header_version, GRAPH_SCHEMA_VERSION
            ));
        }

        let storage: PetGraphStorage = bincode::deserialize(&binary[GRAPH_BINARY_HEADER_LEN..])
            .map_err(|e| format!("Failed to deserialize code graph: {}", e))?;
        // 第二重门禁：载荷自带的版本号同样必须一致
        storage.validate_schema_version()?;

        Ok(storage)
    }

    /// 从二进制文件加载代码图
    pub fn load_from_binary(file_path: &Path) -> Result<PetCodeGraph, String> {
        Ok(Self::read_storage_from_binary(file_path)?.to_petgraph())
    }

    /// 导出为GraphML格式（用于可视化工具）
    pub fn export_to_graphml(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String> {
        let mut graphml = String::new();
        graphml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        graphml.push_str("<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n");
        
        // 定义节点属性
        graphml.push_str("  <key id=\"name\" for=\"node\" attr.name=\"name\" attr.type=\"string\"/>\n");
        graphml.push_str("  <key id=\"file\" for=\"node\" attr.name=\"file\" attr.type=\"string\"/>\n");
        graphml.push_str("  <key id=\"language\" for=\"node\" attr.name=\"language\" attr.type=\"string\"/>\n");
        graphml.push_str("  <key id=\"line_start\" for=\"node\" attr.name=\"line_start\" attr.type=\"int\"/>\n");
        graphml.push_str("  <key id=\"line_end\" for=\"node\" attr.name=\"line_end\" attr.type=\"int\"/>\n");
        
        // 定义边属性
        graphml.push_str("  <key id=\"line_number\" for=\"edge\" attr.name=\"line_number\" attr.type=\"int\"/>\n");
        graphml.push_str("  <key id=\"is_resolved\" for=\"edge\" attr.name=\"is_resolved\" attr.type=\"boolean\"/>\n");
        
        graphml.push_str("  <graph id=\"codegraph\" edgedefault=\"directed\">\n");
        
        // 添加节点
        for (node_index, function) in code_graph.graph.node_indices().zip(code_graph.graph.node_weights()) {
            graphml.push_str(&format!("    <node id=\"n{}\">\n", node_index.index()));
            graphml.push_str(&format!("      <data key=\"name\">{}</data>\n", function.name));
            graphml.push_str(&format!("      <data key=\"file\">{}</data>\n", function.file_path.display()));
            graphml.push_str(&format!("      <data key=\"language\">{}</data>\n", function.language));
            graphml.push_str(&format!("      <data key=\"line_start\">{}</data>\n", function.line_start));
            graphml.push_str(&format!("      <data key=\"line_end\">{}</data>\n", function.line_end));
            graphml.push_str("    </node>\n");
        }
        
        // 添加边
        for (edge_index, edge) in code_graph.graph.edge_indices().zip(code_graph.graph.edge_weights()) {
            if let Some((source, target)) = code_graph.graph.edge_endpoints(edge_index) {
                graphml.push_str(&format!("    <edge id=\"e{}\" source=\"n{}\" target=\"n{}\">\n", 
                    edge_index.index(), source.index(), target.index()));
                graphml.push_str(&format!("      <data key=\"line_number\">{}</data>\n", edge.line_number));
                graphml.push_str(&format!("      <data key=\"is_resolved\">{}</data>\n", edge.is_resolved));
                graphml.push_str("    </edge>\n");
            }
        }
        
        graphml.push_str("  </graph>\n");
        graphml.push_str("</graphml>\n");
        
        fs::write(file_path, graphml)
            .map_err(|e| format!("Failed to write GraphML file {}: {}", file_path.display(), e))?;
        
        Ok(())
    }

    /// 导出为GEXF格式（用于Gephi等工具）
    pub fn export_to_gexf(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String> {
        let mut gexf = String::new();
        gexf.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        gexf.push_str("<gexf xmlns=\"http://www.gexf.net/1.3\" version=\"1.3\">\n");
        gexf.push_str("  <meta lastmodifieddate=\"2024-01-01\">\n");
        gexf.push_str("    <creator>CodeGraph Exporter</creator>\n");
        gexf.push_str("    <description>Code dependency graph</description>\n");
        gexf.push_str("  </meta>\n");
        gexf.push_str("  <graph mode=\"static\" defaultedgetype=\"directed\">\n");
        
        // 定义节点属性
        gexf.push_str("    <attributes class=\"node\">\n");
        gexf.push_str("      <attribute id=\"0\" title=\"name\" type=\"string\"/>\n");
        gexf.push_str("      <attribute id=\"1\" title=\"file\" type=\"string\"/>\n");
        gexf.push_str("      <attribute id=\"2\" title=\"language\" type=\"string\"/>\n");
        gexf.push_str("      <attribute id=\"3\" title=\"line_start\" type=\"integer\"/>\n");
        gexf.push_str("      <attribute id=\"4\" title=\"line_end\" type=\"integer\"/>\n");
        gexf.push_str("    </attributes>\n");
        
        // 定义边属性
        gexf.push_str("    <attributes class=\"edge\">\n");
        gexf.push_str("      <attribute id=\"0\" title=\"line_number\" type=\"integer\"/>\n");
        gexf.push_str("      <attribute id=\"1\" title=\"is_resolved\" type=\"boolean\"/>\n");
        gexf.push_str("    </attributes>\n");
        
        gexf.push_str("    <nodes>\n");
        
        // 添加节点
        for (node_index, function) in code_graph.graph.node_indices().zip(code_graph.graph.node_weights()) {
            gexf.push_str(&format!("      <node id=\"{}\" label=\"{}\">\n", node_index.index(), function.name));
            gexf.push_str("        <attvalues>\n");
            gexf.push_str(&format!("          <attvalue for=\"0\" value=\"{}\"/>\n", function.name));
            gexf.push_str(&format!("          <attvalue for=\"1\" value=\"{}\"/>\n", function.file_path.display()));
            gexf.push_str(&format!("          <attvalue for=\"2\" value=\"{}\"/>\n", function.language));
            gexf.push_str(&format!("          <attvalue for=\"3\" value=\"{}\"/>\n", function.line_start));
            gexf.push_str(&format!("          <attvalue for=\"4\" value=\"{}\"/>\n", function.line_end));
            gexf.push_str("        </attvalues>\n");
            gexf.push_str("      </node>\n");
        }
        
        gexf.push_str("    </nodes>\n");
        gexf.push_str("    <edges>\n");
        
        // 添加边
        for (edge_index, edge) in code_graph.graph.edge_indices().zip(code_graph.graph.edge_weights()) {
            if let Some((source, target)) = code_graph.graph.edge_endpoints(edge_index) {
                gexf.push_str(&format!("      <edge id=\"{}\" source=\"{}\" target=\"{}\">\n", 
                    edge_index.index(), source.index(), target.index()));
                gexf.push_str("        <attvalues>\n");
                gexf.push_str(&format!("          <attvalue for=\"0\" value=\"{}\"/>\n", edge.line_number));
                gexf.push_str(&format!("          <attvalue for=\"1\" value=\"{}\"/>\n", edge.is_resolved));
                gexf.push_str("        </attvalues>\n");
                gexf.push_str("      </edge>\n");
            }
        }
        
        gexf.push_str("    </edges>\n");
        gexf.push_str("  </graph>\n");
        gexf.push_str("</gexf>\n");
        
        fs::write(file_path, gexf)
            .map_err(|e| format!("Failed to write GEXF file {}: {}", file_path.display(), e))?;
        
        Ok(())
    }
} 

impl crate::storage::traits::GraphSerializer for PetGraphStorageManager {
    fn save_to_file(code_graph: &PetCodeGraph, file_path: &std::path::Path, resolver_mode: &str, max_candidates: u16) -> Result<(), String> {
        Self::save_to_file(code_graph, file_path, resolver_mode, max_candidates)
    }

    fn load_from_file(file_path: &std::path::Path) -> Result<PetCodeGraph, String> {
        Self::load_from_file(file_path)
    }

    fn save_to_json(code_graph: &PetCodeGraph, resolver_mode: &str, max_candidates: u16) -> Result<String, String> {
        Self::save_to_json(code_graph, resolver_mode, max_candidates)
    }

    fn load_from_json(json_str: &str) -> Result<PetCodeGraph, String> {
        Self::load_from_json(json_str)
    }

    fn save_to_binary(code_graph: &PetCodeGraph, file_path: &std::path::Path, resolver_mode: &str, max_candidates: u16) -> Result<(), String> {
        Self::save_to_binary(code_graph, file_path, resolver_mode, max_candidates)
    }

    fn load_from_binary(file_path: &std::path::Path) -> Result<PetCodeGraph, String> {
        Self::load_from_binary(file_path)
    }

    fn export_to_graphml(code_graph: &PetCodeGraph, file_path: &std::path::Path) -> Result<(), String> {
        Self::export_to_graphml(code_graph, file_path)
    }

    fn export_to_gexf(code_graph: &PetCodeGraph, file_path: &std::path::Path) -> Result<(), String> {
        Self::export_to_gexf(code_graph, file_path)
    }
}

#[cfg(test)]
mod tests {
    //! 图缓存 schema 版本门禁 + 两种格式往返一致性的单元测试。
    //!
    //! 这些用例是 P0「分层精度名称解析」改造的安全带：
    //! - 往返用例保证「改了存储结构就一定会被逐字段比对抓到」；
    //! - 版本不符用例保证「旧缓存只会被判为 miss，绝不会 panic、也绝不会被错位读成脏数据」。

    use super::*;

    /// 本模块统一使用的"解析档位戳"实参。
    ///
    /// Phase P3 收口：`from_petgraph` / `save_to_json` / `save_to_file` / `save_to_binary`
    /// 都强制要求传入本轮的档位（`resolver_mode`）与候选上限（`max_candidates`），
    /// 单元测试因此必须显式交代这两个值 —— 而不是留一个"默认戳"把契约淡化掉：
    /// 戳一旦缺失，读回时 `validate_resolution_stamp` 必然判失效 ⇒ 每次全量重建。
    const TEST_STAMP_MODE: &str = "layered";
    const TEST_STAMP_MAX: u16 = 8;

    /// 构造一个「字段尽量填满」的样例图：
    /// 4 个函数节点（其中 1 个是未解析调用的临时幽灵节点）分布在 2 个文件，
    /// 2 条已解析边（含 1 条跨文件边）+ 1 条未解析边。
    /// 三条边的行号互不相同 —— 否则会命中 `add_call_relation` 的
    /// `(caller, callee, line_number)` 去重，边会被静默吞掉。
    fn sample_graph() -> PetCodeGraph {
        let mut graph = PetCodeGraph::new();

        let alpha = FunctionInfo {
            id: Uuid::new_v4(),
            name: "alpha".to_string(),
            file_path: PathBuf::from("src/alpha.rs"),
            line_start: 1,
            line_end: 3,
            namespace: "crate".to_string(),
            language: "rust".to_string(),
            signature: Some("alpha".to_string()),
            module_path: None,
            qualified_name: None,
        };
        let beta = FunctionInfo {
            id: Uuid::new_v4(),
            name: "beta".to_string(),
            file_path: PathBuf::from("src/alpha.rs"),
            line_start: 5,
            line_end: 9,
            namespace: "crate".to_string(),
            language: "rust".to_string(),
            signature: Some("beta".to_string()),
            module_path: None,
            qualified_name: None,
        };
        let gamma = FunctionInfo {
            id: Uuid::new_v4(),
            name: "gamma".to_string(),
            file_path: PathBuf::from("lib/beta.py"),
            line_start: 2,
            line_end: 7,
            namespace: "global".to_string(),
            language: "python".to_string(),
            signature: Some("gamma".to_string()),
            module_path: None,
            qualified_name: None,
        };

        let ghost = FunctionInfo {
            id: Uuid::new_v4(),
            name: "unknown_callee".to_string(),
            file_path: PathBuf::from("src/alpha.rs"),
            line_start: 2,
            line_end: 2,
            namespace: "unresolved".to_string(),
            language: "rust".to_string(),
            signature: Some("unresolved_call_unknown_callee".to_string()),
            module_path: None,
            qualified_name: None,
        };
        let ghost_id = ghost.id;

        let (alpha_id, beta_id, gamma_id) = (alpha.id, beta.id, gamma.id);
        graph.add_function(alpha);
        graph.add_function(beta);
        graph.add_function(gamma);
        // 未解析边的 callee 必须先作为（临时）节点存在：`add_call_relation` 要求
        // caller / callee 两端都能在 function_to_node 里查到，这正是 parser
        // `_create_unresolved_call_relation` 里"先造幽灵节点再连边"的原因。
        graph.add_function(ghost);

        graph
            .add_call_relation(CallRelation {
                caller_id: beta_id,
                callee_id: alpha_id,
                caller_name: "beta".to_string(),
                callee_name: "alpha".to_string(),
                caller_file: PathBuf::from("src/alpha.rs"),
                callee_file: PathBuf::from("src/alpha.rs"),
                line_number: 7,
                is_resolved: true,
                resolution_level: 0,
                confidence: 0,
                rank: 0,
                candidate_count: 0,
            })
            .expect("同文件已解析边应能写入");
        graph
            .add_call_relation(CallRelation {
                caller_id: gamma_id,
                callee_id: alpha_id,
                caller_name: "gamma".to_string(),
                callee_name: "alpha".to_string(),
                caller_file: PathBuf::from("lib/beta.py"),
                callee_file: PathBuf::from("src/alpha.rs"),
                line_number: 4,
                is_resolved: true,
                resolution_level: 0,
                confidence: 0,
                rank: 0,
                candidate_count: 0,
            })
            .expect("跨文件已解析边应能写入");
        graph
            .add_call_relation(CallRelation {
                caller_id: alpha_id,
                callee_id: ghost_id,
                caller_name: "alpha".to_string(),
                callee_name: "unknown_callee".to_string(),
                caller_file: PathBuf::from("src/alpha.rs"),
                callee_file: PathBuf::from("src/alpha.rs"),
                line_number: 2,
                is_resolved: false,
                resolution_level: 0,
                confidence: 0,
                rank: 0,
                candidate_count: 0,
            })
            .expect("未解析边（指向临时幽灵节点）的写入应成功");

        graph
    }

    /// 函数集合规范化：只用语义字段，绝不使用节点索引 / HashMap 迭代顺序。
    fn norm_functions(functions: &[FunctionInfo]) -> Vec<(String, String, usize, usize, String, String)> {
        let mut rows: Vec<(String, String, usize, usize, String, String)> = functions
            .iter()
            .map(|f| {
                (
                    f.file_path.to_string_lossy().to_string(),
                    f.name.clone(),
                    f.line_start,
                    f.line_end,
                    f.namespace.clone(),
                    f.language.clone(),
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// 边集合规范化：caller/callee 的 Uuid 翻译成 `(文件, 函数名)` 再排序。
    fn norm_edges(graph: &PetCodeGraph) -> Vec<(String, String, String, usize, bool)> {
        let describe = |id: &Uuid| match graph.get_function_by_id(id) {
            Some(f) => format!("{}::{}", f.file_path.display(), f.name),
            None => "<missing>".to_string(),
        };

        let mut rows: Vec<(String, String, String, usize, bool)> = graph
            .graph
            .edge_references()
            .map(|edge| {
                let rel = edge.weight();
                (
                    describe(&rel.caller_id),
                    describe(&rel.callee_id),
                    rel.callee_name.clone(),
                    rel.line_number,
                    rel.is_resolved,
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// `函数名 -> Uuid 列表` 索引规范化（Uuid 转字符串后排序，避免依赖 id 生成顺序）。
    fn norm_uuid_index(map: &HashMap<String, Vec<Uuid>>) -> Vec<(String, Vec<String>)> {
        let mut rows: Vec<(String, Vec<String>)> = map
            .iter()
            .map(|(key, ids)| {
                let mut sorted: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
                sorted.sort();
                (key.clone(), sorted)
            })
            .collect();
        rows.sort();
        rows
    }

    /// `文件路径 -> Uuid 列表` 索引规范化。
    fn norm_file_index(map: &HashMap<PathBuf, Vec<Uuid>>) -> Vec<(String, Vec<String>)> {
        let mut rows: Vec<(String, Vec<String>)> = map
            .iter()
            .map(|(path, ids)| {
                let mut sorted: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
                sorted.sort();
                (path.to_string_lossy().to_string(), sorted)
            })
            .collect();
        rows.sort();
        rows
    }

    /// stats 逐字段比对（`languages` 是 HashMap，同样先排序）。
    fn assert_stats_eq(actual: &CodeGraphStats, expected: &CodeGraphStats, context: &str) {
        assert_eq!(actual.total_functions, expected.total_functions, "{context}: total_functions");
        assert_eq!(actual.total_files, expected.total_files, "{context}: total_files");
        assert_eq!(actual.total_languages, expected.total_languages, "{context}: total_languages");
        assert_eq!(actual.resolved_calls, expected.resolved_calls, "{context}: resolved_calls");
        assert_eq!(actual.unresolved_calls, expected.unresolved_calls, "{context}: unresolved_calls");

        let mut actual_languages: Vec<(String, usize)> = actual.languages.iter().map(|(k, v)| (k.clone(), *v)).collect();
        let mut expected_languages: Vec<(String, usize)> = expected.languages.iter().map(|(k, v)| (k.clone(), *v)).collect();
        actual_languages.sort();
        expected_languages.sort();
        assert_eq!(actual_languages, expected_languages, "{context}: languages");
    }

    /// 一次往返之后的全量校验：函数 / 边 / 三个索引 / stats 全部逐字段一致。
    fn assert_graphs_equivalent(restored: &PetCodeGraph, original: &PetCodeGraph) {
        assert_eq!(
            norm_functions(&restored.get_all_functions().into_iter().cloned().collect::<Vec<_>>()),
            norm_functions(&original.get_all_functions().into_iter().cloned().collect::<Vec<_>>()),
            "往返后函数集合必须一致"
        );
        assert_eq!(norm_edges(restored), norm_edges(original), "往返后调用边集合必须一致");
        assert_eq!(
            norm_uuid_index(&restored.function_names),
            norm_uuid_index(&original.function_names),
            "往返后 function_names 索引必须一致"
        );
        assert_eq!(
            norm_file_index(&restored.file_functions),
            norm_file_index(&original.file_functions),
            "往返后 file_functions 索引必须一致"
        );
        assert_stats_eq(&restored.stats, &original.stats, "往返后 stats");
        assert_eq!(restored.graph.node_count(), original.graph.node_count(), "往返后节点数");
        assert_eq!(restored.graph.edge_count(), original.graph.edge_count(), "往返后边数");
    }

    #[test]
    fn test_from_petgraph_writes_current_schema_version() {
        let graph = sample_graph();
        let storage = PetGraphStorage::from_petgraph(&graph, TEST_STAMP_MODE, TEST_STAMP_MAX);

        // 版本字段必须由 from_petgraph 自动写入当前常量，任何调用方都不该手填
        assert_eq!(storage.schema_version, GRAPH_SCHEMA_VERSION);
        // 顺带锁住样例图的规模，避免 sample_graph() 被改动后测试失去意义
        assert_eq!(storage.functions.len(), 4);
        assert_eq!(storage.call_relations.len(), 3);
    }

    #[test]
    fn test_json_roundtrip_preserves_all_fields() {
        let graph = sample_graph();
        let original_storage = PetGraphStorage::from_petgraph(&graph, TEST_STAMP_MODE, TEST_STAMP_MAX);

        let json = PetGraphStorageManager::save_to_json(&graph, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("JSON 序列化应成功");
        // JSON 是自描述格式：版本必须以可见的键存在
        let value: serde_json::Value = serde_json::from_str(&json).expect("save_to_json 必须产出合法 JSON");
        assert_eq!(
            value.get("schema_version").and_then(|v| v.as_u64()),
            Some(GRAPH_SCHEMA_VERSION as u64),
            "JSON 缓存里必须带 schema_version 键"
        );

        let restored = PetGraphStorageManager::load_from_json(&json).expect("当前版本 JSON 应加载成功");
        assert_graphs_equivalent(&restored, &graph);

        // 往返之后重新落盘，存储结构本身也要逐字段一致
        let restored_storage = PetGraphStorage::from_petgraph(&restored, TEST_STAMP_MODE, TEST_STAMP_MAX);
        assert_eq!(restored_storage.schema_version, original_storage.schema_version);
        assert_eq!(
            norm_functions(&restored_storage.functions),
            norm_functions(&original_storage.functions)
        );
        assert_eq!(restored_storage.call_relations.len(), original_storage.call_relations.len());
    }

    #[test]
    fn test_file_json_roundtrip_preserves_all_fields() {
        let dir = tempfile::TempDir::new().expect("创建临时目录失败");
        let path = dir.path().join("graph.json");
        let graph = sample_graph();

        PetGraphStorageManager::save_to_file(&graph, &path, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("JSON 落盘应成功");
        let restored = PetGraphStorageManager::load_from_file(&path).expect("当前版本 JSON 文件应加载成功");
        assert_graphs_equivalent(&restored, &graph);
    }

    #[test]
    fn test_binary_roundtrip_preserves_all_fields_and_has_version_header() {
        let dir = tempfile::TempDir::new().expect("创建临时目录失败");
        let path = dir.path().join("graph.bin");
        let graph = sample_graph();

        PetGraphStorageManager::save_to_binary(&graph, &path, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("bincode 落盘应成功");

        // 文件头必须是 magic + 当前版本的小端编码
        let bytes = fs::read(&path).expect("读回缓存文件失败");
        assert_eq!(&bytes[..4], b"CGSP", "graph.bin 必须以 CGSP magic 开头");
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            GRAPH_SCHEMA_VERSION,
            "graph.bin 文件头里的版本必须等于 GRAPH_SCHEMA_VERSION"
        );

        let restored = PetGraphStorageManager::load_from_binary(&path).expect("当前版本 bincode 应加载成功");
        assert_graphs_equivalent(&restored, &graph);
    }

    #[test]
    fn test_json_schema_version_mismatch_is_rejected_as_cache_miss() {
        let graph = sample_graph();
        let json = PetGraphStorageManager::save_to_json(&graph, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("JSON 序列化应成功");

        // 伪造一个「未来版本」的缓存（模拟后续阶段加了字段但忘了递增旧缓存）
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("save_to_json 必须产出合法 JSON");
        let future_version = GRAPH_SCHEMA_VERSION + 100;
        value["schema_version"] = serde_json::Value::from(future_version);
        let tampered = serde_json::to_string(&value).expect("改写后的 JSON 序列化失败");

        let result = PetGraphStorageManager::load_from_json(&tampered);

        // 关键：返回 Err（=缓存 miss 语义），而不是 panic / unwrap 崩掉
        let err = result.expect_err("版本不符的 JSON 缓存必须被门禁拒绝");
        assert!(
            err.contains("schema 版本不匹配"),
            "错误信息必须点名版本不匹配，实际为：{err}"
        );
        assert!(err.contains(&GRAPH_SCHEMA_VERSION.to_string()), "错误信息应带上当前版本，实际为：{err}");
    }

    #[test]
    fn test_json_without_schema_version_field_is_rejected() {
        let graph = sample_graph();
        let json = PetGraphStorageManager::save_to_json(&graph, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("JSON 序列化应成功");

        // 模拟「P0 之前遗留的旧 JSON 缓存」：完全没有 schema_version 键。
        // serde(default) 会把它补成 0，0 != GRAPH_SCHEMA_VERSION，因此必须被门禁拒掉。
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("save_to_json 必须产出合法 JSON");
        assert!(value.as_object_mut().and_then(|obj| obj.remove("schema_version")).is_some());
        let legacy = serde_json::to_string(&value).expect("删键后的 JSON 序列化失败");

        let err = PetGraphStorageManager::load_from_json(&legacy)
            .expect_err("缺版本字段的旧 JSON 缓存必须被判为失效");
        assert!(err.contains("schema 版本不匹配"), "旧 JSON 应由门禁给出明确原因，实际为：{err}");
    }

    #[test]
    fn test_binary_header_version_mismatch_is_rejected_as_cache_miss() {
        let dir = tempfile::TempDir::new().expect("创建临时目录失败");
        let path = dir.path().join("graph.bin");
        let graph = sample_graph();
        PetGraphStorageManager::save_to_binary(&graph, &path, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("bincode 落盘应成功");

        // 只改写文件头里的版本号，载荷保持原样 —— 证明门禁在解码载荷之前就生效
        let mut bytes = fs::read(&path).expect("读回缓存文件失败");
        let future_version = (GRAPH_SCHEMA_VERSION + 100).to_le_bytes();
        bytes[4..8].copy_from_slice(&future_version);
        fs::write(&path, &bytes).expect("写回篡改文件失败");

        let err = PetGraphStorageManager::load_from_binary(&path)
            .expect_err("版本头不符的 bincode 缓存必须被门禁拒绝");
        assert!(err.contains("schema 版本不匹配"), "实际错误：{err}");
    }

    #[test]
    fn test_legacy_raw_bincode_without_header_is_rejected_before_payload_parse() {
        let dir = tempfile::TempDir::new().expect("创建临时目录失败");
        let path = dir.path().join("graph.bin");
        let graph = sample_graph();
        PetGraphStorageManager::save_to_binary(&graph, &path, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("bincode 落盘应成功");

        // 剥掉 8 字节文件头，就得到「P0 之前」的裸 bincode 缓存形状。
        // 这种文件绝不能进 bincode 载荷解析：错位的字节会被当作 Vec 长度而分配天文数字内存。
        let bytes = fs::read(&path).expect("读回缓存文件失败");
        let legacy_path = dir.path().join("graph_legacy.bin");
        fs::write(&legacy_path, &bytes[GRAPH_BINARY_HEADER_LEN..]).expect("写旧格式文件失败");

        let err = PetGraphStorageManager::load_from_binary(&legacy_path)
            .expect_err("无文件头的旧 bincode 缓存必须被拒绝");
        assert!(err.contains("不是受支持的格式"), "实际错误：{err}");

        // 顺带覆盖「过短文件」这一类损坏输入：同样只能是 Err，不许 panic
        let short_path = dir.path().join("graph_short.bin");
        fs::write(&short_path, &bytes[..3]).expect("写过短文件失败");
        let err = PetGraphStorageManager::load_from_binary(&short_path)
            .expect_err("过短的缓存文件必须被判为失效");
        assert!(err.contains("过短"), "实际错误：{err}");
    }

    #[test]
    fn test_validate_schema_version_gate_on_storage_struct() {
        let graph = sample_graph();
        let mut storage = PetGraphStorage::from_petgraph(&graph, TEST_STAMP_MODE, TEST_STAMP_MAX);

        assert!(storage.validate_schema_version().is_ok(), "当前版本必须通过门禁");

        storage.schema_version = GRAPH_SCHEMA_VERSION + 100;
        let err = storage.validate_schema_version().expect_err("未来版本必须被拒绝");
        assert!(err.contains("schema 版本不匹配"), "实际错误：{err}");

        storage.schema_version = 0;
        assert!(storage.validate_schema_version().is_err(), "0（未知/旧格式）必须被拒绝");
    }

    /// 原子写：并发"覆写 + 读取"时，读者只能看到**完整**的旧文件或新文件，
    /// 绝不会看到被截断的半截缓存 —— 这正是 `~/.codeseek/projects/<id>/graph.bin`
    /// 这个全局共享路径上原先 `fs::write` 就地截断带来的偶发假阳性根因。
    #[test]
    fn test_concurrent_rewrite_never_exposes_torn_file() {
        let dir = tempfile::TempDir::new().expect("创建临时目录失败");
        let path = dir.path().join("graph.bin");

        let small = sample_graph();
        PetGraphStorageManager::save_to_binary(&small, &path, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("首次落盘应成功");
        let small_len = fs::read(&path).expect("读回失败").len();

        // 造一个明显更大的图，让"截断重写"的窗口足够宽
        let mut large = sample_graph();
        for index in 0..400 {
            large.add_function(FunctionInfo {
                id: Uuid::new_v4(),
                name: format!("filler_{index}"),
                file_path: PathBuf::from(format!("src/filler_{index}.rs")),
                line_start: index + 100,
                line_end: index + 102,
                namespace: "crate".to_string(),
                language: "rust".to_string(),
                signature: None,
                module_path: None,
                qualified_name: None,
            });
        }
        large.update_stats();
        // 先量出"大图"的完整尺寸（写到旁边的路径，不干扰被测文件）
        let side_path = dir.path().join("sized.bin");
        PetGraphStorageManager::save_to_binary(&large, &side_path, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("测量尺寸落盘应成功");
        let large_len = fs::read(&side_path).expect("读回尺寸样本失败").len();
        assert!(
            large_len > small_len + 1000,
            "样例规模不足以拉开尺寸差：small={small_len}, large={large_len}"
        );

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_flag = std::sync::Arc::clone(&stop);
        let reader_path = path.clone();

        let reader = std::thread::spawn(move || {
            let mut torn_sizes: Vec<usize> = Vec::new();
            while !reader_flag.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(bytes) = fs::read(&reader_path) {
                    // 读到的字节长度只可能是两个"完整文件"之一；出现第三种长度就是半截文件。
                    // 用长度判定而不是就地解码，避免"读完之后、校验之前"文件又被换掉的竞态。
                    if bytes.len() != small_len && bytes.len() != large_len {
                        torn_sizes.push(bytes.len());
                    }
                }
            }
            torn_sizes
        });

        for _ in 0..8 {
            PetGraphStorageManager::save_to_binary(&large, &path, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("反复覆写大图应成功");
            PetGraphStorageManager::save_to_binary(&small, &path, TEST_STAMP_MODE, TEST_STAMP_MAX).expect("反复覆写小图应成功");
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let torn_sizes = reader.join().expect("读者线程不应 panic");

        assert!(
            torn_sizes.is_empty(),
            "读者线程观察到 {} 种非法长度（半截缓存文件），说明落盘不是原子的：{:?}",
            torn_sizes.len(),
            torn_sizes
        );

        // 目录里不应残留临时文件
        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .expect("读目录失败")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "原子写后不应残留临时文件：{leftovers:?}");
    }
}
