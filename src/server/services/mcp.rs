use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::process::Command;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

/// MCP server connection state
#[derive(Debug, Clone)]
struct McpConnection {
    name: String,
    transport: String,       // "stdio", "sse", "http"
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    headers: HashMap<String, String>, // custom headers for HTTP/SSE
    tools: Vec<Value>,       // tool definitions in OpenAI format
    connected: bool,
    error: Option<String>,
}

// Global MCP connections
static MCP_CONNECTIONS: OnceLock<TokioMutex<HashMap<String, McpConnection>>> = OnceLock::new();

fn connections() -> &'static TokioMutex<HashMap<String, McpConnection>> {
    MCP_CONNECTIONS.get_or_init(|| TokioMutex::new(HashMap::new()))
}

/// Initialize MCP servers from settings (reads `mcpTools` array from settings.json)
pub async fn init_mcp_servers() {
    use crate::server::data::get_settings;

    let settings = get_settings().await;
    let mcp_tools = settings.mcp_tools;

    if mcp_tools.is_empty() {
        info!("[MCP] No MCP tools configured");
        return;
    }

    info!("[MCP] Initializing {} MCP server(s)...", mcp_tools.len());

    for tool in &mcp_tools {
        if !tool.enabled {
            info!("[MCP] Skipping disabled server '{}'", tool.name);
            continue;
        }

        // Determine transport type
        let transport = tool.tool_type.as_deref().unwrap_or("auto");

        // Build config Value for connect_server_impl
        let mut config = json!({
            "name": tool.name,
            "url": tool.url,
            "enabled": tool.enabled,
        });

        // For stdio: parse "command arg1 arg2" from url if no explicit command
        if transport == "stdio" || (!tool.url.starts_with("http") && transport == "auto") {
            let parts: Vec<&str> = tool.url.split_whitespace().collect();
            if !parts.is_empty() {
                config["command"] = json!(parts[0]);
                config["args"] = json!(parts[1..]);
            }
            let result = connect_server_impl(&tool.name, "stdio", &config).await;
            if result["ok"].as_bool().unwrap_or(false) {
                info!("[MCP] Connected to '{}' (stdio) — {} tool(s)", tool.name, result["tools"]);
            } else {
                warn!("[MCP] Failed to connect to '{}': {}", tool.name, result["error"].as_str().unwrap_or("unknown"));
            }
        } else {
            // HTTP/SSE
            if let Some(headers) = &tool.headers {
                let h: serde_json::Map<String, Value> = headers
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect();
                config["headers"] = Value::Object(h);
            }
            let t = if transport == "auto" || transport == "http" { "http" } else { transport };
            let result = connect_server_impl(&tool.name, t, &config).await;
            if result["ok"].as_bool().unwrap_or(false) {
                info!("[MCP] Connected to '{}' ({}) — {} tool(s)", tool.name, t, result["tools"]);
            } else {
                warn!("[MCP] Failed to connect to '{}': {}", tool.name, result["error"].as_str().unwrap_or("unknown"));
            }
        }
    }
}

/// Connect to a single MCP server
pub async fn connect_server(config: &Value) -> Value {
    let name = config["name"].as_str().unwrap_or("unknown").to_string();
    let transport = config["transport"].as_str().unwrap_or("stdio").to_string();
    connect_server_impl(&name, &transport, config).await
}

async fn connect_server_impl(name: &str, transport: &str, config: &Value) -> Value {
    match transport {
        "stdio" => connect_stdio(name, config).await,
        "sse" | "http" => connect_http(name, transport, config).await,
        _ => json!({ "ok": false, "error": format!("Unknown transport: {transport}") }),
    }
}

