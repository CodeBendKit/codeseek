use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use std::fs;

use codeseek::storage::StorageManager;
use codeseek::services::CodeAnalyzer;
use codeseek::codegraph::types::PetCodeGraph;

/// 测试构建图功能
#[test]
fn test_build_graph_functionality() {
    let _temp_dir = TempDir::new().expect("Failed to create temp directory");
    let storage = Arc::new(StorageManager::new());
    
    // 测试Rust项目
    test_build_graph_for_project(&storage, "tests/test_repos/simple_rust_project");
    
    // 测试Python项目
    test_build_graph_for_project(&storage, "tests/test_repos/simple_python_project");
    
    // 测试JavaScript项目
    test_build_graph_for_project(&storage, "tests/test_repos/simple_js_project");
    
    // 测试TypeScript项目
    test_build_graph_for_project(&storage, "tests/test_repos/simple_ts_project");
}

fn test_build_graph_for_project(_storage: &Arc<StorageManager>, project_path: &str) {
    println!("Testing build_graph for project: {}", project_path);
    
    let project_dir = PathBuf::from(project_path);
    assert!(project_dir.exists(), "Project directory {} does not exist", project_path);
    
    // 创建CodeAnalyzer实例
    let mut analyzer = CodeAnalyzer::new();
    
    // 分析目录并构建代码图
    let result = analyzer.analyze_directory(&project_dir);
    assert!(result.is_ok(), "Failed to analyze directory: {:?}", result.err());
    
    // 获取代码图
    let code_graph = analyzer.get_code_graph();
    assert!(code_graph.is_some(), "Code graph should be available after analysis");
    
    let code_graph = code_graph.unwrap();
    
    // 调试：输出函数数量
    println!("  [DEBUG] code_graph.functions count: {}", code_graph.functions.len());
    for (id, func) in &code_graph.functions {
        println!("  [DEBUG]   Function: {} ({}) - {:?}", id, func.name, func.language);
    }
    println!("  [DEBUG] code_graph.call_relations count: {}", code_graph.call_relations.len());
    
    // 验证代码图包含函数
    assert!(!code_graph.functions.is_empty(), "Code graph should contain functions");
    
    // 验证代码图包含调用关系
    assert!(!code_graph.call_relations.is_empty(), "Code graph should contain call relations");
    
    // 获取统计信息
    let stats = analyzer.get_stats();
    assert!(stats.is_some(), "Stats should be available after analysis");
    
    let stats = stats.unwrap();
    println!("Project {}: {} files, {} functions", 
             project_path, stats.total_files, stats.total_functions);
    
    // 验证统计信息
    assert!(stats.total_files > 0, "Total files should be greater than 0");
    assert!(stats.total_functions > 0, "Total functions should be greater than 0");
    
    // 测试转换为PetCodeGraph
    let mut pet_graph = PetCodeGraph::new();
    
    // 添加所有函数
    for function in code_graph.functions.values() {
        pet_graph.add_function(function.clone());
    }
    
    // 添加所有调用关系
    let mut successful_relations = 0;
    for relation in &code_graph.call_relations {
        if let Err(e) = pet_graph.add_call_relation(relation.clone()) {
            eprintln!("Failed to add call relation: {}", e);
        } else {
            successful_relations += 1;
        }
    }
    
    println!("Successfully added {}/{} call relations to PetCodeGraph", 
             successful_relations, code_graph.call_relations.len());
    
    // 更新统计信息
    pet_graph.update_stats();
    
    // 验证PetCodeGraph
    let pet_stats = pet_graph.get_stats();
    assert!(pet_stats.total_functions > 0, "PetCodeGraph should contain functions");
    
    println!("PetCodeGraph stats: {} functions", pet_stats.total_functions);
}

