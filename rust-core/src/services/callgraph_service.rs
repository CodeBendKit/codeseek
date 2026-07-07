use std::collections::HashSet;
use uuid::Uuid;

use crate::codegraph::types::PetCodeGraph;

// ─── Core Query Functions ───────────────────────────────────────

/// Execute a callgraph query: given a function name and depth,
/// return formatted output (JSON or text) showing both callers and callees.
pub fn execute_callgraph(
    graph: &PetCodeGraph,
    symbol: &str,
    depth: u32,
    json: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let functions = graph.find_functions_by_name(symbol);

    if functions.is_empty() {
        if json {
            return Ok("{}".to_string());
        }
        return Ok(format!("No function found for '{}'", symbol));
    }

    // Use the first matching function as the center node
    let center = functions[0];

    if json {
        let mut visited = HashSet::new();
        visited.insert(center.id);

        let callers = collect_callers_json(graph, &center.id, depth, &mut visited);
        visited.clear();
        visited.insert(center.id);
        let callees = collect_callees_json(graph, &center.id, depth, &mut visited);

        let result = serde_json::json!({
            "function_name": symbol,
            "depth": depth,
            "center": {
                "name": center.name,
                "file_path": center.file_path,
                "line_start": center.line_start,
                "line_end": center.line_end,
                "signature": center.signature,
                "namespace": center.namespace,
                "language": center.language,
            },
            "callers": callers,
            "callees": callees,
        });

        Ok(serde_json::to_string_pretty(&result)?)
    } else {
        let mut output = format!("Call graph for '{}' (depth={}):\n\n", center.name, depth);

        output.push_str(&format!("== Callers (upstream, depth={}) ==\n", depth));
        let mut visited_callers_text = HashSet::new();
        visited_callers_text.insert(center.id);
        let callers_text = collect_callers_text(graph, &center.id, depth, 1, &mut visited_callers_text);
        if callers_text.is_empty() {
            output.push_str("  (none)\n");
        } else {
            output.push_str(&callers_text);
        }

        output.push_str(&format!("\n== Callees (downstream, depth={}) ==\n", depth));
        let mut visited_callees_text = HashSet::new();
        visited_callees_text.insert(center.id);
        let callees_text = collect_callees_text(graph, &center.id, depth, 1, &mut visited_callees_text);
        if callees_text.is_empty() {
            output.push_str("  (none)\n");
        } else {
            output.push_str(&callees_text);
        }

        Ok(output)
    }
}

/// Recursively collect callers (upstream) as JSON values.
pub fn collect_callers_json(
    graph: &PetCodeGraph,
    function_id: &Uuid,
    depth: u32,
    visited: &mut HashSet<Uuid>,
) -> Vec<serde_json::Value> {
    if depth == 0 {
        return vec![];
    }

    let mut results = vec![];
    for (caller, relation) in graph.get_callers(function_id) {
        // Skip already visited functions (prevents duplicates and cycles)
        if visited.contains(&caller.id) {
            continue;
        }
        visited.insert(caller.id);

        let mut node = serde_json::json!({
            "name": caller.name,
            "file_path": caller.file_path,
            "line_start": caller.line_start,
            "line_end": caller.line_end,
            "signature": caller.signature,
            "call_line": relation.line_number,
        });

        if depth > 1 {
            let sub_callers = collect_callers_json(graph, &caller.id, depth - 1, visited);
            node["callers"] = serde_json::Value::Array(sub_callers);
        } else {
            node["callers"] = serde_json::Value::Array(vec![]);
        }

        results.push(node);
    }

    results
}

/// Recursively collect callees (downstream) as JSON values.
pub fn collect_callees_json(
    graph: &PetCodeGraph,
    function_id: &Uuid,
    depth: u32,
    visited: &mut HashSet<Uuid>,
) -> Vec<serde_json::Value> {
    if depth == 0 {
        return vec![];
    }

    let mut results = vec![];
    for (callee, relation) in graph.get_callees(function_id) {
        // Skip already visited functions (prevents duplicates and cycles)
        if visited.contains(&callee.id) {
            continue;
        }
        visited.insert(callee.id);

        let mut node = serde_json::json!({
            "name": callee.name,
            "file_path": callee.file_path,
            "line_start": callee.line_start,
            "line_end": callee.line_end,
            "signature": callee.signature,
            "call_line": relation.line_number,
        });

        if depth > 1 {
            let sub_callees = collect_callees_json(graph, &callee.id, depth - 1, visited);
            node["callees"] = serde_json::Value::Array(sub_callees);
        } else {
            node["callees"] = serde_json::Value::Array(vec![]);
        }

        results.push(node);
    }

    results
}