/// Connect to an MCP server via stdio (spawn process, send initialize, discover tools)
async fn connect_stdio(name: &str, config: &Value) -> Value {
    let command = config["command"].as_str().unwrap_or("").to_string();
    let args: Vec<String> = config["args"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if command.is_empty() {
        return json!({ "ok": false, "error": "No command specified for stdio transport" });
    }

    // Spawn process and send JSON-RPC initialize
    let result = match Command::new(&command)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

            let stdin = child.stdin.as_mut();
            let stdout = child.stdout.take();

            if let (Some(stdin), Some(stdout)) = (stdin, stdout) {
                // Send initialize request
                let init_req = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": { "name": "TigrimOS", "version": "0.5.2" }
                    }
                });
                let msg = format!("{}\n", serde_json::to_string(&init_req).unwrap());
                let _ = stdin.write_all(msg.as_bytes()).await;

                // Read response
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                let read_result = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    reader.read_line(&mut line),
                )
                .await;

                match read_result {
                    Ok(Ok(_)) => {
                        // Send tools/list request
                        let tools_req = json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "method": "tools/list",
                            "params": {}
                        });
                        let stdin = child.stdin.as_mut().unwrap();
                        let msg = format!("{}\n", serde_json::to_string(&tools_req).unwrap());
                        let _ = stdin.write_all(msg.as_bytes()).await;

                        let mut tools_line = String::new();
                        let tools_result = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            reader.read_line(&mut tools_line),
                        )
                        .await;

                        let tools = match tools_result {
                            Ok(Ok(_)) => {
                                let resp: Value =
                                    serde_json::from_str(&tools_line).unwrap_or(json!({}));
                                resp["result"]["tools"]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default()
                            }
                            _ => vec![],
                        };

                        // Convert MCP tools to OpenAI format
                        let openai_tools: Vec<Value> = tools
                            .iter()
                            .map(|t| {
                                let tool_name = t["name"].as_str().unwrap_or("unknown");
                                let prefixed = format!("mcp_{}_{}", name, tool_name);
                                json!({
                                    "type": "function",
                                    "function": {
                                        "name": prefixed,
                                        "description": t["description"].as_str().unwrap_or(""),
                                        "parameters": t.get("inputSchema").cloned().unwrap_or(json!({
                                            "type": "object",
                                            "properties": {}
                                        }))
                                    }
                                })
                            })
                            .collect();

                        let tool_count = openai_tools.len();

                        // Store connection
                        let conn = McpConnection {
                            name: name.to_string(),
                            transport: "stdio".to_string(),
                            command: Some(command),
                            args,
                            url: None,
                            headers: HashMap::new(),
                            tools: openai_tools,
                            connected: true,
                            error: None,
                        };
                        connections().lock().await.insert(name.to_string(), conn);

                        // Kill the discovery process (real MCP would keep it alive)
                        let _ = child.kill().await;

                        json!({ "ok": true, "tools": tool_count })
                    }
                    _ => {
                        let _ = child.kill().await;
                        json!({ "ok": false, "error": "Timeout waiting for MCP server response" })
                    }
                }
            } else {
                let _ = child.kill().await;
                json!({ "ok": false, "error": "Failed to get stdio handles" })
            }
        }
        Err(e) => json!({ "ok": false, "error": format!("Failed to spawn MCP server: {e}") }),
    };

    result
}

