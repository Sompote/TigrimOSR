use reqwest::Client;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ToolUpdate {
    ToolCall { name: String, args: Value },
    ToolResult { name: String, result: Value },
    TextChunk(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ToolLoopResult {
    pub content: String,
    pub tool_results: Vec<ToolCallRecord>,
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool: String,
    pub result: Value,
}

// ---------------------------------------------------------------------------
// Sub-agent context (passed through the tool loop)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SubAgentConfig {
    pub enabled: bool,
    pub config_file: String,
    pub agent_ids: Vec<String>,       // available agent IDs from YAML
    pub api_key: String,
    pub api_url: String,
    pub model: String,
    pub depth: usize,                 // recursion depth (prevent infinite loops)
    pub session_id: String,           // for JSONL history logging
}

impl Default for SubAgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            config_file: String::new(),
            agent_ids: Vec::new(),
            api_key: String::new(),
            api_url: String::new(),
            model: String::new(),
            depth: 0,
            session_id: String::new(),
        }
    }
}

/// Load agent system YAML and return parsed Value + list of agent IDs
pub fn load_agent_yaml(filename: &str) -> Option<(Value, Vec<String>)> {
    let dir = PathBuf::from("data/agents");
    let fp = dir.join(filename);
    let content = std::fs::read_to_string(&fp).ok()?;
    let parsed: Value = serde_yaml::from_str(&content).ok()?;
    let ids = parsed
        .get("agents")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .filter(|id| id != "human")
                .collect()
        })
        .unwrap_or_default();
    Some((parsed, ids))
}

/// Build a system prompt for a specific agent from the YAML config
fn build_agent_system_prompt(agent_def: &Value, system_config: &Value, available_targets: &[String]) -> String {
    let agent_id = agent_def.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = agent_def.get("name").and_then(|v| v.as_str()).unwrap_or("Agent");
    let role = agent_def.get("role").and_then(|v| v.as_str()).unwrap_or("worker");
    let persona = agent_def.get("persona").and_then(|v| v.as_str()).unwrap_or("");
    let responsibilities = agent_def
        .get("responsibilities")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| format!("  - {}", s))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let system_name = system_config
        .get("system")
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Multi-Agent System");

    let orch_mode = system_config
        .get("system")
        .and_then(|s| s.get("orchestration_mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("hierarchical");

    // Parse workflow sequence for this agent's step and action
    let workflow_step = system_config
        .get("workflow")
        .and_then(|w| w.get("sequence"))
        .and_then(|s| s.as_array())
        .and_then(|steps| {
            steps.iter().find(|s| {
                s.get("agent").and_then(|a| a.as_str()) == Some(agent_id)
            })
        })
        .cloned();

    let my_action = workflow_step
        .as_ref()
        .and_then(|s| s.get("action"))
        .and_then(|a| a.as_str())
        .unwrap_or("");

    let my_step_num = workflow_step
        .as_ref()
        .and_then(|s| s.get("step"))
        .and_then(|s| s.as_u64())
        .unwrap_or(0);

    let outputs_to = workflow_step
        .as_ref()
        .and_then(|s| s.get("outputs_to"))
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|id| *id != "human")
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();

    // Parse who sends work to this agent (inputs_from)
    let inputs_from: Vec<String> = system_config
        .get("connections")
        .and_then(|c| c.as_array())
        .map(|conns| {
            conns.iter()
                .filter(|c| c.get("to").and_then(|v| v.as_str()) == Some(agent_id))
                .filter_map(|c| {
                    let from = c.get("from").and_then(|v| v.as_str())?;
                    let proto = c.get("protocol").and_then(|v| v.as_str()).unwrap_or("tcp");
                    Some(format!("{} ({})", from, proto))
                })
                .collect()
        })
        .unwrap_or_default();

    // Parse bus/mesh config for this agent
    let bus_enabled = agent_def
        .get("bus")
        .and_then(|b| b.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mesh_enabled = agent_def
        .get("mesh")
        .and_then(|m| m.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let bus_topics: Vec<&str> = agent_def
        .get("bus")
        .and_then(|b| b.get("topics"))
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    // Build connectivity section
    let mut connectivity = String::new();
    if !inputs_from.is_empty() {
        connectivity.push_str(&format!("\nReceives work from: {}", inputs_from.join(", ")));
    }
    if !outputs_to.is_empty() {
        connectivity.push_str(&format!("\nOutputs results to: {}", outputs_to));
    }
    if bus_enabled && !bus_topics.is_empty() {
        connectivity.push_str(&format!("\nBus topics: {}", bus_topics.join(", ")));
    }
    if mesh_enabled {
        connectivity.push_str("\nMesh networking: enabled (can communicate with all peers)");
    }

    // Build targets section
    let targets_info = if available_targets.is_empty() {
        String::new()
    } else {
        // Look up names for available targets
        let target_descs: Vec<String> = available_targets.iter().map(|tid| {
            let tname = system_config
                .get("agents")
                .and_then(|a| a.as_array())
                .and_then(|arr| arr.iter().find(|a| a.get("id").and_then(|v| v.as_str()) == Some(tid.as_str())))
                .and_then(|a| a.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or(tid.as_str());
            let trole = system_config
                .get("agents")
                .and_then(|a| a.as_array())
                .and_then(|arr| arr.iter().find(|a| a.get("id").and_then(|v| v.as_str()) == Some(tid.as_str())))
                .and_then(|a| a.get("role"))
                .and_then(|v| v.as_str())
                .unwrap_or("worker");
            format!("  - {} ({}): {}", tid, trole, tname)
        }).collect();
        format!(
            "\n\nAvailable agents to delegate to (use spawn_subagent tool):\n{}",
            target_descs.join("\n")
        )
    };

    let workflow_info = if my_step_num > 0 {
        let action_info = if !my_action.is_empty() && my_action != "Analyze and process assigned tasks" {
            format!("\nYour primary action: {}", my_action)
        } else {
            String::new()
        };
        format!("\nWorkflow step: {}{}", my_step_num, action_info)
    } else {
        String::new()
    };

    format!(
        r#"You are {name}, operating in {system_name}.

ROLE: {role} | ORCHESTRATION: {orch_mode}

PERSONA:
{persona}

RESPONSIBILITIES:
{responsibilities}
{workflow_info}{connectivity}{targets_info}

INSTRUCTIONS:
- Focus strictly on your role and responsibilities
- Use spawn_subagent to delegate to specialist agents when appropriate
- When delegating, provide full context so the sub-agent can work independently
- Return comprehensive, structured results when your task is complete
- Collaborate effectively; your output feeds into the next stage of the workflow"#
    )
}

// ---------------------------------------------------------------------------
// Tool definitions (OpenAI function-calling format)
// ---------------------------------------------------------------------------

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web using DuckDuckGo.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "fetch_url",
                "description": "Fetch content from a URL.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The URL to fetch" },
                        "method": { "type": "string", "description": "HTTP method (GET or POST). Defaults to GET." }
                    },
                    "required": ["url"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_python",
                "description": "Execute Python code in a sandbox.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "code": { "type": "string", "description": "Python code to execute" }
                    },
                    "required": ["code"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_shell",
                "description": "Execute a shell command.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to execute" },
                        "cwd": { "type": "string", "description": "Working directory (optional)" }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file from disk.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path to read" }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write content to a file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path to write" },
                        "content": { "type": "string", "description": "Content to write" },
                        "append": { "type": "boolean", "description": "Append instead of overwrite (default false)" }
                    },
                    "required": ["path", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_files",
                "description": "List files in a directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path (defaults to sandbox root)" },
                        "recursive": { "type": "boolean", "description": "List recursively (default false)" }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_skills",
                "description": "List all installed skills. Returns skill names you can load with load_skill.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "load_skill",
                "description": "Load the full SKILL.md content for a specific installed skill.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "skill": { "type": "string", "description": "Skill name/slug" }
                    },
                    "required": ["skill"]
                }
            }
        }),
    ]
}

