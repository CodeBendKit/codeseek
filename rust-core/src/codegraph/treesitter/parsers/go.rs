use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::RwLock;

use tree_sitter::{Node, Parser, Range};
use uuid::Uuid;
use similar::DiffableStr;
use tracing::debug;

use crate::codegraph::treesitter::ast_instance_structs::{AstSymbolFields, AstSymbolInstanceArc, ClassFieldDeclaration, CommentDefinition, FunctionArg, FunctionDeclaration, ImportDeclaration, ImportType, StructDeclaration, TypeDef, FunctionCall};
use crate::codegraph::treesitter::language_id::LanguageId;
use crate::codegraph::treesitter::parsers::{AstLanguageParser, internal_error, ParserError};
use crate::codegraph::treesitter::parsers::utils::{CandidateInfo, get_children_guids, get_guid};
use crate::codegraph::treesitter::skeletonizer::SkeletonFormatter;
use crate::codegraph::treesitter::ast_instance_structs::SymbolInformation;
use crate::codegraph::treesitter::structs::SymbolType;

pub(crate) struct GoParser {
    pub parser: Parser,
}

pub struct GoSkeletonFormatter;

impl GoParser {
    pub fn new() -> Result<GoParser, ParserError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .map_err(internal_error)?;
        Ok(GoParser { parser })
    }

    pub fn parse_struct_declaration<'a>(&mut self, info: &CandidateInfo<'a>, code: &str, candidates: &mut VecDeque<CandidateInfo<'a>>) -> Vec<AstSymbolInstanceArc> {
        let mut symbols: Vec<AstSymbolInstanceArc> = Default::default();
        let mut decl = StructDeclaration::default();

        decl.ast_fields.language = info.ast_fields.language;
        
        // Find the parent type_declaration node to get the full range
        let mut full_range = info.node.range();
        let mut current_parent = info.node.parent();
        
        // Look up the hierarchy to find type_declaration
        while let Some(parent) = current_parent {
            if parent.kind() == "type_declaration" {
                full_range = parent.range();
                break;
            }
            current_parent = parent.parent();
        }
        
        if full_range == info.node.range() {
            debug!("anonymous struct: {}", code.slice(info.node.byte_range()).to_string());
            return symbols;
        }
        decl.ast_fields.full_range = full_range;
        
        decl.ast_fields.file_path = info.ast_fields.file_path.clone();
        decl.ast_fields.parent_guid = Some(info.parent_guid.clone());
        decl.ast_fields.guid = get_guid();
        decl.ast_fields.is_error = info.ast_fields.is_error;

        // Find the name from the parent type_declaration
        if let Some(parent) = info.node.parent() {
            if parent.kind() == "type_spec" {
                if let Some(name_node) = parent.child_by_field_name("name") {
                    decl.ast_fields.name = code.slice(name_node.byte_range()).to_string();
                    // Declaration range should be just the struct name
                    decl.ast_fields.declaration_range = name_node.range();
                } else {
                    // Default declaration_range if name_node is not found
                    decl.ast_fields.declaration_range = decl.ast_fields.full_range.clone();
                }
            } else {
                // Default declaration_range if parent is not type_spec
                decl.ast_fields.declaration_range = decl.ast_fields.full_range.clone();
            }
        } else {
            // Default declaration_range if parent is not found
            decl.ast_fields.declaration_range = decl.ast_fields.full_range.clone();
        }

        // Parse field declarations
        for i in 0..info.node.child_count() {
            let child = info.node.child(i).unwrap();
            if child.kind() == "field_declaration" {
                candidates.push_back(CandidateInfo {
                    ast_fields: info.ast_fields.clone(),
                    node: child,
                    parent_guid: decl.ast_fields.guid.clone(),
                });
            } else if child.kind() == "field_declaration_list" {
                // Parse each field declaration in the list
                for j in 0..child.child_count() {
                    let field_child = child.child(j).unwrap();
                    if field_child.kind() == "field_declaration" {
                        candidates.push_back(CandidateInfo {
                            ast_fields: info.ast_fields.clone(),
                            node: field_child,
                            parent_guid: decl.ast_fields.guid.clone(),
                        });
                    }
                }
            }
        }

        decl.ast_fields.definition_range = info.node.range();
        decl.ast_fields.childs_guid = get_children_guids(&decl.ast_fields.guid, &symbols);
        let _struct_name = decl.ast_fields.name.clone();
        symbols.push(Arc::new(RwLock::new(Box::new(decl))));
        symbols
    }

    fn parse_field_declaration<'a>(&mut self, info: &CandidateInfo<'a>, code: &str) -> Vec<AstSymbolInstanceArc> {
        let mut symbols: Vec<AstSymbolInstanceArc> = vec![];
        
        // Parse field names
        if let Some(names_node) = info.node.child(0) {
            if names_node.kind() == "identifier" || names_node.kind() == "field_identifier" {
                let mut decl = ClassFieldDeclaration::default();
                decl.ast_fields.language = info.ast_fields.language;
                decl.ast_fields.full_range = info.node.range();
                decl.ast_fields.file_path = info.ast_fields.file_path.clone();
                decl.ast_fields.parent_guid = Some(info.parent_guid.clone());
                decl.ast_fields.guid = get_guid();
                decl.ast_fields.name = code.slice(names_node.byte_range()).to_string();
                decl.ast_fields.is_error = info.ast_fields.is_error;
                
                // Parse field type
                if let Some(type_node) = info.node.child(1) {
                    if let Some(type_) = self.parse_type(&type_node, code) {
                        decl.type_ = type_;
                    }
                    // Declaration range should include the field name and type
                    decl.ast_fields.declaration_range = Range {
                        start_byte: names_node.start_byte(),
                        end_byte: type_node.end_byte(),
                        start_point: names_node.start_position(),
                        end_point: type_node.end_position(),
                    };
                } else {
                    // If no type node, just use the name node
                    decl.ast_fields.declaration_range = names_node.range();
                }

                let _field_name = decl.ast_fields.name.clone();
                symbols.push(Arc::new(RwLock::new(Box::new(decl))));
            }
        }

        symbols
    }

    fn parse_function_declaration<'a>(&mut self, info: &CandidateInfo<'a>, code: &str, candidates: &mut VecDeque<CandidateInfo<'a>>) -> Vec<AstSymbolInstanceArc> {
        let mut symbols: Vec<AstSymbolInstanceArc> = Default::default();
        let mut decl = FunctionDeclaration::default();
        
        decl.ast_fields.language = info.ast_fields.language;
        decl.ast_fields.full_range = info.node.range();
        decl.ast_fields.file_path = info.ast_fields.file_path.clone();
        decl.ast_fields.parent_guid = Some(info.parent_guid.clone());
        decl.ast_fields.guid = get_guid();
        decl.ast_fields.is_error = info.ast_fields.is_error;
        
        // Set default declaration_range to full_range (will be updated if needed)
        decl.ast_fields.declaration_range = decl.ast_fields.full_range.clone();
        
        // Parse function name
        if let Some(name_node) = info.node.child_by_field_name("name") {
            decl.ast_fields.name = code.slice(name_node.byte_range()).to_string();
        }

        // Parse parameters
        if let Some(parameters_node) = info.node.child_by_field_name("parameters") {
            decl.args = self.parse_parameters(&parameters_node, code);
            // Declaration range should include the function signature up to parameters
            decl.ast_fields.declaration_range = Range {
                start_byte: decl.ast_fields.full_range.start_byte,
                end_byte: parameters_node.end_byte(),
                start_point: decl.ast_fields.full_range.start_point,
                end_point: parameters_node.end_position(),
            };
        }

        // Parse return type
        if let Some(result_node) = info.node.child_by_field_name("result") {
            decl.return_type = self.parse_type(&result_node, code);
            // Declaration range should extend to include the return type
            decl.ast_fields.declaration_range = Range {
                start_byte: decl.ast_fields.full_range.start_byte,
                end_byte: result_node.end_byte(),
                start_point: decl.ast_fields.full_range.start_point,
                end_point: result_node.end_position(),
            };
        }

        // Parse function body
        if let Some(body_node) = info.node.child_by_field_name("body") {
            decl.ast_fields.definition_range = body_node.range();
            candidates.push_back(CandidateInfo {
                ast_fields: decl.ast_fields.clone(),
                node: body_node,
                parent_guid: decl.ast_fields.guid.clone(),
            });
        } else {
            decl.ast_fields.definition_range = decl.ast_fields.full_range.clone();
        }

        decl.ast_fields.childs_guid = get_children_guids(&decl.ast_fields.guid, &symbols);
        let _function_name = decl.ast_fields.name.clone();
        symbols.push(Arc::new(RwLock::new(Box::new(decl))));
        symbols
    }

    fn parse_method_declaration<'a>(&mut self, info: &CandidateInfo<'a>, code: &str, candidates: &mut VecDeque<CandidateInfo<'a>>) -> Vec<AstSymbolInstanceArc> {
        let mut symbols: Vec<AstSymbolInstanceArc> = Default::default();
        let mut decl = FunctionDeclaration::default();
        
        decl.ast_fields.language = info.ast_fields.language;
        decl.ast_fields.full_range = info.node.range();
        decl.ast_fields.file_path = info.ast_fields.file_path.clone();
        decl.ast_fields.parent_guid = Some(info.parent_guid.clone());
        decl.ast_fields.guid = get_guid();
        decl.ast_fields.is_error = info.ast_fields.is_error;
        
        // Set default declaration_range to full_range (will be updated if needed)
        decl.ast_fields.declaration_range = decl.ast_fields.full_range.clone();
        
        // Parse method name
        if let Some(name_node) = info.node.child_by_field_name("name") {
            decl.ast_fields.name = code.slice(name_node.byte_range()).to_string();
        }

        // Parse receiver
        if let Some(receiver_node) = info.node.child_by_field_name("receiver") {
            candidates.push_back(CandidateInfo {
                ast_fields: info.ast_fields.clone(),
                node: receiver_node,
                parent_guid: info.parent_guid.clone(),
            });
        }

        // Parse parameters
        if let Some(parameters_node) = info.node.child_by_field_name("parameters") {
            decl.args = self.parse_parameters(&parameters_node, code);
            // Declaration range should include the method signature up to parameters
            decl.ast_fields.declaration_range = Range {
                start_byte: decl.ast_fields.full_range.start_byte,
                end_byte: parameters_node.end_byte(),
                start_point: decl.ast_fields.full_range.start_point,
                end_point: parameters_node.end_position(),
            };
        }

        // Parse return type
        if let Some(result_node) = info.node.child_by_field_name("result") {
            decl.return_type = self.parse_type(&result_node, code);
            // Declaration range should extend to include the return type
            decl.ast_fields.declaration_range = Range {
                start_byte: decl.ast_fields.full_range.start_byte,
                end_byte: result_node.end_byte(),
                start_point: decl.ast_fields.full_range.start_point,
                end_point: result_node.end_position(),
            };
        }

        // Parse method body
        if let Some(body_node) = info.node.child_by_field_name("body") {
            decl.ast_fields.definition_range = body_node.range();
            candidates.push_back(CandidateInfo {
                ast_fields: decl.ast_fields.clone(),
                node: body_node,
                parent_guid: decl.ast_fields.guid.clone(),
            });
        } else {
            decl.ast_fields.definition_range = decl.ast_fields.full_range.clone();
        }

        decl.ast_fields.childs_guid = get_children_guids(&decl.ast_fields.guid, &symbols);
        let _method_name = decl.ast_fields.name.clone();
        symbols.push(Arc::new(RwLock::new(Box::new(decl))));
        symbols
    }

    fn parse_parameters(&self, parent: &Node, code: &str) -> Vec<FunctionArg> {
        let mut args: Vec<FunctionArg> = vec![];
        
        for i in 0..parent.child_count() {
            let child = parent.child(i).unwrap();
            if child.kind() == "parameter_declaration" {
                // Parse parameter names
                if let Some(names_node) = child.child_by_field_name("name") {
                    for j in 0..names_node.child_count() {
                        let name_child = names_node.child(j).unwrap();
                        if name_child.kind() == "identifier" {
                            let mut arg = FunctionArg {
                                name: code.slice(name_child.byte_range()).to_string(),
                                type_: None,
                            };
                            
                            // Parse parameter type
                            if let Some(type_node) = child.child_by_field_name("type") {
                                if let Some(type_) = self.parse_type(&type_node, code) {
                                    arg.type_ = Some(type_);
                                }
                            }
                            
                            args.push(arg);
                        }
                    }
                }
            }
        }
        
        args
    }

    fn parse_type(&self, parent: &Node, code: &str) -> Option<TypeDef> {
        let kind = parent.kind();
        let text = code.slice(parent.byte_range()).to_string();
        
        match kind {
            "type_identifier" => {
                return Some(TypeDef {
                    name: Some(text),
                    inference_info: None,
                    inference_info_guid: None,
                    is_pod: false,
                    namespace: "".to_string(),
                    guid: None,
                    nested_types: vec![],
                });
            }
            "qualified_type" => {
                return Some(TypeDef {
                    name: Some(text),
                    inference_info: None,
                    inference_info_guid: None,
                    is_pod: false,
                    namespace: "".to_string(),
                    guid: None,
                    nested_types: vec![],
                });
            }
            "pointer_type" => {
                if let Some(child) = parent.child(0) {
                    if let Some(child_type) = self.parse_type(&child, code) {
                        let child_name = child_type.name.clone();
                        return Some(TypeDef {
                            name: Some(format!("*{}", child_name.unwrap_or_default())),
                            inference_info: None,
                            inference_info_guid: None,
                            is_pod: false,
                            namespace: "".to_string(),
                            guid: None,
                            nested_types: vec![child_type],
                        });
                    }
                }
            }
            "slice_type" => {
                if let Some(child) = parent.child(0) {
                    if let Some(child_type) = self.parse_type(&child, code) {
                        let child_name = child_type.name.clone();
                        return Some(TypeDef {
                            name: Some(format!("[]{}", child_name.unwrap_or_default())),
                            inference_info: None,
                            inference_info_guid: None,
                            is_pod: false,
                            namespace: "".to_string(),
                            guid: None,
                            nested_types: vec![child_type],
                        });
                    }
                }
            }
            "array_type" => {
                if let Some(element_node) = parent.child_by_field_name("element") {
                    if let Some(element_type) = self.parse_type(&element_node, code) {
                        let element_name = element_type.name.clone();
                        return Some(TypeDef {
                            name: Some(format!("[]{}", element_name.unwrap_or_default())),
                            inference_info: None,
                            inference_info_guid: None,
                            is_pod: false,
                            namespace: "".to_string(),
                            guid: None,
                            nested_types: vec![element_type],
                        });
                    }
                }
            }
            "struct_type" => {
                return Some(TypeDef {
                    name: Some("struct".to_string()),
                    inference_info: None,
                    inference_info_guid: None,
                    is_pod: false,
                    namespace: "".to_string(),
                    guid: None,
                    nested_types: vec![],
                });
            }
            "interface_type" => {
                return Some(TypeDef {
                    name: Some("interface".to_string()),
                    inference_info: None,
                    inference_info_guid: None,
                    is_pod: false,
                    namespace: "".to_string(),
                    guid: None,
                    nested_types: vec![],
                });
            }
            "map_type" => {
                if let Some(key_node) = parent.child_by_field_name("key") {
                    if let Some(value_node) = parent.child_by_field_name("value") {
                        if let (Some(key_type), Some(value_type)) = (self.parse_type(&key_node, code), self.parse_type(&value_node, code)) {
                            let mut nested_types = vec![key_type];
                            nested_types.extend(vec![value_type]);
                            return Some(TypeDef {
                                name: Some("map".to_string()),
                                inference_info: None,
                                inference_info_guid: None,
                                is_pod: false,
                                namespace: "".to_string(),
                                guid: None,
                                nested_types,
                            });
                        }
                    }
                }
            }
            "channel_type" => {
                if let Some(child) = parent.child(0) {
                    if let Some(child_type) = self.parse_type(&child, code) {
                        let child_name = child_type.name.clone();
                        return Some(TypeDef {
                            name: Some(format!("chan {}", child_name.unwrap_or_default())),
                            inference_info: None,
                            inference_info_guid: None,
                            is_pod: false,
                            namespace: "".to_string(),
                            guid: None,
                            nested_types: vec![child_type],
                        });
                    }
                }
            }
            "function_type" => {
                return Some(TypeDef {
                    name: Some("func".to_string()),
                    inference_info: None,
                    inference_info_guid: None,
                    is_pod: false,
                    namespace: "".to_string(),
                    guid: None,
                    nested_types: vec![],
                });
            }
            _ => {}
        }
        
        None
    }

    fn parse_import_declaration<'a>(&mut self, info: &CandidateInfo<'a>, code: &str) -> Vec<AstSymbolInstanceArc> {
        let mut symbols: Vec<AstSymbolInstanceArc> = vec![];
        
        // Parse import spec or import spec list
        if info.node.child_count() >= 2 {
            let import_node = info.node.child(1).unwrap(); // Skip the 'import' keyword
            
            if import_node.kind() == "import_spec_list" {
                // Parse each import spec in the list
                for i in 0..import_node.child_count() {
                    let child = import_node.child(i).unwrap();
                    if child.kind() == "import_spec" {
                        let mut decl = ImportDeclaration::default();
                        decl.ast_fields.language = info.ast_fields.language;
                        // Use the full import declaration range, not just the import spec
                        decl.ast_fields.full_range = info.node.range();
                        decl.ast_fields.file_path = info.ast_fields.file_path.clone();
                        decl.ast_fields.parent_guid = Some(info.parent_guid.clone());
                        decl.ast_fields.guid = get_guid();
                        decl.ast_fields.is_error = info.ast_fields.is_error;

                        // Parse import path
                        if let Some(path_node) = child.child_by_field_name("path") {
                            let path_text = code.slice(path_node.byte_range()).to_string();
                            // Remove quotes
                            let path_text = path_text.trim_matches('"');
                            decl.path_components = path_text.split('/').map(|s| s.to_string()).collect();
                            // Don't set the name for import declarations - keep it empty
                            // decl.ast_fields.name = decl.path_components.last().unwrap_or(&"".to_string()).clone();
                        }

                        // Parse import name/alias
                        if let Some(name_node) = child.child_by_field_name("name") {
                            if name_node.kind() == "dot" {
                                decl.ast_fields.name = ".".to_string();
                            } else if name_node.kind() == "blank_identifier" {
                                decl.ast_fields.name = "_".to_string();
                            } else {
                                decl.ast_fields.name = code.slice(name_node.byte_range()).to_string();
                            }
                        }

                        // Determine import type
                        if let Some(first) = decl.path_components.first() {
                            if first.starts_with(".") {
                                decl.import_type = ImportType::UserModule;
                            } else {
                                decl.import_type = ImportType::System;
                            }
                        }

                        symbols.push(Arc::new(RwLock::new(Box::new(decl))));
                    }
                }
            } else if import_node.kind() == "import_spec" {
                let mut decl = ImportDeclaration::default();
                decl.ast_fields.language = info.ast_fields.language;
                // Use the full import declaration range
                decl.ast_fields.full_range = info.node.range();
                decl.ast_fields.file_path = info.ast_fields.file_path.clone();
                decl.ast_fields.parent_guid = Some(info.parent_guid.clone());
                decl.ast_fields.guid = get_guid();
                decl.ast_fields.is_error = info.ast_fields.is_error;

                // Parse import path
                if let Some(path_node) = import_node.child_by_field_name("path") {
                    let path_text = code.slice(path_node.byte_range()).to_string();
                    // Remove quotes
                    let path_text = path_text.trim_matches('"');
                    decl.path_components = path_text.split('/').map(|s| s.to_string()).collect();
                    // Don't set the name for import declarations - keep it empty
                    // decl.ast_fields.name = decl.path_components.last().unwrap_or(&"".to_string()).clone();
                }

                // Parse import name/alias
                if let Some(name_node) = import_node.child_by_field_name("name") {
                    if name_node.kind() == "dot" {
                        decl.ast_fields.name = ".".to_string();
                    } else if name_node.kind() == "blank_identifier" {
                        decl.ast_fields.name = "_".to_string();
                    } else {
                        decl.ast_fields.name = code.slice(name_node.byte_range()).to_string();
                    }
                }

                // Determine import type
                if let Some(first) = decl.path_components.first() {
                    if first.starts_with(".") {
                        decl.import_type = ImportType::UserModule;
                    } else {
                        decl.import_type = ImportType::System;
                    }
                }

                symbols.push(Arc::new(RwLock::new(Box::new(decl))));
            }
        }

        symbols
    }

    fn parse_usages_<'a>(&mut self, info: &CandidateInfo<'a>, code: &str, candidates: &mut VecDeque<CandidateInfo<'a>>) -> Vec<AstSymbolInstanceArc> {
        let mut symbols: Vec<AstSymbolInstanceArc> = vec![];
        let kind = info.node.kind();
        
        match kind {
            "struct_type" => {
                symbols.extend(self.parse_struct_declaration(info, code, candidates));
            }
            "function_declaration" => {
                symbols.extend(self.parse_function_declaration(info, code, candidates));
            }
            "method_declaration" => {
                symbols.extend(self.parse_method_declaration(info, code, candidates));
            }
            "import_declaration" => {
                symbols.extend(self.parse_import_declaration(info, code));
            }
            "field_declaration" => {
                symbols.extend(self.parse_field_declaration(info, code));
            }
            "comment" => {
                let mut def = CommentDefinition::default();
                def.ast_fields.language = info.ast_fields.language;
                def.ast_fields.full_range = info.node.range();
                def.ast_fields.file_path = info.ast_fields.file_path.clone();
                def.ast_fields.parent_guid = Some(info.parent_guid.clone());
                def.ast_fields.guid = get_guid();
                def.ast_fields.is_error = false;
                symbols.push(Arc::new(RwLock::new(Box::new(def))));
            }
            "type_declaration" => {
                // Handle type declarations (like struct types)
                for i in 0..info.node.child_count() {
                    let child = info.node.child(i).unwrap();
                    candidates.push_back(CandidateInfo {
                        ast_fields: info.ast_fields.clone(),
                        node: child,
                        parent_guid: info.parent_guid.clone(),
                    });
                }
            }
            "source_file" => {
                // Handle top-level declarations
                for i in 0..info.node.child_count() {
                    let child = info.node.child(i).unwrap();
                    candidates.push_back(CandidateInfo {
                        ast_fields: info.ast_fields.clone(),
                        node: child,
                        parent_guid: info.parent_guid.clone(),
                    });
                }
            }
            "call_expression" => {
                symbols.extend(self.parse_call_expression(info, code, candidates));
            }
            _ => {
                // Recursively process child nodes, but don't parse every identifier
                for i in 0..info.node.child_count() {
                    let child = info.node.child(i).unwrap();
                    candidates.push_back(CandidateInfo {
                        ast_fields: info.ast_fields.clone(),
                        node: child,
                        parent_guid: info.parent_guid.clone(),
                    });
                }
            }
        }
        
        symbols
    }

    fn parse_(&mut self, parent: &Node, code: &str, path: &PathBuf) -> Vec<AstSymbolInstanceArc> {
        let mut symbols: Vec<AstSymbolInstanceArc> = Default::default();
        let mut ast_fields = AstSymbolFields::default();
        ast_fields.file_path = path.clone();
        ast_fields.is_error = false;
        ast_fields.language = LanguageId::Go;

        let mut candidates = VecDeque::from(vec![CandidateInfo {
            ast_fields,
            node: parent.clone(),
            parent_guid: get_guid(),
        }]);
        
        while let Some(candidate) = candidates.pop_front() {
            let symbols_l = self.parse_usages_(&candidate, code, &mut candidates);
            symbols.extend(symbols_l);
        }
        
        // Build parent-child relationships
        let guid_to_symbol_map = symbols.iter()
            .map(|s| (s.clone().read().guid().clone(), s.clone())).collect::<HashMap<_, _>>();
        for symbol in symbols.iter_mut() {
            let guid = symbol.read().guid().clone();
            if let Some(parent_guid) = symbol.read().parent_guid() {
                if let Some(parent) = guid_to_symbol_map.get(parent_guid) {
                    parent.write().fields_mut().childs_guid.push(guid);
                }
            }
        }

        symbols
    }

    fn parse_call_expression<'a>(&mut self, info: &CandidateInfo<'a>, code: &str, candidates: &mut VecDeque<CandidateInfo<'a>>) -> Vec<AstSymbolInstanceArc> {
        let mut symbols: Vec<AstSymbolInstanceArc> = Default::default();
        let mut decl = FunctionCall::default();

        // Fill ast fields
        decl.ast_fields.language = info.ast_fields.language;
        decl.ast_fields.full_range = info.node.range();
        decl.ast_fields.file_path = info.ast_fields.file_path.clone();
        decl.ast_fields.parent_guid = Some(info.parent_guid.clone());
        decl.ast_fields.guid = get_guid();
        decl.ast_fields.is_error = info.ast_fields.is_error;

        // Extract function name
        if let Some(function_node) = info.node.child_by_field_name("function") {
            match function_node.kind() {
                // simple function call: foo()
                "identifier" => {
                    decl.ast_fields.name = code.slice(function_node.byte_range()).to_string();
                }
                // method or selector call: pkg.Func() or obj.Method()
                "selector_expression" => {
                    if let Some(field_node) = function_node.child_by_field_name("field") {
                        decl.ast_fields.name = code.slice(field_node.byte_range()).to_string();
                    }
                    if let Some(expr_node) = function_node.child_by_field_name("operand") {
                        candidates.push_back(CandidateInfo {
                            ast_fields: decl.ast_fields.clone(),
                            node: expr_node,
                            parent_guid: info.parent_guid.clone(),
                        });
                    }
                }
                // fallback: keep traversing
                _ => {
                    candidates.push_back(CandidateInfo {
                        ast_fields: decl.ast_fields.clone(),
                        node: function_node,
                        parent_guid: info.parent_guid.clone(),
                    });
                }
            }
        }

        // Parse arguments list to traverse inner expressions
        if let Some(args_node) = info.node.child_by_field_name("arguments")
            .or_else(|| info.node.child_by_field_name("argument_list")) {
            for i in 0..args_node.child_count() {
                let child = args_node.child(i).unwrap();
                candidates.push_back(CandidateInfo {
                    ast_fields: info.ast_fields.clone(),
                    node: child,
                    parent_guid: info.parent_guid.clone(),
                });
            }
        }

        symbols.push(Arc::new(RwLock::new(Box::new(decl))));
        symbols
    }
}