/// Connect to an MCP server via SSE/HTTP
async fn connect_http(name: &str, transport: &str, config: &Value) -> Value {
    let url = match config["url"].as_str() {
        Some(u) => u.to_string(),
        None => return json!({ "ok": false, "error": "No URL specified for HTTP/SSE transport" }),
    };

    // Build client with custom headers if provided
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("Content-Type", "application/json".parse().unwrap());
    if let Some(h) = config["headers"].as_object() {
        for (k, v) in h {
            if let (Ok(hname), Some(hval)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                v.as_str(),
            ) {
                if let Ok(hv) = reqwest::header::HeaderValue::from_str(hval) {
                    headers.insert(hname, hv);
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .default_headers(headers.clone())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // For MCP over HTTP: first try initialize, then tools/list on the base URL
    // Many MCP servers use a single endpoint (the base URL) for all JSON-RPC calls
    let init_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "TigrimOS", "version": "0.5.2" }
        }
    });

    // Try base URL first (standard MCP HTTP), fallback to /tools/list path
    let base_url = url.trim_end_matches('/');
    let init_result = client.post(base_url).json(&init_body).send().await;
    let use_base_url = match &init_result {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    };

    let tools_url = if use_base_url {
        base_url.to_string()
    } else {
        format!("{}/tools/list", base_url)
    };

    let body = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });

    match client.post(&tools_url).json(&body).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                let data: Value = resp.json().await.unwrap_or(json!({}));
                let tools = data["result"]["tools"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();

                let openai_tools: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        let tool_name = t["name"].as_str().unwrap_or("unknown");
                        let prefixed = format!("mcp_{}_{}", name, tool_name);
                        json!({
                            "type": "function",
                            "function": {
                                "name": prefixed,
                                "description": t["description"].as_str().unwrap_or(""),
                                "parameters": t.get("inputSchema").cloned().unwrap_or(json!({
                                    "type": "object",
                                    "properties": {}
                                }))
                            }
                        })
                    })
                    .collect();

                let tool_count = openai_tools.len();

                // Extract headers from config for storage
                let stored_headers: HashMap<String, String> = config["headers"]
                    .as_object()
                    .map(|h| {
                        h.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();

                let conn = McpConnection {
                    name: name.to_string(),
                    transport: transport.to_string(),
                    command: None,
                    args: vec![],
                    url: Some(url),
                    headers: stored_headers,
                    tools: openai_tools,
                    connected: true,
                    error: None,
                };
                connections().lock().await.insert(name.to_string(), conn);

                json!({ "ok": true, "tools": tool_count })
            } else {
                json!({ "ok": false, "error": format!("HTTP {}", resp.status()) })
            }
        }
        Err(e) => json!({ "ok": false, "error": format!("HTTP request failed: {e}") }),
    }
}

/// Disconnect a single MCP server
pub async fn disconnect_server(name: &str) {
    connections().lock().await.remove(name);
    info!("[MCP] Disconnected server '{}'", name);
}

/// Disconnect all MCP servers
pub async fn disconnect_all() {
    connections().lock().await.clear();
    info!("[MCP] All servers disconnected");
}

/// Get all MCP tool definitions in OpenAI function-calling format
pub async fn get_mcp_tools() -> Vec<Value> {
    let conns = connections().lock().await;
    conns
        .values()
        .filter(|c| c.connected)
        .flat_map(|c| c.tools.clone())
        .collect()
}

/// Check if a tool name is an MCP tool (prefixed with mcp_)
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with("mcp_")
}

/// Call an MCP tool by its prefixed name
pub async fn call_mcp_tool(prefixed_name: &str, args: &Value) -> Value {
    // Parse mcp_{server}_{tool} format
    let rest = match prefixed_name.strip_prefix("mcp_") {
        Some(r) => r,
        None => return json!({ "ok": false, "error": "Not an MCP tool" }),
    };

    // Find the server name by checking connections
    let conns = connections().lock().await;
    let mut found_server: Option<&McpConnection> = None;
    let mut tool_name = String::new();

    for (server_name, conn) in conns.iter() {
        let prefix = format!("{}_", server_name);
        if let Some(t) = rest.strip_prefix(&prefix) {
            found_server = Some(conn);
            tool_name = t.to_string();
            break;
        }
    }

    let conn = match found_server {
        Some(c) => c.clone(),
        None => return json!({ "ok": false, "error": format!("MCP server not found for tool '{}'", prefixed_name) }),
    };
    drop(conns);

    if !conn.connected {
        return json!({ "ok": false, "error": format!("MCP server '{}' not connected", conn.name) });
    }

    // Execute based on transport
    match conn.transport.as_str() {
        "stdio" => call_stdio_tool(&conn, &tool_name, args).await,
        "sse" | "http" => call_http_tool(&conn, &tool_name, args).await,
        _ => json!({ "ok": false, "error": "Unknown transport" }),
    }
}