/// Tool definitions with sub-agent tools added when enabled
pub fn tool_definitions_with_subagent(sub_agent: &SubAgentConfig) -> Vec<Value> {
    let mut tools = tool_definitions();
    if sub_agent.enabled && !sub_agent.agent_ids.is_empty() && sub_agent.depth < 3 {
        let agent_list = sub_agent.agent_ids.join(", ");
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "spawn_subagent",
                "description": format!(
                    "Delegate a task to a sub-agent. Available agents: {}. Use this when a task is better handled by a specialist agent.",
                    agent_list
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "agent_id": {
                            "type": "string",
                            "description": format!("ID of the agent to spawn. Must be one of: {}", agent_list)
                        },
                        "task": {
                            "type": "string",
                            "description": "Clear description of the task for the sub-agent"
                        },
                        "context": {
                            "type": "string",
                            "description": "Optional context or data to pass to the sub-agent"
                        }
                    },
                    "required": ["agent_id", "task"]
                }
            }
        }));
    }
    tools
}

// ---------------------------------------------------------------------------
// Agent history JSONL logging (tiger_cowork style)
// ---------------------------------------------------------------------------

async fn write_agent_history(session_id: &str, event: &str, data: serde_json::Value) {
    if session_id.is_empty() {
        return;
    }
    let dir = format!("data/agent_history/{}", session_id);
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    let path = format!("{}/spawn.jsonl", dir);
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let entry = match serde_json::to_string(&json!({
        "timestamp": ts,
        "event": event,
        "data": data
    })) {
        Ok(s) => s,
        Err(_) => return,
    };
    let line = format!("{}\n", entry);
    use tokio::io::AsyncWriteExt;
    if let Ok(mut f) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        let _ = f.write_all(line.as_bytes()).await;
    }
}

// ---------------------------------------------------------------------------
// Tool execution helpers
// ---------------------------------------------------------------------------

const MAX_CONTENT_LEN: usize = 30_000;
const MAX_LIST_ENTRIES: usize = 200;

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...[truncated]", &s[..max])
    }
}

/// Resolve a path relative to the sandbox directory. If the path is absolute
/// it is used as-is; otherwise it is joined with `sandbox_dir`.
fn resolve_path(sandbox_dir: &str, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        PathBuf::from(sandbox_dir).join(p)
    }
}

