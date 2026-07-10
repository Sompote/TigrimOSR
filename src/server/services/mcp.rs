use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

/// MCP server connection state
#[derive(Debug, Clone)]
struct McpConnection {
    name: String,
    transport: String,       // "stdio", "sse", "http"
    command: Option<String>,
    args: Vec<String>,
    env: HashMap<String, String>, // extra env vars for stdio servers
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

/// A live, long-lived stdio MCP server process. Kept alive between tool calls
/// so STATEFUL servers (e.g. a Playwright browser) retain their session — the
/// page navigated in one call is still open for the next call. Each call goes
/// through the same stdin/stdout, serialized by the per-process mutex.
struct StdioProc {
    _child: Child,
    /// PID of the spawned server (== its process-group id, since we spawn it as
    /// a group leader). Used to SIGKILL the whole tree on drop — `npx` spawns
    /// `node` which spawns Chromium, and kill_on_drop only reaps the direct
    /// child, leaving the browser running and holding memory.
    pid: Option<u32>,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

impl Drop for StdioProc {
    fn drop(&mut self) {
        // Reap the entire process group (server + node + Chromium grandchildren)
        // so a finished/cancelled session doesn't leak browser processes.
        if let Some(pid) = self.pid {
            crate::server::services::proc_registry::kill_group(pid);
        }
    }
}

impl StdioProc {
    /// Send a JSON-RPC request and read until the response with the matching id
    /// arrives (skipping interleaved notifications), bounded by `timeout_secs`.
    async fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout_secs: u64,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let msg = format!("{}\n", serde_json::to_string(&req).unwrap());
        self.stdin
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| format!("write failed: {e}"))?;
        let _ = self.stdin.flush().await;

        let reader = &mut self.reader;
        let fut = async {
            loop {
                let mut line = String::new();
                let n = reader
                    .read_line(&mut line)
                    .await
                    .map_err(|e| format!("read failed: {e}"))?;
                if n == 0 {
                    return Err("MCP process closed its stdout".to_string());
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue, // not JSON (stray log line) — skip
                };
                // Match our request id; skip notifications / other ids.
                if v.get("id").and_then(|x| x.as_i64()) == Some(id) {
                    return Ok(v);
                }
            }
        };
        match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), fut).await {
            Ok(r) => r,
            Err(_) => Err("timeout waiting for MCP response".to_string()),
        }
    }

    /// Send a fire-and-forget JSON-RPC notification (no id, no response).
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let req = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let msg = format!("{}\n", serde_json::to_string(&req).unwrap());
        self.stdin
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| format!("write failed: {e}"))?;
        let _ = self.stdin.flush().await;
        Ok(())
    }
}

// Live stdio processes, keyed by server name. Separate from MCP_CONNECTIONS
// (which holds cloneable metadata) because a process handle can't be cloned.
static MCP_PROCESSES: OnceLock<TokioMutex<HashMap<String, Arc<TokioMutex<StdioProc>>>>> =
    OnceLock::new();

fn processes() -> &'static TokioMutex<HashMap<String, Arc<TokioMutex<StdioProc>>>> {
    MCP_PROCESSES.get_or_init(|| TokioMutex::new(HashMap::new()))
}

