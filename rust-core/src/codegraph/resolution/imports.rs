//! Phase P1 交付物 3：单文件 import 绑定表（`ImportMap` / `ImportEntry`）。
//!
//! ## 为什么需要它
//! 当前 import 信息在建图链路上**完全没被读过**：`parser.rs:228` 与 `parser.rs:522` 这两个
//! `symbol_type()` 匹配的 `_ => {}` 兜底分支把 `ImportDeclaration` 吞掉了，
//! 于是 `from mod_x import func as alias` 之后调用 `alias()` 只能得到一个
//! `namespace="unresolved"` 的幽灵节点（P0 基线第 2 项实测就是这样）。
//! `ImportMap` 的职责是把这份原料**忠实地搬出来**，交给 P2 的 `ResolutionLevel::ImportBinding`。
//!
//! ## 数据忠实性原则
//! `module_path` 一律原样保留 AST 给出的 `path_components`，不做任何"猜测性切分"；
//! 本地绑定名 `alias` 按 `import_alias() → name() → path_components.last()` 三级推导，
//! 同时把两个原始来源 `raw_alias` / `raw_name` 一并留住 —— 因为 7 种语言对这些字段的用法
//! 互不相同（见下方 `LOCAL_NAME_RULES`），过早归一化会丢掉 P2 唯一能用来消歧的信息。

use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

use crate::codegraph::treesitter::ast_instance_structs::ImportType;
use crate::codegraph::treesitter::structs::SymbolType;
use crate::codegraph::treesitter::AstSymbolInstanceArc;

/// 各语言 import 形态与本地绑定名的实测规则（P1 逐条跑真实 parser 得到，见本文件单测）：
///
/// ```text
/// Rust   use a::b::{c, d as e};   → 拆成两条：path=["a","b","c"] alias=None
///                                     path=["a","b","d"] alias=Some("e")；name 恒为空
/// Python from x import y as z     → path=["x","y"] alias=Some("z") name="y"
/// Python import x.y               → path=["x","y"] alias=None   name="y"
///                                     ⚠ 与上一行的 path 形状相同，`import` 与
///                                       `from-import` 的差别没有留在 ImportDeclaration 里
/// Java   import a.b.C;            → path=["a","b","C"] alias=None name=""
/// Java   import a.b.*;            → path=["a","b","*"]（通配符在末段）
/// Go     import ( f "fmt" )       → path=["fmt"]        alias=None name="f"（别名写在 name！）
/// Go     import . "fmt" / _ "x"   → name="." / name="_"
/// TS/JS  import {a as b} from "m" → path=["m","a"]      alias=Some("b") name="a"
///                                     ⚠ path_components 里 "m" 是模块、"a" 是成员名
/// TS/JS  import * as n from "m"   → path=["m"]          alias=Some("n") name="n"
/// C++    #include <a/b.h>         → path=["a","b.h"]    alias=None name=""
/// JS     const {x} = require("m") → ⚠ **不产出 ImportDeclaration**
///                                     （js.rs 只有 import_statement 分支会产 import 符号，
///                                      require 走 call_expression → FunctionCall+VariableUsage）
/// ```
///
/// 上述每一行都由本模块 `#[cfg(test)]` 里对应语言的真实 parser 用例锁定。
pub const LOCAL_NAME_RULES: &[&str] = &[
    "alias = import_alias() | name() | path_components.last()",
    "original_name = 仅当 path 末段 != alias 时取末段（否则取 name()）",
    "module_prefix = module_path 去掉末段（单段路径不剥）",
    "is_wildcard = path 末段 == \"*\"",
    "python `import x.y` 与 `from x import y` 同形 → 记 ambiguous_binding_candidates",
    "js `require()` 不产出 ImportDeclaration（原料缺口，见 javascript_require_form_is_documented_as_missing_input）",
];