impl AstLanguageParser for GoParser {
    fn parse(&mut self, code: &str, path: &PathBuf) -> Vec<AstSymbolInstanceArc> {
        let tree = self.parser.parse(code, None).unwrap();
        self.parse_(&tree.root_node(), code, path)
    }
}

impl SkeletonFormatter for GoSkeletonFormatter {
    fn make_skeleton(&self, symbol: &SymbolInformation,
                     text: &String,
                     guid_to_children: &HashMap<Uuid, Vec<Uuid>>,
                     guid_to_info: &HashMap<Uuid, &SymbolInformation>) -> String {
        // For struct declarations, we want the full content, not just the declaration
        let mut res_line = if symbol.symbol_type == SymbolType::StructDeclaration {
            symbol.get_content(text).unwrap()
        } else {
            symbol.get_declaration_content(text).unwrap()
        };
        
        let children = guid_to_children.get(&symbol.guid).unwrap();
        if children.is_empty() {
            return format!("{res_line}\n  ...");
        }
        
        // For struct declarations, just return the struct definition without field details
        if symbol.symbol_type == SymbolType::StructDeclaration {
            // Replace tabs with two spaces
            let normalized = res_line.replace("\t", "  ");
            
            // Normalize multiple consecutive spaces to single spaces, but preserve indentation
            let lines: Vec<&str> = normalized.lines().collect();
            let mut result_lines = Vec::new();
            
            for line in lines {
                if line.trim().is_empty() {
                    result_lines.push(line.to_string());
                    continue;
                }
                
                let indent_len = line.len() - line.trim_start().len();
                let indent = &line[..indent_len];
                let content = line.trim_start();
                
                // Normalize multiple consecutive spaces in content only
                let normalized_content = content.replace("  ", " ");
                let result_line = format!("{}{}", indent, normalized_content);
                result_lines.push(result_line);
            }
            
            return result_lines.join("\n");
        }
        
        res_line = format!("{}\n", res_line);
        for child in children {
            let child_symbol = guid_to_info.get(&child).unwrap();
            match child_symbol.symbol_type {
                SymbolType::FunctionDeclaration => {
                    let content = child_symbol.get_declaration_content(text).unwrap();
                    let lines = content.lines().collect::<Vec<_>>();
                    for line in lines {
                        let trimmed_line = line.trim_start();
                        res_line = format!("{}  {}\n", res_line, trimmed_line);
                    }
                    res_line = format!("{}    ...\n", res_line);
                }
                SymbolType::ClassFieldDeclaration => {
                    res_line = format!("{}  {}\n", res_line, child_symbol.get_content(text).unwrap());
                }
                _ => {}
            }
        }

        res_line
    }