async fn exec_web_search(args: &Value) -> Value {
    let query = args["query"].as_str().unwrap_or("");
    let client = Client::new();
    let mut all_results: Vec<Value> = Vec::new();

    // Primary: DuckDuckGo Python library (same as original TigrimOS)
    // This returns actual web search results with titles, URLs, and snippets
    let safe_query = query.replace('\'', "\\'");
    let py_script = format!(
        r#"import json
try:
    from ddgs import DDGS
    r = list(DDGS().text('{}', max_results=8))
    print(json.dumps(r))
except ImportError:
    from duckduckgo_search import DDGS
    with DDGS() as ddgs:
        r = list(ddgs.text('{}', max_results=8))
        print(json.dumps(r))
"#,
        safe_query, safe_query
    );

    let py_result = timeout(
        Duration::from_secs(30),
        Command::new("python3")
            .arg("-c")
            .arg(&py_script)
            .output(),
    )
    .await;

    let mut ddg_ok = false;
    match py_result {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(results) = serde_json::from_str::<Vec<Value>>(stdout.trim()) {
                for r in results {
                    all_results.push(json!({
                        "source": "web",
                        "title": r.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                        "url": r.get("href").or(r.get("link")).and_then(|u| u.as_str()).unwrap_or(""),
                        "text": r.get("body").or(r.get("snippet")).and_then(|b| b.as_str()).unwrap_or("")
                    }));
                }
                ddg_ok = !all_results.is_empty();
            }
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("[web_search] Python DDG failed: {}", stderr.chars().take(200).collect::<String>());
        }
        Ok(Err(e)) => {
            warn!("[web_search] Failed to spawn python3: {e}");
        }
        Err(_) => {
            warn!("[web_search] Python DDG timed out (30s)");
        }
    }

    // Fallback: DuckDuckGo Instant Answer API (for quick facts/definitions)
    if !ddg_ok {
        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1",
            urlencoding::encode(query)
        );
        if let Ok(resp) = client.get(&url).send().await {
            if let Ok(body) = resp.text().await {
                if let Ok(ddg) = serde_json::from_str::<Value>(&body) {
                    if let Some(abs) = ddg.get("Abstract").and_then(|a| a.as_str()) {
                        if !abs.is_empty() {
                            all_results.push(json!({
                                "source": "abstract",
                                "title": ddg.get("Heading").and_then(|h| h.as_str()).unwrap_or(""),
                                "text": abs,
                                "url": ddg.get("AbstractURL").and_then(|u| u.as_str()).unwrap_or("")
                            }));
                        }
                    }
                    if let Some(topics) = ddg.get("RelatedTopics").and_then(|t| t.as_array()) {
                        for topic in topics.iter().take(5) {
                            if let Some(text) = topic.get("Text").and_then(|t| t.as_str()) {
                                all_results.push(json!({
                                    "source": "related",
                                    "text": text,
                                    "url": topic.get("FirstURL").and_then(|u| u.as_str()).unwrap_or("")
                                }));
                            }
                        }
                    }
                }
            }
        }
    }

    // Also try Wikipedia search (reliable for knowledge queries)
    let wiki_url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&format=json&list=search&srsearch={}&srlimit=3",
        urlencoding::encode(query)
    );
    if let Ok(resp) = client
        .get(&wiki_url)
        .header("User-Agent", "TigrimOS/1.0")
        .send()
        .await
    {
        if let Ok(data) = resp.json::<Value>().await {
            if let Some(results) = data.pointer("/query/search").and_then(|s| s.as_array()) {
                for item in results.iter().take(3) {
                    let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    let snippet = item
                        .get("snippet")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .replace("<span class=\"searchmatch\">", "")
                        .replace("</span>", "");
                    all_results.push(json!({
                        "source": "wikipedia",
                        "title": title,
                        "url": format!("https://en.wikipedia.org/wiki/{}", title.replace(' ', "_")),
                        "text": snippet
                    }));
                }
            }
        }
    }

    if all_results.is_empty() {
        json!({ "ok": true, "results": [], "note": "No results found. Try a different query or use fetch_url to access a specific page." })
    } else {
        json!({ "ok": true, "results": all_results })
    }
}

async fn exec_fetch_url(args: &Value) -> Value {
    let url = args["url"].as_str().unwrap_or("");
    let method = args["method"].as_str().unwrap_or("GET").to_uppercase();

    let client = Client::new();
    let req = match method.as_str() {
        "POST" => client.post(url),
        _ => client.get(url),
    };

    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            match resp.text().await {
                Ok(body) => {
                    let body = truncate(&body, MAX_CONTENT_LEN);
                    json!({ "ok": true, "status": status, "body": body })
                }
                Err(e) => json!({ "ok": false, "error": format!("Failed to read body: {e}") }),
            }
        }
        Err(e) => json!({ "ok": false, "error": format!("Request failed: {e}") }),
    }
}

// ---------------------------------------------------------------------------
// Security: dangerous pattern detection
// ---------------------------------------------------------------------------

/// Patterns that indicate potentially dangerous code/commands
const DANGEROUS_PATTERNS: &[&str] = &[
    // Destructive file operations
    "rm -rf /", "rm -rf ~", "rm -rf $HOME", "rmdir /",
    "shutil.rmtree('/')", "shutil.rmtree(\"/\")",
    // Privilege escalation
    "sudo ", "su -", "doas ",
    // Credential/key theft
    ".ssh/", "id_rsa", "id_ed25519", ".aws/credentials", ".netrc",
    ".env", "keychain", "login.keychain",
    // System modification
    "chmod 777 /", "chown root", "/etc/passwd", "/etc/shadow",
    "launchctl", "crontab",
    // Network exfiltration with credentials
    "curl.*-d.*password", "wget.*password",
    // Reverse shells
    "bash -i >& /dev/tcp", "nc -e /bin", "python.*socket.*connect",
    "/dev/tcp/", "mkfifo",
    // Code injection / download-and-execute
    "curl.*|.*sh", "curl.*|.*bash", "wget.*|.*sh", "wget.*|.*bash",
    "eval(requests", "exec(requests",
    // macOS specific
    "osascript.*administrator", "security find-generic-password",
    "defaults write", "csrutil",
];

/// Check if code/command contains dangerous patterns
fn check_dangerous(code: &str) -> Option<String> {
    let lower = code.to_lowercase();
    for pattern in DANGEROUS_PATTERNS {
        let p = pattern.to_lowercase();
        if lower.contains(&p) {
            return Some(format!("Blocked dangerous pattern: {}", pattern));
        }
    }

    // Block access to paths outside sandbox (absolute paths to sensitive dirs)
    let sensitive_dirs = ["/etc/", "/var/", "/usr/", "/System/", "/Library/",
                          "/Applications/", "/Users/*/.", "/private/"];
    for dir in &sensitive_dirs {
        if lower.contains(&dir.to_lowercase()) {
            // Allow /tmp/ and the sandbox itself
            if !lower.contains("/tmp/") {
                return Some(format!("Blocked access to system path: {}", dir));
            }
        }
    }

    None
}