/// Recursively collect callers as indented text.
pub fn collect_callers_text(
    graph: &PetCodeGraph,
    function_id: &Uuid,
    depth: u32,
    indent: usize,
    visited: &mut HashSet<Uuid>,
) -> String {
    if depth == 0 {
        return String::new();
    }

    let mut output = String::new();
    let indent_str = "  ".repeat(indent);

    for (caller, relation) in graph.get_callers(function_id) {
        if visited.contains(&caller.id) {
            continue;
        }
        visited.insert(caller.id);
        output.push_str(&format!(
            "{}{} ({}:{})\n",
            indent_str, caller.name, caller.file_path.display(), relation.line_number
        ));
        output.push_str(&collect_callers_text(graph, &caller.id, depth - 1, indent + 1, visited));
    }

    output
}

/// Recursively collect callees as indented text.
pub fn collect_callees_text(
    graph: &PetCodeGraph,
    function_id: &Uuid,
    depth: u32,
    indent: usize,
    visited: &mut HashSet<Uuid>,
) -> String {
    if depth == 0 {
        return String::new();
    }

    let mut output = String::new();
    let indent_str = "  ".repeat(indent);

    for (callee, relation) in graph.get_callees(function_id) {
        if visited.contains(&callee.id) {
            continue;
        }
        visited.insert(callee.id);
        output.push_str(&format!(
            "{}{} ({}:{})\n",
            indent_str, callee.name, callee.file_path.display(), relation.line_number
        ));
        output.push_str(&collect_callees_text(graph, &callee.id, depth - 1, indent + 1, visited));
    }

    output
}