    fn get_declaration_with_comments(&self,
                                     symbol: &SymbolInformation,
                                     text: &String,
                                     guid_to_children: &HashMap<Uuid, Vec<Uuid>>,
                                     guid_to_info: &HashMap<Uuid, &SymbolInformation>) -> (String, (usize, usize)) {
        
        if let Some(children) = guid_to_children.get(&symbol.guid) {
            let mut res_line: Vec<String> = Default::default();
            let mut all_symbols = children.iter()
                .filter_map(|guid| guid_to_info.get(guid))
                .collect::<Vec<_>>();
            all_symbols.sort_by(|a, b|
                a.full_range.start_byte.cmp(&b.full_range.start_byte)
            );
            if symbol.symbol_type == SymbolType::FunctionDeclaration {
                res_line = symbol.get_content(text).unwrap().split("\n").map(|x| x.to_string()).collect::<Vec<_>>();
            } else {
                let mut content_lines = symbol.get_content(text).unwrap()
                    .split("\n")
                    .map(|x| x.to_string().replace("\t", "  ")).collect::<Vec<_>>();
                let mut intent_n = 0;
                if let Some(first) = content_lines.first_mut() {
                    intent_n = first.len() - first.trim_start().len();
                }
                
                // Process comments that come before the declaration
                for sym in all_symbols {
                    if sym.symbol_type != SymbolType::CommentDefinition {
                        break;
                    }
                    let content = sym.get_content(text).unwrap();
                    let lines = content.split("\n").collect::<Vec<_>>();
                    let lines = lines.iter()
                        .map(|x| x.to_string())
                        .collect::<Vec<_>>();
                    res_line.extend(lines);
                }
                
                // If we have comments, add them before the content
                if !res_line.is_empty() {
                    res_line.push(format!("{}...", " ".repeat(intent_n + 4)));
                    content_lines.extend(res_line);
                    res_line = content_lines;
                } else {
                    // If no comments, just return the content
                    res_line = content_lines;
                }
            }

            let declaration = res_line.join("\n");
            return (declaration, (symbol.full_range.start_point.row, symbol.full_range.end_point.row));
        }
        ("".to_string(), (0, 0))
    }
}