async fn exec_run_python(args: &Value, sandbox_dir: &str) -> Value {
    let code = args["code"].as_str().unwrap_or("");

    // Security check
    if let Some(reason) = check_dangerous(code) {
        warn!("[SECURITY] Python code blocked: {}", reason);
        return json!({ "ok": false, "error": format!("Security: {reason}") });
    }

    // Prepend matplotlib non-interactive setup so plt.show() saves files instead of opening GUI
    let matplotlib_prelude = r#"
import sys, os, uuid
try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as _plt_orig
    _plt_show_count = [0]
    def _patched_show(*args, **kwargs):
        _plt_show_count[0] += 1
        fname = f'figure_{_plt_show_count[0]}.png'
        _plt_orig.savefig(fname, dpi=150, bbox_inches='tight')
        print(f'[saved figure to {fname}]')
        _plt_orig.close('all')
    import matplotlib.pyplot as plt
    plt.show = _patched_show
except ImportError:
    pass
"#;
    let code = &format!("{matplotlib_prelude}
{code}");

    // Ensure output_file directory exists
    let output_dir = std::path::Path::new(sandbox_dir).join("output_file");
    let _ = tokio::fs::create_dir_all(&output_dir).await;

    // Use sandbox-exec on macOS to restrict file system access
    let sandbox_profile = format!(
        r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow file-read*)
(allow file-write* (subpath "{}"))
(allow file-write* (subpath "/tmp"))
(allow file-write* (subpath "/private/tmp"))
(allow file-write* (subpath "/dev/null"))
(allow file-write* (subpath "/dev/tty"))
(allow sysctl-read)
(allow mach-lookup)
(allow network-outbound)
(allow network-inbound)
(allow signal)"#,
        std::path::Path::new(sandbox_dir)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(sandbox_dir))
            .display()
    );

    let result = timeout(
        Duration::from_secs(60),
        Command::new("sandbox-exec")
            .arg("-p")
            .arg(&sandbox_profile)
            .arg("python3")
            .arg("-c")
            .arg(code)
            .current_dir(sandbox_dir)
            .output(),
    )
    .await;

    // Fallback: if sandbox-exec fails (e.g., not on macOS), run directly
    let result = match &result {
        Ok(Ok(_)) => result,
        _ => {
            warn!("[sandbox] sandbox-exec failed, falling back to direct execution");
            timeout(
                Duration::from_secs(60),
                Command::new("python3")
                    .arg("-c")
                    .arg(code)
                    .current_dir(sandbox_dir)
                    .output(),
            )
            .await
        }
    };

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // Scan sandbox_dir for output files created in last 30 seconds
            let mut output_files = vec![];
            let scan_dirs = [
                std::path::Path::new(sandbox_dir).to_path_buf(),
                output_dir.clone(),
            ];
            for scan_dir in &scan_dirs {
                if let Ok(mut entries) = tokio::fs::read_dir(scan_dir).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        if let Ok(meta) = entry.metadata().await {
                            if let Ok(modified) = meta.modified() {
                                let name = entry.file_name().to_string_lossy().to_string();
                                let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
                                if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "svg" | "pdf" | "html" | "csv" | "md" | "txt" | "json" | "xlsx" | "docx") {
                                    if modified.elapsed().unwrap_or_default().as_secs() < 30 {
                                        let rel = format!("{}/{}", scan_dir.display(), name);
                                        if !output_files.contains(&rel) {
                                            output_files.push(rel);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            json!({
                "ok": output.status.success(),
                "exit_code": output.status.code(),
                "stdout": truncate(&stdout, MAX_CONTENT_LEN),
                "stderr": truncate(&stderr, MAX_CONTENT_LEN),
                "output_files": output_files,
            })
        }
        Ok(Err(e)) => json!({ "ok": false, "error": format!("Failed to spawn python3: {e}") }),
        Err(_) => json!({ "ok": false, "error": "Execution timed out (60s)" }),
    }
}

async fn exec_run_shell(args: &Value, sandbox_dir: &str) -> Value {
    let command = args["command"].as_str().unwrap_or("");

    // Security check
    if let Some(reason) = check_dangerous(command) {
        warn!("[SECURITY] Shell command blocked: {}", reason);
        return json!({ "ok": false, "error": format!("Security: {reason}") });
    }

    let cwd = args["cwd"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| sandbox_dir.to_string());

    // Use sandbox-exec on macOS
    let sandbox_profile = format!(
        r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow file-read*)
(allow file-write* (subpath "{}"))
(allow file-write* (subpath "/tmp"))
(allow file-write* (subpath "/private/tmp"))
(allow file-write* (subpath "/dev/null"))
(allow sysctl-read)
(allow mach-lookup)
(allow network-outbound)
(allow signal)"#,
        std::path::Path::new(sandbox_dir)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(sandbox_dir))
            .display()
    );

    let result = timeout(
        Duration::from_secs(30),
        Command::new("sandbox-exec")
            .arg("-p")
            .arg(&sandbox_profile)
            .arg("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .output(),
    )
    .await;

    // Fallback if sandbox-exec not available
    let result = match &result {
        Ok(Ok(_)) => result,
        _ => {
            warn!("[sandbox] sandbox-exec failed, falling back to direct execution");
            timeout(
                Duration::from_secs(30),
                Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .current_dir(&cwd)
                    .output(),
            )
            .await
        }
    };

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            json!({
                "ok": output.status.success(),
                "exit_code": output.status.code(),
                "stdout": truncate(&stdout, MAX_CONTENT_LEN),
                "stderr": truncate(&stderr, MAX_CONTENT_LEN),
            })
        }
        Ok(Err(e)) => json!({ "ok": false, "error": format!("Failed to spawn shell: {e}") }),
        Err(_) => json!({ "ok": false, "error": "Execution timed out (30s)" }),
    }
}