// ─── Test Helpers ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::codegraph::types::{FunctionInfo, CallRelation};

    /// Helper to build a PetCodeGraph for testing.
    struct GraphBuilder {
        functions: Vec<FunctionInfo>,
        calls: Vec<(Uuid, Uuid, String, String, PathBuf, PathBuf)>,
    }

    impl GraphBuilder {
        fn new() -> Self {
            GraphBuilder {
                functions: Vec::new(),
                calls: Vec::new(),
            }
        }

        /// Add a function with auto-generated UUID.
        fn func(mut self, name: &str, file: &str) -> (Self, Uuid) {
            let id = Uuid::new_v4();
            self.functions.push(FunctionInfo {
                id,
                name: name.to_string(),
                file_path: PathBuf::from(file),
                line_start: 1,
                line_end: 10,
                namespace: String::new(),
                language: "rust".to_string(),
                signature: Some(format!("fn {}()", name)),
            });
            (self, id)
        }

        /// Add a call relation. Returns self for chaining.
        fn call(mut self, caller_id: Uuid, callee_id: Uuid, caller_name: &str, callee_name: &str, caller_file: &str, callee_file: &str) -> Self {
            self.calls.push((
                caller_id,
                callee_id,
                caller_name.to_string(),
                callee_name.to_string(),
                PathBuf::from(caller_file),
                PathBuf::from(callee_file),
            ));
            self
        }

        fn build(self) -> PetCodeGraph {
            let mut graph = PetCodeGraph::new();
            for func in self.functions {
                graph.add_function(func);
            }
            for (caller_id, callee_id, caller_name, callee_name, caller_file, callee_file) in self.calls {
                let relation = CallRelation {
                    caller_id,
                    callee_id,
                    caller_name,
                    callee_name,
                    caller_file,
                    callee_file,
                    line_number: 5,
                    is_resolved: true,
                };
                let _ = graph.add_call_relation(relation);
            }
            graph.update_stats();
            graph
        }
    }

    // ─── Graph Topologies ─────────────────────────────────────────

    /// Linear: A → B → C → D
    fn linear_graph() -> PetCodeGraph {
        let (builder, id_a) = GraphBuilder::new().func("A", "a.rs");
        let (builder, id_b) = builder.func("B", "b.rs");
        let (builder, id_c) = builder.func("C", "c.rs");
        let (builder, id_d) = builder.func("D", "d.rs");
        builder
            .call(id_a, id_b, "A", "B", "a.rs", "b.rs")
            .call(id_b, id_c, "B", "C", "b.rs", "c.rs")
            .call(id_c, id_d, "C", "D", "c.rs", "d.rs")
            .build()
    }

    /// Branch (fan-out): A → B, A → C, A → D
    fn branch_graph() -> PetCodeGraph {
        let (builder, id_a) = GraphBuilder::new().func("A", "a.rs");
        let (builder, id_b) = builder.func("B", "b.rs");
        let (builder, id_c) = builder.func("C", "c.rs");
        let (builder, id_d) = builder.func("D", "d.rs");
        builder
            .call(id_a, id_b, "A", "B", "a.rs", "b.rs")
            .call(id_a, id_c, "A", "C", "a.rs", "c.rs")
            .call(id_a, id_d, "A", "D", "a.rs", "d.rs")
            .build()
    }

    /// Converge (fan-in): B → A, C → A, D → A
    fn converge_graph() -> PetCodeGraph {
        let (builder, id_a) = GraphBuilder::new().func("A", "a.rs");
        let (builder, id_b) = builder.func("B", "b.rs");
        let (builder, id_c) = builder.func("C", "c.rs");
        let (builder, id_d) = builder.func("D", "d.rs");
        builder
            .call(id_b, id_a, "B", "A", "b.rs", "a.rs")
            .call(id_c, id_a, "C", "A", "c.rs", "a.rs")
            .call(id_d, id_a, "D", "A", "d.rs", "a.rs")
            .build()
    }

    /// Cycle: A → B → C → A
    fn cycle_graph() -> PetCodeGraph {
        let (builder, id_a) = GraphBuilder::new().func("A", "a.rs");
        let (builder, id_b) = builder.func("B", "b.rs");
        let (builder, id_c) = builder.func("C", "c.rs");
        builder
            .call(id_a, id_b, "A", "B", "a.rs", "b.rs")
            .call(id_b, id_c, "B", "C", "b.rs", "c.rs")
            .call(id_c, id_a, "C", "A", "c.rs", "a.rs")
            .build()
    }

    /// Diamond: A → B, A → C, B → D, C → D
    fn diamond_graph() -> PetCodeGraph {
        let (builder, id_a) = GraphBuilder::new().func("A", "a.rs");
        let (builder, id_b) = builder.func("B", "b.rs");
        let (builder, id_c) = builder.func("C", "c.rs");
        let (builder, id_d) = builder.func("D", "d.rs");
        builder
            .call(id_a, id_b, "A", "B", "a.rs", "b.rs")
            .call(id_a, id_c, "A", "C", "a.rs", "c.rs")
            .call(id_b, id_d, "B", "D", "b.rs", "d.rs")
            .call(id_c, id_d, "C", "D", "c.rs", "d.rs")
            .build()
    }

    /// Empty graph (no functions)
    fn empty_graph() -> PetCodeGraph {
        PetCodeGraph::new()
    }

    /// Isolated function E (no edges)
    fn isolated_graph() -> PetCodeGraph {
        let (builder, _) = GraphBuilder::new().func("E", "e.rs");
        builder.build()
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Error Cases
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_function_not_found_json() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "NONEXISTENT", 1, true).unwrap();
        assert_eq!(result, "{}");
    }

    #[test]
    fn test_function_not_found_text() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "NONEXISTENT", 1, false).unwrap();
        assert_eq!(result, "No function found for 'NONEXISTENT'");
    }

    #[test]
    fn test_empty_graph_returns_not_found() {
        let graph = empty_graph();
        let result = execute_callgraph(&graph, "anything", 1, false).unwrap();
        assert_eq!(result, "No function found for 'anything'");
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Linear Graph — A → B → C → D
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_linear_callers_depth_1() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "C", 1, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(!result.contains("A"));
    }

    #[test]
    fn test_linear_callers_depth_2() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "C", 2, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("A (a.rs:5)"));
    }

    #[test]
    fn test_linear_callees_depth_1() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 1, false).unwrap();
        // Remove the debug print
        assert!(result.contains("B (b.rs:5)"));
        // Check that function C is not in the callees section (avoid matching "Callers" header)
        assert!(!result.contains("C (c.rs"));
    }

    #[test]
    fn test_linear_callees_depth_2() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 2, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
        assert!(!result.contains("D"));
    }

    #[test]
    fn test_linear_callees_depth_3() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 3, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
        assert!(result.contains("D (d.rs:5)"));
    }

    #[test]
    fn test_linear_root_has_no_callers() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 3, false).unwrap();
        assert!(result.contains("(none)"));
    }

    #[test]
    fn test_linear_leaf_has_no_callees() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "D", 3, false).unwrap();
        assert!(result.contains("(none)"));
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Branch — A → B, A → C, A → D
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_branch_callees_contain_all_children() {
        let graph = branch_graph();
        let result = execute_callgraph(&graph, "A", 1, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
        assert!(result.contains("D (d.rs:5)"));
    }

    #[test]
    fn test_branch_leaf_has_one_caller() {
        let graph = branch_graph();
        let result = execute_callgraph(&graph, "B", 1, false).unwrap();
        assert!(result.contains("A (a.rs:5)"));
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Converge — B → A, C → A, D → A
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_converge_callers_contain_all_callers() {
        let graph = converge_graph();
        let result = execute_callgraph(&graph, "A", 1, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
        assert!(result.contains("D (d.rs:5)"));
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Cycle — A → B → C → A
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_cycle_no_infinite_loop_in_text() {
        let graph = cycle_graph();
        // Must terminate within reasonable time
        let result = execute_callgraph(&graph, "A", 10, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
    }

    #[test]
    fn test_cycle_no_infinite_loop_in_json() {
        let graph = cycle_graph();
        let result = execute_callgraph(&graph, "A", 10, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        // The JSON output should not hang and should contain valid data
        assert_eq!(parsed["function_name"], "A");
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Diamond — A → B → D, A → C → D
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_diamond_depth_1() {
        let graph = diamond_graph();
        let result = execute_callgraph(&graph, "A", 1, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
        assert!(!result.contains("D"));
    }

    #[test]
    fn test_diamond_depth_2() {
        let graph = diamond_graph();
        let result = execute_callgraph(&graph, "A", 2, false).unwrap();
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
        assert!(result.contains("D (d.rs:5)"));
    }

    #[test]
    fn test_diamond_callers_of_d() {
        let graph = diamond_graph();
        let result = execute_callgraph(&graph, "D", 2, false).unwrap();
        // D has callers B and C at depth 1
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
        // A is a caller of B and C (depth 2 from D)
        assert!(result.contains("A (a.rs:5)"));
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: JSON Output
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_json_output_is_valid() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 1, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["function_name"], "A");
        assert_eq!(parsed["depth"], 1);
        assert!(parsed["center"].is_object());
        assert!(parsed["callers"].is_array());
        assert!(parsed["callees"].is_array());
    }

    #[test]
    fn test_json_contains_function_details() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "B", 1, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["center"]["name"], "B");
        assert_eq!(parsed["center"]["file_path"], "b.rs");
        assert_eq!(parsed["center"]["signature"], "fn B()");
    }

    #[test]
    fn test_json_callers_have_nested_structure() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "C", 2, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let callers = parsed["callers"].as_array().unwrap();
        assert!(!callers.is_empty());
        // At depth 2, the caller B should have A as its nested caller
        let b_node = &callers[0];
        assert_eq!(b_node["name"], "B");
        let nested = b_node["callers"].as_array().unwrap();
        assert!(!nested.is_empty());
        assert_eq!(nested[0]["name"], "A");
    }

    #[test]
    fn test_json_callees_have_nested_structure() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 2, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let callees = parsed["callees"].as_array().unwrap();
        assert!(!callees.is_empty());
        let b_node = &callees[0];
        assert_eq!(b_node["name"], "B");
        let nested = b_node["callees"].as_array().unwrap();
        assert!(!nested.is_empty());
        assert_eq!(nested[0]["name"], "C");
    }

    #[test]
    fn test_json_empty_callers_for_root() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 10, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["callers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_json_empty_callees_for_leaf() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "D", 10, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["callees"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_json_output_contains_call_line() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "B", 1, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        if let Some(callers) = parsed["callers"].as_array() {
            if !callers.is_empty() {
                assert_eq!(callers[0]["call_line"], 5);
            }
        }
    }

    #[test]
    fn test_json_function_not_found_returns_empty_object() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "UNKNOWN", 1, true).unwrap();
        assert_eq!(result, "{}");
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Text Output
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_text_output_header() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "B", 1, false).unwrap();
        assert!(result.contains("Call graph for 'B'"));
        assert!(result.contains("== Callers (upstream"));
        assert!(result.contains("== Callees (downstream"));
    }

    #[test]
    fn test_text_output_indentation_by_depth() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "D", 3, false).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        let c_line = lines.iter().find(|l| l.contains("C (c.rs")).unwrap();
        let b_line = lines.iter().find(|l| l.contains("B (b.rs")).unwrap();
        let a_line = lines.iter().find(|l| l.contains("A (a.rs")).unwrap();
        // C is depth 1 from D -> 2 spaces indent
        assert!(c_line.starts_with("  "));
        assert!(!c_line.starts_with("    "));
        // B is depth 2 from D -> 4 spaces indent
        assert!(b_line.starts_with("    "));
        // A is depth 3 from D -> 6 spaces indent
        assert!(a_line.starts_with("      "));
    }

    #[test]
    fn test_text_root_shows_none_for_callers() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 3, false).unwrap();
        assert!(result.contains("(none)"));
    }

    #[test]
    fn test_text_leaf_shows_none_for_callees() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "D", 3, false).unwrap();
        assert!(result.contains("(none)"));
    }

    #[test]
    fn test_text_empty_graph() {
        let graph = empty_graph();
        let result = execute_callgraph(&graph, "main", 1, false).unwrap();
        assert_eq!(result, "No function found for 'main'");
    }

    #[test]
    fn test_text_isolated_function() {
        let graph = isolated_graph();
        let result = execute_callgraph(&graph, "E", 3, false).unwrap();
        assert!(result.contains("(none)")); // both callers and callees should be (none)
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Depth Boundary
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_depth_0_text() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "C", 0, false).unwrap();
        // With depth=0, no callers or callees should be shown
        assert!(result.contains("(none)"));
        assert!(!result.contains("B (b.rs"));
    }

    #[test]
    fn test_depth_0_json() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "C", 0, true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["callers"].as_array().unwrap().is_empty());
        assert!(parsed["callees"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_depth_exceeds_graph_size() {
        let graph = linear_graph();
        let result = execute_callgraph(&graph, "A", 100, false).unwrap();
        // Should show all reachable nodes (B, C, D) without error
        assert!(result.contains("B (b.rs:5)"));
        assert!(result.contains("C (c.rs:5)"));
        assert!(result.contains("D (d.rs:5)"));
    }

    // ═══════════════════════════════════════════════════════════════
    // TESTS: Direct function calls (internal consistency)
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_collect_callers_json_empty_at_depth_0() {
        let graph = linear_graph();
        let funcs = graph.find_functions_by_name("B");
        assert!(!funcs.is_empty());
        let mut visited = HashSet::new();
        let result = collect_callers_json(&graph, &funcs[0].id, 0, &mut visited);
        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_callees_json_empty_at_depth_0() {
        let graph = linear_graph();
        let funcs = graph.find_functions_by_name("B");
        assert!(!funcs.is_empty());
        let mut visited = HashSet::new();
        let result = collect_callees_json(&graph, &funcs[0].id, 0, &mut visited);
        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_callers_text_empty_at_depth_0() {
        let graph = linear_graph();
        let funcs = graph.find_functions_by_name("B");
        assert!(!funcs.is_empty());
        let mut visited = HashSet::new();
        let result = collect_callers_text(&graph, &funcs[0].id, 0, 1, &mut visited);
        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_callees_text_empty_at_depth_0() {
        let graph = linear_graph();
        let funcs = graph.find_functions_by_name("B");
        assert!(!funcs.is_empty());
        let mut visited = HashSet::new();
        let result = collect_callees_text(&graph, &funcs[0].id, 0, 1, &mut visited);
        assert!(result.is_empty());
    }
}