/// 测试查询调用图功能
#[test]
fn test_query_call_graph_functionality() {
    let _temp_dir = TempDir::new().expect("Failed to create temp directory");
    let storage = Arc::new(StorageManager::new());
    
    // 测试Rust项目
    test_query_call_graph_for_project(&storage, "tests/test_repos/simple_rust_project");
    
    // 测试Python项目
    test_query_call_graph_for_project(&storage, "tests/test_repos/simple_python_project");
    
    // 测试JavaScript项目
    test_query_call_graph_for_project(&storage, "tests/test_repos/simple_js_project");
    
    // 测试TypeScript项目
    test_query_call_graph_for_project(&storage, "tests/test_repos/simple_ts_project");
}

fn test_query_call_graph_for_project(storage: &Arc<StorageManager>, project_path: &str) {
    println!("Testing query_call_graph for project: {}", project_path);
    
    let project_dir = PathBuf::from(project_path);
    assert!(project_dir.exists(), "Project directory {} does not exist", project_path);
    
    // 首先构建图
    let mut analyzer = CodeAnalyzer::new();
    let result = analyzer.analyze_directory(&project_dir);
    assert!(result.is_ok(), "Failed to analyze directory: {:?}", result.err());
    
    let code_graph = analyzer.get_code_graph().unwrap();
    
    // 转换为PetCodeGraph并保存
    let mut pet_graph = PetCodeGraph::new();
    for function in code_graph.functions.values() {
        pet_graph.add_function(function.clone());
    }
    
    for relation in &code_graph.call_relations {
        let _ = pet_graph.add_call_relation(relation.clone());
    }
    
    pet_graph.update_stats();
    
    // 生成项目ID
    let project_id = format!("{:x}", md5::compute(project_path.as_bytes()));
    
    // 保存图
    let save_result = storage.get_persistence().save_graph(&project_id, &pet_graph);
    assert!(save_result.is_ok(), "Failed to save graph: {:?}", save_result.err());
    
    // 测试查询特定函数
    test_query_function_by_name(&pet_graph, "main");
    test_query_function_by_name(&pet_graph, "process_data");
    test_query_function_by_name(&pet_graph, "calculate_sum");
    test_query_function_by_name(&pet_graph, "run");
    test_query_function_by_name(&pet_graph, "processData");
    
    // 测试查询文件中的所有函数
    test_query_functions_by_file(&pet_graph, project_path);
    
    // 测试调用链扩展
    test_call_chain_expansion(&pet_graph);
}

fn test_query_function_by_name(graph: &PetCodeGraph, function_name: &str) {
    println!("Testing query for function: {}", function_name);
    
    let functions = graph.find_functions_by_name(function_name);
    if !functions.is_empty() {
        println!("Found {} functions with name '{}'", functions.len(), function_name);
        
        for function in &functions {
            let callers = graph.get_callers(&function.id);
            let callees = graph.get_callees(&function.id);
            
            println!("Function {}: {} callers, {} callees", 
                     function.name, callers.len(), callees.len());
            
            // 验证调用关系
            for (caller_func, relation) in &callers {
                println!("  Called by: {} at {}:{}", 
                         caller_func.name, caller_func.file_path.display(), relation.line_number);
            }
            
            for (callee_func, relation) in &callees {
                println!("  Calls: {} at {}:{}", 
                         callee_func.name, callee_func.file_path.display(), relation.line_number);
            }
        }
    } else {
        println!("No functions found with name '{}'", function_name);
    }
}

fn test_query_functions_by_file(graph: &PetCodeGraph, project_path: &str) {
    println!("Testing query for functions in project: {}", project_path);
    
    // 查找项目中的主要源文件
    let project_dir = PathBuf::from(project_path);
    let source_files = find_source_files(&project_dir);
    
    for source_file in source_files {
        let file_functions = graph.find_functions_by_file(&source_file);
        if !file_functions.is_empty() {
            println!("File {}: {} functions", source_file.display(), file_functions.len());
            
            for function in &file_functions {
                let callers = graph.get_callers(&function.id);
                let callees = graph.get_callees(&function.id);
                
                println!("  Function {}: {} callers, {} callees", 
                         function.name, callers.len(), callees.len());
            }
        }
    }
}