async fn exec_read_file(args: &Value, sandbox_dir: &str) -> Value {
    let path = args["path"].as_str().unwrap_or("");
    let resolved = resolve_path(sandbox_dir, path);

    match fs::read_to_string(&resolved).await {
        Ok(content) => json!({
            "ok": true,
            "content": truncate(&content, MAX_CONTENT_LEN),
            "path": resolved.display().to_string(),
        }),
        Err(e) => json!({ "ok": false, "error": format!("Failed to read file: {e}") }),
    }
}

async fn exec_write_file(args: &Value, sandbox_dir: &str) -> Value {
    let path = args["path"].as_str().unwrap_or("");
    let content = args["content"].as_str().unwrap_or("");
    let append = args["append"].as_bool().unwrap_or(false);
    let resolved = resolve_path(sandbox_dir, path);

    // Ensure parent directory exists
    if let Some(parent) = resolved.parent() {
        if let Err(e) = fs::create_dir_all(parent).await {
            return json!({ "ok": false, "error": format!("Failed to create directories: {e}") });
        }
    }

    let result = if append {
        use tokio::io::AsyncWriteExt;
        let mut file = match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&resolved)
            .await
        {
            Ok(f) => f,
            Err(e) => return json!({ "ok": false, "error": format!("Failed to open file: {e}") }),
        };
        file.write_all(content.as_bytes()).await
    } else {
        fs::write(&resolved, content).await
    };

    match result {
        Ok(()) => json!({
            "ok": true,
            "path": resolved.display().to_string(),
            "bytes_written": content.len(),
        }),
        Err(e) => json!({ "ok": false, "error": format!("Failed to write file: {e}") }),
    }
}

async fn exec_list_files(args: &Value, sandbox_dir: &str) -> Value {
    let path = args["path"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| sandbox_dir.to_string());
    let recursive = args["recursive"].as_bool().unwrap_or(false);
    let resolved = resolve_path(sandbox_dir, &path);

    let mut entries: Vec<String> = Vec::new();

    if recursive {
        if let Err(e) = walk_dir_recursive(&resolved, &mut entries, MAX_LIST_ENTRIES).await {
            return json!({ "ok": false, "error": format!("Failed to list directory: {e}") });
        }
    } else {
        match fs::read_dir(&resolved).await {
            Ok(mut dir) => {
                while let Ok(Some(entry)) = dir.next_entry().await {
                    if entries.len() >= MAX_LIST_ENTRIES {
                        entries.push("...[truncated]".to_string());
                        break;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);
                    entries.push(if is_dir {
                        format!("{}/", name)
                    } else {
                        name
                    });
                }
            }
            Err(e) => {
                return json!({ "ok": false, "error": format!("Failed to read directory: {e}") });
            }
        }
    }

    json!({ "ok": true, "entries": entries, "count": entries.len() })
}

/// Recursively walk a directory, collecting up to `limit` entries.
async fn walk_dir_recursive(
    dir: &PathBuf,
    entries: &mut Vec<String>,
    limit: usize,
) -> Result<(), std::io::Error> {
    let mut stack = vec![dir.clone()];

    while let Some(current) = stack.pop() {
        if entries.len() >= limit {
            entries.push("...[truncated]".to_string());
            break;
        }

        let mut rd = fs::read_dir(&current).await?;
        while let Some(entry) = rd.next_entry().await? {
            if entries.len() >= limit {
                entries.push("...[truncated]".to_string());
                return Ok(());
            }
            let path = entry.path();
            let display = path.display().to_string();
            let is_dir = entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);

            if is_dir {
                entries.push(format!("{}/", display));
                stack.push(path);
            } else {
                entries.push(display);
            }
        }
    }

    Ok(())
}

async fn exec_list_skills(_args: &Value, _sandbox_dir: &str) -> Value {
    // Read skills from data/skills.json
    let skills_path = std::path::Path::new("data/skills.json");
    let skills: Vec<Value> = if let Ok(content) = tokio::fs::read_to_string(skills_path).await {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        vec![]
    };

    let skill_names: Vec<String> = skills
        .iter()
        .filter(|s| s.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true))
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
        .collect();

    // Also check skills/ directory for SKILL.md files
    let mut dir_skills = vec![];
    if let Ok(mut entries) = tokio::fs::read_dir("skills").await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_type()
                .await
                .map(|t| t.is_dir())
                .unwrap_or(false)
            {
                let skill_md = entry.path().join("SKILL.md");
                if tokio::fs::metadata(&skill_md).await.is_ok() {
                    dir_skills.push(entry.file_name().to_string_lossy().to_string());
                }
            }
        }
    }

    json!({ "skills": skill_names, "dir_skills": dir_skills })
}

