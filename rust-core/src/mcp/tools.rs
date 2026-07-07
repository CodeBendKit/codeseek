use serde::Serialize;

/// MCP tool definition.
#[derive(Serialize, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// All codeseek MCP tools.
pub fn all_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "codeseek_search".into(),
            description: "Symbol search — finds symbols by name. Fast, locations-only lookup.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol name or partial name to search for (e.g. \"auth\", \"handleRequest\")"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum results to return (default: 10)",
                        "default": 10
                    }
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "codeseek_callers".into(),
            description: "List functions that call <symbol>. Use to understand upstream dependencies.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Name of the function, method, or class to find callers for"
                    }
                },
                "required": ["symbol"]
            }),
        },
        Tool {
            name: "codeseek_callees".into(),
            description: "List functions that <symbol> calls. Use to understand what a function depends on.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Name of the function, method, or class to find callees for"
                    }
                },
                "required": ["symbol"]
            }),
        },
        Tool {
            name: "codeseek_callgraph".into(),
            description: "Query function call graph with configurable depth. Shows both callers (upstream) and callees (downstream) around a center function. Depth controls how many layers of callers/callees to include (max 3). Use this to understand the full calling context of a function.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "function_name": {
                        "type": "string",
                        "description": "Name of the function to query as the center of the call graph"
                    },
                    "depth": {
                        "type": "number",
                        "description": "Query depth — layers of callers and callees to include (1-3, default: 1)",
                        "default": 1,
                        "minimum": 1,
                        "maximum": 3
                    }
                },
                "required": ["function_name"]
            }),
        },
        Tool {
            name: "codeseek_init".into(),
            description: "Build or update the code index for the current project. Run this first before using other codeseek tools. Idempotent — subsequent runs only re-process changed files.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        Tool {
            name: "codeseek_list".into(),
            description: "List all projects that have been indexed by codeseek. Returns project root paths.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        Tool {
            name: "codeseek_status".into(),
            description: "Index health check — files, symbols, last indexed time.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
    ]
}