/// 一条 import 绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEntry {
    /// **本地绑定名**（推导结果）：本文件里用它去匹配调用点的裸名。
    /// 推导规则见 [`LOCAL_NAME_RULES`]：`import_alias()` → `name()` → `path_components.last()`。
    pub alias: String,
    /// AST 原始 `path_components`，**原样保留**（Rust 用 `::` 切、Java/Go/TS 用 `.` 或 `/` 切）。
    pub module_path: Vec<String>,
    /// 被导入的原始名字（仅当存在"本地名 ≠ 原始名"的别名时才有值）。
    pub original_name: Option<String>,
    /// import 类别（System / Library / UserModule / Unknown），来自 AST 的 `import_type`。
    pub kind: ImportType,
    /// 已解析到的文件（当前 7 个语言 parser **都没有填** `filepath_ref`，恒 `None`；
    /// 字段留着，等 P2 做真实模块路径解析时填充）。
    pub file_ref: Option<PathBuf>,
    /// 通配符导入（Java `import a.b.*;`、TS `import * as n`）。
    pub is_wildcard: bool,
    /// `path_components` 里"模块前缀"部分（去掉成员名/通配符之后的段）。
    pub module_prefix: Vec<String>,
    /// 该 import 符号在 AST 里的 guid（P2 需要回溯到符号本体）。
    pub symbol_guid: Uuid,
    /// 1-based 起始行（= `full_range.start_point.row + 1`，与 `FunctionInfo.line_start` 同口径）。
    pub line: usize,
    /// 原始 `import_alias()` 值（未经推导），保真给 P2。
    pub raw_alias: Option<String>,
    /// 原始 `name()` 值（多数语言为空串），保真给 P2。
    pub raw_name: String,
    /// 原始 `import_filepath_ref()` 值。
    pub raw_filepath_ref: Option<PathBuf>,
    /// ⚠ 原料不足标记：Python 的 `import x.y` 与 `from x import y` 在 AST 里
    /// 形状完全相同（`path_components` 都是 `["x","y"]`），无法区分"绑定顶层 `x`"
    /// 还是"绑定成员 `y`"。这里两种候选都记下来，交给 P2 决策而不是我猜一个。
    pub ambiguous_binding_candidates: Vec<String>,
}

impl ImportEntry {
    /// 从单个 AST 符号构造（只认 `SymbolType::ImportDeclaration`）。
    ///
    /// 走的是 trait 上的**专用 getter**（P1 交付物 1），不需要 downcast。
    pub fn from_symbol(symbol: &dyn crate::codegraph::treesitter::AstSymbolInstance) -> Option<Self> {
        if symbol.symbol_type() != SymbolType::ImportDeclaration {
            return None;
        }

        let module_path: Vec<String> = symbol
            .import_path_components()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let raw_alias = symbol.import_alias().map(|s| s.to_string());
        let raw_name = symbol.name().to_string();
        let raw_filepath_ref = symbol.import_filepath_ref().map(|p| p.to_path_buf());

        // 通配符：末段是 "*"（Java `import a.b.*;`）。
        let is_wildcard = module_path
            .last()
            .map(|last| last == "*")
            .unwrap_or(false);

        // 模块前缀 = 去掉末段成员名/通配符。
        // 注意只有"看起来像成员名"时才剥：单段路径（Go `import "fmt"`）本身就是模块。
        let module_prefix: Vec<String> = if is_wildcard {
            module_path[..module_path.len().saturating_sub(1)].to_vec()
        } else if module_path.len() >= 2 {
            module_path[..module_path.len() - 1].to_vec()
        } else {
            module_path.clone()
        };

        // 本地绑定名三级推导。
        let alias = raw_alias
            .clone()
            .or_else(|| {
                if raw_name.is_empty() {
                    None
                } else {
                    Some(raw_name.clone())
                }
            })
            .or_else(|| module_path.last().cloned())
            .filter(|name| name != "*")
            .unwrap_or_default();

        // 只有"本地名与原始名不同"才是真别名，才填 original_name。
        let original_name = match module_path.last() {
            Some(last) if !alias.is_empty() && last != &alias && last != "*" => Some(last.clone()),
            _ => {
                // TS/JS 形态里 alias 是本地名、path 末段是成员名（`{a as b}` → path 末段 "a"）。
                if !raw_name.is_empty() && raw_name != alias {
                    Some(raw_name.clone())
                } else {
                    None
                }
            }
        };

        let kind = symbol.import_type().unwrap_or(ImportType::Unknown);

        let mut ambiguous_binding_candidates = Vec::new();
        if raw_alias.is_none() && module_path.len() >= 2 && !raw_name.is_empty() {
            // Python 形态：`import x.y`（绑定 x）与 `from x import y`（绑定 y）同形 → 两个都记。
            ambiguous_binding_candidates.push(raw_name.clone());
            if let Some(first) = module_path.first() {
                if first != &raw_name {
                    ambiguous_binding_candidates.push(first.clone());
                }
            }
        }

        Some(Self {
            alias,
            module_path,
            original_name,
            kind,
            file_ref: raw_filepath_ref.clone(),
            is_wildcard,
            module_prefix,
            symbol_guid: *symbol.guid(),
            line: symbol.full_range().start_point.row + 1,
            raw_alias,
            raw_name,
            raw_filepath_ref,
            ambiguous_binding_candidates,
        })
    }