async fn call_stdio_tool(conn: &McpConnection, tool_name: &str, args: &Value) -> Value {
    let command = match &conn.command {
        Some(c) => c.clone(),
        None => return json!({ "ok": false, "error": "No command for stdio transport" }),
    };

    // Spawn a fresh process for each call (stateless mode)
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut child = match Command::new(&command)
        .args(&conn.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": format!("Failed to spawn: {e}") }),
    };

    let stdin = child.stdin.as_mut().unwrap();
    let stdout = child.stdout.take().unwrap();

    // Initialize
    let init_req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "TigrimOS", "version": "0.5.2" }
        }
    });
    let _ = stdin
        .write_all(format!("{}\n", serde_json::to_string(&init_req).unwrap()).as_bytes())
        .await;

    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), reader.read_line(&mut line)).await;

    // Call tool
    let call_req = json!({
        "jsonrpc": "2.0", "id": 2,
        "method": "tools/call",
        "params": { "name": tool_name, "arguments": args }
    });
    let stdin = child.stdin.as_mut().unwrap();
    let _ = stdin
        .write_all(format!("{}\n", serde_json::to_string(&call_req).unwrap()).as_bytes())
        .await;

    let mut result_line = String::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        reader.read_line(&mut result_line),
    )
    .await;

    let _ = child.kill().await;

    match result {
        Ok(Ok(_)) => {
            let resp: Value = serde_json::from_str(&result_line).unwrap_or(json!({}));
            let content = &resp["result"]["content"];
            if content.is_array() {
                let text: String = content
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                json!({ "ok": true, "result": text })
            } else {
                json!({ "ok": true, "result": resp["result"] })
            }
        }
        _ => json!({ "ok": false, "error": "Timeout waiting for MCP tool result" }),
    }
}

async fn call_http_tool(conn: &McpConnection, tool_name: &str, args: &Value) -> Value {
    let base_url = match &conn.url {
        Some(u) => u.clone(),
        None => return json!({ "ok": false, "error": "No URL for HTTP transport" }),
    };

    // Use base URL directly for MCP JSON-RPC (standard MCP HTTP transport)
    let call_url = base_url.trim_end_matches('/').to_string();

    let body = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": tool_name, "arguments": args }
    });

    // Build client with stored headers
    let mut req_headers = reqwest::header::HeaderMap::new();
    req_headers.insert("Content-Type", "application/json".parse().unwrap());
    for (k, v) in &conn.headers {
        if let (Ok(hname), Ok(hval)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            req_headers.insert(hname, hval);
        }
    }

    let client = reqwest::Client::builder()
        .default_headers(req_headers)
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    match client.post(&call_url).json(&body).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                let data: Value = resp.json().await.unwrap_or(json!({}));
                let content = &data["result"]["content"];
                if content.is_array() {
                    let text: String = content
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|c| c["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    json!({ "ok": true, "result": text })
                } else {
                    json!({ "ok": true, "result": data["result"] })
                }
            } else {
                json!({ "ok": false, "error": format!("HTTP {}", resp.status()) })
            }
        }
        Err(e) => json!({ "ok": false, "error": format!("HTTP request failed: {e}") }),
    }
}

/// Get status of all MCP connections
pub async fn get_mcp_status() -> Value {
    let conns = connections().lock().await;
    let servers: Vec<Value> = conns
        .values()
        .map(|c| {
            json!({
                "name": c.name,
                "transport": c.transport,
                "connected": c.connected,
                "toolCount": c.tools.len(),
                "error": c.error,
            })
        })
        .collect();
    json!({ "servers": servers })
}
