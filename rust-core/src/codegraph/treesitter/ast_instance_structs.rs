use async_trait::async_trait;
use dyn_partial_eq::{dyn_partial_eq, DynPartialEq};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::cmp::min;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use std::{fs, io};
use tokio::fs::read_to_string;
use tree_sitter::{Point, Range};
use uuid::Uuid;

use crate::codegraph::treesitter::language_id::LanguageId;
use crate::codegraph::treesitter::structs::{RangeDef, SymbolType};

#[derive(Eq, Hash, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct TypeDef {
    pub name: Option<String>,
    pub inference_info: Option<String>,
    pub inference_info_guid: Option<Uuid>,
    pub is_pod: bool,
    pub namespace: String,
    pub guid: Option<Uuid>,
    pub nested_types: Vec<TypeDef>, // for nested types, presented in templates
}

impl Default for TypeDef {
    fn default() -> Self {
        TypeDef {
            name: None,
            inference_info: None,
            inference_info_guid: None,
            is_pod: false,
            namespace: String::from(""),
            guid: None,
            nested_types: vec![],
        }
    }
}

impl TypeDef {
    pub fn to_string(&self) -> String {
        let mut res = String::from("");
        if let Some(name) = &self.name {
            res.push_str(&name);
        }
        for nested in &self.nested_types {
            res.push_str(&format!("_{}", &nested.to_string()));
        }
        res
    }

    pub fn get_nested_types(&self) -> Vec<TypeDef> {
        let mut types = vec![];
        let mut nested_types = vec![];
        for nested in self.nested_types.iter() {
            types.push(nested.clone());
        }
        for nested in types.iter() {
            nested_types.append(&mut nested.get_nested_types())
        }
        types.append(&mut nested_types);
        types
    }

    /// Phase P2a 交付物 1 的取名辅助：把一个 `TypeDef` 规范成**单个类型标识符**。
    ///
    /// 只用于 `resolution` 的 L3 receiver 类型推断，不参与任何序列化/图产出。
    /// 取名顺序：
    /// 1. 自己的 `name`（非空、且不是 `parse_type` 给元组塞的占位名 `"tuple"`）；
    /// 2. 否则退到**第一个**有名字的嵌套类型（`Vec<Calc>` → `Calc`、`tuple` → 首个元素类型）。
    /// 两者都拿不到 ⇒ `None`，调用方必须按"信息不足"下沉，**绝不猜**。
    pub fn binding_type_name(&self) -> Option<&str> {
        if let Some(name) = self.name.as_deref() {
            let trimmed = name.trim();
            if !trimmed.is_empty() && trimmed != "tuple" {
                return Some(trimmed);
            }
        }
        self.nested_types.iter().find_map(TypeDef::binding_type_name)
    }

    pub fn mutate_nested_types<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut TypeDef),
    {
        for nested in &mut self.nested_types {
            f(nested);
            nested.mutate_nested_types_ref(&mut f);
        }
    }

    fn mutate_nested_types_ref<F>(&mut self, f: &mut F)
    where
        F: FnMut(&mut TypeDef),
    {
        for nested in &mut self.nested_types {
            f(nested);
            nested.mutate_nested_types_ref(f);
        }
    }
}


#[derive(PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct AstSymbolFields {
    pub guid: Uuid,
    pub name: String,
    pub language: LanguageId,
    pub file_path: PathBuf,
    pub namespace: String,
    pub parent_guid: Option<Uuid>,
    pub childs_guid: Vec<Uuid>,
    #[serde(with = "RangeDef")]
    pub full_range: Range,
    #[serde(with = "RangeDef")]
    pub declaration_range: Range,
    #[serde(with = "RangeDef")]
    pub definition_range: Range,
    // extra fields for usage structs to prevent multiple downcast operations
    pub linked_decl_guid: Option<Uuid>,
    pub linked_decl_type: Option<TypeDef>,
    pub caller_guid: Option<Uuid>,
    pub is_error: bool,
    pub caller_depth: Option<usize>,
}

impl AstSymbolFields {
    pub fn from_data(language: LanguageId, file_path: PathBuf, is_error: bool) -> Self {
        AstSymbolFields {
            language,
            file_path,
            is_error,
            ..Default::default()
        }
    }