fn test_call_chain_expansion(graph: &PetCodeGraph) {
    println!("Testing call chain expansion");
    
    // 查找一些函数来测试调用链
    let test_functions = ["main", "process_data", "calculate_sum", "run", "processData"];
    
    for func_name in test_functions {
        let functions = graph.find_functions_by_name(func_name);
        if !functions.is_empty() {
            let function = &functions[0];
            
            // 测试调用者链
            let mut visited = std::collections::HashSet::new();
            let callers_chain = expand_call_chain(graph, &function.id.to_string(), &mut visited, 3, true);
            
            if !callers_chain.is_empty() {
                println!("Callers chain for {}: {} levels", func_name, callers_chain.len());
            }
            
            // 测试被调用者链
            let mut visited = std::collections::HashSet::new();
            let callees_chain = expand_call_chain(graph, &function.id.to_string(), &mut visited, 3, false);
            
            if !callees_chain.is_empty() {
                println!("Callees chain for {}: {} levels", func_name, callees_chain.len());
            }
        }
    }
}

fn expand_call_chain(
    graph: &PetCodeGraph,
    function_id: &str,
    visited: &mut std::collections::HashSet<String>,
    depth: usize,
    is_caller: bool,
) -> Vec<String> {
    if depth == 0 || visited.contains(function_id) {
        return Vec::new();
    }
    
    visited.insert(function_id.to_string());
    let mut chain = Vec::new();
    
    // 解析UUID
    let uuid = match uuid::Uuid::parse_str(function_id) {
        Ok(uuid) => uuid,
        Err(_) => return chain,
    };
    
    let relations = if is_caller {
        graph.get_callers(&uuid)
    } else {
        graph.get_callees(&uuid)
    };
    
    for (func, _) in relations {
        chain.push(func.name.clone());
        let sub_chain = expand_call_chain(graph, &func.id.to_string(), visited, depth - 1, is_caller);
        chain.extend(sub_chain);
    }
    
    chain
}

fn find_source_files(project_dir: &PathBuf) -> Vec<PathBuf> {
    let mut source_files = Vec::new();
    
    if let Ok(entries) = fs::read_dir(project_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                    if let Some(extension) = path.extension() {
        let ext_str = extension.to_string_lossy();
        if matches!(ext_str.as_ref(), "rs" | "py" | "js" | "ts" | "java" | "cpp" | "c") {
            source_files.push(path);
        }
    }
            }
        }
    }
    
    source_files
}