/// Spawn a stdio MCP server and perform the `initialize` handshake, leaving the
/// process alive and ready for `tools/list` / `tools/call`. stderr is drained
/// on a background task so a chatty server can't fill the pipe and stall.
async fn spawn_and_init(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<StdioProc, String> {
    // Run the server from the sandbox dir so relative output paths it returns
    // (e.g. a browser screenshot saved as `./shot.png`) land inside the sandbox,
    // where the agent's read_file and the web file-server can reach them.
    let sandbox = crate::server::data::get_sandbox_dir_sync();
    let mut cmd = Command::new(command);
    cmd.args(args)
        .envs(env)
        // TigrimOS's own web port must not leak into MCP children: servers
        // that read a generic PORT (e.g. workspace-mcp's OAuth callback
        // listener) would bind next to OUR port instead of their documented
        // default — seen in the field as a Google login redirecting to
        // localhost:3002 instead of localhost:8000.
        .env_remove("PORT")
        .current_dir(&sandbox)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Own process group (pgid == child pid) so we can SIGKILL the whole tree —
    // the server's node + Chromium grandchildren — on drop.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn MCP server: {e}"))?;
    let pid = child.id();

    let stdin = child.stdin.take().ok_or("no stdin handle")?;
    let stdout = child.stdout.take().ok_or("no stdout handle")?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {
                // discard server logs to keep the pipe drained
            }
        });
    }

    let mut proc = StdioProc {
        _child: child,
        pid,
        stdin,
        reader: BufReader::new(stdout),
        next_id: 1,
    };

    proc.request(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "TigrimOS", "version": "0.5.3" }
        }),
        15,
    )
    .await?;
    // Per MCP spec, signal readiness after initialize.
    let _ = proc.notify("notifications/initialized", json!({})).await;
    Ok(proc)
}