    pub fn from_fields(fields: &AstSymbolFields) -> Self {
        Self {
            language: fields.language,
            file_path: fields.file_path.clone(),
            is_error: fields.is_error,
            ..Default::default()
        }
    }
}

#[derive(PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct SymbolInformation {
    pub guid: Uuid,
    pub name: String,
    pub parent_guid: Uuid,
    pub linked_decl_guid: Uuid,
    pub caller_guid: Uuid,
    pub symbol_type: SymbolType,
    pub symbol_path: String,
    pub language: LanguageId,
    pub file_path: PathBuf,
    pub namespace: String,
    pub is_error: bool,
    #[serde(with = "RangeDef")]
    pub full_range: Range,
    #[serde(with = "RangeDef")]
    pub declaration_range: Range,
    #[serde(with = "RangeDef")]
    pub definition_range: Range,
}

impl SymbolInformation {
    pub fn get_content(&self, content: &String) -> io::Result<String> {
        let lines: Vec<&str> = content.split("\n").collect();
        let mut end_row = self.full_range.end_point.row + 1;
        let raw_content = content.get(self.full_range.start_byte..self.full_range.end_byte);
        if raw_content.is_none() {
            return Err(io::Error::other("Incorrect content range"));
        }
        let raw_content = raw_content.unwrap();
        if raw_content.ends_with("\n") {
            end_row -= 1;
        }

        let mut start_row = min(self.full_range.start_point.row, lines.len());
        let end_row = min(end_row, lines.len());
        start_row = min(start_row, end_row);
        let selected_text = lines[start_row..end_row].join("\n");
        Ok(selected_text)
    }

    pub async fn get_content_from_file(&self) -> io::Result<String> {
        let content = read_to_string(&self.file_path).await?;
        self.get_content(&content)
    }

    pub fn get_content_from_file_blocked(&self) -> io::Result<String> {
        let content = fs::read_to_string(&self.file_path)?;
        self.get_content(&content)
    }

    pub fn get_declaration_content(&self, content: &String) -> io::Result<String> {
        let content = content.get(self.declaration_range.start_byte..self.declaration_range.end_byte);
        if content.is_none() {
            return Err(io::Error::other("Incorrect declaration range"));
        }
        Ok(content.unwrap().to_string())
    }

    pub async fn get_declaration_content_from_file(&self) -> io::Result<String> {
        let content = read_to_string(&self.file_path).await?;
        self.get_declaration_content(&content)
    }

    pub fn get_declaration_content_from_file_blocked(&self) -> io::Result<String> {
        let content = fs::read_to_string(&self.file_path)?;
        self.get_declaration_content(&content)
    }
}

impl Default for AstSymbolFields {
    fn default() -> Self {
        AstSymbolFields {
            guid: Uuid::default(),
            name: "".to_string(),
            language: LanguageId::Unknown,
            file_path: PathBuf::new(),
            namespace: "".to_string(),
            parent_guid: None,
            childs_guid: vec![],
            full_range: Range {
                start_byte: 0,
                end_byte: 0,
                start_point: Default::default(),
                end_point: Default::default(),
            },
            declaration_range: Range {
                start_byte: 0,
                end_byte: 0,
                start_point: Default::default(),
                end_point: Default::default(),
            },
            definition_range: Range {
                start_byte: 0,
                end_byte: 0,
                start_point: Default::default(),
                end_point: Default::default(),
            },
            linked_decl_guid: None,
            linked_decl_type: None,
            caller_guid: None,
            is_error: false,
            caller_depth: None,
        }
    }
}


#[async_trait]
#[typetag::serde]
#[dyn_partial_eq]
pub trait AstSymbolInstance: Debug + Send + Sync + Any {
    fn fields(&self) -> &AstSymbolFields;

    fn fields_mut(&mut self) -> &mut AstSymbolFields;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn symbol_info_struct(&self) -> SymbolInformation {
        SymbolInformation {
            guid: self.guid().clone(),
            name: self.name().to_string(),
            parent_guid: self.parent_guid().clone().unwrap_or_default(),
            linked_decl_guid: self.get_linked_decl_guid().clone().unwrap_or_default(),
            caller_guid: self.get_caller_guid().clone().unwrap_or_default(),
            symbol_type: self.symbol_type(),
            symbol_path: "".to_string(),
            language: self.language().clone(),
            file_path: self.file_path().clone(),
            namespace: self.namespace().to_string(),
            is_error: self.is_error(),
            full_range: self.full_range().clone(),
            declaration_range: self.declaration_range().clone(),
            definition_range: self.definition_range().clone(),
        }
    }