    /// 该 import 是否在 `name` 上给本文件引入了一个可调用绑定。
    pub fn binds(&self, name: &str) -> bool {
        if self.is_wildcard {
            return false; // 通配符不绑定具体名字
        }
        !self.alias.is_empty() && self.alias == name
    }
}

/// 单文件 import 表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportMap {
    entries: Vec<ImportEntry>,
    /// 本地名 → entries 下标。**必须是 Vec**：同名 import 多份时一条都不能丢
    /// （这正是 `_build_function_name_index` 的 `or_insert` 在函数索引上犯的错）。
    by_local_name: HashMap<String, Vec<usize>>,
    /// 通配符导入的模块前缀集合（Java `import a.b.*;` → ["a","b"]）。
    wildcard_prefixes: Vec<Vec<String>>,
}

impl ImportMap {
    /// 从 AST 符号列表建表：只挑 `SymbolType::ImportDeclaration`，其余忽略。
    pub fn build(symbols: &[AstSymbolInstanceArc]) -> Self {
        let mut map = Self::default();
        for symbol in symbols {
            let guard = symbol.read();
            if let Some(entry) = ImportEntry::from_symbol(guard.as_ref()) {
                map.push(entry);
            }
        }
        map
    }

    fn push(&mut self, entry: ImportEntry) {
        let idx = self.entries.len();
        if entry.is_wildcard {
            self.wildcard_prefixes.push(entry.module_prefix.clone());
        } else if !entry.alias.is_empty() {
            self.by_local_name
                .entry(entry.alias.clone())
                .or_default()
                .push(idx);
        }
        self.entries.push(entry);
    }

    /// 直接由已构造好的 entry 建表（脱离 AST 的单测入口）。
    pub fn from_entries(entries: impl IntoIterator<Item = ImportEntry>) -> Self {
        let mut map = Self::default();
        for entry in entries {
            map.push(entry);
        }
        map
    }

