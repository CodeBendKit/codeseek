//! Minimal MCP stdio server (JSON-RPC 2.0).
//! Reads from stdin, writes to stdout, delegates to CLI commands.

use std::io::{self, BufRead, Write};
use serde_json::{Value, json};
use super::tools::all_tools;

pub async fn run_mcp_server() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = request.get("id").cloned();

        let response = match method {
            "initialize" => handle_initialize(id),
            "notifications/initialized" => None, // no response for notifications
            "tools/list" => handle_tools_list(id),
            "tools/call" => handle_tools_call(id, &request),
            _ => {
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {}", method)
                    }
                }))
            }
        };

        if let Some(resp) = response {
            writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
            stdout.flush()?;
        }
    }

    Ok(())
}

fn handle_initialize(id: Option<Value>) -> Option<Value> {
    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "codeseek",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Code intelligence CLI — AST-based call graph + semantic search. Use codeseek_search to find symbols, codeseek_callers/codeseek_callees to trace call relationships, and codeseek_status to check index health.\n\nTool selection:\n- codeseek_search — quick symbol lookup (name → location)\n- codeseek_callers — who depends on this?\n- codeseek_callees — what does this depend on?\n- codeseek_status — is the index up to date?"
        }
    }))
}

fn handle_tools_list(id: Option<Value>) -> Option<Value> {
    let tools = all_tools();
    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": tools
        }
    }))
}

fn handle_tools_call(id: Option<Value>, request: &Value) -> Option<Value> {
    let params = request.get("params")?;
    let tool_name = params.get("name")?.as_str()?;
    let default_args = json!({});
    let arguments = params.get("arguments").unwrap_or(&default_args);

    let result = match tool_name {
        "codeseek_search" => {
            let query = arguments.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = arguments.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
            run_cli(&["search", query, "--limit", &limit.to_string(), "--json"])
        }
        "codeseek_callers" => {
            let symbol = arguments.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
            run_cli(&["callers", symbol, "--json"])
        }
        "codeseek_callees" => {
            let symbol = arguments.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
            run_cli(&["callees", symbol, "--json"])
        }
        "codeseek_init" => {
            run_cli(&["init"])
        }
        "codeseek_list" => {
            run_cli(&["list", "--json"])
        }
        "codeseek_status" => {
            run_cli(&["status", "--json"])
        }
        _ => Err(format!("Unknown tool: {}", tool_name)),
    };

    match result {
        Ok(output) => {
            let content = if let Ok(parsed) = serde_json::from_str::<Value>(&output) {
                json!([{ "type": "text", "text": serde_json::to_string_pretty(&parsed).unwrap_or(output) }])
            } else {
                json!([{ "type": "text", "text": output }])
            };
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "content": content }
            }))
        }
        Err(e) => {
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                    "isError": true
                }
            }))
        }
    }
}

/// Run the codeseek CLI binary and capture its stdout.
fn run_cli(args: &[&str]) -> Result<String, String> {
    let bin = std::env::current_exe()
        .map_err(|e| format!("Failed to get binary path: {}", e))?;

    // Ensure cwd is inherited from the MCP client (Claude Code's workspace)
    let output = std::process::Command::new(&bin)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run codeseek: {}", e))?;

    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|e| format!("Invalid UTF-8 output: {}", e))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stderr.is_empty() && stdout.is_empty() {
            Err(format!("codeseek exited with {}", output.status))
        } else if stderr.is_empty() {
            Err(format!("codeseek exited with {}: {}", output.status, stdout.trim()))
        } else {
            Err(format!("codeseek exited with {}: {}", output.status, stderr.trim()))
        }
    }
}