    fn guid(&self) -> &Uuid {
        &self.fields().guid
    }

    fn name(&self) -> &str {
        &self.fields().name
    }

    fn language(&self) -> &LanguageId {
        &self.fields().language
    }

    fn file_path(&self) -> &PathBuf { &self.fields().file_path }

    fn is_type(&self) -> bool;

    fn is_declaration(&self) -> bool;

    fn types(&self) -> Vec<TypeDef>;

    fn set_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>);

    fn set_inference_info_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>);

    fn namespace(&self) -> &str {
        &self.fields().namespace
    }

    fn parent_guid(&self) -> &Option<Uuid> {
        &self.fields().parent_guid
    }

    fn childs_guid(&self) -> &Vec<Uuid> {
        &self.fields().childs_guid
    }

    fn symbol_type(&self) -> SymbolType;

    fn full_range(&self) -> &Range {
        &self.fields().full_range
    }

    // ie function signature, class signature, full range otherwise
    fn declaration_range(&self) -> &Range {
        &self.fields().declaration_range
    }

    // ie function body, class body, full range otherwise
    fn definition_range(&self) -> &Range {
        &self.fields().definition_range
    }

    fn get_caller_guid(&self) -> &Option<Uuid> {
        &self.fields().caller_guid
    }

    fn set_caller_guid(&mut self, caller_guid: Uuid) {
        self.fields_mut().caller_guid = Some(caller_guid);
    }

    fn get_linked_decl_guid(&self) -> &Option<Uuid> {
        &self.fields().linked_decl_guid
    }

    fn set_linked_decl_guid(&mut self, linked_decl_guid: Option<Uuid>) {
        self.fields_mut().linked_decl_guid = linked_decl_guid;
    }

    fn get_linked_decl_type(&self) -> &Option<TypeDef> {
        &self.fields().linked_decl_type
    }

    fn set_linked_decl_type(&mut self, linked_decl_type: TypeDef) {
        self.fields_mut().linked_decl_type = Some(linked_decl_type);
    }

    fn temporary_types_cleanup(&mut self);

    fn get_caller_depth(&self) -> &Option<usize> {
        &self.fields().caller_depth
    }

    fn set_caller_depth(&mut self, caller_depth: Option<usize>) {
        self.fields_mut().caller_depth = caller_depth;
    }

    fn is_error(&self) -> bool {
        self.fields().is_error
    }

    // ============================================================================================
    // Phase P1 交付物 1：import 原料的专用只读接口。
    //
    // 背景：`ImportDeclaration` 的 path_components / alias / import_type / filepath_ref 四个字段
    // 一直存在于 AST 层，但通用 trait 上没有 getter，导致上层（parser.rs 的两处 symbol_type
    // 匹配循环）只能看到 `name()` 为空串 —— P0 基线测试正是用这个空串断言锁住了该缺口。
    //
    // 这里全部提供**带默认实现**的方法：非 import 符号一律返回空值，因此 7 个语言结构体
    // 无需任何改动；只有 `ImportDeclaration` 的 impl 覆盖为返回真实字段。
    // 返回类型刻意不含泛型/`Self`，以保持对象安全（`typetag` + `dyn_partial_eq` 的前提）。
    // ============================================================================================

    /// import 路径的各段（Rust `use a::b::c` → ["a","b","c"]；Java `import a.b.C` → ["a","b","C"]）。
    fn import_path_components(&self) -> &[String] {
        &[]
    }

    /// 显式别名（Rust `use a::b as c` / Python `from x import y as z` / JS `import {a as b}`）。
    fn import_alias(&self) -> Option<&str> {
        None
    }

    /// import 的类别（系统 / 三方库 / 用户模块 / 未知）。返回 owned 值：`ImportType` 只有 `Clone`。
    fn import_type(&self) -> Option<ImportType> {
        None
    }

    /// 已解析到的文件路径（当前各语言 parser 均未填充，保留接口给 P2 的真实模块解析）。
    fn import_filepath_ref(&self) -> Option<&PathBuf> {
        None
    }

    // ============================================================================================
    // Phase P1 交付物 2(a)：方法调用 receiver 的只读接口。
    //
    // `rust.rs` 的 field_expression 分支过去只把方法名写进 `name`，receiver 退化成一个
    // 独立的 `VariableUsage` 符号（源码注释自称 "dirty hack"）；现在同时把 receiver 的源码
    // 文本记在调用符号自己的 `receiver` 字段上。默认 `None` → 其它符号类型无需改动。
    // ============================================================================================

    /// 调用点 receiver 的源码文本（`calc.add(x)` → Some("calc")）；非方法调用返回 `None`。
    fn call_receiver(&self) -> Option<&str> {
        None
    }

    /// 函数定义的形参个数（`FunctionDeclaration.args.len()`）。
    ///
    /// Phase P1 交付物 3 需要它来给 `resolution::FunctionTag.param_count` 赋值
    /// —— 当前图里 `FunctionInfo.signature` 只是函数名的副本（`parser.rs` 两处都是
    /// `signature: Some(symbol_ref.name())`），完全没有参数信息，arity 消歧无从下手。
    /// 默认 `None`：只有 `FunctionDeclaration` 覆盖，其余符号类型无需改动。
    fn param_count(&self) -> Option<usize> {
        None
    }

    // ============================================================================================
    // Phase P2a 交付物 1 的原料：receiver 类型推断（L3 ReceiverTypeMember）需要的"形参名 → 类型名"。
    //
    // 为什么不能再复用 `types()`：`FunctionDeclaration::types()` 把 **返回类型排在最前**，
    // 且会把嵌套类型展开成多元素（`Vec<TypeDef>`），与 `args` **没有下标对应关系**，
    // 因此无法回答"这个调用点的 receiver `calc` 是哪来的、类型是什么"。
    // 而 `args` 里每个 `FunctionArg { name, type_ }` 恰好就是 P2a 允许的三种推断方式之一
    // （"函数形参 `v: T` / `T v`"）。
    //
    // 与 P1 的 import/receiver getter 同一取向：**带默认实现**（非函数声明返回空），
    // 只读、不改任何符号、不改序列化形状，因此 `tests/parsers` 的 `compare_symbols`
    // 与 skeletonizer/declaration_formatter 的精确断言完全不受影响。
    // ============================================================================================

    /// 形参名 → 规范化类型名（`fn f(calc: &Calc)` → `[("calc", "Calc")]`）。
    ///
    /// 类型名规范化：优先 `TypeDef.name`；无名时退到第一个有名字的嵌套类型（如
    /// `Vec<Calc>`/`&dyn Runner` 这类别名/引用形式在 `parse_type` 里已经被剥成内层名字）。
    /// 无类型标注的形参（C 风格 `T v` 之外的大多数动态语言）不进结果集 —— **宁缺毋滥**，
    /// P2a 的 L3 判据要求"推不出类型必须下沉不许猜"。
    fn param_bindings(&self) -> Vec<(String, String)> {
        vec![]
    }

    /// 该函数声明是否**带函数体**（`definition_range` 非空）。
    ///
    /// 这是 P2a 区分"具体实现"与"抽象签名"的唯一可用信号：
    /// `rust.rs::parse_function_declaration` 只在存在 `body` 节点时才写 `definition_range`，
    /// 所以 trait 方法 `fn run(&self) -> i32;`（`tests/fixtures/resolution/ambiguous_trait_impl/runners.rs:6`）
    /// 的 `definition_range` 是默认零区间。
    ///
    /// 用途：L3 命中一个"无体的抽象签名"时**不得**声称唯一解析成功 ——
    /// AST 里没有 impl→trait 的反向链接，无法枚举实现者，只能拒绝唯一判定并下沉。
    /// 默认 `true`：非函数声明符号一律视为"有体"，不影响任何既有行为。
    fn has_definition_body(&self) -> bool {
        true
    }

    fn remove_linked_guids(&mut self, guids: &HashSet<Uuid>) {
        let mut new_guids = vec![];
        for t in self
            .types()
            .iter() {
            if guids.contains(&t.guid.unwrap_or_default()) {
                new_guids.push(None);
            } else {
                new_guids.push(t.guid.clone());
            }
        }
        self.set_guids_to_types(&new_guids);

        match self.get_linked_decl_guid() {
            Some(guid) => {
                if guids.contains(guid) {
                    self.set_linked_decl_guid(None);
                }
            }
            None => {}
        }
    }

    fn distance_to_cursor(&self, cursor: &Point) -> usize {
        cursor.row.abs_diff(self.full_range().start_point.row)
    }
}