async fn exec_load_skill(args: &Value, _sandbox_dir: &str) -> Value {
    let skill_name = args.get("skill").and_then(|s| s.as_str()).unwrap_or("");
    if skill_name.is_empty() {
        return json!({"ok": false, "error": "Missing skill name"});
    }

    let skill_path = std::path::Path::new("skills")
        .join(skill_name)
        .join("SKILL.md");
    match tokio::fs::read_to_string(&skill_path).await {
        Ok(content) => {
            let truncated = content.len() > 15000;
            let content = if truncated {
                content[..15000].to_string()
            } else {
                content
            };
            json!({"ok": true, "skill": skill_name, "content": content, "truncated": truncated})
        }
        Err(_) => json!({"ok": false, "error": format!("Skill '{}' not found", skill_name)}),
    }
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

/// Execute spawn_subagent: loads agent config, builds persona prompt, calls LLM recursively
fn exec_spawn_subagent(
    args: &Value,
    sub_agent: &SubAgentConfig,
    sandbox_dir: &str,
    on_update: std::sync::Arc<dyn Fn(ToolUpdate) + Send + Sync + 'static>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send>> {
    let args = args.clone();
    let sub_agent = sub_agent.clone();
    let sandbox_dir = sandbox_dir.to_string();

    Box::pin(async move {
    let args = &args;
    let sub_agent = &sub_agent;
    let sandbox_dir = &sandbox_dir;
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");

    if agent_id.is_empty() || task.is_empty() {
        return json!({"ok": false, "error": "agent_id and task are required"});
    }

    if !sub_agent.agent_ids.contains(&agent_id.to_string()) {
        return json!({"ok": false, "error": format!("Unknown agent: {}. Available: {:?}", agent_id, sub_agent.agent_ids)});
    }

    if sub_agent.depth >= 3 {
        return json!({"ok": false, "error": "Max sub-agent recursion depth reached (3)"});
    }

    info!("[SubAgent] Spawning agent '{}' (depth={}) for task: {}", agent_id, sub_agent.depth, &task[..task.len().min(80)]);

    // Load YAML config
    let (yaml_val, _) = match load_agent_yaml(&sub_agent.config_file) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "Failed to load agent config"}),
    };

    // Find agent definition
    let agent_def = yaml_val
        .get("agents")
        .and_then(|a| a.as_array())
        .and_then(|arr| arr.iter().find(|a| a.get("id").and_then(|v| v.as_str()) == Some(agent_id)));

    let agent_def = match agent_def {
        Some(d) => d.clone(),
        None => return json!({"ok": false, "error": format!("Agent '{}' not found in config", agent_id)}),
    };

    let agent_name = agent_def.get("name").and_then(|v| v.as_str()).unwrap_or(agent_id);
    let agent_role = agent_def.get("role").and_then(|v| v.as_str()).unwrap_or("worker");

    // Log SUBAGENT_SPAWN to history JSONL
    write_agent_history(&sub_agent.session_id, "SUBAGENT_SPAWN", json!({
        "agent_id": agent_id,
        "agent_name": agent_name,
        "role": agent_role,
        "depth": sub_agent.depth,
        "task": task,
        "context": context,
        "config_file": sub_agent.config_file,
    })).await;

    // Find which agents this agent can delegate to (from connections)
    let targets: Vec<String> = yaml_val
        .get("connections")
        .and_then(|c| c.as_array())
        .map(|conns| {
            conns
                .iter()
                .filter(|c| c.get("from").and_then(|v| v.as_str()) == Some(agent_id))
                .filter_map(|c| c.get("to").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .filter(|id| id != "human")
                .collect()
        })
        .unwrap_or_default();

    // Check if mesh mode — all agents accessible
    let orch_mode = yaml_val
        .get("system")
        .and_then(|s| s.get("orchestration_mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let agent_mesh = agent_def
        .get("mesh")
        .and_then(|m| m.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let available_targets = if orch_mode == "mesh" || agent_mesh {
        sub_agent.agent_ids.iter().filter(|id| id.as_str() != agent_id).cloned().collect()
    } else {
        targets
    };

    // Build system prompt for this agent
    let system_prompt = build_agent_system_prompt(&agent_def, &yaml_val, &available_targets);

    // Build message with task
    let user_msg = if context.is_empty() {
        format!("Task: {}", task)
    } else {
        format!("Task: {}\n\nContext:\n{}", task, context)
    };

    let messages = vec![json!({"role": "user", "content": user_msg})];

    // Create sub-agent config with incremented depth
    let child_config = SubAgentConfig {
        enabled: !available_targets.is_empty(),
        config_file: sub_agent.config_file.clone(),
        agent_ids: available_targets,
        api_key: sub_agent.api_key.clone(),
        api_url: sub_agent.api_url.clone(),
        model: sub_agent.model.clone(),
        depth: sub_agent.depth + 1,
        session_id: sub_agent.session_id.clone(),
    };

    // Recursive call — propagate ToolCall/ToolResult to parent UI (drop TextChunk to avoid mangling main text)
    let on_update_child = on_update.clone();
    let result = call_with_tools(
        &sub_agent.api_key,
        &sub_agent.api_url,
        &sub_agent.model,
        messages,
        Some(system_prompt),
        sandbox_dir,
        move |update| {
            match &update {
                ToolUpdate::TextChunk(_) => {}, // don't merge sub-agent text into parent stream
                _ => on_update_child(update),
            }
        },
        child_config,
    )
    .await;

    let result_preview = if result.content.len() > 200 {
        format!("{}...", &result.content[..200])
    } else {
        result.content.clone()
    };

    info!("[SubAgent] Agent '{}' completed. Result length: {} chars", agent_id, result.content.len());

    // Log SUBAGENT_DONE to history JSONL
    write_agent_history(&sub_agent.session_id, "SUBAGENT_DONE", json!({
        "agent_id": agent_id,
        "agent_name": agent_name,
        "depth": sub_agent.depth,
        "result_length": result.content.len(),
        "tool_calls": result.tool_results.len(),
        "files_generated": result.files.len(),
        "result_preview": result_preview,
    })).await;

    json!({
        "ok": true,
        "agent_id": agent_id,
        "agent_name": agent_name,
        "role": agent_role,
        "result": result.content,
        "tool_calls_count": result.tool_results.len(),
        "output_files": result.files,
    })
    }) // end Box::pin(async move)
}

async fn execute_tool(name: &str, args: &Value, sandbox_dir: &str) -> Value {
    match name {
        "web_search" => exec_web_search(args).await,
        "fetch_url" => exec_fetch_url(args).await,
        "run_python" => exec_run_python(args, sandbox_dir).await,
        "run_shell" => exec_run_shell(args, sandbox_dir).await,
        "read_file" => exec_read_file(args, sandbox_dir).await,
        "write_file" => exec_write_file(args, sandbox_dir).await,
        "list_files" => exec_list_files(args, sandbox_dir).await,
        "list_skills" => exec_list_skills(args, sandbox_dir).await,
        "load_skill" => exec_load_skill(args, sandbox_dir).await,
        _ => json!({ "ok": false, "error": format!("Unknown tool: {name}") }),
    }
}

// ---------------------------------------------------------------------------
// URL encoding helper (inline, avoids extra crate)
// ---------------------------------------------------------------------------

mod urlencoding {
    pub fn encode(input: &str) -> String {
        let mut result = String::with_capacity(input.len() * 3);
        for byte in input.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    result.push(byte as char);
                }
                _ => {
                    result.push('%');
                    result.push_str(&format!("{:02X}", byte));
                }
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Tool-calling loop
// ---------------------------------------------------------------------------

const MAX_ROUNDS: usize = 8;
const MAX_TOTAL_TOOL_CALLS: usize = 12;
const MAX_LOOP_REPEATS: usize = 3;
const MAX_CONSECUTIVE_ERRORS: usize = 3;

pub async fn call_with_tools(
    api_key: &str,
    api_url: &str,
    model: &str,
    messages: Vec<Value>,
    system_prompt: Option<String>,
    sandbox_dir: &str,
    on_update: impl Fn(ToolUpdate) + Send + Sync + 'static,
    sub_agent: SubAgentConfig,
) -> ToolLoopResult {
    let on_update = std::sync::Arc::new(on_update);
    let client = Client::new();
    let tools = if sub_agent.enabled {
        tool_definitions_with_subagent(&sub_agent)
    } else {
        tool_definitions()
    };

    let mut all_messages = Vec::new();

    // Prepend system prompt if provided
    if let Some(sys) = &system_prompt {
        all_messages.push(json!({ "role": "system", "content": sys }));
    }
    all_messages.extend(messages);

    let mut tool_records: Vec<ToolCallRecord> = Vec::new();
    let mut collected_files: Vec<String> = Vec::new();
    let mut total_tool_calls: usize = 0;
    let mut consecutive_errors: usize = 0;

    // For loop detection: track recent (tool_name, args_signature) tuples
    let mut recent_signatures: Vec<String> = Vec::new();

    for round in 0..MAX_ROUNDS {
        info!("Tool loop round {}", round + 1);

        // Build the request body
        let body = json!({
            "model": model,
            "messages": all_messages,
            "tools": tools,
            "tool_choice": "auto",
        });

        let response = match client
            .post(api_url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("API request failed: {e}");
                error!("{}", msg);
                on_update(ToolUpdate::Error(msg.clone()));
                consecutive_errors += 1;
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    return ToolLoopResult {
                        content: format!("Error: {msg}"),
                        tool_results: tool_records,
                        files: collected_files.clone(),
                    };
                }
                continue;
            }
        };

        let resp_json: Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("Failed to parse API response: {e}");
                error!("{}", msg);
                on_update(ToolUpdate::Error(msg.clone()));
                consecutive_errors += 1;
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    return ToolLoopResult {
                        content: format!("Error: {msg}"),
                        tool_results: tool_records,
                        files: collected_files.clone(),
                    };
                }
                continue;
            }
        };

        // Reset consecutive errors on successful API response
        consecutive_errors = 0;

        // Check for API-level error
        if let Some(err) = resp_json.get("error") {
            let msg = format!("API error: {}", err);
            error!("{}", msg);
            on_update(ToolUpdate::Error(msg.clone()));
            return ToolLoopResult {
                content: format!("Error: {msg}"),
                tool_results: tool_records,
                files: collected_files.clone(),
            };
        }

        let choice = &resp_json["choices"][0];
        let message = &choice["message"];
        let _finish_reason = choice["finish_reason"].as_str().unwrap_or("");

        // Append the assistant message to the conversation
        all_messages.push(message.clone());

        // Check for tool calls
        let tool_calls = message["tool_calls"].as_array();

        if let Some(calls) = tool_calls {
            if calls.is_empty() {
                // No tool calls -- treat as final response
                let content = message["content"].as_str().unwrap_or("").to_string();
                on_update(ToolUpdate::TextChunk(content.clone()));
                return ToolLoopResult {
                    content,
                    tool_results: tool_records,
                    files: collected_files.clone(),
                };
            }

            for call in calls {
                if total_tool_calls >= MAX_TOTAL_TOOL_CALLS {
                    warn!("Max total tool calls reached ({})", MAX_TOTAL_TOOL_CALLS);
                    let content = force_final_response(
                        &client, api_key, api_url, model, &all_messages, &tool_records,
                        total_tool_calls,
                    ).await;
                    on_update(ToolUpdate::TextChunk(content.clone()));
                    return ToolLoopResult {
                        content,
                        tool_results: tool_records,
                        files: collected_files.clone(),
                    };
                }

                let tool_name = call["function"]["name"].as_str().unwrap_or("unknown");
                let tool_args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
                let tool_id = call["id"].as_str().unwrap_or("");

                let tool_args: Value =
                    serde_json::from_str(tool_args_str).unwrap_or_else(|_| json!({}));

                // Loop detection
                let signature = format!("{}:{}", tool_name, tool_args);
                recent_signatures.push(signature.clone());
                if recent_signatures.len() >= MAX_LOOP_REPEATS {
                    let tail = &recent_signatures[recent_signatures.len() - MAX_LOOP_REPEATS..];
                    if tail.iter().all(|s| s == &signature) {
                        warn!("Loop detected: same tool+args repeated {} times", MAX_LOOP_REPEATS);
                        on_update(ToolUpdate::Error(
                            "Loop detected: same tool call repeated".to_string(),
                        ));
                        let content = force_final_response(
                            &client, api_key, api_url, model, &all_messages, &tool_records,
                            total_tool_calls,
                        ).await;
                        on_update(ToolUpdate::TextChunk(content.clone()));
                        return ToolLoopResult {
                            content,
                            tool_results: tool_records,
                            files: collected_files.clone(),
                        };
                    }
                }

                on_update(ToolUpdate::ToolCall {
                    name: tool_name.to_string(),
                    args: tool_args.clone(),
                });

                info!("Executing tool: {} with args: {}", tool_name, tool_args);
                let result = if tool_name == "spawn_subagent" {
                    exec_spawn_subagent(&tool_args, &sub_agent, sandbox_dir, on_update.clone()).await
                } else {
                    execute_tool(tool_name, &tool_args, sandbox_dir).await
                };

                on_update(ToolUpdate::ToolResult {
                    name: tool_name.to_string(),
                    result: result.clone(),
                });

                tool_records.push(ToolCallRecord {
                    tool: tool_name.to_string(),
                    result: result.clone(),
                });

                // Collect output files from tool result
                if let Some(files) = result.get("output_files").and_then(|v| v.as_array()) {
                    for f in files {
                        if let Some(s) = f.as_str() {
                            if !collected_files.contains(&s.to_string()) {
                                collected_files.push(s.to_string());
                            }
                        }
                    }
                }

                // Append tool result message (with truncation)
                let mut result_str =
                    serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
                let max_len = if tool_name == "load_skill" {
                    3000
                } else {
                    6000
                };
                if result_str.len() > max_len {
                    result_str = format!("{}...(truncated)", &result_str[..max_len]);
                }
                all_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_id,
                    "content": result_str,
                }));

                total_tool_calls += 1;
            }

            // Continue to next round (the model will see the tool results)
        } else {
            // No tool calls -- this is the final text response
            let content = message["content"].as_str().unwrap_or("").to_string();
            on_update(ToolUpdate::TextChunk(content.clone()));
            return ToolLoopResult {
                content,
                tool_results: tool_records,
                files: collected_files.clone(),
            };
        }
    }

    // Exhausted max rounds — force a final text response
    warn!("Tool loop exhausted max rounds ({})", MAX_ROUNDS);
    let content = force_final_response(
        &client, api_key, api_url, model, &all_messages, &tool_records,
        total_tool_calls,
    ).await;
    on_update(ToolUpdate::TextChunk(content.clone()));

    ToolLoopResult {
        content,
        tool_results: tool_records,
        files: collected_files,
    }
}