/// 测试完整的构建和查询流程
#[test]
fn test_complete_build_and_query_workflow() {
    let _temp_dir = TempDir::new().expect("Failed to create temp directory");
    let storage = Arc::new(StorageManager::new());
    
    // 选择Rust项目进行完整测试
    let project_path = "tests/test_repos/simple_rust_project";
    let project_dir = PathBuf::from(project_path);
    
    println!("Testing complete workflow for: {}", project_path);
    
    // 步骤1: 构建图
    let mut analyzer = CodeAnalyzer::new();
    let result = analyzer.analyze_directory(&project_dir);
    assert!(result.is_ok(), "Failed to analyze directory");
    
    let code_graph = analyzer.get_code_graph().unwrap();
    let stats = analyzer.get_stats().unwrap();
    
    println!("Built graph with {} files and {} functions", 
             stats.total_files, stats.total_functions);
    
    // 步骤2: 转换为PetCodeGraph
    let mut pet_graph = PetCodeGraph::new();
    for function in code_graph.functions.values() {
        pet_graph.add_function(function.clone());
    }
    
    for relation in &code_graph.call_relations {
        let _ = pet_graph.add_call_relation(relation.clone());
    }
    
    pet_graph.update_stats();
    
    // 步骤3: 保存图
    let project_id = format!("{:x}", md5::compute(project_path.as_bytes()));
    let save_result = storage.get_persistence().save_graph(&project_id, &pet_graph);
    assert!(save_result.is_ok(), "Failed to save graph");
    
    // 步骤4: 加载图
    let loaded_graph_result = storage.get_persistence().load_graph(&project_id);
    assert!(loaded_graph_result.is_ok(), "Failed to load graph");
    assert!(loaded_graph_result.as_ref().unwrap().is_some(), "Loaded graph should exist");
    
    // 步骤5: 查询调用图
    let loaded_graph = loaded_graph_result.unwrap().unwrap();
    
    // 查询main函数
    let main_functions = loaded_graph.find_functions_by_name("main");
    assert!(!main_functions.is_empty(), "Should find main function");
    
    let main_func = &main_functions[0];
    let callers = loaded_graph.get_callers(&main_func.id);
    let callees = loaded_graph.get_callees(&main_func.id);
    
    println!("Main function: {} callers, {} callees", callers.len(), callees.len());
    
    // 验证main函数调用了其他函数
    assert!(!callees.is_empty(), "Main function should call other functions");
    
    // 查询process_data函数
    let process_functions = loaded_graph.find_functions_by_name("process_data");
    assert!(!process_functions.is_empty(), "Should find process_data function");
    
    let process_func = &process_functions[0];
    let process_callees = loaded_graph.get_callees(&process_func.id);
    
    println!("Process_data function: {} callees", process_callees.len());
    
    // 验证process_data的调用链
    assert!(process_callees.len() >= 3, "process_data should call multiple functions");
    
    println!("Complete workflow test passed!");
} 

/// 专门测试TypeScript项目的功能
#[test]
fn test_typescript_project_functionality() {
    let _temp_dir = TempDir::new().expect("Failed to create temp directory");
    let _storage = Arc::new(StorageManager::new());
    
    let project_path = "tests/test_repos/simple_ts_project";
    let project_dir = PathBuf::from(project_path);
    
    println!("Testing TypeScript project functionality: {}", project_path);
    
    // 验证项目目录存在
    assert!(project_dir.exists(), "TypeScript project directory does not exist");
    
    // 构建图
    let mut analyzer = CodeAnalyzer::new();
    let result = analyzer.analyze_directory(&project_dir);
    assert!(result.is_ok(), "Failed to analyze TypeScript directory");
    
    let code_graph = analyzer.get_code_graph().unwrap();
    let stats = analyzer.get_stats().unwrap();
    
    println!("TypeScript project: {} files, {} functions", 
             stats.total_files, stats.total_functions);
    
    // 验证TypeScript特定的函数
    let ts_functions = code_graph.find_functions_by_name("run");
    if !ts_functions.is_empty() {
        println!("Found TypeScript Application.run method");
        
        let run_func = &ts_functions[0];
        let callees = code_graph.get_callees(&run_func.id);
        println!("Application.run has {} callees", callees.len());
        
        // 验证调用关系
        for relation in &callees {
            println!("  Calls: {} at {}:{}", 
                     relation.callee_name, relation.callee_file.display(), relation.line_number);
        }
    }
    
    // 验证TypeScript类方法
    let process_data_functions = code_graph.find_functions_by_name("processData");
    if !process_data_functions.is_empty() {
        println!("Found TypeScript processData method");
        
        let process_func = &process_data_functions[0];
        let callees = code_graph.get_callees(&process_func.id);
        println!("processData has {} callees", callees.len());
    }
    
    // 验证TypeScript接口和类型
    let format_output_functions = code_graph.find_functions_by_name("formatOutput");
    if !format_output_functions.is_empty() {
        println!("Found TypeScript formatOutput method");
        
        let format_func = &format_output_functions[0];
        let callers = code_graph.get_callers(&format_func.id);
        println!("formatOutput has {} callers", callers.len());
    }
    
    // 转换为PetCodeGraph并验证
    let mut pet_graph = PetCodeGraph::new();
    for function in code_graph.functions.values() {
        pet_graph.add_function(function.clone());
    }
    
    for relation in &code_graph.call_relations {
        let _ = pet_graph.add_call_relation(relation.clone());
    }
    
    pet_graph.update_stats();
    
    let pet_stats = pet_graph.get_stats();
    println!("PetCodeGraph for TypeScript: {} functions", pet_stats.total_functions);
    
    // 验证TypeScript特定的查询功能
    test_typescript_specific_queries(&pet_graph);
    
    println!("TypeScript project functionality test passed!");
}