// pub type AstSymbolInstanceRc = Rc<RefCell<Box<dyn AstSymbolInstance>>>;
pub type AstSymbolInstanceArc = Arc<RwLock<Box<dyn AstSymbolInstance>>>;


/*
StructDeclaration
*/
#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct StructDeclaration {
    pub ast_fields: AstSymbolFields,
    pub template_types: Vec<TypeDef>,
    pub inherited_types: Vec<TypeDef>,
}

impl Default for StructDeclaration {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
            template_types: vec![],
            inherited_types: vec![],
        }
    }
}


#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for StructDeclaration {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn types(&self) -> Vec<TypeDef> {
        let mut types: Vec<TypeDef> = vec![];
        for t in self.inherited_types.iter() {
            types.push(t.clone());
            types.extend(t.get_nested_types());
        }
        for t in self.template_types.iter() {
            types.push(t.clone());
            types.extend(t.get_nested_types());
        }
        types
    }

    fn set_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        for t in self.inherited_types.iter_mut() {
            t.guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.guid = guids[idx].clone();
                idx += 1;
            })
        }
        for t in self.template_types.iter_mut() {
            t.guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.guid = guids[idx].clone();
                idx += 1;
            })
        }
    }

    fn set_inference_info_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        for t in self.inherited_types.iter_mut() {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.inference_info_guid = guids[idx].clone();
                idx += 1;
            })
        }
        for t in self.template_types.iter_mut() {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.inference_info_guid = guids[idx].clone();
                idx += 1;
            })
        }
    }

    fn temporary_types_cleanup(&mut self) {
        for t in self.inherited_types.iter_mut() {
            t.inference_info = None;
            t.mutate_nested_types(|t| {
                t.inference_info = None
            })
        }
        for t in self.template_types.iter_mut() {
            t.inference_info = None;
            t.mutate_nested_types(|t| {
                t.inference_info = None
            })
        }
    }

    fn is_type(&self) -> bool {
        true
    }

    fn is_declaration(&self) -> bool { true }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::StructDeclaration
    }
}