/// After hitting tool limits, call the LLM one more time WITHOUT tools to force a text summary.
/// Mirrors tiger_cowork's final-response pattern.
async fn force_final_response(
    client: &Client,
    api_key: &str,
    api_url: &str,
    model: &str,
    all_messages: &[Value],
    tool_records: &[ToolCallRecord],
    total_tool_calls: usize,
) -> String {
    // Build compact tool summary
    let tool_summary = tool_records.iter().map(|tr| {
        let brief = if let Some(files) = tr.result.get("output_files").and_then(|v| v.as_array()) {
            if !files.is_empty() {
                let names: Vec<_> = files.iter().filter_map(|v| v.as_str()).collect();
                format!("Generated: {}", names.join(", "))
            } else if let Some(stdout) = tr.result.get("stdout").and_then(|v| v.as_str()) {
                stdout.chars().take(300).collect::<String>()
            } else if let Some(results) = tr.result.get("results").and_then(|v| v.as_array()) {
                format!("{} results found", results.len())
            } else if tr.result.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                tr.result.get("error").and_then(|v| v.as_str()).unwrap_or("failed").to_string()
            } else if let Some(res) = tr.result.get("result").and_then(|v| v.as_str()) {
                res.chars().take(200).collect::<String>()
            } else {
                serde_json::to_string(&tr.result).unwrap_or_default().chars().take(200).collect::<String>()
            }
        } else {
            serde_json::to_string(&tr.result).unwrap_or_default().chars().take(200).collect::<String>()
        };
        format!("[{}]: {}", tr.tool, brief)
    }).collect::<Vec<_>>().join("\n");

    // Build minimal message list: system + user messages only
    let mut final_messages: Vec<Value> = all_messages.iter().filter(|m| {
        let role = m["role"].as_str().unwrap_or("");
        role == "system" || role == "user"
    }).cloned().collect();

    final_messages.push(json!({
        "role": "system",
        "content": format!(
            "You have executed {} tool calls. Summary of results:\n{}\n\n\
            Now provide a clear, helpful final response to the user based on these results. \
            Synthesize the information into a coherent answer. Mention any generated files. \
            Do NOT call any tools. Do NOT include tool names or markers like [web_search] in your response.",
            total_tool_calls, tool_summary
        )
    }));

    let body = json!({
        "model": model,
        "messages": final_messages,
        // no "tools" — forces text-only response
    });

    match client
        .post(api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            match resp.json::<Value>().await {
                Ok(data) => {
                    let content = data["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    if !content.is_empty() {
                        return content;
                    }
                }
                Err(e) => warn!("[FinalResponse] Failed to parse response: {e}"),
            }
        }
        Err(e) => warn!("[FinalResponse] API call failed: {e}"),
    }

    // Absolute fallback: build a plain summary from stdout/results
    let fallback = tool_records.iter()
        .filter_map(|tr| {
            tr.result.get("stdout").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                .or_else(|| tr.result.get("result").and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
        })
        .take(3)
        .map(|s| s.chars().take(500).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n\n");

    if !fallback.is_empty() {
        fallback
    } else {
        "Task completed. The agents gathered information but could not generate a final summary.".to_string()
    }
}