    pub fn entries(&self) -> &[ImportEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 某个本地名的**全部** import 绑定（不塌缩）。
    ///
    /// 返回 `Vec`（克隆）而不是 `&[ImportEntry]`：内部索引是 `HashMap<String, Vec<usize>>`，
    /// 无法把"下标集合"直接映射成一段连续切片；与其造一个只能返回空切片的假 API，
    /// 不如明确返回 Owned 列表 —— import 条目数量级很小，克隆成本可忽略。
    pub fn lookup(&self, name: &str) -> Vec<ImportEntry> {
        match self.by_local_name.get(name) {
            Some(idxs) => idxs
                .iter()
                .filter_map(|idx| self.entries.get(*idx))
                .cloned()
                .collect(),
            None => vec![],
        }
    }

    /// 某个本地名被 import 了几次（>1 = 歧义，P2 必须走 Ambiguous）。
    pub fn binding_count(&self, name: &str) -> usize {
        self.by_local_name.get(name).map(Vec::len).unwrap_or(0)
    }

    /// **仅当恰好一条**时返回该绑定；多条歧义时返回 `None`（绝不 first-wins）。
    pub fn unique_binding(&self, name: &str) -> Option<&ImportEntry> {
        let idxs = self.by_local_name.get(name)?;
        if idxs.len() == 1 {
            self.entries.get(idxs[0])
        } else {
            None
        }
    }

    /// 通配符模块前缀集合（P2 用它们在"本文件所有已导入包"里找成员）。
    /// 已去重并按字典序排序，保证与插入顺序无关。
    pub fn wildcard_prefixes(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .wildcard_prefixes
            .iter()
            .map(|parts| parts.join("."))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// 所有非通配符的本地绑定名（去重 + 排序，稳定输出）。
    pub fn bound_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.by_local_name.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegraph::treesitter::language_id::LanguageId;
    use crate::codegraph::treesitter::parsers::get_ast_parser;
    use std::path::PathBuf;

    /// 走**真实语言 parser**（不落盘，`parse(code, path)` 只需要 path 做符号归属标记）。
    fn parse_with(lang: LanguageId, code: &str, filename: &str) -> Vec<AstSymbolInstanceArc> {
        let mut parser = get_ast_parser(lang).expect("get_ast_parser");
        parser.parse(code, &PathBuf::from(filename))
    }

    /// 从符号列表里只挑 import 符号的摘要行，方便断言与报错。
    fn import_rows(symbols: &[AstSymbolInstanceArc]) -> Vec<(Vec<String>, Option<String>, String, usize)> {
        symbols
            .iter()
            .filter_map(|symbol| {
                let guard = symbol.read();
                let s = guard.as_ref();
                if s.symbol_type() != SymbolType::ImportDeclaration {
                    return None;
                }
                Some((
                    s.import_path_components().to_vec(),
                    s.import_alias().map(|v| v.to_string()),
                    s.name().to_string(),
                    s.full_range().start_point.row + 1,
                ))
            })
            .collect()
    }

    /// 交付物 1 的**缺口打开证明**：从 `Vec<AstSymbolInstanceArc>` 里真能读出
    /// `path_components` / `alias` / `import_type`（P0 基线只能看到 `name=""`）。
    #[test]
    fn trait_getters_expose_import_payload_from_real_rust_parser() {
        let symbols = parse_with(
            LanguageId::Rust,
            "use crate::calc::Calc;\nuse a::b::{c, d as e};\n",
            "t.rs",
        );
        let rows = import_rows(&symbols);
        assert!(!rows.is_empty(), "rust.rs 的 use 分支必须产出 ImportDeclaration");
        // 通用 name() 仍是空串 —— 这条**不是**放宽 P0 断言，而是与专用 getter 并存的事实。
        assert!(
            rows.iter().all(|(_, _, name, _)| name.is_empty()),
            "P0 锁定的 `ImportDeclaration name=\"\"` 事实不变，专用 getter 才是本次打开的缺口"
        );
        // 但 path_components / import_type 现在拿得到。
        assert!(
            rows.iter().any(|(path, _, _, _)| path == &vec![
                "crate".to_string(),
                "calc".to_string(),
                "Calc".to_string()
            ]),
            "应能读出 use crate::calc::Calc 的三段路径，实际 = {rows:?}"
        );
        assert!(
            rows.iter().any(|(_, alias, _, _)| alias.as_deref() == Some("e")),
            "应能读出 `d as e` 的别名 e，实际 = {rows:?}"
        );
        let kinds: Vec<ImportType> = {
            symbols
                .iter()
                .filter_map(|symbol| {
                    let guard = symbol.read();
                    (guard.symbol_type() == SymbolType::ImportDeclaration).then(|| guard.import_type())
                })
                .flatten()
                .collect()
        };
        assert!(!kinds.is_empty(), "import_type() 必须对 import 符号返回 Some");
        assert!(
            kinds.contains(&ImportType::UserModule),
            "`crate::` 前缀应被判为 UserModule，实际 = {kinds:?}"
        );
    }

    /// 非 import 符号走 trait 默认实现 → 全空。这条同时证明 trait 仍对象安全。
    #[test]
    fn default_getters_return_empty_for_non_import_symbols() {
        let symbols = parse_with(LanguageId::Rust, "fn a(x: i32) -> i32 { a(x) }\n", "t.rs");
        let mut checked = 0usize;
        for symbol in &symbols {
            let guard = symbol.read();
            let s = guard.as_ref();
            if s.symbol_type() == SymbolType::ImportDeclaration {
                continue;
            }
            assert!(s.import_path_components().is_empty(), "{:?}", s.symbol_type());
            assert_eq!(s.import_alias(), None);
            assert_eq!(s.import_type(), None);
            assert_eq!(s.import_filepath_ref(), None);
            checked += 1;
        }
        assert!(checked > 0, "至少要检查到函数定义与调用符号");
    }

    #[test]
    fn rust_braced_use_is_split_into_one_entry_per_binding() {
        let symbols = parse_with(LanguageId::Rust, "use a::b::{c, d as e};\n", "t.rs");
        let map = ImportMap::build(&symbols);
        assert_eq!(map.len(), 2, "实测 = {:?}", map.entries());
        let plain = map.unique_binding("c").expect("c 应有一条绑定");
        assert_eq!(plain.module_path, vec!["a", "b", "c"]);
        assert_eq!(plain.alias, "c");
        assert_eq!(plain.original_name, None);
        assert_eq!(plain.module_prefix, vec!["a", "b"]);

        let aliased = map.unique_binding("e").expect("别名 e 应有一条绑定");
        assert_eq!(aliased.alias, "e");
        assert_eq!(aliased.original_name.as_deref(), Some("d"));
        assert_eq!(aliased.module_path, vec!["a", "b", "d"]);
        assert_eq!(map.binding_count("e"), 1);
    }

    #[test]
    fn python_from_import_alias_and_plain_import_are_both_captured() {
        let symbols = parse_with(
            LanguageId::Python,
            "from mod_x import func as alias\nimport x.y\n",
            "t.py",
        );
        let map = ImportMap::build(&symbols);
        let aliased = map.unique_binding("alias").expect("from x import y as z 必须绑定 alias");
        assert_eq!(aliased.module_path, vec!["mod_x", "func"]);
        assert_eq!(aliased.original_name.as_deref(), Some("func"));

        // `import x.y`：绑定名推导为 "y"，但 Python 真实语义是绑定顶层 "x"。
        // 这里不猜，把两个候选都留住交给 P2。
        let plain = map.entries().iter().find(|e| e.alias == "y").expect("应有 import x.y 的条目");
        assert_eq!(plain.module_path, vec!["x", "y"]);
        assert_eq!(
            plain.ambiguous_binding_candidates,
            vec!["y".to_string(), "x".to_string()],
            "Python 的 import/from-import 同形，必须把两个候选都记下来"
        );
    }

    #[test]
    fn java_class_import_and_wildcard_import_are_distinguished() {
        let symbols = parse_with(
            LanguageId::Java,
            "import java.util.ArrayList;\nimport java.util.*;\n",
            "T.java",
        );
        let map = ImportMap::build(&symbols);
        let class_import = map
            .unique_binding("ArrayList")
            .expect("import a.b.C 应绑定简单名 C");
        assert_eq!(class_import.module_path, vec!["java", "util", "ArrayList"]);
        assert_eq!(class_import.module_prefix, vec!["java", "util"]);
        assert!(!class_import.is_wildcard);
        assert_eq!(class_import.kind, ImportType::System, "java 前缀应判为 System");

        assert_eq!(map.wildcard_prefixes(), vec!["java.util"], "实测 = {:?}", map.entries());
        assert_eq!(map.binding_count("*"), 0, "通配符不得占用一个本地绑定名");
    }

    #[test]
    fn go_aliased_import_records_alias_from_name_field() {
        let symbols = parse_with(
            LanguageId::Go,
            "import (\n\tf \"fmt\"\n\t\"os\"\n)\n",
            "t.go",
        );
        let map = ImportMap::build(&symbols);
        // Go parser 把别名写进 `name`（不是 `alias`），所以推导规则的第二级必须生效。
        let aliased = map.unique_binding("f").expect("别名 f 应被绑定");
        assert_eq!(aliased.module_path, vec!["fmt"]);
        assert_eq!(aliased.raw_alias, None, "go.rs 不填 alias 字段");
        assert_eq!(aliased.raw_name, "f");
        assert_eq!(aliased.original_name.as_deref(), Some("fmt"));
        assert!(map.unique_binding("os").is_some(), "无别名时按 path 末段绑定");
    }

    #[test]
    fn typescript_named_and_namespace_imports_are_captured() {
        let symbols = parse_with(
            LanguageId::TypeScript,
            "import {a as b} from \"m\";\nimport * as n from \"mod\";\n",
            "t.ts",
        );
        let map = ImportMap::build(&symbols);
        let named = map.unique_binding("b").expect("{a as b} 必须绑定 b");
        assert_eq!(named.module_path, vec!["m", "a"], "path_components = source + 成员名");
        assert_eq!(named.module_prefix, vec!["m"]);
        assert_eq!(named.original_name.as_deref(), Some("a"));

        let ns = map.unique_binding("n").expect("* as n 必须绑定 n");
        assert_eq!(ns.module_path, vec!["mod"]);
        assert_eq!(ns.raw_alias.as_deref(), Some("n"));
    }

    #[test]
    fn javascript_require_form_is_documented_as_missing_input() {
        // 实测结论：js.rs 只在 `import_statement` 分支产出 ImportDeclaration；
        // `const {x} = require("m")` 走 call_expression → FunctionCall + VariableUsage，
        // **根本产不出 import 符号**。这里把这个缺口锁成断言，避免假装已处理。
        let symbols = parse_with(
            LanguageId::JavaScript,
            "const {x} = require(\"m\");\n",
            "t.js",
        );
        let map = ImportMap::build(&symbols);
        assert!(
            map.is_empty(),
            "require 形态目前不产出 ImportDeclaration —— 缺口已由 P1 记录，需 P2 在 js.rs 补产出：\n{:?}",
            map.entries()
        );

        // 同一文件里 ES import 形态则必须被正常捕获（证明不是 ImportMap 整体失效）。
        let esm = parse_with(LanguageId::JavaScript, "import {a as b} from \"m\";\n", "t.js");
        let esm_map = ImportMap::build(&esm);
        assert_eq!(esm_map.binding_count("b"), 1, "实测 = {:?}", esm_map.entries());
    }

    #[test]
    fn cpp_preproc_include_becomes_a_path_import() {
        let symbols = parse_with(LanguageId::Cpp, "#include <a/b.h>\n", "t.cpp");
        let map = ImportMap::build(&symbols);
        assert_eq!(map.len(), 1, "实测 = {:?}", map.entries());
        let entry = &map.entries()[0];
        assert_eq!(entry.module_path, vec!["a", "b.h"]);
        assert_eq!(entry.alias, "b.h", "无别名时按末段绑定");
        // 实测事实：cpp.rs 的 preproc_include 分支用 SYSTEM_HEADERS **白名单**判定，
        // 尖括号本身不代表系统头 ⇒ 自定义路径的 kind 是 Unknown（不是 System）。
        assert_eq!(entry.kind, ImportType::Unknown, "cpp.rs 按白名单判 System，a/b.h 不在表内");
    }

    /// 核心性质：同名多份 import **绝不塌缩**。
    #[test]
    fn duplicate_local_bindings_are_all_kept_never_collapsed() {
        let map = ImportMap::from_entries(vec![
            ImportEntry {
                alias: "helper".into(),
                module_path: vec!["pkg_a".into(), "helper".into()],
                original_name: Some("helper".into()),
                kind: ImportType::Library,
                file_ref: None,
                is_wildcard: false,
                module_prefix: vec!["pkg_a".into()],
                symbol_guid: Uuid::nil(),
                line: 1,
                raw_alias: Some("helper".into()),
                raw_name: String::new(),
                raw_filepath_ref: None,
                ambiguous_binding_candidates: vec![],
            },
            ImportEntry {
                alias: "helper".into(),
                module_path: vec!["pkg_b".into(), "helper".into()],
                original_name: Some("helper".into()),
                kind: ImportType::Library,
                file_ref: None,
                is_wildcard: false,
                module_prefix: vec!["pkg_b".into()],
                symbol_guid: Uuid::nil(),
                line: 2,
                raw_alias: Some("helper".into()),
                raw_name: String::new(),
                raw_filepath_ref: None,
                ambiguous_binding_candidates: vec![],
            },
        ]);
        assert_eq!(map.binding_count("helper"), 2, "两条同名绑定都必须留住");
        assert_eq!(map.lookup("helper").len(), 2);
        assert!(
            map.unique_binding("helper").is_none(),
            "歧义时不得返回任意一条（当前 first-wins 的做法）"
        );
        assert_eq!(map.bound_names(), vec!["helper".to_string()]);
    }

    #[test]
    fn binds_ignores_wildcards_and_matches_only_local_name() {
        let symbols = parse_with(LanguageId::Java, "import java.util.*;\n", "T.java");
        let map = ImportMap::build(&symbols);
        let entry = &map.entries()[0];
        assert!(entry.is_wildcard);
        assert!(!entry.binds("ArrayList"), "通配符不得声称自己绑定了具体名字");
    }

    /// 临时探针（P2a 探查各语言调用符号形状，交付前删除）
    #[test]
    #[ignore = "临时探针"]
    fn tmp_probe_call_shapes() {
        use crate::codegraph::treesitter::structs::SymbolType;
        let cases: &[(&str, LanguageId, &str, &str)] = &[
            ("rust", LanguageId::Rust, "rs", "fn g(){ a::b::f(1); obj.m(2); h(); let v: Calc = c; let w = Calc::new(); }"),
        ];
        for (_name, lang, _ext, code) in cases {
            let mut parser = get_ast_parser(*lang).expect("parser");
            let symbols = parser.parse(code, &PathBuf::from(format!("probe.{}", _ext)));
            println!("\n=== {} ===", _name);
            for sym in symbols.iter() {
                let guard = sym.read();
                let s = guard.as_ref();
                match s.symbol_type() {
                    SymbolType::FunctionCall => {
                        println!("   call name={:?} ns={:?} recv={:?}", s.name(), s.namespace(), s.call_receiver());
                    }
                    SymbolType::VariableDefinition => {
                        println!("   var name={:?} types={:?}", s.name(), s.types());
                    }
                    _ => {}
                }
            }
        }

        // 真实 fixture：形参类型形状
        for rel in [
            "tests/fixtures/resolution/ambiguous_trait_impl/driver.rs",
            "tests/fixtures/resolution/method_receiver/receiver_user.rs",
        ] {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
            let ts = crate::codegraph::treesitter::TreeSitterParser::new();
            let ast = ts.parse_file_with_metadata(&path).unwrap();
            println!("\n=== decl args {} ===", rel);
            for sym in ast.symbols.iter() {
                let guard = sym.read();
                let s = guard.as_ref();
                if s.symbol_type() == SymbolType::FunctionDeclaration {
                    println!("   fn {:?} types={:?}", s.name(), s.types());
                }
            }
        }
    }
}