/*
TypeAlias
*/
#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct TypeAlias {
    pub ast_fields: AstSymbolFields,
    pub types: Vec<TypeDef>,
}

impl Default for TypeAlias {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
            types: vec![],
        }
    }
}

#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for TypeAlias {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn types(&self) -> Vec<TypeDef> {
        let mut types: Vec<TypeDef> = vec![];
        for t in self.types.iter() {
            types.push(t.clone());
            types.extend(t.get_nested_types());
        }
        types
    }

    fn set_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        for t in self.types.iter_mut() {
            t.guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.guid = guids[idx].clone();
                idx += 1;
            })
        }
    }

    fn set_inference_info_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        for t in self.types.iter_mut() {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.inference_info_guid = guids[idx].clone();
                idx += 1;
            })
        }
    }

    fn temporary_types_cleanup(&mut self) {
        for t in self.types.iter_mut() {
            t.inference_info = None;
            t.mutate_nested_types(|t| {
                t.inference_info = None
            })
        }
    }

    fn is_type(&self) -> bool {
        true
    }

    fn is_declaration(&self) -> bool { true }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::TypeAlias
    }
}


/*
ClassFieldDeclaration
*/
#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct ClassFieldDeclaration {
    pub ast_fields: AstSymbolFields,
    pub type_: TypeDef,
}

impl Default for ClassFieldDeclaration {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
            type_: TypeDef::default(),
        }
    }
}