/// Get the live process for `name`, spawning a fresh one if absent (e.g. after
/// a crash). Used by call_stdio_tool so a dead server transparently recovers.
async fn get_or_spawn_proc(
    name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<Arc<TokioMutex<StdioProc>>, String> {
    {
        let procs = processes().lock().await;
        if let Some(p) = procs.get(name) {
            return Ok(p.clone());
        }
    }
    let proc = spawn_and_init(command, args, env).await?;
    let arc = Arc::new(TokioMutex::new(proc));
    processes().lock().await.insert(name.to_string(), arc.clone());
    Ok(arc)
}

/// Initialize MCP servers from settings (reads `mcpTools` array from settings.json)
pub async fn init_mcp_servers() {
    use crate::server::data::get_settings;

    let settings = get_settings().await;
    let mcp_tools = &settings.mcp_tools;

    if mcp_tools.is_empty() {
        info!("[MCP] No user MCP tools configured");
    } else {
        info!("[MCP] Initializing {} MCP server(s)...", mcp_tools.len());
    }

    for tool in mcp_tools {
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

        // Check if this is a stdio server (has command field, or non-HTTP url)
        let is_stdio = tool.command.is_some()
            || transport == "stdio"
            || (!tool.url.starts_with("http") && transport == "auto");

        if is_stdio {
            if let Some(env) = &tool.env {
                config["env"] = json!(env);
            }
            // Use explicit command/args if provided (Claude Desktop format)
            if let Some(cmd) = &tool.command {
                config["command"] = json!(cmd);
                config["args"] = json!(tool.args.as_deref().unwrap_or(&[]));
            } else {
                // Fallback: parse "command arg1 arg2" from url field
                let parts: Vec<&str> = tool.url.split_whitespace().collect();
                if !parts.is_empty() {
                    config["command"] = json!(parts[0]);
                    config["args"] = json!(parts[1..]);
                }
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

    // Built-in browser control (Playwright MCP), gated by the safety toggle.
    if settings.browser_control_enabled == Some(true) {
        // Defer to a user-defined "browser" server if one exists.
        let user_defined = settings.mcp_tools.iter().any(|t| t.enabled && t.name == "browser");
        if user_defined {
            info!("[MCP] Browser control on, but a user-defined 'browser' server exists — using that");
        } else {
            connect_builtin_browser(&settings).await;
        }
    }
}

/// Build the `(command, args)` to launch the browser-control MCP server for the
/// configured engine.
///
/// - "chromium"/"chrome": Playwright MCP via `npx @playwright/mcp@latest`, with
///   the Chromium user-data-dir profile, headless flag, and (for per-agent
///   browsers) `--isolated`.
/// - "obscura": the native `obscura mcp` stdio server. It's a drop-in — it
///   exposes the SAME `browser_*` tool names as Playwright MCP, so no routing
///   or search-code changes are needed. Obscura is always headless and doesn't
///   use a Chromium profile/singleton-lock, so `profile`, `headless`, and
///   `isolated` don't apply; `--stealth` enables its anti-detection + tracker
///   blocking (well-suited to search scraping).
///
/// `profile` is the Chromium user-data-dir (None for per-agent isolated
/// browsers). `output` is the screenshot/snapshot dir. `isolated` gives
/// Playwright an ephemeral profile so multiple windows run side by side.
fn build_browser_launch(
    settings: &crate::server::data::Settings,
    profile: Option<&str>,
    output: &str,
    isolated: bool,
) -> (String, Vec<String>) {
    let engine = settings
        .browser_engine
        .clone()
        .unwrap_or_else(|| "chromium".to_string());

    if engine == "obscura" {
        let cmd = settings
            .browser_obscura_path
            .clone()
            .unwrap_or_else(|| "obscura".to_string());
        return (cmd, vec!["mcp".to_string(), "--stealth".to_string()]);
    }

    // Browser headless mode is decoupled from the server's UI-headless mode:
    // an explicit `browserHeadless` setting wins, so a UI-less server can still
    // drive a *real* (headful) browser to beat headless-detection blocking
    // (Google etc.). With no display you must supply a virtual one (xvfb-run).
    // When the setting is unset, fall back to the legacy behaviour of following
    // the process `--headless` flag (a headed launch needs an X server).
    let headless = settings
        .browser_headless
        .unwrap_or_else(|| std::env::args().any(|a| a == "--headless"));

    let mut args: Vec<String> = vec!["@playwright/mcp@latest".to_string()];
    if headless {
        args.push("--headless".to_string());
    }
    args.push("--browser".to_string());
    args.push(engine);
    if let Some(p) = profile {
        args.push("--user-data-dir".to_string());
        args.push(p.to_string());
    }
    if isolated {
        args.push("--isolated".to_string());
    }
    args.push("--output-dir".to_string());
    args.push(output.to_string());
    ("npx".to_string(), args)
}

/// Register the built-in browser-control server. Engine is "chromium" (bundled),
/// "chrome" (system), or "obscura" (native `obscura mcp` stdio server).
async fn connect_builtin_browser(settings: &crate::server::data::Settings) {
    let engine = settings
        .browser_engine
        .clone()
        .unwrap_or_else(|| "chromium".to_string());

    // Profile lives under the data dir (internal browser state, not served).
    let data_dir = crate::server::data::data_dir().to_string_lossy().to_string();
    let profile = format!("{}/browser-profile-{}", data_dir, engine);

    // A crashed/killed browser (e.g. a Docker container that died without a clean
    // shutdown) leaves stale Chromium singleton locks in the profile. Playwright
    // MCP then sees the profile as "already in use" and refuses to launch
    // ("use --isolated to run multiple instances"). The profile is dedicated to
    // this built-in browser, so clearing stale locks before launch is safe.
    // Obscura doesn't use a Chromium profile, so there are no locks to clear.
    if engine != "obscura" {
        clear_stale_browser_locks(&profile);
    }
    // Screenshots/snapshots go in the sandbox so the agent (read_file) and the
    // web UI (file-server) can read/display them.
    let output = format!("{}/browser-output", crate::server::data::get_sandbox_dir_sync());

    let (command, args) = build_browser_launch(settings, Some(&profile), &output, false);

    let config = json!({
        "name": "browser",
        "command": command,
        "args": args,
    });

    let result = connect_server_impl("browser", "stdio", &config).await;
    if result["ok"].as_bool().unwrap_or(false) {
        info!(
            "[MCP] Browser control enabled ({}) — {} tool(s)",
            engine, result["tools"]
        );
    } else {
        let need = if engine == "obscura" {
            "is the `obscura` binary installed and on PATH?"
        } else {
            "is Node/npx installed?"
        };
        warn!(
            "[MCP] Browser control failed to start ({}): {}",
            need,
            result["error"].as_str().unwrap_or("unknown")
        );
    }
}

// ---------------------------------------------------------------------------
// Per-agent isolated browsers
//
// In a parallel swarm, every agent calling the single shared `browser` server
// drives the SAME Chrome page — they clobber each other's navigation. To let
// agents browse truly in parallel, each agent gets its OWN isolated Playwright
// MCP server (its own Chromium window/profile), launched lazily on first use.
// These servers are registered under the AGENT_BROWSER_PREFIX so they're hidden
// from get_mcp_tools() (the LLM still calls the generic `mcp_browser_*` names;
// we route them per-agent). Keyed by session+agent so they're cleaned up with
// the session.
// ---------------------------------------------------------------------------

const AGENT_BROWSER_PREFIX: &str = "agentbrowser__";

/// Serializes lazy browser launches so two concurrent first-calls can't spawn
/// the same Chromium twice.
static AGENT_BROWSER_LAUNCH_LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
fn agent_browser_launch_lock() -> &'static TokioMutex<()> {
    AGENT_BROWSER_LAUNCH_LOCK.get_or_init(|| TokioMutex::new(()))
}

fn agent_browser_key(session_id: &str, agent_id: &str) -> String {
    // Keep it filesystem/identifier-safe.
    let safe = |s: &str| s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect::<String>();
    format!("{}{}__{}", AGENT_BROWSER_PREFIX, safe(session_id), safe(agent_id))
}

/// Lazily launch an isolated Playwright MCP browser dedicated to one agent.
/// No-op if already running. Returns true when a browser is available.
pub async fn ensure_agent_browser(session_id: &str, agent_id: &str) -> bool {
    let key = agent_browser_key(session_id, agent_id);
    if is_server_connected(&key).await {
        return true;
    }
    // Serialize launches and re-check inside the lock (avoid double-spawn).
    let _guard = agent_browser_launch_lock().lock().await;
    if is_server_connected(&key).await {
        return true;
    }

    let settings = crate::server::data::get_settings().await;
    let engine = settings.browser_engine.clone().unwrap_or_else(|| "chromium".to_string());
    let output = format!("{}/browser-output", crate::server::data::get_sandbox_dir_sync());

    // Per-agent browser: no shared profile (Playwright uses --isolated for an
    // ephemeral one; each `obscura mcp` is already its own process).
    let (command, args) = build_browser_launch(&settings, None, &output, true);

    let config = json!({ "name": key, "command": command, "args": args });
    let result = connect_server_impl(&key, "stdio", &config).await;
    let ok = result["ok"].as_bool().unwrap_or(false);
    if ok {
        info!("[MCP] Launched isolated browser for agent '{}' ({})", agent_id, engine);
    } else {
        warn!(
            "[MCP] Failed to launch isolated browser for agent '{}': {} — falling back to shared browser",
            agent_id,
            result["error"].as_str().unwrap_or("unknown")
        );
    }
    ok
}

/// Route a generic `mcp_browser_*` call to this agent's OWN isolated browser.
/// Falls back to the shared browser if the per-agent one can't start.
pub async fn call_browser_tool_for_agent(
    session_id: &str,
    agent_id: &str,
    prefixed_name: &str,
    args: &Value,
) -> Value {
    let tool = match prefixed_name.strip_prefix("mcp_browser_") {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return call_mcp_tool(prefixed_name, args).await,
    };
    if !ensure_agent_browser(session_id, agent_id).await {
        return call_mcp_tool(prefixed_name, args).await; // fall back to shared
    }
    let key = agent_browser_key(session_id, agent_id);
    let conn = {
        let conns = connections().lock().await;
        conns.get(&key).cloned()
    };
    match conn {
        Some(c) if c.connected => call_stdio_tool(&c, &tool, args).await,
        _ => call_mcp_tool(prefixed_name, args).await, // fall back
    }
}

/// Shut down every per-agent browser belonging to a session (frees the Chromium
/// instances). Called when the realtime session ends.
pub async fn shutdown_agent_browsers(session_id: &str) {
    let prefix = agent_browser_key(session_id, "");
    // agent_browser_key(session_id, "") ends with "__" before the (empty) agent;
    // match on the session portion so we get every agent for this session.
    let session_marker = prefix.trim_end_matches('_');
    let keys: Vec<String> = {
        let conns = connections().lock().await;
        conns
            .keys()
            .filter(|k| k.starts_with(session_marker) && k.starts_with(AGENT_BROWSER_PREFIX))
            .cloned()
            .collect()
    };
    for k in keys {
        disconnect_server(&k).await;
    }
}

/// Remove stale Chromium singleton lock files from a dedicated browser profile.
///
/// Chromium guards a profile against concurrent use with `SingletonLock`,
/// `SingletonSocket`, and `SingletonCookie` (symlinks under the profile root).
/// When a browser process dies abnormally — common in Docker when the container
/// is stopped — these are left behind and the next launch reports the profile as
/// already in use. Since this profile belongs solely to the built-in browser,
/// no live Chromium should legitimately hold these, so we clear them.
fn clear_stale_browser_locks(profile: &str) {
    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let path = std::path::Path::new(profile).join(name);
        // These are symlinks; symlink_metadata avoids following a dangling target.
        if std::fs::symlink_metadata(&path).is_ok() {
            match std::fs::remove_file(&path) {
                Ok(_) => info!("[MCP] Cleared stale browser lock: {}", path.display()),
                Err(e) => warn!("[MCP] Could not clear browser lock {}: {}", path.display(), e),
            }
        }
    }
}

/// Connect to a single MCP server (used by the Google quick-connect route).
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
    let env: HashMap<String, String> = config["env"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    if command.is_empty() {
        return json!({ "ok": false, "error": "No command specified for stdio transport" });
    }

    // Spawn the server and keep it ALIVE — stateful servers (e.g. a Playwright
    // browser) need the same process across calls so the open page survives.
    let mut proc = match spawn_and_init(&command, &args, &env).await {
        Ok(p) => p,
        Err(e) => return json!({ "ok": false, "error": e }),
    };

    // Discover tools over the same live connection.
    let tools = match proc.request("tools/list", json!({}), 15).await {
        Ok(resp) => resp["result"]["tools"].as_array().cloned().unwrap_or_default(),
        Err(e) => return json!({ "ok": false, "error": format!("tools/list failed: {e}") }),
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

    // Store metadata...
    let conn = McpConnection {
        name: name.to_string(),
        transport: "stdio".to_string(),
        command: Some(command),
        args,
        env,
        url: None,
        headers: HashMap::new(),
        tools: openai_tools,
        connected: true,
        error: None,
    };
    connections().lock().await.insert(name.to_string(), conn);
    // ...and keep the live process for subsequent tool calls.
    processes()
        .lock()
        .await
        .insert(name.to_string(), Arc::new(TokioMutex::new(proc)));

    json!({ "ok": true, "tools": tool_count })
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
            "clientInfo": { "name": "TigrimOS", "version": "0.5.3" }
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
                    env: HashMap::new(),
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
    // Dropping the StdioProc kills the child (kill_on_drop) once no in-flight
    // call still holds the Arc.
    processes().lock().await.remove(name);
    info!("[MCP] Disconnected server '{}'", name);
}

/// Disconnect all MCP servers
pub async fn disconnect_all() {
    connections().lock().await.clear();
    processes().lock().await.clear();
    info!("[MCP] All servers disconnected");
}

/// Get connection status for all servers (for UI display)
pub async fn get_connection_status() -> Vec<(String, bool, usize, Option<String>)> {
    let conns = connections().lock().await;
    conns.values().map(|c| {
        (c.name.clone(), c.connected, c.tools.len(), c.error.clone())
    }).collect()
}

/// Get all MCP tool definitions in OpenAI function-calling format
pub async fn get_mcp_tools() -> Vec<Value> {
    let conns = connections().lock().await;
    conns
        .values()
        // Per-agent isolated browsers are routed to explicitly via the generic
        // mcp_browser_* names — never expose their duplicated tools to the LLM.
        .filter(|c| c.connected && !c.name.starts_with(AGENT_BROWSER_PREFIX))
        .flat_map(|c| c.tools.clone())
        .collect()
}

/// Like get_mcp_tools, but restricted to the named servers (connection names,
/// matching Settings.mcp_tools[].name and the mcp_{server}_{tool} prefix).
/// None = no restriction. Used by agent-loop profiles; dispatch is unaffected
/// — filtering only controls which tools the LLM sees.
pub async fn get_mcp_tools_filtered(servers: Option<&[String]>) -> Vec<Value> {
    let conns = connections().lock().await;
    conns
        .values()
        .filter(|c| c.connected && !c.name.starts_with(AGENT_BROWSER_PREFIX))
        .filter(|c| servers.map_or(true, |s| s.iter().any(|n| n == &c.name)))
        .flat_map(|c| c.tools.clone())
        .collect()
}

/// Bare tool names (without the mcp_{server}_ prefix) exposed by one connected
/// server. Used e.g. by the Google connector to find the auth-trigger tool.
pub async fn server_tool_names(server: &str) -> Vec<String> {
    let prefix = format!("mcp_{}_", server);
    let conns = connections().lock().await;
    conns
        .get(server)
        .map(|c| {
            c.tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str())
                .filter_map(|n| n.strip_prefix(&prefix).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Check if a tool name is an MCP tool (prefixed with mcp_)
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with("mcp_")
}

/// True if an MCP server with this name is currently connected. Used by
/// web_search to decide whether it can route through the live browser.
pub async fn is_server_connected(name: &str) -> bool {
    connections()
        .lock()
        .await
        .get(name)
        .map(|c| c.connected)
        .unwrap_or(false)
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

    let call_params = json!({ "name": tool_name, "arguments": args });

    // Reuse the live process so prior state (open browser page, etc.) persists.
    let proc_arc = match get_or_spawn_proc(&conn.name, &command, &conn.args, &conn.env).await {
        Ok(a) => a,
        Err(e) => return json!({ "ok": false, "error": format!("MCP spawn failed: {e}") }),
    };

    let first_err = {
        let mut proc = proc_arc.lock().await;
        match proc.request("tools/call", call_params.clone(), 120).await {
            Ok(resp) => return stdio_result(resp),
            Err(e) => e,
        }
    };

    // The request failed — the process likely died. Drop it, respawn, retry once.
    warn!(
        "[MCP] '{}' tool call failed ({}); restarting server and retrying",
        conn.name, first_err
    );
    processes().lock().await.remove(&conn.name);
    let proc_arc = match get_or_spawn_proc(&conn.name, &command, &conn.args, &conn.env).await {
        Ok(a) => a,
        Err(e) => return json!({ "ok": false, "error": format!("MCP respawn failed: {e}") }),
    };
    let mut proc = proc_arc.lock().await;
    match proc.request("tools/call", call_params, 120).await {
        Ok(resp) => stdio_result(resp),
        Err(e) => json!({
            "ok": false,
            "error": format!("MCP call failed after restart: {e} (first: {first_err})")
        }),
    }
}

/// Shape an MCP `tools/call` JSON-RPC response into TigrimOS's tool-result form.
fn stdio_result(resp: Value) -> Value {
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
        json!({ "ok": true, "result": resp["result"].clone() })
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
#[allow(dead_code)] // public API kept for external callers / future routes
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