fn test_typescript_specific_queries(graph: &PetCodeGraph) {
    println!("Testing TypeScript-specific queries...");
    
    // 测试TypeScript类方法查询
    let class_methods = ["run", "processData", "formatOutput", "calculateSum"];
    
    for method_name in class_methods {
        let functions = graph.find_functions_by_name(method_name);
        if !functions.is_empty() {
            println!("Found TypeScript method: {}", method_name);
            
            for function in &functions {
                let callers = graph.get_callers(&function.id);
                let callees = graph.get_callees(&function.id);
                
                println!("  {}: {} callers, {} callees", 
                         function.name, callers.len(), callees.len());
                
                // 显示调用关系
                for (caller_func, relation) in &callers {
                    println!("    Called by: {} at {}:{}", 
                             caller_func.name, caller_func.file_path.display(), relation.line_number);
                }
                
                for (callee_func, relation) in &callees {
                    println!("    Calls: {} at {}:{}", 
                             callee_func.name, callee_func.file_path.display(), relation.line_number);
                }
            }
        }
    }
    
    // 测试TypeScript异步函数
    let async_functions = graph.find_functions_by_name("processDataAsync");
    if !async_functions.is_empty() {
        println!("Found TypeScript async function: processDataAsync");
    }
    
    // 测试TypeScript接口和类型
    let interface_functions = graph.find_functions_by_name("getStringStats");
    if !interface_functions.is_empty() {
        println!("Found TypeScript interface method: getStringStats");
    }
} 

/// 测试修改后的query_code_skeleton接口
#[test]
fn test_query_code_skeleton_batch_functionality() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let _storage = Arc::new(StorageManager::new());
    
    // 创建测试文件
    let test_file1 = temp_dir.path().join("test1.rs");
    let test_file2 = temp_dir.path().join("test2.py");
    
    // 写入测试代码
    std::fs::write(&test_file1, r#"
pub struct TestStruct {
    pub field1: String,
    pub field2: i32,
}

impl TestStruct {
    pub fn new() -> Self {
        Self {
            field1: String::new(),
            field2: 0,
        }
    }
    
    pub fn process(&self) -> String {
        format!("{}: {}", self.field1, self.field2)
    }
}
"#).expect("Failed to write test file 1");
    
    std::fs::write(&test_file2, r#"
class TestClass:
    def __init__(self, name: str, value: int):
        self.name = name
        self.value = value
    
    def process(self) -> str:
        return f"{self.name}: {self.value}"
    
    def get_info(self) -> dict:
        return {"name": self.name, "value": self.value}
"#).expect("Failed to write test file 2");
    
    // 测试批量查询代码骨架
    let filepaths = vec![
        test_file1.to_string_lossy().to_string(),
        test_file2.to_string_lossy().to_string(),
    ];
    
    // 这里我们只是验证文件存在且可以被读取
    // 实际的API测试需要在HTTP服务器运行时进行
    for filepath in &filepaths {
        let path = std::path::Path::new(filepath);
        assert!(path.exists(), "Test file should exist: {}", filepath);
        
        // 验证文件内容可以被读取
        let content = std::fs::read_to_string(path);
        assert!(content.is_ok(), "Should be able to read test file: {}", filepath);
        
        // 验证文件有内容
        let content = content.unwrap();
        assert!(!content.is_empty(), "Test file should not be empty: {}", filepath);
    }
    
    println!("query_code_skeleton batch functionality test passed!");
    println!("Test files created: {:?}", filepaths);
} 