#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for ClassFieldDeclaration {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn types(&self) -> Vec<TypeDef> {
        let mut types: Vec<TypeDef> = vec![];
        types.push(self.type_.clone());
        types.extend(self.type_.get_nested_types());
        types
    }

    fn set_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        self.type_.guid = guids[idx].clone();
        idx += 1;
        self.type_.mutate_nested_types(|t| {
            t.guid = guids[idx].clone();
            idx += 1;
        })
    }

    fn set_inference_info_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        self.type_.inference_info_guid = guids[idx].clone();
        idx += 1;
        self.type_.mutate_nested_types(|t| {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
        })
    }

    fn temporary_types_cleanup(&mut self) {
        self.type_.inference_info = None;
        self.type_.mutate_nested_types(|t| {
            t.inference_info = None
        })
    }

    fn is_type(&self) -> bool {
        false
    }

    fn is_declaration(&self) -> bool { true }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::ClassFieldDeclaration
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ImportType {
    System,
    Library,
    UserModule,
    Unknown,
}

/*
ImportDeclaration
*/
#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct ImportDeclaration {
    pub ast_fields: AstSymbolFields,
    pub path_components: Vec<String>,
    pub alias: Option<String>,
    pub import_type: ImportType,
    pub filepath_ref: Option<PathBuf>,
}

impl Default for ImportDeclaration {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
            path_components: vec![],
            alias: None,
            import_type: ImportType::Unknown,
            filepath_ref: None,
        }
    }
}

#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for ImportDeclaration {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn types(&self) -> Vec<TypeDef> {
        vec![]
    }

    fn set_guids_to_types(&mut self, _: &Vec<Option<Uuid>>) {}

    fn set_inference_info_guids_to_types(&mut self, _: &Vec<Option<Uuid>>) {}

    fn temporary_types_cleanup(&mut self) {}

    fn is_type(&self) -> bool {
        false
    }

    fn is_declaration(&self) -> bool { false }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::ImportDeclaration
    }

    // ---- Phase P1 交付物 1：覆盖 trait 默认实现，把原本"拿不到"的四个 import 原料暴露出去 ----

    fn import_path_components(&self) -> &[String] {
        &self.path_components
    }

    fn import_alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }

    fn import_type(&self) -> Option<ImportType> {
        Some(self.import_type.clone())
    }

    fn import_filepath_ref(&self) -> Option<&PathBuf> {
        self.filepath_ref.as_ref()
    }
}


/*
VariableDefinition
*/
#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct VariableDefinition {
    pub ast_fields: AstSymbolFields,
    pub type_: TypeDef,
}

impl Default for VariableDefinition {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
            type_: TypeDef::default(),
        }
    }
}

#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for VariableDefinition {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn types(&self) -> Vec<TypeDef> {
        let mut types: Vec<TypeDef> = vec![];
        types.push(self.type_.clone());
        types.extend(self.type_.get_nested_types());
        types
    }

    fn set_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        self.type_.guid = guids[idx].clone();
        idx += 1;
        self.type_.mutate_nested_types(|t| {
            t.guid = guids[idx].clone();
            idx += 1;
        })
    }

    fn set_inference_info_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        self.type_.inference_info_guid = guids[idx].clone();
        idx += 1;
        self.type_.mutate_nested_types(|t| {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
        })
    }

    fn temporary_types_cleanup(&mut self) {
        self.type_.inference_info = None;
        self.type_.mutate_nested_types(|t| {
            t.inference_info = None
        })
    }

    fn is_type(&self) -> bool {
        false
    }

    fn is_declaration(&self) -> bool { true }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::VariableDefinition
    }
}


/*
FunctionDeclaration
*/
#[derive(PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct FunctionCaller {
    pub inference_info: String,
    pub guid: Option<Uuid>,
}

#[derive(Eq, Hash, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct FunctionArg {
    pub name: String,
    pub type_: Option<TypeDef>,
}

impl Default for FunctionArg {
    fn default() -> Self {
        Self {
            name: String::default(),
            type_: None,
        }
    }
}

#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct FunctionDeclaration {
    pub ast_fields: AstSymbolFields,
    pub template_types: Vec<TypeDef>,
    pub args: Vec<FunctionArg>,
    pub return_type: Option<TypeDef>,
}

impl Default for FunctionDeclaration {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
            template_types: vec![],
            args: vec![],
            return_type: None,
        }
    }
}

#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for FunctionDeclaration {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn is_type(&self) -> bool {
        false
    }

    fn types(&self) -> Vec<TypeDef> {
        let mut types = vec![];
        if let Some(t) = self.return_type.clone() {
            types.push(t.clone());
            types.extend(t.get_nested_types());
        }
        for t in self.args.iter() {
            if let Some(t) = t.type_.clone() {
                types.push(t.clone());
                types.extend(t.get_nested_types());
            }
        }
        types
    }

    fn set_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        if let Some(t) = &mut self.return_type {
            t.guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.guid = guids[idx].clone();
                idx += 1;
            })
        }
        for t in self.args.iter_mut() {
            if let Some(t) = &mut t.type_ {
                t.guid = guids[idx].clone();
                idx += 1;
                t.mutate_nested_types(|t| {
                    t.guid = guids[idx].clone();
                    idx += 1;
                })
            }
        }
    }

    fn set_inference_info_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        if let Some(t) = &mut self.return_type {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.inference_info_guid = guids[idx].clone();
                idx += 1;
            })
        }
        for t in self.args.iter_mut() {
            if let Some(t) = &mut t.type_ {
                t.inference_info_guid = guids[idx].clone();
                idx += 1;
                t.mutate_nested_types(|t| {
                    t.inference_info_guid = guids[idx].clone();
                    idx += 1;
                })
            }
        }
    }

    fn temporary_types_cleanup(&mut self) {
        if let Some(t) = &mut self.return_type {
            t.inference_info = None;
            t.mutate_nested_types(|t| {
                t.inference_info = None
            });
        }
        for t in self.args.iter_mut() {
            if let Some(t) = &mut t.type_ {
                t.inference_info = None;
                t.mutate_nested_types(|t| {
                    t.inference_info = None
                });
            }
        }
    }

    fn is_declaration(&self) -> bool { true }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::FunctionDeclaration
    }

    /// Phase P1 交付物 3：暴露形参个数供 `resolution::FunctionTag` 使用。
    fn param_count(&self) -> Option<usize> {
        Some(self.args.len())
    }

    /// Phase P2a 交付物 1：形参名 → 类型名（L3 receiver 类型推断的唯一"形参"来源）。
    ///
    /// 只做**规范化取名**，不做任何跨文件/跨作用域推断；无名或无类型的形参直接跳过，
    /// 让上层 `resolution` 走"信息不足即下沉"的分支。
    fn param_bindings(&self) -> Vec<(String, String)> {
        self.args
            .iter()
            .filter_map(|arg| {
                let type_name = arg
                    .type_
                    .as_ref()
                    .and_then(TypeDef::binding_type_name)?
                    .to_string();
                if arg.name.is_empty() || type_name.is_empty() {
                    return None;
                }
                Some((arg.name.clone(), type_name))
            })
            .collect()
    }

    /// Phase P2a 交付物 1：trait/abstract 方法（只有签名、没有 body）→ `false`。
    ///
    /// `definition_range` 由 `rust.rs`（及 Java/Go/TS 等各语言 parser）仅在存在 body 节点时写入，
    /// 默认值是零区间，因此 `start_byte == end_byte` 即"没有函数体"。
    fn has_definition_body(&self) -> bool {
        self.ast_fields.definition_range.start_byte != self.ast_fields.definition_range.end_byte
    }
}


/*
CommentDefinition
*/
#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct CommentDefinition {
    pub ast_fields: AstSymbolFields,
}

impl Default for CommentDefinition {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
        }
    }
}

#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for CommentDefinition {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn is_type(&self) -> bool {
        false
    }

    fn types(&self) -> Vec<TypeDef> {
        vec![]
    }

    fn set_guids_to_types(&mut self, _: &Vec<Option<Uuid>>) {}

    fn set_inference_info_guids_to_types(&mut self, _: &Vec<Option<Uuid>>) {}

    fn temporary_types_cleanup(&mut self) {}

    fn is_declaration(&self) -> bool { true }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::CommentDefinition
    }
}


/*
FunctionCall
*/
#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct FunctionCall {
    pub ast_fields: AstSymbolFields,
    pub template_types: Vec<TypeDef>,
    /// Phase P1 交付物 2(a)：方法调用 receiver 的**源码文本**（`calc.add(x)` → `Some("calc")`）。
    ///
    /// 为什么新开一个字段而不是复用 `ast_fields.namespace`：
    /// `namespace` 已被 `rust.rs` 的 `scoped_identifier` 分支用于模块路径（`mod_a::pick`），
    /// 两者语义不同，混用会让 P2 无法区分"包/模块限定"与"对象 receiver"；
    /// 且 P0 基线第 3、5 两项分别断言 `namespace == ""`，改写 namespace 会直接打穿基线。
    ///
    /// `#[serde(default)]` 保证旧的参考 JSON fixture（没有该键）仍能反序列化；
    /// `skip_serializing_if` 保证 `None` 时序列化字节与改造前逐字节一致。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
}

impl Default for FunctionCall {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
            template_types: vec![],
            receiver: None,
        }
    }
}

#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for FunctionCall {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn is_type(&self) -> bool {
        false
    }

    fn types(&self) -> Vec<TypeDef> {
        let mut types = vec![];
        if let Some(t) = self.ast_fields.linked_decl_type.clone() {
            types.push(t.clone());
            types.extend(t.get_nested_types());
        }
        for t in self.template_types.iter() {
            types.push(t.clone());
            types.extend(t.get_nested_types());
        }
        types
    }

    fn set_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        if let Some(t) = &mut self.ast_fields.linked_decl_type {
            t.guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.guid = guids[idx].clone();
                idx += 1;
            })
        }
        for t in self.template_types.iter_mut() {
            t.guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.guid = guids[idx].clone();
                idx += 1;
            })
        }
    }

    fn set_inference_info_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        if let Some(t) = &mut self.ast_fields.linked_decl_type {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.inference_info_guid = guids[idx].clone();
                idx += 1;
            })
        }
        for t in self.template_types.iter_mut() {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.inference_info_guid = guids[idx].clone();
                idx += 1;
            })
        }
    }

    fn temporary_types_cleanup(&mut self) {
        if let Some(t) = &mut self.ast_fields.linked_decl_type {
            t.inference_info = None;
            t.mutate_nested_types(|t| {
                t.inference_info = None
            });
        }
        for t in self.template_types.iter_mut() {
            t.inference_info = None;
            t.mutate_nested_types(|t| {
                t.inference_info = None
            });
        }
    }

    fn is_declaration(&self) -> bool { false }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::FunctionCall
    }

    /// Phase P1 交付物 2(a)：覆盖 trait 默认实现，暴露 receiver 源码文本。
    fn call_receiver(&self) -> Option<&str> {
        self.receiver.as_deref()
    }
}


/*
VariableUsage
*/
#[derive(DynPartialEq, PartialEq, Debug, Serialize, Deserialize, Clone)]
pub struct VariableUsage {
    pub ast_fields: AstSymbolFields,
}

impl Default for VariableUsage {
    fn default() -> Self {
        Self {
            ast_fields: AstSymbolFields::default(),
        }
    }
}

#[async_trait]
#[typetag::serde]
impl AstSymbolInstance for VariableUsage {
    fn fields(&self) -> &AstSymbolFields {
        &self.ast_fields
    }

    fn fields_mut(&mut self) -> &mut AstSymbolFields {
        &mut self.ast_fields
    }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    fn is_type(&self) -> bool {
        false
    }

    fn types(&self) -> Vec<TypeDef> {
        let mut types = vec![];
        if let Some(t) = self.ast_fields.linked_decl_type.clone() {
            types.push(t.clone());
            types.extend(t.get_nested_types());
        }
        types
    }

    fn set_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        if let Some(t) = &mut self.ast_fields.linked_decl_type {
            t.guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.guid = guids[idx].clone();
                idx += 1;
            })
        }
    }

    fn set_inference_info_guids_to_types(&mut self, guids: &Vec<Option<Uuid>>) {
        let mut idx = 0;
        if let Some(t) = &mut self.ast_fields.linked_decl_type {
            t.inference_info_guid = guids[idx].clone();
            idx += 1;
            t.mutate_nested_types(|t| {
                t.inference_info_guid = guids[idx].clone();
                idx += 1;
            })
        }
    }

    fn temporary_types_cleanup(&mut self) {
        if let Some(t) = &mut self.ast_fields.linked_decl_type {
            t.inference_info = None;
            t.mutate_nested_types(|t| {
                t.inference_info = None
            });
        }
    }

    fn is_declaration(&self) -> bool { false }

    fn symbol_type(&self) -> SymbolType {
        SymbolType::VariableUsage
    }
}
