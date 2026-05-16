use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::fs;
use tokio::process::Command;
use tokio::sync::{Mutex as TokioMutex, Notify};
use tokio::time::{timeout, Duration};
use tracing::{error, info, warn};
use crate::server::services::protocols;
use crate::server::services::compact;
use crate::server::services::mcp;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find python binary — checks PATH first, then common platform-specific locations.
pub(crate) fn find_python() -> String {
    // On Windows, try "python" first (standard), then "python3"
    // On Unix, try "python3" first, then "python"
    #[cfg(target_os = "windows")]
    let (primary, secondary) = ("python", "python3");
    #[cfg(not(target_os = "windows"))]
    let (primary, secondary) = ("python3", "python");

    // Try `which` (Unix) or `where` (Windows) to find in PATH
    #[cfg(target_os = "windows")]
    let which_cmd = "where";
    #[cfg(not(target_os = "windows"))]
    let which_cmd = "/usr/bin/which";

    for name in &[primary, secondary] {
        if let Ok(output) = std::process::Command::new(which_cmd).arg(name).output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout)
                    .lines().next().unwrap_or("").trim().to_string();
                if !path.is_empty() {
                    return path;
                }
            }
        }
    }

    // Platform-specific fallback locations
    #[cfg(target_os = "macos")]
    {
        for candidate in &[
            "/usr/bin/python3",
            "/usr/local/bin/python3",
            "/opt/homebrew/bin/python3",
            "/Library/Frameworks/Python.framework/Versions/Current/bin/python3",
        ] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for candidate in &["/usr/bin/python3", "/usr/local/bin/python3", "/usr/bin/python"] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Check common Windows Python install locations
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let programs = std::path::PathBuf::from(&local).join("Programs").join("Python");
            if let Ok(entries) = std::fs::read_dir(&programs) {
                for entry in entries.flatten() {
                    let py = entry.path().join("python.exe");
                    if py.exists() {
                        return py.to_string_lossy().to_string();
                    }
                }
            }
        }
        for candidate in &[
            r"C:\Python312\python.exe",
            r"C:\Python311\python.exe",
            r"C:\Python310\python.exe",
            r"C:\Python39\python.exe",
        ] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
    }

    primary.to_string()
}

/// Ensure PATH includes common tool locations (for .app bundles / Windows shortcuts).
fn ensure_full_path() {
    let current = std::env::var("PATH").unwrap_or_default();

    #[cfg(not(target_os = "windows"))]
    {
        if !current.contains("/opt/homebrew/bin") || !current.contains("/usr/local/bin") {
            let full = format!(
                "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}",
                current
            );
            std::env::set_var("PATH", full);
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Ensure common Windows Python/tool paths are on PATH
        let mut extra = Vec::new();
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let programs = std::path::PathBuf::from(&local).join("Programs").join("Python");
            if let Ok(entries) = std::fs::read_dir(&programs) {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if dir.is_dir() {
                        extra.push(dir.to_string_lossy().to_string());
                        extra.push(dir.join("Scripts").to_string_lossy().to_string());
                    }
                }
            }
        }
        if !extra.is_empty() {
            let full = format!("{};{}", extra.join(";"), current);
            std::env::set_var("PATH", full);
        }
    }
}

/// Check if the VM is running by probing the SSH port.
async fn is_vm_running() -> bool {
    tokio::net::TcpStream::connect(format!("127.0.0.1:{}", crate::vm::VmConfig::SSH_HOST_PORT))
        .await
        .is_ok()
}

/// Run a command inside the VM via SSH. Returns Ok((stdout, stderr, success)) or Err.
async fn run_in_vm(cmd: &str, timeout_secs: u64) -> Result<(String, String, bool), String> {
    let port = crate::vm::VmConfig::SSH_HOST_PORT.to_string();
    let result = timeout(
        Duration::from_secs(timeout_secs),
        Command::new("sshpass")
            .args([
                "-p", "tigris",
                "ssh",
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "ConnectTimeout=5",
                "-o", "LogLevel=ERROR",
                "-p", &port,
                "tigris@localhost",
                cmd,
            ])
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok((stdout, stderr, output.status.success()))
        }
        Ok(Err(e)) => Err(format!("Failed to run in VM: {e}")),
        Err(_) => Err(format!("VM execution timed out ({timeout_secs}s)")),
    }
}

/// Build a Command for Python execution.
fn python_command() -> Command {
    ensure_full_path();
    let python = find_python();
    Command::new(&python)
}

/// Build a shell Command.
fn shell_command() -> Command {
    ensure_full_path();
    #[cfg(target_os = "windows")]
    { Command::new("cmd.exe") }
    #[cfg(not(target_os = "windows"))]
    { Command::new("/bin/sh") }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ToolUpdate {
    ToolCall { name: String, args: Value },
    ToolResult { name: String, result: Value },
    TextChunk(String),
    Error(String),
    /// Request user approval before executing a dangerous tool.
    /// The UI must call `respond_tool_approval(true/false)` to continue.
    ApprovalRequired { name: String, args: Value },
}

// ---------------------------------------------------------------------------
// Tool approval gate (global channel for UI ↔ toolbox)
// ---------------------------------------------------------------------------

static APPROVAL_TX: OnceLock<TokioMutex<Option<tokio::sync::oneshot::Sender<bool>>>> =
    OnceLock::new();
static APPROVAL_RX: OnceLock<TokioMutex<Option<tokio::sync::oneshot::Receiver<bool>>>> =
    OnceLock::new();

/// Check if a tool requires user approval based on settings
async fn tool_requires_approval(tool_name: &str) -> bool {
    let settings = crate::server::data::get_settings().await;
    match tool_name {
        "run_shell" => settings.approval_required_for_shell.unwrap_or(true),
        "run_python" | "run_react" => settings.approval_required_for_python.unwrap_or(true),
        "write_file" => settings.approval_required_for_file_write.unwrap_or(false),
        "delete_file" => settings.approval_required_for_file_delete.unwrap_or(true),
        "claude_code_agent" => settings.approval_required_for_agent_spawn.unwrap_or(false),
        "gemini_cli_agent" => settings.approval_required_for_agent_spawn.unwrap_or(false),
        _ => false,
    }
}

/// Called by the tool dispatch to request approval. Returns true if approved.
async fn request_tool_approval(
    tool_name: &str,
    tool_args: &Value,
    on_update: &Arc<dyn Fn(ToolUpdate) + Send + Sync + 'static>,
) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();

    // Store the receiver so the approval response can be read
    {
        let lock = APPROVAL_RX.get_or_init(|| TokioMutex::new(None));
        *lock.lock().await = Some(rx);
    }
    {
        let lock = APPROVAL_TX.get_or_init(|| TokioMutex::new(None));
        *lock.lock().await = Some(tx);
    }

    // Notify UI
    on_update(ToolUpdate::ApprovalRequired {
        name: tool_name.to_string(),
        args: tool_args.clone(),
    });

    // Wait for user response (with timeout)
    let rx_lock = APPROVAL_RX.get_or_init(|| TokioMutex::new(None));
    let rx = rx_lock.lock().await.take();
    match rx {
        Some(rx) => {
            match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
                Ok(Ok(approved)) => approved,
                _ => false, // timeout or channel error → deny
            }
        }
        None => false,
    }
}

/// Called by the UI to approve or deny a pending tool execution.
pub async fn respond_tool_approval(approved: bool) {
    let lock = APPROVAL_TX.get_or_init(|| TokioMutex::new(None));
    if let Some(tx) = lock.lock().await.take() {
        let _ = tx.send(approved);
    }
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
    pub agent_id: String,             // current agent's own ID (for protocol tools)
    pub mode: String,                 // "fully_auto", "auto", "auto_swarm", "manual"
    pub agent_role: String,            // "orchestrator", "worker", etc. — used for tool filtering
    pub cancel_flag: Arc<AtomicBool>,  // set to true to abort the tool loop (saves checkpoint)
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
            agent_id: "main".to_string(),
            mode: "auto".to_string(),
            agent_role: String::new(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

// ---------------------------------------------------------------------------
// Realtime multi-agent session (tiger_cowork realtime mode clone)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AgentResult {
    pub result: String,
    pub output_files: Vec<String>,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct AgentTask {
    pub from: String,
    pub task: String,
    pub context: Option<String>,
}

#[allow(dead_code)]
pub struct RealtimeAgentHandle {
    pub agent_def: Value,
    pub status: Arc<TokioMutex<String>>,          // "idle" | "working"
    pub task_tx: tokio::sync::mpsc::Sender<AgentTask>,
}

#[allow(dead_code)]
pub struct RealtimeSession {
    pub session_id: String,
    pub agents: HashMap<String, RealtimeAgentHandle>,
    pub system_config: Value,
    /// Completed results keyed by agent_id — consumed by wait_result
    pub results: Arc<TokioMutex<HashMap<String, AgentResult>>>,
    /// Notified whenever a new result is published
    pub result_notify: Arc<Notify>,
    pub abort_tx: tokio::sync::broadcast::Sender<()>,
}

// Global store: session_id -> RealtimeSession
static REALTIME_SESSIONS: std::sync::OnceLock<TokioMutex<HashMap<String, Arc<TokioMutex<RealtimeSession>>>>> =
    std::sync::OnceLock::new();

fn realtime_sessions() -> &'static TokioMutex<HashMap<String, Arc<TokioMutex<RealtimeSession>>>> {
    REALTIME_SESSIONS.get_or_init(|| TokioMutex::new(HashMap::new()))
}

// Global store: session_id -> selected swarm filename
static AUTO_SWARM_SELECTIONS: std::sync::OnceLock<TokioMutex<HashMap<String, String>>> =
    std::sync::OnceLock::new();

fn auto_swarm_selections() -> &'static TokioMutex<HashMap<String, String>> {
    AUTO_SWARM_SELECTIONS.get_or_init(|| TokioMutex::new(HashMap::new()))
}

// Global store: session_id -> auto-created architecture filename
static AUTO_CREATED_ARCHITECTURES: std::sync::OnceLock<TokioMutex<HashMap<String, String>>> =
    std::sync::OnceLock::new();

fn auto_created_architectures() -> &'static TokioMutex<HashMap<String, String>> {
    AUTO_CREATED_ARCHITECTURES.get_or_init(|| TokioMutex::new(HashMap::new()))
}

// Global broadcast channel for subagent activity visible in the UI chat log.
// Each message: (session_id, agent_id, line)
static SUBAGENT_LOG_TX: std::sync::OnceLock<tokio::sync::broadcast::Sender<(String, String, String)>> =
    std::sync::OnceLock::new();

fn subagent_log_tx() -> &'static tokio::sync::broadcast::Sender<(String, String, String)> {
    SUBAGENT_LOG_TX.get_or_init(|| {
        let (tx, _) = tokio::sync::broadcast::channel(256);
        tx
    })
}

/// Subscribe to subagent activity log for a given session.
pub fn subscribe_subagent_log() -> tokio::sync::broadcast::Receiver<(String, String, String)> {
    subagent_log_tx().subscribe()
}

/// Append a line to the session's activity log file (mirrors TS appendSessionProgress).
/// Used by orchestrator tool call logging so the Activity panel shows orchestrator actions.
pub fn append_session_progress(session_id: &str, text: &str) {
    let log_dir = crate::server::data::data_dir().join("activity_logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{}.log", session_id));
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
}

/// Log orchestrator tool call to the Activity panel (mirrors TS socket.ts onToolCall).
fn log_orchestrator_tool_call(session_id: &str, name: &str, args: &Value) {
    if name.starts_with("proto_") {
        let proto_name = name.replace("proto_", "").split('_').next().unwrap_or("").to_uppercase();
        append_session_progress(session_id,
            &format!("> **{}** **Orchestrator** → `{}`\n", proto_name, name));
    } else if name == "send_task" {
        let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("agent");
        append_session_progress(session_id,
            &format!("> **Orchestrator** delegating task to {}\n", to));
    } else if name == "wait_result" {
        let from = args.get("from").and_then(|v| v.as_str()).unwrap_or("agent");
        append_session_progress(session_id,
            &format!("> **Orchestrator** waiting for {}\n", from));
    } else {
        append_session_progress(session_id,
            &format!("> **Orchestrator** → `{}`\n", name));
    }
}

// Global signal: when fully_auto creates an architecture, set this so the Agents tab can auto-load it.
static PENDING_ARCH_FILE: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

/// Set the pending architecture file for the Agents tab to auto-load.
pub fn set_pending_arch_file(filename: &str) {
    let m = PENDING_ARCH_FILE.get_or_init(|| std::sync::Mutex::new(None));
    *m.lock().unwrap() = Some(filename.to_string());
}

/// Take the pending architecture file (clears it after reading).
pub fn take_pending_arch_file() -> Option<String> {
    let m = PENDING_ARCH_FILE.get_or_init(|| std::sync::Mutex::new(None));
    m.lock().unwrap().take()
}

/// Query live agent statuses from the active realtime session.
/// Returns a map of agent_id -> status ("idle", "working").
pub async fn get_all_agent_statuses() -> HashMap<String, String> {
    let map = realtime_sessions().lock().await;
    let mut result = HashMap::new();
    for (_sid, session_arc) in map.iter() {
        let session = session_arc.lock().await;
        for (id, handle) in &session.agents {
            let status = handle.status.lock().await.clone();
            result.insert(id.clone(), status);
        }
    }
    result
}

/// Boot all agents in the YAML config as persistent tokio tasks.
/// Returns immediately; agents wait for tasks on their mpsc channels.
/// Fire-and-forget helper to boot a realtime session without blocking the caller.
/// Uses a oneshot to wait for the session to be ready before returning.
fn boot_realtime_session_deferred(
    session_id: String, config_file: String,
    api_key: String, api_url: String, model: String,
) {
    tokio::spawn(async move {
        start_realtime_session(&session_id, &config_file, &api_key, &api_url, &model, "sandbox").await;
    });
}

/// Force create_architecture on first message in auto_create mode.
/// Called proactively by the UI — does NOT wait for LLM to choose the tool.
/// Returns (ok, config_filename, summary_message).
pub async fn force_create_architecture(
    user_message: &str,
    sub_agent: &SubAgentConfig,
    _sandbox_dir: &str,
) -> (bool, Option<String>, String) {
    let args = serde_json::json!({
        "description": user_message,
        "architectureType": "hierarchical",
        "agentCount": "auto",
    });
    let result = exec_create_architecture(&args, sub_agent).await;
    let ok = result["ok"].as_bool().unwrap_or(false);
    if ok {
        let filename = result["filename"].as_str().unwrap_or("").to_string();
        let message = result["message"].as_str().unwrap_or("Architecture created.").to_string();
        // NOTE: realtime session boot is handled by the caller (chat.rs)
        (true, Some(filename), message)
    } else {
        let err = result["error"].as_str().unwrap_or("Unknown error").to_string();
        (false, None, format!("Failed to create architecture: {}", err))
    }
}

/// Check if this session already has an auto-created architecture.
pub async fn get_session_architecture(session_id: &str) -> Option<String> {
    auto_created_architectures().lock().await.get(session_id).cloned()
}

pub async fn start_realtime_session(
    session_id: &str,
    config_file: &str,
    api_key: &str,
    api_url: &str,
    model: &str,
    sandbox_dir: &str,
) -> bool {
    // Already running?
    {
        let map = realtime_sessions().lock().await;
        if map.contains_key(session_id) {
            info!("[Realtime] Session {} already active", session_id);
            return true;
        }
    }

    let (yaml_val, _) = match load_agent_yaml(config_file) {
        Some(v) => v,
        None => {
            error!("[Realtime] Failed to load agent config: {}", config_file);
            return false;
        }
    };

    let (abort_tx, _) = tokio::sync::broadcast::channel::<()>(4);
    let results = Arc::new(TokioMutex::new(HashMap::<String, AgentResult>::new()));
    let result_notify = Arc::new(Notify::new());

    let agents_arr = yaml_val["agents"].as_array().cloned().unwrap_or_default();
    let mut agent_handles: HashMap<String, RealtimeAgentHandle> = HashMap::new();

    for agent_def in &agents_arr {
        let agent_id = match agent_def.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let role = agent_def.get("role").and_then(|v| v.as_str()).unwrap_or("worker");

        let (task_tx, task_rx) = tokio::sync::mpsc::channel::<AgentTask>(8);
        let status = Arc::new(TokioMutex::new("idle".to_string()));

        // Human nodes don't get an LLM loop
        if role != "human" {
            let abort_rx = abort_tx.subscribe();
            tokio::spawn(realtime_agent_loop(
                agent_def.clone(),
                session_id.to_string(),
                yaml_val.clone(),
                api_key.to_string(),
                api_url.to_string(),
                model.to_string(),
                sandbox_dir.to_string(),
                task_rx,
                results.clone(),
                result_notify.clone(),
                status.clone(),
                abort_rx,
            ));
        }

        agent_handles.insert(agent_id.clone(), RealtimeAgentHandle {
            agent_def: agent_def.clone(),
            status,
            task_tx,
        });

        // Write SUBAGENT_SPAWN to agent history so the graphic view can show this agent
        if role != "human" {
            let name = agent_def.get("name").and_then(|v| v.as_str()).unwrap_or(&agent_id);
            write_agent_history(session_id, "SUBAGENT_SPAWN", json!({
                "agent_name": agent_id,
                "display_name": name,
                "role": role,
                "parent": "main",
            })).await;
        }

        info!("[Realtime] Agent {} ({}) booted and listening", agent_id, role);
    }

    // Write SESSION_CONFIG so the graphic view knows the orchestration mode and YAML connections
    let orch_mode = yaml_val["system"]["orchestration_mode"].as_str().unwrap_or("hierarchical");
    write_agent_history(session_id, "SESSION_CONFIG", json!({
        "orchestration_mode": orch_mode,
        "connections": yaml_val.get("connections").cloned().unwrap_or(json!([])),
        "workflow": yaml_val.get("workflow").cloned().unwrap_or(json!({})),
    })).await;

    let session = Arc::new(TokioMutex::new(RealtimeSession {
        session_id: session_id.to_string(),
        agents: agent_handles,
        system_config: yaml_val,
        results,
        result_notify,
        abort_tx,
    }));

    realtime_sessions().lock().await.insert(session_id.to_string(), session);
    info!("[Realtime] Session {} started with {} agents", session_id, agents_arr.len());
    true
}

/// Shut down all agent tasks for a session.
pub async fn shutdown_realtime_session(session_id: &str) {
    let mut map = realtime_sessions().lock().await;
    if let Some(session_arc) = map.remove(session_id) {
        let session = session_arc.lock().await;
        let _ = session.abort_tx.send(());
        info!("[Realtime] Session {} shut down", session_id);
    }
}

/// Get all non-human agent IDs from the system config.
fn all_non_human_ids(system_config: &Value) -> Vec<String> {
    system_config["agents"].as_array()
        .map(|arr| arr.iter()
            .filter_map(|a| a["id"].as_str().map(|s| s.to_string()))
            .filter(|id| id != "human")
            .collect())
        .unwrap_or_default()
}

/// Get downstream targets for an agent from workflow + connections.
fn get_downstream(agent_id: &str, system_config: &Value) -> Vec<String> {
    let workflow_outputs: Vec<String> = system_config
        .get("workflow").and_then(|w| w.get("sequence")).and_then(|s| s.as_array())
        .and_then(|arr| arr.iter().find(|s| s.get("agent").and_then(|v| v.as_str()) == Some(agent_id)))
        .and_then(|step| step.get("outputs_to").and_then(|v| v.as_array()))
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let conn_targets: Vec<String> = system_config
        .get("connections").and_then(|v| v.as_array())
        .map(|arr| arr.iter()
            .filter(|c| c.get("from").and_then(|v| v.as_str()) == Some(agent_id))
            .filter_map(|c| c.get("to").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect())
        .unwrap_or_default();

    let mut d = workflow_outputs;
    for t in conn_targets { if !d.contains(&t) { d.push(t); } }
    d.retain(|id| id != "human");
    d
}

/// Check if an agent has mesh.enabled in its YAML definition.
fn agent_has_mesh(agent_def: &Value) -> bool {
    agent_def.get("mesh")
        .and_then(|m| m.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Get mesh-enabled agent IDs from system config.
fn mesh_agent_ids(system_config: &Value) -> Vec<String> {
    system_config["agents"].as_array()
        .map(|arr| arr.iter()
            .filter(|a| agent_has_mesh(a) && a["role"].as_str() != Some("human"))
            .filter_map(|a| a["id"].as_str().map(|s| s.to_string()))
            .collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Skills advertising — mirrors tiger_cowork buildEnabledSkillsBlock()
// ---------------------------------------------------------------------------

const SUBAGENT_SKILLS_PERSONA: &str =
    "BEFORE you start the assigned task, scan this skill list. If a skill's description \
     matches the task, you MUST call load_skill(\"<skill-name>\") FIRST and follow its \
     SKILL.md instructions instead of writing your own code from scratch. These skills are \
     shared with the orchestrator and other agents — using them keeps the team consistent.";

/// Build the `=== INSTALLED SKILLS ===` block that is injected into sub-agent
/// and realtime-agent prompts so they can discover available skills without
/// having to call `list_skills` first.  Mirrors TS `buildEnabledSkillsBlock`.
async fn build_enabled_skills_block(persona: Option<&str>) -> String {
    let data = crate::server::data::data_dir();
    let skills_path = data.join("skills.json");
    let registry: Vec<Value> = match tokio::fs::read_to_string(&skills_path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => vec![],
    };

    let enabled: Vec<&Value> = registry
        .iter()
        .filter(|s| s.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true))
        .collect();

    // Also scan data/skills/ directory for custom skills with SKILL.md
    let skills_dir = data.join("skills");
    let mut custom_skills: Vec<(String, String, Vec<String>)> = Vec::new(); // (name, description, files)
    if let Ok(mut entries) = tokio::fs::read_dir(&skills_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                let skill_md = entry.path().join("SKILL.md");
                if tokio::fs::metadata(&skill_md).await.is_ok() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Don't double-list skills already in registry
                    let already = enabled.iter().any(|s| {
                        let sn = s.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        sn == name || slugify_skill_name(sn) == slugify_skill_name(&name)
                    });
                    if already { continue; }
                    // Read description from SKILL.md frontmatter
                    let desc = if let Ok(content) = tokio::fs::read_to_string(&skill_md).await {
                        content.lines()
                            .find(|l| l.starts_with("description:"))
                            .map(|l| l.trim_start_matches("description:").trim().trim_matches('"').to_string())
                            .unwrap_or_default()
                    } else { String::new() };
                    // List supporting files
                    let mut files = Vec::new();
                    if let Ok(mut fentries) = tokio::fs::read_dir(entry.path()).await {
                        while let Ok(Some(f)) = fentries.next_entry().await {
                            let fname = f.file_name().to_string_lossy().to_string();
                            if fname != "SKILL.md" && !fname.starts_with('.') && fname != "__MACOSX" {
                                files.push(fname);
                            }
                        }
                    }
                    custom_skills.push((name, desc, files));
                }
            }
        }
    }

    if enabled.is_empty() && custom_skills.is_empty() {
        return String::new();
    }

    let lead = persona.unwrap_or(
        "IMPORTANT: BEFORE answering any user request, scan the skill list below. \
         If a skill's description matches the user's task, you MUST load and use that \
         skill FIRST by calling load_skill(\"<skill-name>\"), then follow its SKILL.md \
         instructions. Do NOT write your own code from scratch when a matching skill exists. \
         Skills contain tested implementations and supporting files (like Python engines) that should be used."
    );

    let mut block = format!("\n\n=== INSTALLED SKILLS ===\n{}", lead);

    if !custom_skills.is_empty() {
        block.push_str("\n\nCustom skills (priority — always prefer these):");
        for (name, desc, files) in &custom_skills {
            let files_str = if files.is_empty() { String::new() } else { format!(" [files: {}]", files.join(", ")) };
            if desc.is_empty() {
                block.push_str(&format!("\n  - \"{}\"{}", name, files_str));
            } else {
                block.push_str(&format!("\n  - \"{}\": {}{}", name, desc, files_str));
            }
        }
    }

    if !enabled.is_empty() {
        block.push_str("\n\nRegistered skills:");
        for s in &enabled {
            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let desc = s.get("description").and_then(|d| d.as_str()).unwrap_or("");
            let source = s.get("source").and_then(|n| n.as_str()).unwrap_or("unknown");
            // Check if this skill has files on disk
            let has_files = resolve_skill_dir(name, Some(s)).is_some();
            let files_flag = if has_files { " [has SKILL.md]" } else { "" };
            if desc.is_empty() {
                block.push_str(&format!("\n  - {} ({}){}", name, source, files_flag));
            } else {
                let short_desc = if desc.len() > 150 { &desc[..150] } else { desc };
                block.push_str(&format!("\n  - {} ({}) — {}{}", name, source, short_desc, files_flag));
            }
        }
    }

    block.push_str("\n\nSkill usage workflow: 1) call load_skill(\"<name>\") to read SKILL.md \
        and see supporting files, 2) if the skill has supporting .py files, use read_file to \
        load them, 3) use run_python or run_shell to execute following the skill instructions.");

    block
}

/// Public wrapper for chat.rs to inject skills into the main agent prompt.
pub async fn build_enabled_skills_block_pub() -> String {
    build_enabled_skills_block(None).await
}

/// Build the system prompt for a realtime agent — mode-aware (tiger_cowork clone).
fn build_realtime_agent_prompt(agent_def: &Value, system_config: &Value) -> String {
    let agent_id = agent_def["id"].as_str().unwrap_or("agent");
    let name = agent_def["name"].as_str().unwrap_or("Agent");
    let role = agent_def["role"].as_str().unwrap_or("worker");
    let persona = agent_def.get("persona").and_then(|v| v.as_str()).unwrap_or("");
    let responsibilities = agent_def
        .get("responsibilities")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter()
            .filter_map(|v| v.as_str())
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n"))
        .unwrap_or_default();

    let orch_mode = system_config["system"]["orchestration_mode"]
        .as_str().unwrap_or("hierarchical");

    let all_agents = all_non_human_ids(system_config);
    let others: Vec<&str> = all_agents.iter()
        .filter(|id| id.as_str() != agent_id)
        .map(|s| s.as_str()).collect();
    let downstream = get_downstream(agent_id, system_config);

    // Base prompt
    let mut prompt = format!(
        "You are {name} (ID: {agent_id}) in a REALTIME multi-agent system.\n\
        {persona}\n\
        Your responsibilities:\n{responsibilities}\n"
    );

    // Mode-specific instructions
    match orch_mode {
        "hierarchical" => {
            if role == "orchestrator" {
                prompt += &format!(
                    "\n\nHIERARCHICAL MODE (ORCHESTRATOR): You control the team and coordinate all work.\n\
                    Your downstream agents: [{}]\n\
                    DELEGATION: Use send_task({{to: \"<agentId>\", task: \"...\"}}) then wait_result({{from: \"<agentId>\"}}).\n\
                    Send tasks to MULTIPLE agents in a SINGLE response for parallel execution.\n\
                    Do NOT do research or analysis yourself — delegate everything to your workers.\n\
                    Synthesize agent results into a comprehensive final response.",
                    downstream.join(", ")
                );
            } else {
                prompt += "\n\nHIERARCHICAL MODE (WORKER): You receive tasks from the orchestrator.\n\
                    Complete each task using your available tools, then provide your result as a clear text response.\n\
                    Do NOT delegate — execute the task yourself.";
                if !downstream.is_empty() {
                    prompt += &format!(
                        "\nException: You can delegate sub-tasks to: [{}]",
                        downstream.join(", ")
                    );
                }
            }
        }
        "hybrid" => {
            let mesh_ids = mesh_agent_ids(system_config);
            if role == "orchestrator" {
                prompt += &format!(
                    "\n\nHYBRID MODE (ORCHESTRATOR): You control the team and coordinate all work.\n\
                    Your connected agents: [{}]\n\
                    Mesh-enabled agents (can collaborate freely): [{}]\n\
                    You are responsible for:\n\
                      1. Delegating tasks to your connected agents via send_task\n\
                      2. Monitoring mesh agents' progress via check_agents\n\
                      3. Collecting results via wait_result and synthesizing the final output\n\
                      4. Ensuring work completes — if mesh agents loop or stall, reassign the task",
                    downstream.join(", "),
                    mesh_ids.join(", ")
                );
            } else if agent_has_mesh(agent_def) {
                prompt += &format!(
                    "\n\nHYBRID MODE (MESH WORKER): You can collaborate freely with other mesh agents.\n\
                    Mesh peers: [{}]\n\
                    Use send_task/wait_result to delegate sub-tasks to any mesh peer.\n\
                    Complete your assigned tasks using available tools.",
                    mesh_ids.iter().filter(|id| id.as_str() != agent_id).cloned().collect::<Vec<_>>().join(", ")
                );
            } else {
                prompt += "\n\nHYBRID MODE (WORKER): You receive tasks from the orchestrator.\n\
                    Complete each task using your available tools.";
            }
        }
        "mesh" => {
            prompt += &format!(
                "\n\nMESH MODE: All agents are peer collaborators — no hierarchy.\n\
                You can send tasks to ANY agent: [{}]\n\
                Use send_task({{to: \"<agentId>\", task: \"...\"}}) then wait_result({{from: \"<agentId>\"}}).\n\
                Send tasks to MULTIPLE agents in a SINGLE response for parallel execution.\n\
                Collaborate dynamically. Do NOT use proto_tcp_send or proto_bus_publish for tasks — use send_task.",
                others.join(", ")
            );
        }
        "pipeline" => {
            if downstream.is_empty() {
                prompt += "\n\nPIPELINE MODE (FINAL STAGE): You are the last agent in the pipeline.\n\
                    Process the input you receive and provide your final result.\n\
                    Synthesize all upstream work into a comprehensive response.";
            } else {
                prompt += &format!(
                    "\n\nPIPELINE MODE: You are one stage in a sequential pipeline.\n\
                    After completing your work, you MUST forward your result to the next agent: [{}]\n\
                    Use send_task({{to: \"{}\", task: \"<your result + instructions>\"}}) then wait_result.\n\
                    Pass along ALL relevant context and data to the next stage.",
                    downstream.join(", "),
                    downstream.first().unwrap_or(&String::new())
                );
            }
        }
        "p2p" | "p2p_orchestrator" => {
            if role == "orchestrator" {
                prompt += &format!(
                    "\n\nP2P ORCHESTRATOR MODE: You can delegate tasks in two ways:\n\
                    1. DIRECT: send_task to connected agents: [{}]\n\
                    2. BIDDING: bb_propose → bb_read (check bids) → bb_award → send_task to winner\n\
                    After bb_award, you MUST send_task to the winner — award alone does NOT send.\n\
                    Use check_agents to monitor progress.",
                    downstream.join(", ")
                );
            } else {
                prompt += &format!(
                    "\n\nPEER-TO-PEER SWARM MODE: You are an autonomous peer agent in a flat P2P swarm.\n\
                    No agent holds persistent authority. Peer agents: [{}]\n\
                    COORDINATION PROTOCOL (Contract Net):\n\
                      1. PROPOSE: Use bb_propose to post work on the blackboard\n\
                      2. BID: When you see open tasks via bb_read, use bb_bid with your confidence score (0-1)\n\
                      3. AWARD: The proposer calls bb_award — highest-confidence bidder wins\n\
                      4. SEND: After awarding, proposer MUST send_task to winner\n\
                      5. EXECUTE: The winning agent executes the task\n\
                      6. COMPLETE: Report results via bb_complete\n\
                    RULES: Only bid on tasks matching your expertise. Yield to higher-confidence peers.\n\
                    Avoid livelock — if negotiating too long, accept current best bid.",
                    others.join(", ")
                );
            }
        }
        _ => {
            // Fallback: generic realtime agent
            prompt += "\n\nComplete tasks using your available tools. Provide clear text responses.";
            if !downstream.is_empty() {
                prompt += &format!(
                    "\nYou can delegate sub-tasks to: [{}]",
                    downstream.join(", ")
                );
            }
        }
    }

    prompt += "\n\nERROR RECOVERY: If a tool fails, analyze the error, fix it, and retry. Try a different approach after two failures.";
    prompt
}

/// The LLM loop for a realtime agent — waits for tasks, runs tools, publishes results.
async fn realtime_agent_loop(
    agent_def: Value,
    session_id: String,
    system_config: Value,
    api_key: String,
    api_url: String,
    model: String,
    sandbox_dir: String,
    mut task_rx: tokio::sync::mpsc::Receiver<AgentTask>,
    results: Arc<TokioMutex<HashMap<String, AgentResult>>>,
    result_notify: Arc<Notify>,
    status: Arc<TokioMutex<String>>,
    mut abort_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let agent_id = agent_def["id"].as_str().unwrap_or("agent").to_string();
    let mut system_prompt = build_realtime_agent_prompt(&agent_def, &system_config);

    // Advertise enabled skills (incl. auto-generated) — same block the
    // orchestrator sees, so realtime agents can discover newly approved skills.
    match build_enabled_skills_block(Some(SUBAGENT_SKILLS_PERSONA)).await {
        block if !block.is_empty() => system_prompt.push_str(&block),
        _ => {}
    }

    info!("[Realtime] Agent {} loop started", agent_id);

    loop {
        // Wait for a task or shutdown signal
        let task = tokio::select! {
            t = task_rx.recv() => match t {
                Some(t) => t,
                None => {
                    info!("[Realtime] Agent {} channel closed — exiting", agent_id);
                    break;
                }
            },
            _ = abort_rx.recv() => {
                info!("[Realtime] Agent {} aborted", agent_id);
                break;
            }
        };

        *status.lock().await = "working".to_string();
        info!("[Realtime] Agent {} received task from {}: {:.80}", agent_id, task.from, task.task);

        // ── P2P bid request handling ──────────────────────────────────────
        // If this is a bid request (from bb_propose in P2P mode), evaluate
        // with a constrained tool set (bb_read + bb_bid only) and continue.
        if task.from.starts_with("bid_request:") {
            info!("[Realtime] Agent {} evaluating bid request: {:.80}", agent_id, task.task);
            let bid_messages = vec![json!({"role": "user", "content": task.task})];
            let bid_system = format!(
                "{}\n\nYou are evaluating a bid request. Read the task details, \
                 then decide whether to bid using bb_bid (with your confidence 0-1 and reasoning) \
                 or simply respond with text if you choose not to bid.",
                system_prompt
            );
            let bid_sub = SubAgentConfig {
                enabled: false,
                session_id: session_id.clone(),
                agent_id: agent_id.clone(),
                ..SubAgentConfig::default()
            };
            let bid_log_tx = subagent_log_tx().clone();
            let bid_log_sid = session_id.clone();
            let bid_log_aid = agent_id.clone();
            let bid_progress_sid = session_id.clone();
            let bid_on_update = move |update: ToolUpdate| {
                let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                let line = match &update {
                    ToolUpdate::ToolCall { name, args } => {
                        log_orchestrator_tool_call(&bid_progress_sid, name, args);
                        let a = serde_json::to_string(args).unwrap_or_default();
                        let a_short = if a.len() > 300 { format!("{}...", &a[..300]) } else { a };
                        format!("[{}] [{}] BID TOOL CALL: {}\n  args: {}", ts, bid_log_aid, name, a_short)
                    }
                    ToolUpdate::ToolResult { name, result } => {
                        let r = serde_json::to_string(result).unwrap_or_default();
                        let r_short = if r.len() > 500 { format!("{}...", &r[..500]) } else { r };
                        format!("[{}] [{}] BID TOOL RESULT: {}\n  {}", ts, bid_log_aid, name, r_short)
                    }
                    ToolUpdate::TextChunk(t) => {
                        format!("[{}] [{}] BID TEXT: {}", ts, bid_log_aid, if t.len() > 100 { &t[..100] } else { t })
                    }
                    ToolUpdate::Error(e) => format!("[{}] [{}] BID ERROR: {}", ts, bid_log_aid, e),
                    ToolUpdate::ApprovalRequired { name, .. } => format!("[{}] [{}] BID APPROVAL: {}", ts, bid_log_aid, name),
                };
                let _ = bid_log_tx.send((bid_log_sid.clone(), bid_log_aid.clone(), line));
            };
            // Use the standard call_with_tools (non-realtime, no delegation) —
            // the dispatcher already routes bb_bid / bb_read to the correct handlers.
            let _ = call_with_tools(
                &api_key, &api_url, &model, bid_messages,
                Some(bid_system), &sandbox_dir, bid_on_update, bid_sub,
            ).await;
            *status.lock().await = "idle".to_string();
            continue;
        }
        // ── End bid request handling ──────────────────────────────────────

        // Build messages for this agent's LLM call
        let user_content = if let Some(ctx) = &task.context {
            format!("Task from {}: {}\n\nContext: {}", task.from, task.task, ctx)
        } else {
            format!("Task from {}: {}", task.from, task.task)
        };

        let messages = vec![json!({"role": "user", "content": user_content})];

        // Sub-agent config: who gets send_task/wait_result depends on orchestration mode
        let role = agent_def["role"].as_str().unwrap_or("worker");
        let orch_mode = system_config["system"]["orchestration_mode"]
            .as_str().unwrap_or("hierarchical");
        let is_orchestrator = role == "orchestrator";
        let has_mesh = agent_has_mesh(&agent_def);
        let downstream = get_downstream(&agent_id, &system_config);

        // Determine if this agent can delegate (gets send_task/wait_result)
        let can_delegate = match orch_mode {
            "hierarchical" => is_orchestrator || !downstream.is_empty(),
            "hybrid" => is_orchestrator || has_mesh || !downstream.is_empty(),
            "mesh" => true,           // all agents can send to any other
            "pipeline" => !downstream.is_empty(), // only if has next stage
            "p2p" | "p2p_orchestrator" => true, // all peers can delegate
            _ => is_orchestrator || !downstream.is_empty(),
        };

        // Determine which agents this agent can reach
        let reachable_ids: Vec<String> = match orch_mode {
            "mesh" | "p2p" | "p2p_orchestrator" => {
                // Can reach ANY non-human agent
                all_non_human_ids(&system_config).into_iter()
                    .filter(|id| id != &agent_id)
                    .collect()
            }
            "hybrid" if has_mesh => {
                // Mesh agents can reach any other agent
                all_non_human_ids(&system_config).into_iter()
                    .filter(|id| id != &agent_id)
                    .collect()
            }
            _ => {
                // Hierarchical/pipeline/hybrid non-mesh: only downstream
                downstream.clone()
            }
        };

        let sub_agent = SubAgentConfig {
            enabled: can_delegate,
            mode: if can_delegate { "manual".to_string() } else { String::new() },
            agent_ids: reachable_ids,
            session_id: session_id.clone(),
            agent_id: agent_id.clone(),
            agent_role: role.to_string(),
            ..SubAgentConfig::default()
        };

        let result_arc = results.clone();
        let notify_arc = result_notify.clone();
        let _aid = agent_id.clone();
        let status_arc = status.clone();

        let log_tx = subagent_log_tx().clone();
        let fwd_log_tx = log_tx.clone(); // for pipeline auto-forward logging
        let log_sid = session_id.clone();
        let log_aid = agent_id.clone();
        let progress_sid = session_id.clone(); // for activity log
        let on_update_cb = move |update: ToolUpdate| {
            let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
            let line = match &update {
                ToolUpdate::ToolCall { name, args } => {
                    // Mirror orchestrator tool calls into the Activity panel
                    // (matches TS appendSessionProgress in socket.ts onToolCall)
                    log_orchestrator_tool_call(&progress_sid, name, args);

                    let args_str = serde_json::to_string(args).unwrap_or_default();
                    let args_short = if args_str.len() > 300 { format!("{}...", &args_str[..300]) } else { args_str };
                    format!("[{}] [{}] TOOL CALL: {}\n  args: {}", ts, log_aid, name, args_short)
                }
                ToolUpdate::ToolResult { name, result } => {
                    let r_str = serde_json::to_string(result).unwrap_or_default();
                    let r_short = if r_str.len() > 500 { format!("{}...", &r_str[..500]) } else { r_str };
                    format!("[{}] [{}] TOOL RESULT: {}\n  {}", ts, log_aid, name, r_short)
                }
                ToolUpdate::TextChunk(t) => {
                    if t.len() > 100 {
                        format!("[{}] [{}] TEXT: {}...", ts, log_aid, &t[..100])
                    } else {
                        format!("[{}] [{}] TEXT: {}", ts, log_aid, t)
                    }
                }
                ToolUpdate::Error(e) => {
                    format!("[{}] [{}] ERROR: {}", ts, log_aid, e)
                }
                ToolUpdate::ApprovalRequired { name, .. } => {
                    format!("[{}] [{}] APPROVAL REQUIRED: {}", ts, log_aid, name)
                }
            };
            let _ = log_tx.send((log_sid.clone(), log_aid.clone(), line));
        };
        // Check agent type: "remote", "cli", or default "llm"
        let agent_type = agent_def.get("type").and_then(|v| v.as_str()).unwrap_or("llm");

        let loop_result = match agent_type {
            "remote" => {
                // Delegate to a remote TigrimOS instance
                let remote_url = agent_def.get("remote_url")
                    .or_else(|| agent_def.get("url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let remote_token = agent_def.get("remote_token")
                    .or_else(|| agent_def.get("token"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if remote_url.is_empty() {
                    ToolLoopResult {
                        content: format!("Remote agent '{}' has no URL configured", agent_id),
                        tool_results: vec![],
                        files: vec![],
                    }
                } else {
                    info!("[Realtime] Agent {} delegating to remote: {}", agent_id, remote_url);
                    let result = exec_remote_task(&json!({
                        "instance": json!({"url": remote_url, "token": remote_token}).to_string(),
                        "task": user_content,
                    })).await;
                    ToolLoopResult {
                        content: result["result"].as_str().unwrap_or(
                            result["error"].as_str().unwrap_or("Remote task completed")
                        ).to_string(),
                        tool_results: vec![],
                        files: vec![],
                    }
                }
            }
            "cli" => {
                // Route to a CLI agent (claude_code or codex)
                let cli_provider = agent_def.get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("claude_code");

                info!("[Realtime] Agent {} running via CLI provider: {}", agent_id, cli_provider);
                let cli_result = match cli_provider {
                    "codex" => {
                        exec_run_shell(&json!({
                            "command": format!("echo '{}' | codex --quiet", user_content.replace('\'', "'\\''")),
                        }), &sandbox_dir).await
                    }
                    _ => {
                        // Default to claude_code CLI
                        exec_run_shell(&json!({
                            "command": format!("echo '{}' | claude --print", user_content.replace('\'', "'\\''")),
                        }), &sandbox_dir).await
                    }
                };
                ToolLoopResult {
                    content: cli_result["stdout"].as_str()
                        .or(cli_result["output"].as_str())
                        .unwrap_or("CLI agent completed").to_string(),
                    tool_results: vec![],
                    files: vec![],
                }
            }
            _ => {
                // Default LLM agent (existing code)
                if can_delegate {
                    call_with_tools_realtime(
                        &api_key, &api_url, &model, messages,
                        Some(system_prompt.clone()), &sandbox_dir, on_update_cb, sub_agent,
                    ).await
                } else {
                    call_with_tools(
                        &api_key, &api_url, &model, messages,
                        Some(system_prompt.clone()), &sandbox_dir, on_update_cb, sub_agent,
                    ).await
                }
            }
        };

        let agent_result = AgentResult {
            result: if loop_result.content.is_empty() {
                "Task completed.".to_string()
            } else {
                loop_result.content.clone()
            },
            output_files: loop_result.files.clone(),
            ok: true,
            error: None,
        };

        info!("[Realtime] Agent {} finished. Result length: {}", agent_id, agent_result.result.len());

        // Pipeline auto-forwarding: if this agent has a downstream target in pipeline mode,
        // automatically send_task to the next agent in the chain.
        let orch_mode = system_config["system"]["orchestration_mode"]
            .as_str().unwrap_or("hierarchical");
        if orch_mode == "pipeline" {
            let downstream = get_downstream(&agent_id, &system_config);
            if let Some(next_agent) = downstream.first() {
                info!("[Realtime] Pipeline auto-forward: {} → {}", agent_id, next_agent);
                let forward_task = format!(
                    "Previous pipeline stage ({}) produced the following result. Continue processing:\n\n{}",
                    agent_id, loop_result.content
                );
                let forward_args = serde_json::json!({
                    "to": next_agent,
                    "task": forward_task,
                });
                let fwd_result = exec_send_task_from(&forward_args, &session_id, &agent_id).await;
                let fwd_ok = fwd_result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                if fwd_ok {
                    info!("[Realtime] Pipeline auto-forward {} → {} succeeded", agent_id, next_agent);
                    let _ = fwd_log_tx.send((
                        session_id.clone(), agent_id.clone(),
                        format!("[{}] [{}] PIPELINE FORWARD → {}", chrono::Utc::now().format("%H:%M:%S"), agent_id, next_agent),
                    ));
                } else {
                    warn!("[Realtime] Pipeline auto-forward {} → {} failed: {:?}", agent_id, next_agent, fwd_result);
                }
            }
        }

        // Publish result — consumed by wait_result
        {
            let mut map = result_arc.lock().await;
            map.insert(agent_id.clone(), agent_result);
        }
        notify_arc.notify_waiters();

        *status_arc.lock().await = "idle".to_string();
    }
}

/// Tool: send_task — sends a task to a realtime agent with mode-aware access control
pub async fn exec_send_task(args: &Value, session_id: &str) -> Value {
    exec_send_task_from(args, session_id, "main").await
}

/// send_task with caller ID for access control
pub async fn exec_send_task_from(args: &Value, session_id: &str, caller_id: &str) -> Value {
    let to = match args.get("to").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return json!({"ok": false, "error": "Missing 'to' parameter"}),
    };
    let task = match args.get("task").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return json!({"ok": false, "error": "Missing 'task' parameter"}),
    };
    let context = args.get("context").and_then(|v| v.as_str()).map(|s| s.to_string());

    let map = realtime_sessions().lock().await;
    let session_arc = match map.get(session_id) {
        Some(s) => s.clone(),
        None => return json!({"ok": false, "error": "No realtime session active. Ensure sub-agent mode is 'realtime'."}),
    };
    let session = session_arc.lock().await;

    // Access control based on orchestration mode
    let orch_mode = session.system_config["system"]["orchestration_mode"]
        .as_str().unwrap_or("hierarchical");

    let handle = match session.agents.get(&to) {
        Some(h) => h,
        None => {
            let available: Vec<&str> = session.agents.keys().map(|s| s.as_str()).collect();
            return json!({"ok": false, "error": format!("Agent '{}' not found. Available: {}", to, available.join(", "))});
        }
    };

    // When "main" sends tasks in hierarchical/hybrid mode, enforce orchestrator-first routing.
    // The main LLM must delegate through the orchestrator, not directly to workers.
    if caller_id == "main" && matches!(orch_mode, "hierarchical" | "hybrid") {
        // Find if there's an orchestrator agent in the session
        let has_orchestrator = session.agents.iter().any(|(_, h)| {
            h.agent_def.get("role").and_then(|r| r.as_str()) == Some("orchestrator")
        });
        if has_orchestrator {
            let target_role = handle.agent_def["role"].as_str().unwrap_or("");
            if target_role != "orchestrator" {
                // Find the orchestrator's ID for the error message
                let orch_id = session.agents.iter()
                    .find(|(_, h)| h.agent_def.get("role").and_then(|r| r.as_str()) == Some("orchestrator"))
                    .map(|(id, _)| id.as_str())
                    .unwrap_or("orchestrator");
                return json!({
                    "ok": false,
                    "error": format!("In {} mode, tasks must go through the orchestrator '{}' first. Send your task to the orchestrator, who will delegate to workers.", orch_mode, orch_id)
                });
            }
        }
    }

    // Hierarchical/pipeline: enforce connection-based access control
    if matches!(orch_mode, "hierarchical" | "pipeline") && caller_id != "main" {
        let caller_def = session.system_config["agents"].as_array()
            .and_then(|arr| arr.iter().find(|a| a["id"].as_str() == Some(caller_id)));

        // Block non-orchestrators from sending to orchestrator (circular delegation)
        let target_role = handle.agent_def["role"].as_str().unwrap_or("");
        if target_role == "orchestrator" && caller_def.map(|d| d["role"].as_str() != Some("orchestrator")).unwrap_or(true) {
            return json!({"ok": false, "error": format!("Agent '{}' cannot send tasks to orchestrator (circular delegation)", caller_id)});
        }

        // Check connections
        let downstream = get_downstream(caller_id, &session.system_config);
        if !downstream.is_empty() && !downstream.contains(&to) {
            return json!({"ok": false, "error": format!("Agent '{}' is not connected to '{}'. Connected to: {}", caller_id, to, downstream.join(", "))});
        }
    }
    // hybrid: mesh agents bypass, others check connections
    else if orch_mode == "hybrid" && caller_id != "main" {
        let caller_def = session.system_config["agents"].as_array()
            .and_then(|arr| arr.iter().find(|a| a["id"].as_str() == Some(caller_id)));

        // Block non-orchestrators from sending to orchestrator (circular delegation)
        let target_role = handle.agent_def["role"].as_str().unwrap_or("");
        if target_role == "orchestrator" {
            if caller_def.map(|d| d["role"].as_str() != Some("orchestrator")).unwrap_or(true) {
                return json!({"ok": false, "error": format!("Agent '{}' cannot send tasks to orchestrator (circular delegation)", caller_id)});
            }
        }

        let caller_has_mesh = caller_def.map(|d| agent_has_mesh(d)).unwrap_or(false);
        if !caller_has_mesh {
            let downstream = get_downstream(caller_id, &session.system_config);
            if !downstream.is_empty() && !downstream.contains(&to) {
                return json!({"ok": false, "error": format!("Agent '{}' is not connected to '{}'", caller_id, to)});
            }
        }
    }
    // p2p_orchestrator: bidder-only agents must be awarded on blackboard first
    else if orch_mode == "p2p_orchestrator" && caller_id != "main" {
        let caller_def = session.system_config["agents"].as_array()
            .and_then(|arr| arr.iter().find(|a| a["id"].as_str() == Some(caller_id)));
        let is_caller_orchestrator = caller_def
            .map(|d| d["role"].as_str() == Some("orchestrator"))
            .unwrap_or(false);

        if is_caller_orchestrator {
            // Orchestrator can send to direct agents (in connections/workflow) freely.
            // But bidder-only agents (not in connections) require blackboard award first.
            let downstream = get_downstream(caller_id, &session.system_config);
            if !downstream.contains(&to) {
                // Check if target was awarded on blackboard
                let was_awarded = protocols::blackboard_check_awarded(session_id, &to).await;
                if !was_awarded {
                    return json!({
                        "ok": false,
                        "error": format!(
                            "Agent '{}' is a bidder-only agent — not in your direct connections [{}]. \
                             You must use blackboard bidding first: bb_propose → wait for bids → bb_award, \
                             then send_task to the awarded winner.",
                            to, downstream.join(", ")
                        )
                    });
                }
            }
        }
    }
    // mesh, p2p: no access control — any agent can send to any

    let agent_task = AgentTask { from: caller_id.to_string(), task: task.clone(), context };
    match handle.task_tx.send(agent_task).await {
        Ok(_) => json!({
            "ok": true,
            "agentId": to,
            "agentName": handle.agent_def["name"].as_str().unwrap_or(&to),
            "sent": true,
            "note": format!("Task sent to {}. Use wait_result({{\"from\": \"{}\"}}) to collect the result.", to, to)
        }),
        Err(e) => json!({"ok": false, "error": format!("Failed to send task: {e}")}),
    }
}

/// Tool: wait_result — blocks until the target agent publishes its result
pub async fn exec_wait_result(args: &Value, session_id: &str) -> Value {
    let from = match args.get("from").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return json!({"ok": false, "error": "Missing 'from' parameter"}),
    };
    let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(120);

    let (results, result_notify) = {
        let map = realtime_sessions().lock().await;
        match map.get(session_id) {
            Some(s) => {
                let session = s.lock().await;
                // Verify agent exists
                if !session.agents.contains_key(&from) {
                    let available: Vec<&str> = session.agents.keys().map(|s| s.as_str()).collect();
                    return json!({"ok": false, "error": format!("Agent '{}' not found. Available: {}", from, available.join(", "))});
                }
                (session.results.clone(), session.result_notify.clone())
            }
            None => return json!({"ok": false, "error": "No realtime session active."}),
        }
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        // Check if result is already available
        {
            let mut map = results.lock().await;
            if let Some(result) = map.remove(&from) {
                return json!({
                    "ok": result.ok,
                    "agentId": from,
                    "result": result.result,
                    "output_files": result.output_files,
                });
            }
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return json!({
                "ok": false,
                "error": format!("Timeout waiting for result from '{}'", from),
                "agentId": from,
                "hint": "The agent may still be working. Try wait_result again or check with check_agents."
            });
        }

        // Wait for any result notification or deadline
        tokio::select! {
            _ = result_notify.notified() => continue,
            _ = tokio::time::sleep(remaining) => {
                return json!({
                    "ok": false,
                    "error": format!("Timeout waiting for result from '{}'", from),
                    "agentId": from,
                });
            }
        }
    }
}

/// Tool: check_agents — list status of all agents in the realtime session
pub async fn exec_check_agents(session_id: &str) -> Value {
    let map = realtime_sessions().lock().await;
    match map.get(session_id) {
        Some(session_arc) => {
            let session = session_arc.lock().await;
            let mut agents = vec![];
            for (id, handle) in &session.agents {
                let status = handle.status.lock().await.clone();
                agents.push(json!({
                    "id": id,
                    "name": handle.agent_def["name"].as_str().unwrap_or(id),
                    "role": handle.agent_def["role"].as_str().unwrap_or("worker"),
                    "status": status,
                }));
            }
            json!({"ok": true, "agents": agents, "total": agents.len()})
        }
        None => json!({"ok": false, "error": "No realtime session active."}),
    }
}

/// Load agent system YAML and return parsed Value + list of agent IDs
pub fn load_agent_yaml(filename: &str) -> Option<(Value, Vec<String>)> {
    let dir = crate::server::data::data_dir().join("agents");
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
        json!({
            "type": "function",
            "function": {
                "name": "delete_file",
                "description": "Delete a file or directory from disk.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File or directory path to delete" }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_local_mounts",
                "description": "List locally mounted host directories accessible outside the sandbox.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_react",
                "description": "Execute React/JSX code. Compiled and rendered in the output panel. Recharts available as globals.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "code": { "type": "string", "description": "React JSX component code" },
                        "title": { "type": "string", "description": "Title for the output (optional)" }
                    },
                    "required": ["code"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "remote_task",
                "description": "Delegate a task to a TigrimOS instance on another machine.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "instance": { "type": "string", "description": "Remote instance name/id or inline JSON {url,token}" },
                        "task": { "type": "string", "description": "Task to send to the remote instance" },
                        "idle_timeout": { "type": "number", "description": "Idle timeout seconds (default 60)" },
                        "max_timeout": { "type": "number", "description": "Max wait seconds (default 1800)" }
                    },
                    "required": ["instance", "task"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "claude_code_agent",
                "description": "Run a task using Claude Code CLI (autonomous agent with Read/Edit/Bash tools). Requires 'claude' in PATH.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "Task for Claude Code to complete" },
                        "system_prompt": { "type": "string", "description": "Optional system context" },
                        "timeout": { "type": "number", "description": "Timeout seconds (default 300)" },
                        "max_turns": { "type": "number", "description": "Max tool turns (default 25)" },
                        "model": { "type": "string", "description": "Sub-model e.g. 'sonnet'" }
                    },
                    "required": ["task"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "gemini_cli_agent",
                "description": "Run a task using Gemini CLI (autonomous agent with code execution and tool use). Requires 'gemini' in PATH.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "Task for Gemini CLI to complete" },
                        "system_prompt": { "type": "string", "description": "Optional system context" },
                        "timeout": { "type": "number", "description": "Timeout seconds (default 300)" },
                        "model": { "type": "string", "description": "Model e.g. 'gemini-2.5-pro'" }
                    },
                    "required": ["task"]
                }
            }
        }),
        // Protocol: TCP
        json!({
            "type": "function",
            "function": {
                "name": "proto_tcp_send",
                "description": "Send a message to another agent via TCP point-to-point channel.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": { "type": "string", "description": "Target agent ID" },
                        "topic": { "type": "string", "description": "Message topic" },
                        "payload": { "description": "Message content" }
                    },
                    "required": ["to", "topic", "payload"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "proto_tcp_read",
                "description": "Read all messages from a TCP channel with another agent.",
                "parameters": {
                    "type": "object",
                    "properties": { "peer": { "type": "string", "description": "The other agent's ID" } },
                    "required": ["peer"]
                }
            }
        }),
        // Protocol: Bus
        json!({
            "type": "function",
            "function": {
                "name": "proto_bus_publish",
                "description": "Publish a message to the shared event bus.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "topic": { "type": "string", "description": "Topic to publish to" },
                        "payload": { "description": "Message content" }
                    },
                    "required": ["topic", "payload"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "proto_bus_history",
                "description": "Read the event bus message history, optionally filtered by topic.",
                "parameters": {
                    "type": "object",
                    "properties": { "topic": { "type": "string", "description": "Optional topic filter" } }
                }
            }
        }),
        // Protocol: Queue
        json!({
            "type": "function",
            "function": {
                "name": "proto_queue_send",
                "description": "Enqueue a message to another agent's FIFO queue.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": { "type": "string", "description": "Target agent ID" },
                        "topic": { "type": "string", "description": "Message topic" },
                        "payload": { "description": "Message content" }
                    },
                    "required": ["to", "topic", "payload"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "proto_queue_receive",
                "description": "Dequeue the next message from your queue.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "Sender agent ID to read from" },
                        "topic": { "type": "string", "description": "Optional topic filter" }
                    },
                    "required": ["from"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "proto_queue_peek",
                "description": "Peek at queue messages without consuming them.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "Sender agent ID" },
                        "topic": { "type": "string", "description": "Optional topic filter" },
                        "count": { "type": "number", "description": "Number to peek (default 5)" }
                    },
                    "required": ["from"]
                }
            }
        }),
        // Blackboard / P2P
        json!({
            "type": "function",
            "function": {
                "name": "bb_propose",
                "description": "Propose a task on the shared blackboard for peer agents to bid on (Contract Net Protocol).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "description": { "type": "string", "description": "Task description" },
                        "task_id": { "type": "string", "description": "Optional custom task ID" }
                    },
                    "required": ["description"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "bb_bid",
                "description": "Submit a bid for a blackboard task with a confidence score.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "Task ID to bid on" },
                        "confidence": { "type": "number", "description": "Confidence score 0-1" },
                        "cost": { "type": "number", "description": "Optional estimated cost" },
                        "reasoning": { "type": "string", "description": "Why you're suited for this task" }
                    },
                    "required": ["task_id", "confidence"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "bb_award",
                "description": "Award a blackboard task to the best bidder.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "Task ID to award" },
                        "award_to": { "type": "string", "description": "Specific agent ID (overrides scoring)" },
                        "orchestrator_scores": {
                            "type": "array",
                            "description": "Your scores for each bidder: [{agent_id, score 0-1, reason}]",
                            "items": { "type": "object" }
                        }
                    },
                    "required": ["task_id"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "bb_complete",
                "description": "Mark a blackboard task as completed with a result.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "Task ID to complete" },
                        "result": { "type": "string", "description": "Task result / output" }
                    },
                    "required": ["task_id", "result"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "bb_read",
                "description": "Read the shared blackboard — see all tasks, bids, and results.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "Optional: specific task ID" },
                        "status": { "type": "string", "description": "Optional: filter by status" }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "bb_log",
                "description": "Read the blackboard audit log of all proposals, bids, awards, and completions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "number", "description": "Max entries (default 50)" }
                    }
                }
            }
        }),
        // clawhub_search
        json!({
            "type": "function",
            "function": {
                "name": "clawhub_search",
                "description": "Search the ClawHub/OpenClaw skill marketplace.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "limit": { "type": "integer", "description": "Max results (default 10)" }
                    },
                    "required": ["query"]
                }
            }
        }),
        // clawhub_install
        json!({
            "type": "function",
            "function": {
                "name": "clawhub_install",
                "description": "Install a skill from ClawHub marketplace by slug.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "slug": { "type": "string", "description": "Skill slug to install" },
                        "force": { "type": "boolean", "description": "Force reinstall" }
                    },
                    "required": ["slug"]
                }
            }
        }),
        // openrouter_web_search
        json!({
            "type": "function",
            "function": {
                "name": "openrouter_web_search",
                "description": "Search the web using OpenRouter's Responses API with the web search plugin. Returns AI-summarized results with source citations.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" }
                    },
                    "required": ["query"]
                }
            }
        }),
    ]
}

/// Tool definitions with sub-agent tools added when enabled
/// Build tool definitions for a given sub-agent config and mode.
/// `session_activated` is true if a swarm/architecture has been created/selected this session.
#[allow(dead_code)]
pub fn tool_definitions_with_subagent(sub_agent: &SubAgentConfig, realtime: bool) -> Vec<Value> {
    tool_definitions_for_mode(sub_agent, realtime, false)
}

fn realtime_tools(agent_list: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "send_task",
                "description": format!(
                    "Send a task to a live realtime agent. Available agents: {}. Use this to delegate work, then call wait_result to get the response.",
                    agent_list
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "to": { "type": "string", "description": format!("ID of the target agent. Must be one of: {}", agent_list) },
                        "task": { "type": "string", "description": "Clear description of the task for the agent" },
                        "context": { "type": "string", "description": "Optional context or data to pass to the agent" }
                    },
                    "required": ["to", "task"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "wait_result",
                "description": "Wait for a result from an agent that was previously sent a task via send_task. Blocks until the agent finishes.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": format!("ID of the agent to wait for. Must be one of: {}", agent_list) },
                        "timeout": { "type": "integer", "description": "Optional timeout in seconds (default: 120)" }
                    },
                    "required": ["from"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "check_agents",
                "description": "Check the current status of all realtime agents (idle, working, etc.)",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        }),
    ]
}

fn create_architecture_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "create_architecture",
            "description": "Analyze the user's task and create an appropriate multi-agent architecture. Generates a YAML agent config, saves it, and boots all agents in realtime mode. Call this FIRST before doing any work.",
            "parameters": {
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "Description of the task/goal for the agent team" },
                    "architectureType": {
                        "type": "string",
                        "enum": ["hierarchical", "flat", "mesh", "hybrid", "pipeline", "p2p"],
                        "description": "Architecture type"
                    },
                    "agentCount": { "type": "string", "description": "Number of agents or 'auto'" }
                },
                "required": ["description"]
            }
        }
    })
}

fn select_swarm_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "select_swarm",
            "description": "Select the best agent swarm configuration for the current task. Review available swarms and pick the one whose description best matches the user's request.",
            "parameters": {
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "description": "The YAML filename to select (e.g. 'research_team.yaml')" },
                    "reason": { "type": "string", "description": "Brief explanation of why this swarm is the best fit" }
                },
                "required": ["filename"]
            }
        }
    })
}

fn spawn_subagent_tool(agent_list: &str) -> Value {
    json!({
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
                    "agent_id": { "type": "string", "description": format!("ID of the agent to spawn. Must be one of: {}", agent_list) },
                    "task": { "type": "string", "description": "Clear description of the task for the sub-agent" },
                    "context": { "type": "string", "description": "Optional context or data to pass to the sub-agent" }
                },
                "required": ["agent_id", "task"]
            }
        }
    })
}

/// Build tool list dynamically based on mode and whether a session has been activated.
pub fn tool_definitions_for_mode(sub_agent: &SubAgentConfig, realtime: bool, session_activated: bool) -> Vec<Value> {
    let mut tools = tool_definitions();
    if !sub_agent.enabled || sub_agent.depth >= 3 {
        return tools;
    }
    // Remove CLI agent tools from swarm modes — agents should use send_task/spawn_subagent,
    // not spawn external CLI processes that aren't part of the architecture.
    tools.retain(|t| {
        let name = t["function"]["name"].as_str().unwrap_or("");
        !matches!(name, "claude_code_agent" | "gemini_cli_agent")
    });
    // For fully_auto/auto_swarm, agent_ids might be empty before architecture is created
    // — that's OK, the mode logic handles it
    if sub_agent.agent_ids.is_empty()
        && !matches!(sub_agent.mode.as_str(), "fully_auto" | "auto_swarm")
    {
        return tools;
    }
    let agent_list = sub_agent.agent_ids.join(", ");
    let mode = sub_agent.mode.as_str();

    match mode {
        "fully_auto" => {
            if session_activated {
                // Architecture created, agents running — ONLY realtime tools
                // No base tools (web_search, read_file, etc.) — agents handle those.
                // Only keep write_file + run_python for formatting final output.
                let mut rt_tools = realtime_tools(&agent_list);
                rt_tools.push(create_architecture_tool()); // allow recreation
                // Add write_file and run_python for output formatting only
                for t in &tools {
                    let name = t["function"]["name"].as_str().unwrap_or("");
                    if matches!(name, "write_file" | "run_python") {
                        rt_tools.push(t.clone());
                    }
                }
                return rt_tools;
            } else {
                // No architecture yet — ONLY create_architecture (forces LLM to create first)
                return vec![create_architecture_tool()];
            }
        }
        "auto_swarm" => {
            if session_activated {
                // Swarm selected, agents running — use realtime tools
                tools.extend(realtime_tools(&agent_list));
                tools.push(select_swarm_tool()); // allow switching
            } else {
                // No swarm selected — ONLY select_swarm
                return vec![select_swarm_tool()];
            }
        }
        "manual" => {
            // Manual: user pre-selected YAML, agents boot immediately
            // Only give coordination tools — agents handle research/execution.
            // Keep write_file, run_python, read_file, list_files for output formatting.
            let mut rt_tools = realtime_tools(&agent_list);
            for t in &tools {
                let name = t["function"]["name"].as_str().unwrap_or("");
                if matches!(name, "write_file" | "run_python" | "read_file" | "list_files") {
                    rt_tools.push(t.clone());
                }
            }
            return rt_tools;
        }
        _ => {
            // "auto" mode: spawn_subagent (depth-limited)
            if realtime {
                tools.extend(realtime_tools(&agent_list));
            } else {
                tools.push(spawn_subagent_tool(&agent_list));
            }
            tools.push(create_architecture_tool());
            tools.push(select_swarm_tool());
        }
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
    let dir = crate::server::data::data_dir().join("agent_history").join(session_id);
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    let path = dir.join("spawn.jsonl");
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
    let sandbox = std::fs::canonicalize(sandbox_dir)
        .unwrap_or_else(|_| PathBuf::from(sandbox_dir).canonicalize().unwrap_or_else(|_| PathBuf::from(sandbox_dir)));

    let candidate = if PathBuf::from(path).is_absolute() {
        PathBuf::from(path)
    } else {
        sandbox.join(path)
    };

    // Resolve symlinks and ../ to get the real path, then check it's inside sandbox
    let resolved = candidate.canonicalize().unwrap_or(candidate.clone());
    if resolved.starts_with(&sandbox) {
        resolved
    } else {
        // Path escapes sandbox — force it inside sandbox as a relative name
        let filename = PathBuf::from(path)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "blocked".to_string());
        sandbox.join(filename)
    }
}

async fn exec_web_search(args: &Value) -> Value {
    let query = args["query"].as_str().unwrap_or("");
    let client = Client::new();
    let mut all_results: Vec<Value> = Vec::new();

    // Primary: DuckDuckGo Python library (same as original TigrimOS)
    // This returns actual web search results with titles, URLs, and snippets
    let safe_query = query.replace('\'', "\\'").replace('\\', "\\\\");
    let py_script = format!(
        r#"import json, sys
try:
    try:
        from ddgs import DDGS
        r = list(DDGS().text('{}', max_results=8))
    except ImportError:
        from duckduckgo_search import DDGS
        with DDGS() as ddgs:
            r = list(ddgs.text('{}', max_results=8))
    print(json.dumps(r))
except Exception as e:
    print(json.dumps([]))
    print(str(e), file=sys.stderr)
"#,
        safe_query, safe_query
    );

    let py_result = timeout(
        Duration::from_secs(30),
        python_command()
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

    // If Google Custom Search is configured, also try Google
    if !ddg_ok {
        let settings_path = crate::server::data::data_dir().join("settings.json");
        if let Ok(settings_str) = tokio::fs::read_to_string(&settings_path).await {
            if let Ok(settings) = serde_json::from_str::<Value>(&settings_str) {
                let engine = settings["webSearchEngine"].as_str().unwrap_or("");
                let api_key = settings["webSearchApiKey"].as_str().unwrap_or("");
                let cx = settings["googleSearchCx"].as_str().unwrap_or("");
                if engine == "google" && !api_key.is_empty() {
                    let google_url = format!(
                        "https://www.googleapis.com/customsearch/v1?key={}&cx={}&q={}",
                        api_key, cx, urlencoding::encode(query)
                    );
                    if let Ok(resp) = client.get(&google_url).send().await {
                        if let Ok(data) = resp.json::<Value>().await {
                            if let Some(items) = data["items"].as_array() {
                                for item in items.iter().take(5) {
                                    all_results.push(json!({
                                        "source": "google",
                                        "title": item["title"].as_str().unwrap_or(""),
                                        "url": item["link"].as_str().unwrap_or(""),
                                        "text": item["snippet"].as_str().unwrap_or("")
                                    }));
                                }
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

    // If still no web results (only Wikipedia), try OpenRouter web search as last resort
    let has_web_results = all_results.iter().any(|r| {
        let src = r["source"].as_str().unwrap_or("");
        src == "web" || src == "google"
    });
    if !has_web_results {
        let or_result = exec_openrouter_web_search(&json!({ "query": query })).await;
        if or_result["ok"].as_bool() == Some(true) {
            if let Some(text) = or_result["text"].as_str() {
                if !text.is_empty() {
                    // Add OpenRouter citations as results
                    if let Some(citations) = or_result["citations"].as_array() {
                        for cite in citations.iter().take(8) {
                            all_results.push(json!({
                                "source": "web",
                                "title": cite["title"].as_str().unwrap_or(""),
                                "url": cite["url"].as_str().unwrap_or(""),
                                "text": ""
                            }));
                        }
                    }
                    // Add summarized text as top result
                    all_results.insert(0, json!({
                        "source": "web_summary",
                        "title": "AI-Summarized Web Results",
                        "text": &text[..text.len().min(3000)],
                        "url": ""
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
    // But allow the app's own data/sandbox paths (e.g. ~/Library/Application Support/TigrimOS/)
    let app_data_path = crate::server::data::data_dir().display().to_string().to_lowercase();
    let app_support_path = app_data_path.replace("/data", "");
    let sensitive_dirs = ["/etc/", "/var/", "/usr/", "/System/", "/Library/",
                          "/Applications/", "/Users/*/.", "/private/"];
    for dir in &sensitive_dirs {
        if lower.contains(&dir.to_lowercase()) {
            // Allow /tmp/, the sandbox itself, and app's own data directory
            if !lower.contains("/tmp/")
                && !lower.contains(&app_data_path)
                && !lower.contains(&app_support_path)
            {
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

    // Try VM execution first if VM is running
    if is_vm_running().await {
        info!("[VM] Routing run_python to VM via SSH");
        // Escape single quotes in code for shell
        let escaped = code.replace('\'', "'\\''");
        let vm_cmd = format!("cd /app && python3 -c '{}'", escaped);
        match run_in_vm(&vm_cmd, 60).await {
            Ok((stdout, stderr, success)) => {
                return json!({
                    "ok": success,
                    "stdout": truncate(&stdout, MAX_CONTENT_LEN),
                    "stderr": truncate(&stderr, MAX_CONTENT_LEN),
                    "output_files": [],
                    "vm": true,
                });
            }
            Err(e) => {
                warn!("[VM] SSH execution failed, falling back to local: {}", e);
            }
        }
    }

    #[cfg(target_os = "macos")]
    let abs_sandbox = std::path::Path::new(sandbox_dir)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(sandbox_dir));

    // On macOS, try sandboxed execution first; on other platforms, execute directly.
    #[cfg(target_os = "macos")]
    let result = {
        // Primary: Apple container CLI (macOS containerization)
        let r = timeout(
            Duration::from_secs(60),
            Command::new("/usr/bin/container")
                .args([
                    "run", "--rm",
                    "-v", &format!("{}:/sandbox", abs_sandbox.display()),
                    "-w", "/sandbox",
                    "python:3.11-slim",
                    "python3", "-c", code,
                ])
                .output(),
        )
        .await;

        // Fallback 1: sandbox-exec (macOS legacy)
        let r = match &r {
            Ok(Ok(o)) if o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty() => r,
            _ => {
                warn!("[sandbox] container CLI not available, trying sandbox-exec");
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
                    abs_sandbox.display()
                );
                timeout(
                    Duration::from_secs(60),
                    Command::new("/usr/bin/sandbox-exec")
                        .arg("-p")
                        .arg(&sandbox_profile)
                        .arg(&find_python())
                        .arg("-c")
                        .arg(code)
                        .current_dir(sandbox_dir)
                        .output(),
                )
                .await
            }
        };

        // Fallback 2: direct execution
        match &r {
            Ok(Ok(_)) => r,
            _ => {
                warn!("[sandbox] sandbox-exec failed, falling back to direct execution");
                timeout(
                    Duration::from_secs(60),
                    python_command()
                        .arg("-c")
                        .arg(code)
                        .current_dir(sandbox_dir)
                        .output(),
                )
                .await
            }
        }
    };

    #[cfg(not(target_os = "macos"))]
    let result = timeout(
        Duration::from_secs(60),
        python_command()
            .arg("-c")
            .arg(code)
            .current_dir(sandbox_dir)
            .output(),
    )
    .await;

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

    // Try VM execution first if VM is running
    if is_vm_running().await {
        info!("[VM] Routing run_shell to VM via SSH");
        let vm_cmd = format!("cd /app && {}", command);
        match run_in_vm(&vm_cmd, 30).await {
            Ok((stdout, stderr, success)) => {
                return json!({
                    "ok": success,
                    "stdout": truncate(&stdout, MAX_CONTENT_LEN),
                    "stderr": truncate(&stderr, MAX_CONTENT_LEN),
                    "vm": true,
                });
            }
            Err(e) => {
                warn!("[VM] SSH execution failed, falling back to local: {}", e);
            }
        }
    }

    let cwd = args["cwd"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| sandbox_dir.to_string());

    #[cfg(target_os = "macos")]
    let abs_sandbox = std::path::Path::new(sandbox_dir)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(sandbox_dir));

    // On macOS, try sandboxed execution first; on other platforms, execute directly.
    #[cfg(target_os = "macos")]
    let result = {
        // Primary: Apple container CLI
        let r = timeout(
            Duration::from_secs(30),
            Command::new("/usr/bin/container")
                .args([
                    "run", "--rm",
                    "-v", &format!("{}:/sandbox", abs_sandbox.display()),
                    "-w", "/sandbox",
                    "alpine:latest",
                    "sh", "-c", command,
                ])
                .output(),
        )
        .await;

        // Fallback 1: sandbox-exec (macOS legacy)
        let r = match &r {
            Ok(Ok(o)) if o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty() => r,
            _ => {
                warn!("[sandbox] container CLI not available, trying sandbox-exec");
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
                    abs_sandbox.display()
                );
                timeout(
                    Duration::from_secs(30),
                    Command::new("/usr/bin/sandbox-exec")
                        .arg("-p")
                        .arg(&sandbox_profile)
                        .arg("/bin/sh")
                        .arg("-c")
                        .arg(command)
                        .current_dir(&cwd)
                        .output(),
                )
                .await
            }
        };

        // Fallback 2: direct execution
        match &r {
            Ok(Ok(_)) => r,
            _ => {
                warn!("[sandbox] sandbox-exec failed, falling back to direct execution");
                timeout(
                    Duration::from_secs(30),
                    shell_command()
                        .arg("-c")
                        .arg(command)
                        .current_dir(&cwd)
                        .output(),
                )
                .await
            }
        }
    };

    #[cfg(not(target_os = "macos"))]
    let result = {
        let mut cmd = shell_command();
        #[cfg(target_os = "windows")]
        cmd.arg("/c").arg(command);
        #[cfg(not(target_os = "windows"))]
        cmd.arg("-c").arg(command);
        timeout(Duration::from_secs(30), cmd.current_dir(&cwd).output()).await
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

    // Handle PDF files — extract text instead of reading as raw string
    if resolved.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("pdf")).unwrap_or(false) {
        let resolved_clone = resolved.clone();
        let result = tokio::task::spawn_blocking(move || {
            pdf_extract::extract_text(&resolved_clone)
        }).await;
        return match result {
            Ok(Ok(text)) => {
                compact::track_file_read(&resolved.display().to_string(), &text);
                json!({
                    "ok": true,
                    "content": truncate(&text, MAX_CONTENT_LEN),
                    "path": resolved.display().to_string(),
                    "format": "pdf",
                })
            }
            Ok(Err(e)) => json!({ "ok": false, "error": format!("Failed to extract PDF text: {e}") }),
            Err(e) => json!({ "ok": false, "error": format!("PDF extraction task failed: {e}") }),
        };
    }

    match fs::read_to_string(&resolved).await {
        Ok(content) => {
            // Track file read for post-compact context restoration
            compact::track_file_read(&resolved.display().to_string(), &content);
            json!({
                "ok": true,
                "content": truncate(&content, MAX_CONTENT_LEN),
                "path": resolved.display().to_string(),
            })
        }
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
    // Ensure sandbox directory exists
    let _ = fs::create_dir_all(sandbox_dir).await;

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

// ---------------------------------------------------------------------------
// Skill resolution helpers — mirrors TS slugifySkillName / skillCandidates / resolveSkillDir
// ---------------------------------------------------------------------------

fn slugify_skill_name(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.to_lowercase().trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn skill_candidates(skill_name: &str, registry_entry: Option<&Value>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut push = |v: &str| {
        if !v.is_empty() && v.len() < 200 && seen.insert(v.to_string()) {
            out.push(v.to_string());
        }
    };
    push(skill_name);
    if let Some(entry) = registry_entry {
        // Only use "script" as candidate if it looks like a filename (not content)
        if let Some(script) = entry.get("script").and_then(|s| s.as_str()) {
            if !script.contains('\n') && script.len() < 200 {
                push(script);
            }
        }
    }
    push(&slugify_skill_name(skill_name));
    out
}

fn skills_search_dirs() -> Vec<(std::path::PathBuf, &'static str)> {
    let data = crate::server::data::data_dir();
    vec![
        (data.join("skills"), "custom"),
        (std::path::PathBuf::from("Tiger_bot/skills"), "clawhub"),
        (std::path::PathBuf::from("skills"), "custom"),
    ]
}

fn read_skills_registry() -> Vec<Value> {
    let path = crate::server::data::data_dir().join("skills.json");
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => vec![],
    }
}

fn resolve_skill_dir(skill_name: &str, registry_entry: Option<&Value>) -> Option<(std::path::PathBuf, String)> {
    for cand in skill_candidates(skill_name, registry_entry) {
        for (dir, label) in skills_search_dirs() {
            let base = dir.join(&cand);
            if base.join("SKILL.md").exists() {
                return Some((base, label.to_string()));
            }
        }
    }
    None
}

async fn exec_list_skills(_args: &Value, _sandbox_dir: &str) -> Value {
    let registry = read_skills_registry();

    // Registered skills with hasFiles check
    let registered_skills: Vec<Value> = registry
        .iter()
        .map(|s| {
            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let source = s.get("source").and_then(|n| n.as_str()).unwrap_or("unknown");
            let enabled = s.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
            let resolved = resolve_skill_dir(name, Some(s));
            json!({
                "name": name,
                "source": source,
                "enabled": enabled,
                "hasFiles": resolved.is_some(),
            })
        })
        .collect();

    let skill_names: Vec<String> = registered_skills
        .iter()
        .filter(|s| s["enabled"].as_bool().unwrap_or(true))
        .filter_map(|s| s["name"].as_str().map(|n| n.to_string()))
        .collect();

    // Also check data/skills/ directory for SKILL.md files
    let mut dir_skills = vec![];
    let skills_scan_dir = crate::server::data::data_dir().join("skills");
    if let Ok(mut entries) = tokio::fs::read_dir(&skills_scan_dir).await {
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

    json!({
        "skills": skill_names,
        "dir_skills": dir_skills,
        "registered_skills": registered_skills,
        "hint": "Use load_skill with a skill name to see its SKILL.md and supporting files. Skills where hasFiles=false are registered but have no SKILL.md on disk and cannot be loaded."
    })
}

async fn exec_load_skill(args: &Value, _sandbox_dir: &str) -> Value {
    let skill_name = args.get("skill").and_then(|s| s.as_str()).unwrap_or("");
    if skill_name.is_empty() {
        return json!({"ok": false, "error": "Missing skill name"});
    }

    let registry = read_skills_registry();
    let registry_entry = registry.iter()
        .find(|s| s.get("name").and_then(|n| n.as_str()) == Some(skill_name))
        .or_else(|| registry.iter().find(|s| {
            s.get("name").and_then(|n| n.as_str())
                .map(|n| slugify_skill_name(n) == slugify_skill_name(skill_name))
                .unwrap_or(false)
        }));

    if let Some((skill_base_dir, source)) = resolve_skill_dir(skill_name, registry_entry) {
        let skill_file = skill_base_dir.join("SKILL.md");
        let base_dir_str = skill_base_dir.display().to_string();
        match tokio::fs::read_to_string(&skill_file).await {
            Ok(raw_content) => {
                let content = raw_content.replace("{baseDir}", &base_dir_str);
                let truncated = content.len() > 15000;
                let content = if truncated { content[..15000].to_string() } else { content };

                // Read _meta.json if present
                let meta = match tokio::fs::read_to_string(skill_base_dir.join("_meta.json")).await {
                    Ok(m) => serde_json::from_str::<Value>(&m).unwrap_or(json!({})),
                    Err(_) => json!({}),
                };

                // List supporting files
                let mut supporting_files = Vec::new();
                fn walk_skill_dir(d: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
                    if let Ok(entries) = std::fs::read_dir(d) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if name.starts_with('.') || name == "__MACOSX" { continue; }
                            let rel = if prefix.is_empty() { name.clone() } else { format!("{}/{}", prefix, name) };
                            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                                walk_skill_dir(&entry.path(), &rel, out);
                            } else if name != "SKILL.md" && name != "_meta.json" {
                                out.push(rel);
                            }
                        }
                    }
                }
                walk_skill_dir(&skill_base_dir, "", &mut supporting_files);

                return json!({
                    "ok": true,
                    "skill": skill_name,
                    "source": source,
                    "skillDir": base_dir_str,
                    "content": content,
                    "meta": meta,
                    "supportingFiles": supporting_files,
                    "truncated": truncated,
                });
            }
            Err(e) => return json!({"ok": false, "error": format!("Failed to read SKILL.md: {}", e)}),
        }
    }

    // Not found — try self-healing: if script field contains SKILL.md content, write it to disk
    if let Some(entry) = registry_entry {
        if let Some(script) = entry.get("script").and_then(|s| s.as_str()) {
            if script.contains('\n') && script.len() > 50 {
                // script field contains the actual SKILL.md content — save it to disk
                let slug = slugify_skill_name(skill_name);
                let skill_dir = crate::server::data::data_dir().join("skills").join(&slug);
                let _ = std::fs::create_dir_all(&skill_dir);
                let skill_file = skill_dir.join("SKILL.md");
                if std::fs::write(&skill_file, script).is_ok() {
                    info!("[Skills] Self-healed: wrote SKILL.md for {} to {}", skill_name, skill_file.display());
                    // Retry the load now that the file exists
                    let content = script.replace("{baseDir}", &skill_dir.display().to_string());
                    let truncated = content.len() > 15000;
                    let content = if truncated { content[..15000].to_string() } else { content };
                    return json!({
                        "ok": true,
                        "skill": skill_name,
                        "source": "self-healed",
                        "content": content,
                        "truncated": truncated,
                    });
                }
            }
        }
    }

    // Not found — provide helpful error
    let tried = skill_candidates(skill_name, registry_entry);
    let dirs_str = skills_search_dirs().iter().map(|(d, _)| d.display().to_string()).collect::<Vec<_>>().join(" and ");
    if registry_entry.is_some() {
        return json!({
            "ok": false,
            "error": format!("Skill \"{}\" is registered but no SKILL.md was found on disk. Tried folder names [{}] under {}.", skill_name, tried.join(", "), dirs_str),
            "registered": true,
            "triedCandidates": tried,
        });
    }
    json!({
        "ok": false,
        "error": format!("Skill \"{}\" not found. Tried folder names [{}] under {}.", skill_name, tried.join(", "), dirs_str),
        "registered": false,
        "triedCandidates": tried,
    })
}

// ---------------------------------------------------------------------------
// New tool exec functions (ported from tiger_cowork)
// ---------------------------------------------------------------------------

/// delete_file — port of tiger_cowork deleteFileTool
async fn exec_delete_file(args: &Value, sandbox_dir: &str) -> Value {
    let file_path = args["path"].as_str().unwrap_or("");
    if file_path.is_empty() {
        return json!({ "ok": false, "error": "No path provided" });
    }
    let target = resolve_path(sandbox_dir, file_path);
    if !target.exists() {
        return json!({ "ok": false, "error": format!("File not found: {}", target.display()) });
    }
    if target.is_dir() {
        match tokio::fs::remove_dir_all(&target).await {
            Ok(_) => json!({ "ok": true, "deleted": target.display().to_string() }),
            Err(e) => json!({ "ok": false, "error": format!("Delete dir failed: {e}") }),
        }
    } else {
        match tokio::fs::remove_file(&target).await {
            Ok(_) => json!({ "ok": true, "deleted": target.display().to_string() }),
            Err(e) => json!({ "ok": false, "error": format!("Delete file failed: {e}") }),
        }
    }
}

/// list_local_mounts — returns configured local mount points from settings
async fn exec_list_local_mounts(_args: &Value) -> Value {
    let settings_path = crate::server::data::data_dir().join("settings.json");
    let settings: Value = match tokio::fs::read_to_string(&settings_path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or(json!({})),
        Err(_) => json!({}),
    };
    let mounts = settings
        .get("localFileMounts")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let enabled: Vec<Value> = mounts
        .iter()
        .filter(|m| m.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false))
        .map(|m| json!({
            "id": m.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            "label": m.get("label").and_then(|v| v.as_str()).unwrap_or(""),
            "path": m.get("path").and_then(|v| v.as_str()).unwrap_or(""),
            "permissions": m.get("permissions").and_then(|v| v.as_str()).unwrap_or("readonly"),
        }))
        .collect();
    if enabled.is_empty() {
        return json!({
            "ok": true, "mounts": [],
            "message": "No local directories are mounted. The user can add mounts in Settings > Local Files."
        });
    }
    json!({
        "ok": true, "mounts": enabled,
        "instructions": "Use read_file, write_file, delete_file, and list_files with the absolute paths above."
    })
}

/// run_react — compile JSX/React code, save compiled JS for the output panel
async fn exec_run_react(args: &Value, sandbox_dir: &str) -> Value {
    let code = args["code"].as_str().unwrap_or("");
    let title = args["title"].as_str().unwrap_or("React Component");
    if code.is_empty() {
        return json!({ "ok": false, "error": "No code provided", "output_files": [] });
    }

    // Strip import statements
    let cleaned: String = code
        .lines()
        .filter(|l| !l.trim().starts_with("import "))
        .collect::<Vec<_>>()
        .join("\n");

    // Detect exported component name via simple pattern matching
    let exported_component = {
        let mut name = String::new();
        for line in cleaned.lines() {
            let t = line.trim();
            if t.starts_with("export default function ") {
                name = t.split_whitespace().nth(3).unwrap_or("").to_string();
                break;
            } else if t.starts_with("export default ") {
                name = t.split_whitespace().nth(2).unwrap_or("").trim_end_matches(';').to_string();
                break;
            }
        }
        name
    };

    // Strip export keywords
    let cleaned = cleaned
        .replace("export default function ", "function ")
        .replace("export default class ", "class ")
        .replace("export default ", "");
    let cleaned: String = cleaned
        .lines()
        .map(|l| if l.trim().starts_with("export ") { l.replacen("export ", "", 1) } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n");

    // Detect component names (uppercase)
    let component_names: Vec<String> = cleaned
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            for prefix in &["function ", "const ", "class "] {
                if t.starts_with(prefix) {
                    let rest = &t[prefix.len()..];
                    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                        return Some(name);
                    }
                }
            }
            None
        })
        .collect();

    let render_target = if !exported_component.is_empty() {
        exported_component
    } else {
        component_names.iter().find(|n| n.as_str() == "App")
            .cloned()
            .or_else(|| component_names.last().cloned())
            .unwrap_or_default()
    };

    let wrapped = format!(
        "const {{ useState, useEffect, useRef, useMemo, useCallback, useReducer, useContext, createContext, Fragment, memo, forwardRef, lazy, Suspense }} = React;\nconst _Recharts = typeof Recharts !== 'undefined' ? Recharts : {{}};\nconst {{ LineChart, Line, BarChart, Bar, AreaChart, Area, PieChart, Pie, Cell, XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer, ScatterChart, Scatter }} = _Recharts;\n\n{}\n\nreturn {};",
        cleaned,
        if render_target.is_empty() { "null".to_string() } else { render_target.clone() }
    );

    let output_dir = PathBuf::from(sandbox_dir).join("output_file");
    let _ = tokio::fs::create_dir_all(&output_dir).await;
    let filename = format!("react_{}.jsx.js", chrono::Utc::now().timestamp_millis());
    let out_path = output_dir.join(&filename);

    let meta = serde_json::to_string(&json!({ "title": title, "renderTarget": render_target }))
        .unwrap_or_default();
    let final_output = format!("// __REACT_META__={}\n{}", meta, wrapped);

    // Try compile via npx esbuild
    ensure_full_path();
    let jsx_tmp = std::env::temp_dir().join(format!("react_{}.jsx", chrono::Utc::now().timestamp_millis()));
    if tokio::fs::write(&jsx_tmp, &wrapped).await.is_ok() {
        // Find npx: check PATH, then common locations
        let npx_bin = {
            #[cfg(target_os = "windows")]
            let which_cmd = "where";
            #[cfg(not(target_os = "windows"))]
            let which_cmd = "/usr/bin/which";
            let found = std::process::Command::new(which_cmd).arg("npx").output().ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().map(|s| s.trim().to_string()));
            if let Some(p) = found {
                p
            } else {
                #[cfg(target_os = "macos")]
                {
                    ["/opt/homebrew/bin/npx", "/usr/local/bin/npx"]
                        .iter()
                        .find(|c| std::path::Path::new(c).exists())
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "npx".to_string())
                }
                #[cfg(not(target_os = "macos"))]
                { "npx".to_string() }
            }
        };
        let compiled = timeout(
            Duration::from_secs(30),
            Command::new(&npx_bin)
                .args(["--yes", "esbuild", jsx_tmp.to_str().unwrap_or(""), "--bundle=false", "--loader=jsx"])
                .output(),
        ).await;
        let _ = tokio::fs::remove_file(&jsx_tmp).await;
        if let Ok(Ok(out)) = compiled {
            if out.status.success() {
                let code_str = String::from_utf8_lossy(&out.stdout);
                let content = format!("// __REACT_META__={}\n{}", meta, code_str);
                if tokio::fs::write(&out_path, &content).await.is_ok() {
                    let full = out_path.display().to_string();
                    return json!({ "ok": true, "output_files": [full], "message": format!("React component compiled to {}.", full) });
                }
            }
        }
    }

    // Fallback: save raw
    match tokio::fs::write(&out_path, &final_output).await {
        Ok(_) => {
            let full = out_path.display().to_string();
            json!({ "ok": true, "output_files": [full], "message": format!("React component saved to {} (raw JSX).", full) })
        }
        Err(e) => json!({ "ok": false, "error": format!("Write failed: {e}"), "output_files": [] }),
    }
}

/// remote_task — delegate to another TigrimOS instance via HTTP
async fn exec_remote_task(args: &Value) -> Value {
    let instance_arg = args["instance"].as_str().unwrap_or("");
    let task = args["task"].as_str().unwrap_or("");
    let idle_timeout_secs = args["idle_timeout"].as_u64().unwrap_or(60);
    let max_timeout_secs = args["max_timeout"].as_u64().unwrap_or(1800);

    if task.is_empty() {
        return json!({ "ok": false, "error": "task is required" });
    }

    let settings: Value = match tokio::fs::read_to_string(crate::server::data::data_dir().join("settings.json")).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or(json!({})),
        Err(_) => json!({}),
    };

    let (url, token) = {
        let mut found_url = String::new();
        let mut found_token = String::new();
        if let Some(instances) = settings.get("remoteInstances").and_then(|v| v.as_array()) {
            if let Some(inst) = instances.iter().find(|i| {
                i.get("id").and_then(|v| v.as_str()) == Some(instance_arg)
                    || i.get("name").and_then(|v| v.as_str()) == Some(instance_arg)
            }) {
                found_url = inst.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                found_token = inst.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
            }
        }
        if found_url.is_empty() {
            if let Ok(parsed) = serde_json::from_str::<Value>(instance_arg) {
                found_url = parsed.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                found_token = parsed.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
            }
        }
        (found_url, found_token)
    };

    if url.is_empty() {
        return json!({ "ok": false, "error": format!("Remote instance \"{}\" not found", instance_arg) });
    }

    let base_url = url.trim_end_matches('/').to_string();
    let client = Client::new();

    let submit = match client
        .post(format!("{}/api/remote/task", base_url))
        .header("Authorization", format!("Bearer {}", token))
        .json(&json!({ "task": task }))
        .send().await
    {
        Ok(r) => r,
        Err(e) => return json!({ "ok": false, "error": format!("Network error: {e}") }),
    };

    if !submit.status().is_success() {
        let status = submit.status().as_u16();
        let body = submit.text().await.unwrap_or_default();
        return json!({ "ok": false, "error": format!("Submit failed: {} {}", status, &body[..body.len().min(500)]) });
    }

    let submit_data: Value = match submit.json().await {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": format!("Parse error: {e}") }),
    };

    let task_id = match submit_data.get("taskId").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return json!({ "ok": false, "error": "No taskId in response" }),
    };

    info!("[Remote] Task {} submitted to {}", task_id, base_url);

    let hard_deadline = tokio::time::Instant::now() + Duration::from_secs(max_timeout_secs);
    let idle_duration = Duration::from_secs(idle_timeout_secs);
    let mut last_activity = tokio::time::Instant::now();
    let mut last_seq: u64 = 0;

    loop {
        if tokio::time::Instant::now() > hard_deadline {
            return json!({ "ok": false, "error": format!("Timed out after {}s", max_timeout_secs) });
        }
        tokio::time::sleep(Duration::from_millis(2000)).await;
        let poll = match client
            .get(format!("{}/api/remote/task/{}", base_url, task_id))
            .header("Authorization", format!("Bearer {}", token))
            .send().await
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        let data: Value = match poll.json().await { Ok(v) => v, Err(_) => continue };
        let seq = data.get("progressSeq").and_then(|v| v.as_u64()).unwrap_or(0);
        if seq > last_seq { last_seq = seq; last_activity = tokio::time::Instant::now(); }
        if tokio::time::Instant::now().duration_since(last_activity) > idle_duration {
            return json!({ "ok": false, "error": format!("Idle timeout after {}s", idle_timeout_secs) });
        }
        match data.get("status").and_then(|v| v.as_str()) {
            Some("completed") => return json!({ "ok": true, "result": data.get("result").cloned().unwrap_or(json!(null)) }),
            Some("error") => return json!({ "ok": false, "error": data.get("error").and_then(|v| v.as_str()).unwrap_or("Remote task failed") }),
            _ => continue,
        }
    }
}

/// Claude Code CLI agent — spawn `claude -p` and parse stream-json output
async fn exec_claude_code_agent(args: &Value, sandbox_dir: &str) -> Value {
    let task = args["task"].as_str().unwrap_or("");
    let system_prompt = args["system_prompt"].as_str().unwrap_or("");
    let timeout_secs = args["timeout"].as_u64().unwrap_or(300);
    let max_turns = args["max_turns"].as_u64().unwrap_or(25);
    let model = args["model"].as_str().unwrap_or("");

    if task.is_empty() {
        return json!({ "ok": false, "error": "task is required" });
    }

    let full_prompt = if system_prompt.is_empty() {
        task.to_string()
    } else {
        format!("{}\n\n---\n\nTASK:\n{}", system_prompt, task)
    };

    let mut cli_args = vec![
        "-p".to_string(), full_prompt,
        "--output-format".to_string(), "stream-json".to_string(),
        "--max-turns".to_string(), max_turns.to_string(),
        "--allowedTools".to_string(), "Read,Edit,Write,Bash,Glob,Grep".to_string(),
        "--verbose".to_string(),
    ];
    if !model.is_empty() {
        cli_args.push("--model".to_string());
        cli_args.push(model.to_string());
    }

    info!("[ClaudeCode] Spawning in {} (timeout: {}s, maxTurns: {})", sandbox_dir, timeout_secs, max_turns);

    match timeout(Duration::from_secs(timeout_secs), Command::new("claude").args(&cli_args).current_dir(sandbox_dir).output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut result_text = String::new();
            let mut tool_calls: Vec<String> = Vec::new();
            for line in stdout.lines() {
                if let Ok(event) = serde_json::from_str::<Value>(line) {
                    if event["type"] == "assistant" {
                        if let Some(content) = event["message"]["content"].as_array() {
                            for block in content {
                                if block["type"] == "text" { result_text.push_str(block["text"].as_str().unwrap_or("")); }
                                if block["type"] == "tool_use" { if let Some(n) = block["name"].as_str() { tool_calls.push(n.to_string()); } }
                            }
                        }
                    } else if event["type"] == "result" {
                        if let Some(r) = event["result"].as_str() { result_text = r.to_string(); }
                    }
                }
            }
            if result_text.is_empty() && !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return json!({ "ok": false, "error": format!("claude exited {:?}: {}", output.status.code(), &stderr[..stderr.len().min(1000)]) });
            }
            json!({ "ok": true, "content": if result_text.is_empty() { "(no output)".to_string() } else { result_text }, "tool_calls": tool_calls })
        }
        Ok(Err(e)) => json!({ "ok": false, "error": format!("Failed to spawn claude: {e}. Is 'claude' in PATH?") }),
        Err(_) => json!({ "ok": false, "error": format!("Claude Code timed out after {}s", timeout_secs) }),
    }
}

/// Gemini CLI agent — spawn `gemini -p` and parse stream-json output
async fn exec_gemini_cli_agent(args: &Value, sandbox_dir: &str) -> Value {
    let task = args["task"].as_str().unwrap_or("");
    let system_prompt = args["system_prompt"].as_str().unwrap_or("");
    let timeout_secs = args["timeout"].as_u64().unwrap_or(300);
    let model = args["model"].as_str().unwrap_or("");

    if task.is_empty() {
        return json!({ "ok": false, "error": "task is required" });
    }

    let full_prompt = if system_prompt.is_empty() {
        task.to_string()
    } else {
        format!("{}\n\n---\n\nTASK:\n{}", system_prompt, task)
    };

    let mut cli_args = vec![
        "-p".to_string(), full_prompt,
        "-o".to_string(), "stream-json".to_string(),
        "--yolo".to_string(),
    ];
    if !model.is_empty() {
        cli_args.push("-m".to_string());
        cli_args.push(model.to_string());
    }

    let home = resolve_home();
    info!("[GeminiCLI] Spawning agent in {} (timeout: {}s)", sandbox_dir, timeout_secs);

    match timeout(Duration::from_secs(timeout_secs),
        Command::new("gemini")
            .args(&cli_args)
            .current_dir(sandbox_dir)
            .env("PATH", cli_env_path())
            .env("HOME", &home)
            .output()
    ).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut result_text = String::new();
            let mut tool_calls: Vec<String> = Vec::new();
            for line in stdout.lines() {
                if let Ok(event) = serde_json::from_str::<Value>(line) {
                    // Gemini CLI stream-json events
                    let etype = event["type"].as_str().unwrap_or("");
                    if etype == "message" || etype == "assistant" {
                        if let Some(content) = event["message"]["content"].as_array()
                            .or_else(|| event["content"].as_array()) {
                            for block in content {
                                if block["type"] == "text" {
                                    result_text.push_str(block["text"].as_str().unwrap_or(""));
                                }
                                if block["type"] == "tool_use" || block["type"] == "functionCall" {
                                    if let Some(n) = block["name"].as_str() { tool_calls.push(n.to_string()); }
                                }
                            }
                        }
                    } else if etype == "result" {
                        if let Some(r) = event["result"].as_str() { result_text = r.to_string(); }
                    } else if etype == "text" {
                        if let Some(t) = event["text"].as_str() { result_text.push_str(t); }
                    }
                }
            }
            // Fallback: if stream-json didn't parse, use raw stdout
            if result_text.is_empty() {
                result_text = stdout.trim().to_string();
            }
            if result_text.is_empty() && !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return json!({ "ok": false, "error": format!("gemini exited {:?}: {}", output.status.code(), &stderr[..stderr.len().min(1000)]) });
            }
            json!({ "ok": true, "content": if result_text.is_empty() { "(no output)".to_string() } else { result_text }, "tool_calls": tool_calls })
        }
        Ok(Err(e)) => json!({ "ok": false, "error": format!("Failed to spawn gemini: {e}. Is 'gemini' in PATH?") }),
        Err(_) => json!({ "ok": false, "error": format!("Gemini CLI timed out after {}s", timeout_secs) }),
    }
}

// ---------------------------------------------------------------------------
// Protocol tool exec functions (TCP / Bus / Queue / Blackboard)
// ---------------------------------------------------------------------------

async fn exec_proto_tcp_send(args: &Value, session_id: &str, agent_id: &str) -> Value {
    let to = args["to"].as_str().unwrap_or("");
    let topic = args["topic"].as_str().unwrap_or("");
    let payload = args["payload"].clone();
    let _ = protocols::tcp_open(agent_id, to, Some(session_id)).await;
    let sent = protocols::tcp_send(agent_id, to, topic, payload).await;
    json!({ "ok": sent, "protocol": "tcp", "from": agent_id, "to": to, "topic": topic })
}

async fn exec_proto_tcp_read(args: &Value, agent_id: &str) -> Value {
    let peer = args["peer"].as_str().unwrap_or("");
    let messages = protocols::tcp_read(agent_id, peer).await;
    let msgs: Vec<Value> = messages.iter().map(|m| serde_json::to_value(m).unwrap_or(json!({}))).collect();
    json!({ "ok": true, "protocol": "tcp", "peer": peer, "messages": msgs, "count": msgs.len() })
}

async fn exec_proto_bus_publish(args: &Value, session_id: &str, agent_id: &str) -> Value {
    let topic = args["topic"].as_str().unwrap_or("");
    let payload = args["payload"].clone();
    protocols::bus_publish(session_id, agent_id, topic, payload).await;
    json!({ "ok": true, "protocol": "bus", "from": agent_id, "topic": topic })
}

async fn exec_proto_bus_history(args: &Value, session_id: &str) -> Value {
    let topic = args["topic"].as_str();
    let messages = protocols::bus_history(session_id, topic).await;
    let msgs: Vec<Value> = messages.iter().map(|m| serde_json::to_value(m).unwrap_or(json!({}))).collect();
    json!({ "ok": true, "protocol": "bus", "topic": topic.unwrap_or("all"), "messages": msgs, "count": msgs.len() })
}

async fn exec_proto_queue_send(args: &Value, session_id: &str, agent_id: &str) -> Value {
    let to = args["to"].as_str().unwrap_or("");
    let topic = args["topic"].as_str().unwrap_or("");
    let payload = args["payload"].clone();
    let depth = protocols::queue_enqueue(agent_id, to, topic, payload, Some(session_id)).await;
    json!({ "ok": true, "protocol": "queue", "from": agent_id, "to": to, "topic": topic, "queue_depth": depth })
}

async fn exec_proto_queue_receive(args: &Value, agent_id: &str) -> Value {
    let from = args["from"].as_str().unwrap_or("");
    let topic = args["topic"].as_str();
    match protocols::queue_dequeue(from, agent_id, topic).await {
        Some(msg) => json!({ "ok": true, "protocol": "queue", "message": serde_json::to_value(&msg).unwrap_or(json!({})) }),
        None => json!({ "ok": true, "protocol": "queue", "message": null, "note": "Queue empty" }),
    }
}

async fn exec_proto_queue_peek(args: &Value, agent_id: &str) -> Value {
    let from = args["from"].as_str().unwrap_or("");
    let topic = args["topic"].as_str();
    let count = args["count"].as_u64().unwrap_or(5) as usize;
    let msgs = protocols::queue_peek(from, agent_id, topic, count).await;
    let msgs: Vec<Value> = msgs.iter().map(|m| serde_json::to_value(m).unwrap_or(json!({}))).collect();
    json!({ "ok": true, "protocol": "queue", "messages": msgs, "count": msgs.len() })
}

async fn exec_bb_propose(args: &Value, session_id: &str, agent_id: &str) -> Value {
    let description = args["description"].as_str().unwrap_or("");
    if description.is_empty() { return json!({ "ok": false, "error": "description is required" }); }
    let (task, skipped) = protocols::blackboard_propose(session_id, agent_id, description, args["task_id"].as_str()).await;
    let task_json = serde_json::to_value(&task).unwrap_or(json!({}));
    if skipped {
        return json!({ "ok": true, "protocol": "blackboard", "action": "propose", "task": task_json, "skipped": true });
    }
    protocols::bus_publish(session_id, agent_id, "bb:proposal", json!({ "task_id": task.task_id, "description": description, "proposed_by": agent_id })).await;

    // In P2P modes, send bid request tasks to all eligible agents via their task channels
    // so the realtime_agent_loop can evaluate and bid automatically.
    {
        let map = realtime_sessions().lock().await;
        if let Some(session_arc) = map.get(session_id) {
            let session = session_arc.lock().await;
            let orch_mode = session.system_config["system"]["orchestration_mode"]
                .as_str().unwrap_or("hierarchical");
            if matches!(orch_mode, "p2p" | "p2p_orchestrator") {
                let task_id = &task.task_id;
                for (aid, handle) in &session.agents {
                    if aid == agent_id { continue; } // don't bid on own task
                    let role = handle.agent_def.get("role").and_then(|r| r.as_str()).unwrap_or("");
                    if role == "human" || role == "orchestrator" { continue; }
                    let _ = handle.task_tx.send(AgentTask {
                        from: format!("bid_request:{}", task_id),
                        task: format!(
                            "BID REQUEST: Evaluate this task and decide if you should bid.\n\
                             Task ID: {}\nDescription: {}\n\
                             Use bb_bid tool with your confidence score if you want to bid, \
                             or do nothing if this task is outside your expertise.",
                            task_id, description
                        ),
                        context: None,
                    }).await;
                }
            }
        }
    }

    json!({ "ok": true, "protocol": "blackboard", "action": "propose", "task": task_json, "hint": format!("Task \"{}\" posted. Bidders notified via bus.", task.task_id) })
}

async fn exec_bb_bid(args: &Value, session_id: &str, agent_id: &str) -> Value {
    let task_id = args["task_id"].as_str().unwrap_or("");
    let confidence = args["confidence"].as_f64().unwrap_or(0.5);
    match protocols::blackboard_bid(session_id, agent_id, task_id, confidence, args["cost"].as_f64(), args["reasoning"].as_str()).await {
        Ok(_) => {
            protocols::bus_publish(session_id, agent_id, "bb:bid_received", json!({ "task_id": task_id, "bidder": agent_id, "confidence": confidence })).await;
            json!({ "ok": true, "protocol": "blackboard", "action": "bid" })
        }
        Err(e) => json!({ "ok": false, "protocol": "blackboard", "action": "bid", "error": e }),
    }
}

async fn exec_bb_award(args: &Value, session_id: &str, agent_id: &str) -> Value {
    let task_id = args["task_id"].as_str().unwrap_or("");
    let award_to = args["award_to"].as_str();
    let computed = if award_to.is_none() {
        if let Some(scores) = args["orchestrator_scores"].as_array() {
            if let Some(task) = protocols::blackboard_get_task(session_id, task_id).await {
                let best = task.bids.iter().map(|bid| {
                    let orch = scores.iter().find(|s| s["agent_id"].as_str() == Some(&bid.agent_id))
                        .and_then(|s| s["score"].as_f64()).unwrap_or(0.5);
                    (bid.agent_id.clone(), bid.confidence * 0.5 + orch * 0.5)
                }).max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
                best.map(|(id, _)| id)
            } else { None }
        } else { None }
    } else { award_to.map(|s| s.to_string()) };
    match protocols::blackboard_award(session_id, task_id, computed.as_deref(), None).await {
        Ok(winner) => {
            let _ = protocols::blackboard_start_task(session_id, &winner, task_id).await;
            protocols::bus_publish(session_id, agent_id, "bb:task_awarded", json!({ "task_id": task_id, "awarded_to": winner })).await;
            json!({ "ok": true, "protocol": "blackboard", "action": "award", "awarded_to": winner, "next_step": format!("Task awarded to \"{winner}\". Use wait_result({{\"from\": \"{winner}\"}}) to collect result.") })
        }
        Err(e) => json!({ "ok": false, "protocol": "blackboard", "action": "award", "error": e }),
    }
}

async fn exec_bb_complete(args: &Value, session_id: &str, agent_id: &str) -> Value {
    let task_id = args["task_id"].as_str().unwrap_or("");
    let result = args["result"].clone();
    match protocols::blackboard_complete_task(session_id, agent_id, task_id, result).await {
        Ok(_) => json!({ "ok": true, "protocol": "blackboard", "action": "complete" }),
        Err(e) => json!({ "ok": false, "protocol": "blackboard", "action": "complete", "error": e }),
    }
}

async fn exec_bb_read(args: &Value, session_id: &str) -> Value {
    if let Some(task_id) = args["task_id"].as_str() {
        let task = protocols::blackboard_get_task(session_id, task_id).await;
        json!({ "ok": true, "protocol": "blackboard", "action": "read", "task": task.map(|t| serde_json::to_value(&t).unwrap_or(json!(null))) })
    } else {
        let tasks = protocols::blackboard_get_tasks(session_id, args["status"].as_str()).await;
        let tasks: Vec<Value> = tasks.iter().map(|t| serde_json::to_value(t).unwrap_or(json!({}))).collect();
        json!({ "ok": true, "protocol": "blackboard", "action": "read", "tasks": tasks, "count": tasks.len() })
    }
}

async fn exec_bb_log(args: &Value, session_id: &str) -> Value {
    let limit = args["limit"].as_u64().map(|n| n as usize).or(Some(50));
    let log = protocols::blackboard_get_log(session_id, limit).await;
    let log: Vec<Value> = log.iter().map(|e| serde_json::to_value(e).unwrap_or(json!({}))).collect();
    json!({ "ok": true, "protocol": "blackboard", "action": "log", "entries": log, "count": log.len() })
}

// ---------------------------------------------------------------------------
// ClawHub marketplace tools
// ---------------------------------------------------------------------------

async fn exec_clawhub_search(args: &Value) -> Value {
    let query = args["query"].as_str().unwrap_or("");
    let limit = args["limit"].as_u64().unwrap_or(10).min(50).max(1) as usize;

    // Try to find clawhub binary
    let candidates = ["Tiger_bot/node_modules/.bin/clawhub", "clawhub"];
    let mut bin = String::new();
    for candidate in &candidates {
        if Command::new(candidate)
            .arg("--cli-version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            bin = candidate.to_string();
            break;
        }
    }
    if bin.is_empty() {
        return json!({ "ok": false, "error": "clawhub CLI not found" });
    }

    let workdir = std::path::Path::new("Tiger_bot")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("Tiger_bot"));

    match Command::new(&bin)
        .args([
            "search", query,
            "--limit", &limit.to_string(),
            "--no-input",
            "--workdir", &workdir.display().to_string(),
            "--dir", "skills",
        ])
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            json!({ "ok": output.status.success(), "output": stdout.trim(), "warning": stderr.trim() })
        }
        Err(e) => json!({ "ok": false, "error": format!("Failed to run clawhub: {e}") }),
    }
}

async fn exec_clawhub_install(args: &Value) -> Value {
    let slug = args["slug"].as_str().unwrap_or("");
    let force = args["force"].as_bool().unwrap_or(false);

    // Validate slug format
    if !regex::Regex::new(r"^[a-z0-9][a-z0-9-]*$").unwrap().is_match(slug) {
        return json!({ "ok": false, "error": "Invalid slug format" });
    }

    let candidates = ["Tiger_bot/node_modules/.bin/clawhub", "clawhub"];
    let mut bin = String::new();
    for candidate in &candidates {
        if Command::new(candidate)
            .arg("--cli-version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            bin = candidate.to_string();
            break;
        }
    }
    if bin.is_empty() {
        return json!({ "ok": false, "error": "clawhub CLI not found" });
    }

    let workdir = std::path::Path::new("Tiger_bot")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("Tiger_bot"));
    let workdir_str = workdir.display().to_string();

    let mut argv = vec![
        "install", slug, "--no-input",
        "--workdir", &workdir_str,
        "--dir", "skills",
    ];
    if force {
        argv.push("--force");
    }

    match timeout(
        Duration::from_secs(120),
        Command::new(&bin).args(&argv).output(),
    )
    .await
    {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            json!({
                "ok": output.status.success(),
                "slug": slug,
                "output": stdout.trim(),
                "warning": stderr.trim()
            })
        }
        Ok(Err(e)) => json!({ "ok": false, "error": format!("Failed to run clawhub: {e}") }),
        Err(_) => json!({ "ok": false, "error": "clawhub install timed out (120s)" }),
    }
}

// ---------------------------------------------------------------------------
// OpenRouter web search
// ---------------------------------------------------------------------------

async fn exec_openrouter_web_search(args: &Value) -> Value {
    let query = args["query"].as_str().unwrap_or("");

    // Load settings to get API key
    let settings_path = crate::server::data::data_dir().join("settings.json");
    let settings: Value = match tokio::fs::read_to_string(&settings_path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or(json!({})),
        Err(_) => return json!({ "ok": false, "error": "Could not read settings.json" }),
    };

    let api_key = match settings["openRouterSearchApiKey"].as_str() {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => return json!({ "ok": false, "error": "OpenRouter API key not configured" }),
    };

    let model = settings["openRouterSearchModel"]
        .as_str()
        .unwrap_or("openai/gpt-4.1-mini")
        .to_string();
    let max_tokens = settings["openRouterSearchMaxTokens"]
        .as_u64()
        .unwrap_or(4096);
    let max_results = settings["openRouterSearchMaxResults"]
        .as_u64()
        .unwrap_or(5)
        .min(10)
        .max(1);

    let client = reqwest::Client::new();
    let body = json!({
        "model": model,
        "input": query,
        "max_output_tokens": max_tokens,
        "tools": [{ "type": "web_search_preview", "search_context_size": "medium" }],
        "plugins": [{ "id": "web", "max_results": max_results }],
    });

    let resp = match client
        .post("https://openrouter.ai/api/v1/responses")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({ "ok": false, "error": format!("Request failed: {e}") }),
    };

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let err_text = resp.text().await.unwrap_or_default();
        return json!({ "ok": false, "error": format!("OpenRouter API error {status}: {err_text}") });
    }

    let data: Value = match resp.json().await {
        Ok(d) => d,
        Err(e) => return json!({ "ok": false, "error": format!("Failed to parse response: {e}") }),
    };

    // Extract text and citations
    let mut text = String::new();
    let mut citations: Vec<Value> = Vec::new();

    if let Some(output) = data["output"].as_array() {
        for item in output {
            if item["type"].as_str() == Some("message") {
                if let Some(content) = item["content"].as_array() {
                    for block in content {
                        if block["type"].as_str() == Some("output_text") {
                            if let Some(t) = block["text"].as_str() {
                                text.push_str(t);
                            }
                            if let Some(annotations) = block["annotations"].as_array() {
                                for ann in annotations {
                                    if ann["type"].as_str() == Some("url_citation") {
                                        if let Some(url) = ann["url"].as_str() {
                                            citations.push(json!({
                                                "url": url,
                                                "title": ann["title"].as_str().unwrap_or("")
                                            }));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    json!({
        "ok": true,
        "text": truncate(&text, 15000),
        "citations": &citations[..citations.len().min(20)],
        "model": model,
        "usage": data["usage"],
    })
}

// ---------------------------------------------------------------------------
// create_architecture — generate multi-agent YAML via LLM
// ---------------------------------------------------------------------------

async fn exec_create_architecture(
    args: &Value,
    sub_agent: &SubAgentConfig,
) -> Value {
    let description = args["description"].as_str().unwrap_or("");
    if description.is_empty() {
        return json!({ "ok": false, "error": "description is required" });
    }

    let arch_type = args["architectureType"]
        .as_str()
        .unwrap_or("hierarchical")
        .to_string();
    let count = args["agentCount"]
        .as_str()
        .unwrap_or("auto")
        .to_string();
    let session_id = if sub_agent.session_id.is_empty() {
        "default".to_string()
    } else {
        sub_agent.session_id.clone()
    };

    // Load base template if available
    let mut base_template_prompt = String::new();
    if !sub_agent.config_file.is_empty() {
        if let Some((cfg, _)) = load_agent_yaml(&sub_agent.config_file) {
            if let Some(agents) = cfg["agents"].as_array() {
                let non_human: Vec<&Value> = agents.iter()
                    .filter(|a| a["role"].as_str() != Some("human"))
                    .collect();
                let agent_lines: Vec<String> = non_human.iter()
                    .map(|a| format!("- {} ({}): {}",
                        a["id"].as_str().unwrap_or("?"),
                        a["role"].as_str().unwrap_or("?"),
                        a["persona"].as_str().unwrap_or(a["name"].as_str().unwrap_or("?"))))
                    .collect();
                base_template_prompt = format!(
                    "\n\nBASE TEMPLATE (from \"{}\"):\nSystem: {}, Mode: {}\nAgents:\n{}\n\nUse this as a starting point.",
                    sub_agent.config_file,
                    cfg["system"]["name"].as_str().unwrap_or("Unknown"),
                    cfg["system"]["orchestration_mode"].as_str().unwrap_or("hierarchical"),
                    agent_lines.join("\n"),
                );
            }
        }
    }

    // Use LLM to generate architecture
    let prompt = format!(
        r#"Based on this description, generate a complete multi-agent system configuration as a JSON object.

User Request: {description}
{base_template_prompt}
Architecture Type: {arch_type}
Number of Agents: {count}
Connection Protocol: tcp

Return ONLY a valid JSON object (no markdown, no code fences) with this structure:
{{
  "system": {{
    "name": "System Name",
    "orchestration_mode": "{arch_type}",
    "communication_protocol": "structured_handoff",
    "context_passing": "full_chain"
  }},
  "agents": [
    {{
      "id": "unique_snake_case_id",
      "name": "Agent Display Name",
      "role": "one of: human, orchestrator, worker, checker, reporter, researcher, peer",
      "persona": "Detailed 2-3 sentence persona description",
      "responsibilities": ["r1", "r2", "r3"],
      "bus": {{ "enabled": true, "topics": ["topic1"] }},
      "mesh": {{ "enabled": false }}
    }}
  ],
  "connections": [
    {{ "from": "source_id", "to": "target_id", "label": "label", "protocol": "tcp", "topics": ["topic1"] }}
  ],
  "workflow": {{
    "sequence": [
      {{ "step": 1, "agent": "agent_id", "action": "what this agent does", "outputs_to": ["next_agent_id"] }}
    ]
  }}
}}

RULES:
- Always include ONE agent with role "human" and id "human"
- For hierarchical: human → orchestrator → workers. Use role "orchestrator" for the coordinator.
- For flat: human → all agents directly
- For mesh: no connections (mesh mode bypasses access control)
- For hybrid: human → orchestrator → workers (workers have mesh.enabled: true)
- For pipeline: agents form a LINEAR SEQUENTIAL CHAIN. human → agent1 → agent2 → agent3 → ... → final_agent. Each agent connects to exactly ONE next agent. Do NOT use an orchestrator role. Do NOT create star topology (one agent connecting to many). Connections MUST form a strict linear chain. The workflow.sequence MUST list agents in order with outputs_to pointing to the next agent. The last agent has outputs_to: [].
- For p2p: all non-human agents use role "peer", no connections
- Agent IDs must be snake_case
- Generate 3-8 agents including human
- Always include workflow.sequence listing the processing order"#
    );

    // Call the LLM to generate the architecture (routes through claude-code if needed)
    let client = reqwest::Client::new();
    let messages = vec![
        json!({ "role": "system", "content": "You are an expert multi-agent system architect. Generate complete, well-structured agent system configurations as JSON. Return ONLY valid JSON, nothing else." }),
        json!({ "role": "user", "content": prompt }),
    ];

    let resp_body = match llm_call(
        &client, &sub_agent.api_key, &sub_agent.api_url, &sub_agent.model,
        &messages, None, 0.7, 16384,
    ).await {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": format!("API request failed: {e}") }),
    };

    let content_text = resp_body["choices"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|c| {
            let msg = &c["message"];
            msg["content"].as_str().filter(|s| !s.is_empty())
                .or_else(|| msg["reasoning_content"].as_str())
        })
        .unwrap_or("");

    if content_text.is_empty() {
        return json!({ "ok": false, "error": "No response from LLM" });
    }

    // Extract JSON from response
    let json_str = if let Some(m) = regex::Regex::new(r"\{[\s\S]*\}").unwrap().find(content_text) {
        m.as_str()
    } else {
        content_text
    };

    let parsed: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return json!({ "ok": false, "error": "Failed to parse generated architecture" }),
    };

    if parsed["system"].is_null() || !parsed["agents"].is_array() {
        return json!({ "ok": false, "error": "Generated architecture has invalid structure" });
    }

    // Convert to YAML and save
    let yaml_content = match serde_yaml::to_string(&parsed) {
        Ok(y) => y,
        Err(e) => return json!({ "ok": false, "error": format!("Failed to serialize YAML: {e}") }),
    };

    let safe_name = parsed["system"]["name"]
        .as_str()
        .unwrap_or("auto_created")
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>();
    let safe_name = safe_name.trim_matches('_');
    let filename = format!("{}_auto.yaml", safe_name);

    let agents_dir = crate::server::data::data_dir().join("agents");
    let _ = tokio::fs::create_dir_all(&agents_dir).await;
    if let Err(e) = tokio::fs::write(agents_dir.join(&filename), &yaml_content).await {
        return json!({ "ok": false, "error": format!("Failed to save YAML: {e}") });
    }

    // Track creation
    auto_created_architectures().lock().await.insert(session_id.clone(), filename.clone());
    auto_swarm_selections().lock().await.insert(session_id.clone(), filename.clone());

    // NOTE: realtime session boot is handled by the caller (chat.rs)
    // to avoid double-booting and race conditions.

    let all_agents: Vec<Value> = parsed["agents"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter(|a| a["role"].as_str() != Some("human"))
        .map(|a| json!({
            "id": a["id"], "name": a["name"], "role": a["role"],
            "persona": a["persona"], "responsibilities": a["responsibilities"],
        }))
        .collect();

    let mode = parsed["system"]["orchestration_mode"]
        .as_str()
        .unwrap_or(&arch_type);

    let orchestrator = parsed["agents"]
        .as_array()
        .and_then(|arr| arr.iter().find(|a| a["role"].as_str() == Some("orchestrator")));

    let delegation = if let Some(orch) = orchestrator {
        let orch_id = orch["id"].as_str().unwrap_or("orchestrator");
        format!("Orchestrator \"{}\" manages the team. Use send_task({{to: \"{}\", task: \"...\"}}) then wait_result.", orch_id, orch_id)
    } else {
        let ids: Vec<&str> = all_agents.iter().filter_map(|a| a["id"].as_str()).collect();
        format!("Send tasks directly to agents. Available: {}.", ids.join(", "))
    };

    json!({
        "ok": true,
        "created": true,
        "filename": filename,
        "systemName": parsed["system"]["name"],
        "mode": mode,
        "agents": all_agents,
        "realtimeMode": true,
        "yamlContent": yaml_content,
        "message": format!("Architecture \"{}\" created and saved. All agents are LIVE. {} Do NOT do work yourself — delegate via send_task/wait_result.",
            parsed["system"]["name"].as_str().unwrap_or(&filename), delegation),
    })
}

// ---------------------------------------------------------------------------
// select_swarm — pick an existing agent YAML and boot it
// ---------------------------------------------------------------------------

async fn exec_select_swarm(
    args: &Value,
    sub_agent: &SubAgentConfig,
) -> Value {
    let filename = args["filename"].as_str().unwrap_or("");
    let reason = args["reason"].as_str().unwrap_or("");
    if filename.is_empty() {
        return json!({ "ok": false, "error": "filename is required" });
    }

    let (config, _ids) = match load_agent_yaml(filename) {
        Some(v) => v,
        None => return json!({ "ok": false, "error": format!("Config file \"{}\" not found or invalid", filename) }),
    };

    let all_agents: Vec<Value> = config["agents"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter(|a| a["role"].as_str() != Some("human"))
        .collect::<Vec<_>>()
        .iter()
        .map(|a| json!({
            "id": a["id"], "name": a["name"], "role": a["role"],
            "persona": a["persona"], "responsibilities": a["responsibilities"],
        }))
        .collect();

    if all_agents.is_empty() {
        return json!({ "ok": false, "error": "Selected config has no usable agents" });
    }

    let session_id = if sub_agent.session_id.is_empty() {
        "default".to_string()
    } else {
        sub_agent.session_id.clone()
    };

    auto_swarm_selections().lock().await.insert(session_id.clone(), filename.to_string());

    // Boot the realtime session (fire-and-forget to avoid recursive type cycle)
    boot_realtime_session_deferred(
        session_id.clone(), filename.to_string(),
        sub_agent.api_key.clone(), sub_agent.api_url.clone(),
        sub_agent.model.clone(),
    );

    let mode = config["system"]["orchestration_mode"]
        .as_str()
        .unwrap_or("hierarchical");

    let orchestrator = config["agents"]
        .as_array()
        .and_then(|arr| arr.iter().find(|a| a["role"].as_str() == Some("orchestrator")));

    let delegation = if let Some(orch) = orchestrator {
        let orch_id = orch["id"].as_str().unwrap_or("orchestrator");
        format!("Orchestrator \"{}\" manages the team. Use send_task({{to: \"{}\", task: \"...\"}}) then wait_result.", orch_id, orch_id)
    } else {
        let ids: Vec<&str> = all_agents.iter().filter_map(|a| a["id"].as_str()).collect();
        format!("Send tasks directly. Available agents: {}.", ids.join(", "))
    };

    json!({
        "ok": true,
        "selected": filename,
        "systemName": config["system"]["name"],
        "mode": mode,
        "reason": reason,
        "agents": all_agents,
        "realtimeMode": true,
        "message": format!("Swarm \"{}\" selected ({}). All agents are LIVE. {} Delegate via send_task/wait_result.",
            config["system"]["name"].as_str().unwrap_or(filename), mode, delegation),
    })
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
    let mut system_prompt = build_agent_system_prompt(&agent_def, &yaml_val, &available_targets);

    // Advertise enabled skills so sub-agents can discover them without calling list_skills.
    match build_enabled_skills_block(Some(SUBAGENT_SKILLS_PERSONA)).await {
        block if !block.is_empty() => system_prompt.push_str(&block),
        _ => {}
    }

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
        agent_id: agent_id.to_string(),
        mode: sub_agent.mode.clone(),
        agent_role: agent_role.to_string(),
        cancel_flag: sub_agent.cancel_flag.clone(),
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

#[allow(dead_code)]
async fn execute_tool(name: &str, args: &Value, sandbox_dir: &str) -> Value {
    execute_tool_with_context(name, args, sandbox_dir, "default", "main").await
}

async fn execute_tool_with_context(
    name: &str,
    args: &Value,
    sandbox_dir: &str,
    session_id: &str,
    agent_id: &str,
) -> Value {
    match name {
        "web_search" => exec_web_search(args).await,
        "fetch_url" => exec_fetch_url(args).await,
        "run_python" => exec_run_python(args, sandbox_dir).await,
        "run_shell" => exec_run_shell(args, sandbox_dir).await,
        "read_file" => exec_read_file(args, sandbox_dir).await,
        "write_file" => exec_write_file(args, sandbox_dir).await,
        "list_files" => exec_list_files(args, sandbox_dir).await,
        "delete_file" => exec_delete_file(args, sandbox_dir).await,
        "list_local_mounts" => exec_list_local_mounts(args).await,
        "run_react" => exec_run_react(args, sandbox_dir).await,
        "list_skills" => exec_list_skills(args, sandbox_dir).await,
        "load_skill" => exec_load_skill(args, sandbox_dir).await,
        "remote_task" => exec_remote_task(args).await,
        "claude_code_agent" => exec_claude_code_agent(args, sandbox_dir).await,
        "gemini_cli_agent" => exec_gemini_cli_agent(args, sandbox_dir).await,
        "proto_tcp_send" => exec_proto_tcp_send(args, session_id, agent_id).await,
        "proto_tcp_read" => exec_proto_tcp_read(args, agent_id).await,
        "proto_bus_publish" => exec_proto_bus_publish(args, session_id, agent_id).await,
        "proto_bus_history" => exec_proto_bus_history(args, session_id).await,
        "proto_queue_send" => exec_proto_queue_send(args, session_id, agent_id).await,
        "proto_queue_receive" => exec_proto_queue_receive(args, agent_id).await,
        "proto_queue_peek" => exec_proto_queue_peek(args, agent_id).await,
        "bb_propose" => exec_bb_propose(args, session_id, agent_id).await,
        "bb_bid" => exec_bb_bid(args, session_id, agent_id).await,
        "bb_award" => exec_bb_award(args, session_id, agent_id).await,
        "bb_complete" => exec_bb_complete(args, session_id, agent_id).await,
        "bb_read" => exec_bb_read(args, session_id).await,
        "bb_log" => exec_bb_log(args, session_id).await,
        "clawhub_search" => exec_clawhub_search(args).await,
        "clawhub_install" => exec_clawhub_install(args).await,
        "openrouter_web_search" => exec_openrouter_web_search(args).await,
        _ if mcp::is_mcp_tool(name) => {
            info!("[execute_tool] Routing MCP tool call: {name}");
            mcp::call_mcp_tool(name, args).await
        }
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
// Tool-calling loop — faithful port of tiger_cowork tigerbot.ts
// ---------------------------------------------------------------------------

// Default constants — overridden by settings when available
const DEFAULT_MAX_ROUNDS: usize = 15;
const DEFAULT_MAX_TOOL_CALLS: usize = 25;
const MAX_LOOP_REPEATS: usize = 3;
const DEFAULT_MAX_CONSECUTIVE_ERRORS: usize = 3;
const DEFAULT_MAX_ERROR_RECOVERIES: usize = 5; // tiger_cowork: 5 for resilience
const DEFAULT_COMPRESSION_INTERVAL: usize = 5;
const DEFAULT_COMPRESSION_WINDOW: usize = 10;
const DEFAULT_MAX_CONTEXT_TOKENS: usize = 100_000;
const OVERLOAD_MAX_RETRIES: usize = 4;
const LLM_MAX_RETRIES: usize = 3;
const ARG_TRUNCATE_THRESHOLD: usize = 4000;
const ARG_VALUE_TRUNCATE: usize = 500;
const CHECKPOINT_INTERVAL: usize = 5;

/// Load agent loop settings from settings.json (cached per call)
fn load_agent_settings() -> Value {
    std::fs::read_to_string(crate::server::data::data_dir().join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}))
}

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
    call_with_tools_inner(api_key, api_url, model, messages, system_prompt, sandbox_dir, on_update, sub_agent, false).await
}

pub async fn call_with_tools_realtime(
    api_key: &str,
    api_url: &str,
    model: &str,
    messages: Vec<Value>,
    system_prompt: Option<String>,
    sandbox_dir: &str,
    on_update: impl Fn(ToolUpdate) + Send + Sync + 'static,
    sub_agent: SubAgentConfig,
) -> ToolLoopResult {
    call_with_tools_inner(api_key, api_url, model, messages, system_prompt, sandbox_dir, on_update, sub_agent, true).await
}

// ---------------------------------------------------------------------------
// LLM call with Anthropic + OpenAI dual format support
// ---------------------------------------------------------------------------

/// Detect if the API URL is Anthropic native
fn is_anthropic_api(api_url: &str) -> bool {
    api_url.contains("anthropic.com")
}

/// Convert OpenAI-format messages to Anthropic format (extract system, transform messages)
fn to_anthropic_messages(messages: &[Value]) -> (Option<String>, Vec<Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut anthropic_msgs: Vec<Value> = Vec::new();

    for m in messages {
        let role = m["role"].as_str().unwrap_or("");
        match role {
            "system" => {
                if let Some(s) = m["content"].as_str() {
                    system_parts.push(s.to_string());
                }
            }
            "assistant" => {
                // Convert tool_calls to Anthropic content blocks
                let mut content_blocks: Vec<Value> = Vec::new();
                if let Some(text) = m["content"].as_str() {
                    if !text.is_empty() {
                        content_blocks.push(json!({"type": "text", "text": text}));
                    }
                }
                if let Some(tool_calls) = m["tool_calls"].as_array() {
                    for tc in tool_calls {
                        let name = tc["function"]["name"].as_str().unwrap_or("");
                        let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                        let args_val: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                        content_blocks.push(json!({
                            "type": "tool_use",
                            "id": tc["id"],
                            "name": name,
                            "input": args_val,
                        }));
                    }
                }
                if content_blocks.is_empty() {
                    content_blocks.push(json!({"type": "text", "text": ""}));
                }
                anthropic_msgs.push(json!({"role": "assistant", "content": content_blocks}));
            }
            "tool" => {
                // Convert tool result to Anthropic tool_result block
                anthropic_msgs.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": m["tool_call_id"],
                        "content": m["content"],
                    }],
                }));
            }
            "user" => {
                anthropic_msgs.push(json!({"role": "user", "content": m["content"]}));
            }
            _ => {
                anthropic_msgs.push(m.clone());
            }
        }
    }

    let system = if system_parts.is_empty() { None } else { Some(system_parts.join("\n\n")) };
    (system, anthropic_msgs)
}

/// Convert OpenAI tool definitions to Anthropic format
fn to_anthropic_tools(tools: &[Value]) -> Vec<Value> {
    tools.iter().filter_map(|t| {
        let func = t.get("function")?;
        Some(json!({
            "name": func["name"],
            "description": func["description"],
            "input_schema": func["parameters"],
        }))
    }).collect()
}

/// Unified LLM call supporting both Anthropic and OpenAI formats
/// Find the `codex` CLI binary.
/// Build a PATH string that includes common node/bun/homebrew bin dirs.
/// Needed because .app bundles launch with minimal PATH (/usr/bin:/bin).
pub fn cli_env_path() -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USER").map(|u| format!("/Users/{}", u)))
        .unwrap_or_else(|_| "/Users/sompoteyouwai".to_string());
    let extra_dirs = [
        format!("{}/.nvm/versions/node/v22.16.0/bin", home),
        format!("{}/.bun/bin", home),
        format!("{}/.npm/bin", home),
        format!("{}/.local/bin", home),
        format!("{}/.cargo/bin", home),
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
    ];
    let system_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".to_string());
    let mut parts: Vec<String> = Vec::new();
    for d in &extra_dirs {
        if std::path::Path::new(d).is_dir() && !system_path.contains(d.as_str()) {
            parts.push(d.clone());
        }
    }
    parts.push(system_path);
    parts.join(":")
}

/// Resolve the home directory (works even when launched from .app bundle).
pub fn resolve_home() -> String {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() { return h; }
    }
    if let Ok(u) = std::env::var("USER") {
        if !u.is_empty() { return format!("/Users/{}", u); }
    }
    // Last resort: ask the OS for the current username
    if let Ok(o) = std::process::Command::new("/usr/bin/id").args(["-un"]).output() {
        if o.status.success() {
            let user = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !user.is_empty() {
                return format!("/Users/{}", user);
            }
        }
    }
    "/Users/sompoteyouwai".to_string()
}

/// Find a node binary.
fn find_node() -> String {
    let home = resolve_home();
    let candidates = [
        format!("{}/.nvm/versions/node/v22.16.0/bin/node", home),
        "/usr/local/bin/node".to_string(),
        "/opt/homebrew/bin/node".to_string(),
    ];
    for c in &candidates {
        if std::path::Path::new(c).exists() {
            return c.to_string();
        }
    }
    "node".to_string()
}

/// Resolve a symlink to its real JS script path.
fn resolve_script(symlink: &str) -> Option<String> {
    let p = std::path::Path::new(symlink);
    if !p.exists() { return None; }
    // Try to canonicalize (follows symlinks AND resolves ..)
    if let Ok(canon) = p.canonicalize() {
        return Some(canon.to_string_lossy().to_string());
    }
    // Fallback: manual symlink resolve
    if let Ok(target) = std::fs::read_link(p) {
        let resolved = if target.is_relative() {
            p.parent().unwrap_or(std::path::Path::new("/")).join(&target)
        } else {
            target
        };
        if resolved.exists() {
            return Some(resolved.to_string_lossy().to_string());
        }
    }
    Some(symlink.to_string())
}

/// Find codex CLI. Returns (node_path, script_path) for direct node invocation.
/// Falls back to ("codex", "") if not found (uses shebang).
pub fn find_codex_cli() -> (String, String) {
    let home = resolve_home();
    let candidates = [
        format!("{}/.nvm/versions/node/v22.16.0/bin/codex", home),
        format!("{}/.npm/bin/codex", home),
        format!("{}/.local/bin/codex", home),
        "/usr/local/bin/codex".to_string(),
        "/opt/homebrew/bin/codex".to_string(),
    ];
    let node = find_node();
    for c in &candidates {
        if let Some(script) = resolve_script(c) {
            return (node.clone(), script);
        }
    }
    ("codex".to_string(), String::new())
}

/// Find claude CLI. Returns (node_path, script_path) for direct node invocation.
pub fn find_claude_cli() -> (String, String) {
    let home = resolve_home();
    let candidates = [
        format!("{}/.bun/bin/claude", home),
        format!("{}/.npm/bin/claude", home),
        format!("{}/.nvm/versions/node/v22.16.0/bin/claude", home),
        format!("{}/.local/bin/claude", home),
        "/usr/local/bin/claude".to_string(),
        "/opt/homebrew/bin/claude".to_string(),
    ];
    let node = find_node();
    for c in &candidates {
        if let Some(script) = resolve_script(c) {
            return (node.clone(), script);
        }
    }
    ("claude".to_string(), String::new())
}

/// Call Claude Code CLI instead of HTTP API.
async fn llm_call_claude_code(
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    max_tokens: u64,
) -> Result<Value, String> {
    // Build the prompt from messages
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut system_text = String::new();
    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");
        let content = msg["content"].as_str().unwrap_or("");
        if role == "system" {
            system_text.push_str(content);
            system_text.push('\n');
        } else if role == "user" {
            prompt_parts.push(content.to_string());
        } else if role == "assistant" {
            // Include assistant context
            if !content.is_empty() {
                prompt_parts.push(format!("[Previous assistant response: {}]", &content[..content.len().min(500)]));
            }
        }
    }

    let prompt = prompt_parts.join("\n\n");
    if prompt.is_empty() {
        return Err("No user message found".to_string());
    }

    // Build tool descriptions for Claude Code
    let mut tool_desc = String::new();
    if let Some(t) = tools {
        if !t.is_empty() {
            tool_desc.push_str("\n\n[TOOL CALLING INSTRUCTIONS]\nYou have these tools. You MUST call a tool when the task requires action.\nTo call a tool, output EXACTLY this JSON on its own line (no markdown, no backticks):\n");
            tool_desc.push_str("{\"tool_call\":{\"name\":\"TOOL_NAME\",\"arguments\":{...}}}\n\n");
            tool_desc.push_str("Available tools:\n");
            for tool in t {
                let name = tool["function"]["name"].as_str().unwrap_or("");
                let desc = tool["function"]["description"].as_str().unwrap_or("");
                let params = &tool["function"]["parameters"]["properties"];
                tool_desc.push_str(&format!("- {} : {} ", name, desc));
                if let Some(props) = params.as_object() {
                    let param_names: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
                    tool_desc.push_str(&format!("(params: {})", param_names.join(", ")));
                }
                tool_desc.push('\n');
            }
            tool_desc.push_str("\nIMPORTANT: If the task requires using a tool, you MUST output the JSON tool_call. Do NOT describe what you would do — actually call the tool.\n");
            tool_desc.push_str("Example: {\"tool_call\":{\"name\":\"web_search\",\"arguments\":{\"query\":\"TigrimOS market analysis\"}}}\n");
        }
    }

    let full_prompt = if system_text.is_empty() {
        format!("{}{}", prompt, tool_desc)
    } else {
        format!("{}\n\n{}{}", system_text, prompt, tool_desc)
    };

    let (node_bin, script_path) = find_claude_cli();
    info!("[ClaudeCode] LLM call via CLI (node: {}, script: {}, model: {}, prompt: {}chars)", node_bin, script_path, model, full_prompt.len());

    // Build args: if we have a script path, run `node script.js <args>`, else run `claude <args>`
    let mut cli_args: Vec<String> = Vec::new();
    if !script_path.is_empty() {
        cli_args.push(script_path);
    }
    cli_args.extend_from_slice(&[
        "-p".to_string(), full_prompt,
        "--output-format".to_string(), "text".to_string(),
    ]);
    if !model.is_empty() {
        cli_args.push("--model".to_string());
        cli_args.push(model.to_string());
    }

    let home = resolve_home();
    let result = timeout(
        Duration::from_secs(120),
        Command::new(&node_bin)
            .args(&cli_args)
            .env("PATH", cli_env_path())
            .env("HOME", &home)
            .env("CLAUDE_MAX_OUTPUT_TOKENS", max_tokens.to_string())
            .output(),
    ).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if !output.status.success() && stdout.is_empty() {
                return Err(format!("Claude Code CLI failed: {}", &stderr[..stderr.len().min(500)]));
            }

            // Try to extract tool calls from the response
            // Look for {"tool_call": ...} pattern anywhere in the text
            for line in stdout.lines() {
                let trimmed = line.trim().trim_start_matches("```json").trim_start_matches("```").trim();
                if trimmed.contains("\"tool_call\"") {
                    // Try to parse as JSON
                    if let Ok(tc_val) = serde_json::from_str::<Value>(trimmed) {
                        if let (Some(name), Some(args)) = (tc_val["tool_call"]["name"].as_str(), tc_val["tool_call"].get("arguments")) {
                            info!("[ClaudeCode] Extracted tool call: {}", name);
                            return Ok(json!({
                                "choices": [{
                                    "message": {
                                        "role": "assistant",
                                        "content": null,
                                        "tool_calls": [{
                                            "id": format!("call_{}", chrono::Utc::now().timestamp_millis()),
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": serde_json::to_string(args).unwrap_or_default(),
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }]
                            }));
                        }
                    }
                }
            }
            // Also try to find tool_call JSON embedded in text (with surrounding content)
            if let Some(tc_start) = stdout.find("{\"tool_call\"") {
                let remaining = &stdout[tc_start..];
                // Find matching closing brace
                let mut depth = 0;
                let mut end_idx = 0;
                for (i, ch) in remaining.char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 { end_idx = i + 1; break; }
                        }
                        _ => {}
                    }
                }
                if end_idx > 0 {
                    let tc_json = &remaining[..end_idx];
                    if let Ok(tc_val) = serde_json::from_str::<Value>(tc_json) {
                        if let (Some(name), Some(args)) = (tc_val["tool_call"]["name"].as_str(), tc_val["tool_call"].get("arguments")) {
                            info!("[ClaudeCode] Extracted embedded tool call: {}", name);
                            return Ok(json!({
                                "choices": [{
                                    "message": {
                                        "role": "assistant",
                                        "content": null,
                                        "tool_calls": [{
                                            "id": format!("call_{}", chrono::Utc::now().timestamp_millis()),
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": serde_json::to_string(args).unwrap_or_default(),
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }]
                            }));
                        }
                    }
                }
            }

            // Regular text response
            Ok(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": stdout.trim(),
                    },
                    "finish_reason": "stop"
                }]
            }))
        }
        Ok(Err(e)) => Err(format!("Failed to spawn claude CLI: {e}. Is 'claude' installed?")),
        Err(_) => Err("Claude Code CLI timed out (120s)".to_string()),
    }
}

/// Gemini CLI (Local) — spawn `gemini -p` and parse output
async fn llm_call_gemini_cli(
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    _max_tokens: u64,
) -> Result<Value, String> {
    // Build prompt from messages
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut system_text = String::new();
    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");
        let content = msg["content"].as_str().unwrap_or("");
        if role == "system" {
            system_text.push_str(content);
            system_text.push('\n');
        } else if role == "user" {
            prompt_parts.push(content.to_string());
        } else if role == "assistant" && !content.is_empty() {
            prompt_parts.push(format!("[Previous assistant response: {}]", &content[..content.len().min(500)]));
        }
    }

    let prompt = prompt_parts.join("\n\n");
    if prompt.is_empty() {
        return Err("No user message found".to_string());
    }

    // Build tool descriptions for prompt-based tool calling
    let mut tool_desc = String::new();
    if let Some(t) = tools {
        if !t.is_empty() {
            tool_desc.push_str("\n\n[TOOL CALLING INSTRUCTIONS]\nYou have these tools. You MUST call a tool when the task requires action.\nTo call a tool, output EXACTLY this JSON on its own line (no markdown, no backticks):\n");
            tool_desc.push_str("{\"tool_call\":{\"name\":\"TOOL_NAME\",\"arguments\":{...}}}\n\n");
            tool_desc.push_str("Available tools:\n");
            for tool in t {
                let name = tool["function"]["name"].as_str().unwrap_or("");
                let desc = tool["function"]["description"].as_str().unwrap_or("");
                let params = &tool["function"]["parameters"]["properties"];
                tool_desc.push_str(&format!("- {} : {} ", name, desc));
                if let Some(props) = params.as_object() {
                    let param_names: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
                    tool_desc.push_str(&format!("(params: {})", param_names.join(", ")));
                }
                tool_desc.push('\n');
            }
            tool_desc.push_str("\nIMPORTANT: If the task requires using a tool, you MUST output the JSON tool_call. Do NOT describe what you would do — actually call the tool.\n");
            tool_desc.push_str("Example: {\"tool_call\":{\"name\":\"web_search\",\"arguments\":{\"query\":\"TigrimOS market analysis\"}}}\n");
        }
    }

    let full_prompt = if system_text.is_empty() {
        format!("{}{}", prompt, tool_desc)
    } else {
        format!("{}\n\n{}{}", system_text, prompt, tool_desc)
    };

    info!("[GeminiCLI] LLM call via CLI (model: {}, prompt: {}chars)", model, full_prompt.len());

    let mut cli_args = vec![
        "-p".to_string(), full_prompt,
        "-o".to_string(), "text".to_string(),
        "--approval-mode".to_string(), "plan".to_string(), // read-only, no tool execution
    ];
    if !model.is_empty() {
        cli_args.push("-m".to_string());
        cli_args.push(model.to_string());
    }

    let home = resolve_home();
    let result = timeout(
        Duration::from_secs(180),
        Command::new("gemini")
            .args(&cli_args)
            .env("PATH", cli_env_path())
            .env("HOME", &home)
            .stderr(std::process::Stdio::piped())
            .output(),
    ).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = format!("{}\n{}", stdout, stderr);

            // Check for quota/API errors in output
            if combined.contains("QuotaError") || combined.contains("QUOTA_EXHAUSTED") || combined.contains("exhausted your capacity") {
                let msg = if let Some(pos) = combined.find("You have exhausted") {
                    let end = combined[pos..].find('\n').unwrap_or(combined.len() - pos);
                    &combined[pos..pos+end]
                } else { "Gemini API quota exhausted" };
                return Err(format!("Gemini: {}", msg));
            }
            if combined.contains("Error when talking to Gemini API") && stdout.trim().is_empty() {
                let err_line = combined.lines()
                    .find(|l| l.contains("Error") || l.contains("message:"))
                    .unwrap_or("Unknown Gemini API error");
                return Err(format!("Gemini API error: {}", &err_line[..err_line.len().min(300)]));
            }

            if !output.status.success() && stdout.trim().is_empty() {
                return Err(format!("Gemini CLI failed: {}", &stderr[..stderr.len().min(500)]));
            }

            // Filter out skill conflict warnings from stdout
            let clean_stdout: String = stdout.lines()
                .filter(|l| !l.contains("Skill conflict detected") && !l.contains("Loaded cached credentials"))
                .collect::<Vec<_>>()
                .join("\n");

            // Try to extract tool calls from the response
            let stdout = clean_stdout;
            for line in stdout.lines() {
                let trimmed = line.trim().trim_start_matches("```json").trim_start_matches("```").trim();
                if trimmed.contains("\"tool_call\"") {
                    if let Ok(tc_val) = serde_json::from_str::<Value>(trimmed) {
                        if let (Some(name), Some(args)) = (tc_val["tool_call"]["name"].as_str(), tc_val["tool_call"].get("arguments")) {
                            info!("[GeminiCLI] Extracted tool call: {}", name);
                            return Ok(json!({
                                "choices": [{
                                    "message": {
                                        "role": "assistant",
                                        "content": null,
                                        "tool_calls": [{
                                            "id": format!("call_{}", chrono::Utc::now().timestamp_millis()),
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": serde_json::to_string(args).unwrap_or_default(),
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }]
                            }));
                        }
                    }
                }
            }
            // Also try embedded tool_call JSON
            if let Some(tc_start) = stdout.find("{\"tool_call\"") {
                let remaining = &stdout[tc_start..];
                let mut depth = 0;
                let mut end_idx = 0;
                for (i, ch) in remaining.char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => { depth -= 1; if depth == 0 { end_idx = i + 1; break; } }
                        _ => {}
                    }
                }
                if end_idx > 0 {
                    if let Ok(tc_val) = serde_json::from_str::<Value>(&remaining[..end_idx]) {
                        if let (Some(name), Some(args)) = (tc_val["tool_call"]["name"].as_str(), tc_val["tool_call"].get("arguments")) {
                            info!("[GeminiCLI] Extracted embedded tool call: {}", name);
                            return Ok(json!({
                                "choices": [{
                                    "message": {
                                        "role": "assistant",
                                        "content": null,
                                        "tool_calls": [{
                                            "id": format!("call_{}", chrono::Utc::now().timestamp_millis()),
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": serde_json::to_string(args).unwrap_or_default(),
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }]
                            }));
                        }
                    }
                }
            }

            // Regular text response
            Ok(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": stdout.trim(),
                    },
                    "finish_reason": "stop"
                }]
            }))
        }
        Ok(Err(e)) => Err(format!("Failed to spawn gemini CLI: {e}. Is 'gemini' in PATH?")),
        Err(_) => Err("Gemini CLI timed out (120s)".to_string()),
    }
}

/// Call Codex CLI instead of HTTP API.
async fn llm_call_codex_cli(
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    _max_tokens: u64,
) -> Result<Value, String> {
    // Build prompt from messages (system + user combined)
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut system_text = String::new();
    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");
        let content = msg["content"].as_str().unwrap_or("");
        if role == "system" {
            system_text.push_str(content);
            system_text.push('\n');
        } else if role == "user" {
            prompt_parts.push(content.to_string());
        }
    }

    let task = prompt_parts.join("\n\n");
    if task.is_empty() {
        return Err("No user message found".to_string());
    }

    // Build tool descriptions for the prompt
    let mut tool_desc = String::new();
    if let Some(t) = tools {
        if !t.is_empty() {
            tool_desc.push_str("\n\n[TOOL CALLING INSTRUCTIONS]\nYou have these tools. You MUST call a tool when the task requires action.\nTo call a tool, output EXACTLY this JSON on its own line:\n");
            tool_desc.push_str("{\"tool_call\":{\"name\":\"TOOL_NAME\",\"arguments\":{...}}}\n\n");
            tool_desc.push_str("Available tools:\n");
            for tool in t {
                let name = tool["function"]["name"].as_str().unwrap_or("");
                let desc = tool["function"]["description"].as_str().unwrap_or("");
                tool_desc.push_str(&format!("- {}: {}\n", name, desc));
            }
            tool_desc.push_str("\nIMPORTANT: Output the JSON tool_call to use a tool. Do NOT just describe — actually call it.\n");
        }
    }

    // Combine: system prompt + task + tool instructions
    let full_prompt = if system_text.is_empty() {
        format!("{}{}", task, tool_desc)
    } else {
        format!("{}\n\n---\n\nTASK:\n{}{}", system_text.trim(), task, tool_desc)
    };

    let (node_bin, script_path) = find_codex_cli();
    let sandbox_dir = {
        let s = load_agent_settings();
        let sd = s["sandboxDir"].as_str().unwrap_or("./sandbox");
        let raw = if sd.is_empty() { "./sandbox" } else { sd };
        // Make absolute relative to data dir
        let p = std::path::Path::new(raw);
        if p.is_relative() {
            crate::server::data::data_dir().join(raw).to_string_lossy().to_string()
        } else {
            raw.to_string()
        }
    };
    // Ensure sandbox dir exists
    let _ = std::fs::create_dir_all(&sandbox_dir);
    let home = resolve_home();
    info!("[Codex] LLM call via CLI (node: {}, script: {}, model: {}, prompt: {}chars, cwd: {})", node_bin, script_path, model, full_prompt.len(), sandbox_dir);

    // Build args: `node codex.js exec "<prompt>" --json --full-auto [-m <model>]`
    let mut cli_args: Vec<String> = Vec::new();
    if !script_path.is_empty() {
        cli_args.push(script_path.clone());
    }
    cli_args.extend_from_slice(&[
        "exec".to_string(),
        full_prompt.clone(),
        "--json".to_string(),
        "--full-auto".to_string(),
    ]);
    if !model.is_empty() {
        cli_args.push("-m".to_string());
        cli_args.push(model.to_string());
    }

    let result = timeout(
        Duration::from_secs(180),
        Command::new(&node_bin)
            .args(&cli_args)
            .current_dir(&sandbox_dir)
            .env("PATH", cli_env_path())
            .env("HOME", &home)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    ).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if !output.status.success() && stdout.trim().is_empty() {
                return Err(format!("Codex CLI failed: {}", &stderr[..stderr.len().min(500)]));
            }

            // Parse JSONL events from codex --json output
            let mut result_text = String::new();
            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }

                // First check for our tool_call format in the text
                if trimmed.contains("\"tool_call\"") {
                    let clean = trimmed.trim_start_matches("```json").trim_start_matches("```").trim();
                    if let Ok(tc_val) = serde_json::from_str::<Value>(clean) {
                        if let (Some(name), Some(args)) = (tc_val["tool_call"]["name"].as_str(), tc_val["tool_call"].get("arguments")) {
                            info!("[Codex] Extracted tool call: {}", name);
                            return Ok(json!({
                                "choices": [{
                                    "message": {
                                        "role": "assistant",
                                        "content": null,
                                        "tool_calls": [{
                                            "id": format!("call_{}", chrono::Utc::now().timestamp_millis()),
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": serde_json::to_string(args).unwrap_or_default(),
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }]
                            }));
                        }
                    }
                }

                // Parse codex JSONL events
                if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
                    let event_type = event["type"].as_str().unwrap_or("");

                    if event_type == "item.completed" {
                        let item = &event["item"];
                        let item_type = item["type"].as_str().unwrap_or("");

                        if item_type == "agent_message" {
                            if let Some(text) = item["text"].as_str() {
                                result_text.push_str(text);
                            }
                        } else if item_type == "command_execution" {
                            if let Some(out) = item["aggregated_output"].as_str() {
                                result_text.push_str(out);
                            }
                        } else if item_type == "message" {
                            // Handle content array or string
                            if let Some(content) = item["content"].as_array() {
                                for block in content {
                                    if let Some(text) = block["text"].as_str().or(block["output"].as_str()) {
                                        result_text.push_str(text);
                                    } else if let Some(s) = block.as_str() {
                                        result_text.push_str(s);
                                    }
                                }
                            }
                        }
                    } else if event_type == "message" {
                        if let Some(c) = event["content"].as_str() {
                            result_text.push_str(c);
                        }
                    }
                } else if !trimmed.starts_with('{') {
                    // Plain text line (not JSON)
                    result_text.push_str(trimmed);
                    result_text.push('\n');
                }
            }

            // Also check result_text for embedded tool calls
            for line in result_text.lines() {
                let clean = line.trim().trim_start_matches("```json").trim_start_matches("```").trim();
                if clean.contains("\"tool_call\"") {
                    if let Ok(tc_val) = serde_json::from_str::<Value>(clean) {
                        if let (Some(name), Some(args)) = (tc_val["tool_call"]["name"].as_str(), tc_val["tool_call"].get("arguments")) {
                            info!("[Codex] Extracted tool call from result text: {}", name);
                            return Ok(json!({
                                "choices": [{
                                    "message": {
                                        "role": "assistant",
                                        "content": null,
                                        "tool_calls": [{
                                            "id": format!("call_{}", chrono::Utc::now().timestamp_millis()),
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": serde_json::to_string(args).unwrap_or_default(),
                                            }
                                        }]
                                    },
                                    "finish_reason": "tool_calls"
                                }]
                            }));
                        }
                    }
                }
            }

            let final_text = result_text.trim();
            let content = if final_text.is_empty() { stdout.trim() } else { final_text };

            Ok(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": content,
                    },
                    "finish_reason": "stop"
                }]
            }))
        }
        Ok(Err(e)) => Err(format!("Failed to spawn codex CLI (node={}, script={}, cwd={}): {e}", node_bin, script_path, sandbox_dir)),
        Err(_) => Err("Codex CLI timed out (180s)".to_string()),
    }
}

/// Sanitize messages before sending to LLM API (ported from tiger_cowork sanitizeMessages).
/// Handles: null content, orphaned tool pairs, mid-conversation system messages,
/// consecutive user messages, missing user before assistant, reasoning_content stripping.
fn sanitize_messages(messages: &[Value]) -> Vec<Value> {
    // 1. Collect valid tool_call IDs and tool result IDs
    let mut valid_tc_ids = std::collections::HashSet::new();
    let mut tool_result_ids = std::collections::HashSet::new();
    for msg in messages {
        if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    valid_tc_ids.insert(id.to_string());
                }
            }
        }
        if msg["role"].as_str() == Some("tool") {
            if let Some(id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                tool_result_ids.insert(id.to_string());
            }
        }
    }

    // 2. Filter and fix messages
    let mut filtered: Vec<Value> = Vec::new();
    for msg in messages {
        let mut m = msg.clone();
        let role = m["role"].as_str().unwrap_or("").to_string();

        // Remove tool results referencing non-existent tool calls
        if role == "tool" {
            if let Some(id) = m.get("tool_call_id").and_then(|v| v.as_str()) {
                if !valid_tc_ids.contains(id) { continue; }
            }
        }

        // Remove orphaned assistant tool_calls (no matching results)
        if role == "assistant" {
            if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
                if !tcs.is_empty() {
                    let tc_ids: Vec<String> = tcs.iter()
                        .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .collect();
                    let has_any_result = tc_ids.iter().any(|id| tool_result_ids.contains(id));
                    if !tc_ids.is_empty() && !has_any_result {
                        // Strip tool_calls, keep as plain assistant message
                        m.as_object_mut().map(|o| o.remove("tool_calls"));
                    } else {
                        // Ensure every tool_call has type: "function"
                        if let Some(tcs) = m.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                            for tc in tcs {
                                if tc.get("type").is_none() {
                                    tc["type"] = json!("function");
                                }
                            }
                        }
                    }
                }
            }
        }

        // Ensure content is never null/empty for assistant messages
        if role == "assistant" {
            let content_empty = match &m["content"] {
                Value::Null => true,
                Value::String(s) => s.is_empty(),
                _ => false,
            };
            if content_empty {
                m["content"] = json!("(thinking...)");
            }
            // Strip reasoning_content
            m.as_object_mut().map(|o| o.remove("reasoning_content"));
        }

        // Ensure content is never null for other roles
        if m["content"].is_null() && role != "assistant" {
            m["content"] = json!("");
        }

        filtered.push(m);
    }

    // 3. Merge consecutive system messages
    let mut merged: Vec<Value> = Vec::new();
    for msg in filtered {
        if msg["role"].as_str() == Some("system") {
            if let Some(last) = merged.last_mut() {
                if last["role"].as_str() == Some("system") {
                    let prev = last["content"].as_str().unwrap_or("");
                    let cur = msg["content"].as_str().unwrap_or("");
                    last["content"] = json!(format!("{}\n\n{}", prev, cur));
                    continue;
                }
            }
        }
        merged.push(msg);
    }

    // 4. Convert mid-conversation system messages to user role
    let mut seen_first_system = false;
    for msg in merged.iter_mut() {
        if msg["role"].as_str() == Some("system") {
            if seen_first_system {
                msg["role"] = json!("user");
                let content = msg["content"].as_str().unwrap_or("").to_string();
                msg["content"] = json!(format!("[System Instructions]\n{}", content));
            }
            seen_first_system = true;
        }
    }

    // 5. Merge consecutive user messages
    let mut deduped: Vec<Value> = Vec::new();
    for msg in merged {
        if msg["role"].as_str() == Some("user") {
            if let Some(last) = deduped.last_mut() {
                if last["role"].as_str() == Some("user")
                    && last["content"].is_string()
                    && msg["content"].is_string()
                {
                    let prev = last["content"].as_str().unwrap_or("");
                    let cur = msg["content"].as_str().unwrap_or("");
                    last["content"] = json!(format!("{}\n\n{}", prev, cur));
                    continue;
                }
            }
        }
        deduped.push(msg);
    }

    // 6. Ensure a user message exists before the first assistant message
    let first_non_system = deduped.iter().position(|m| m["role"].as_str() != Some("system"));
    if let Some(idx) = first_non_system {
        if deduped[idx]["role"].as_str() != Some("user") {
            deduped.insert(idx, json!({"role": "user", "content": "Continue with the task."}));
        }
    }

    deduped
}

async fn llm_call(
    client: &Client,
    api_key: &str,
    api_url: &str,
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    temperature: f64,
    max_tokens: u64,
) -> Result<Value, String> {
    // Route to local CLI providers
    if api_url.starts_with("claude-code") {
        return llm_call_claude_code(model, messages, tools, max_tokens).await;
    }
    if api_url.starts_with("gemini-cli") {
        return llm_call_gemini_cli(model, messages, tools, max_tokens).await;
    }
    if api_url.starts_with("codex-cli") {
        return llm_call_codex_cli(model, messages, tools, max_tokens).await;
    }

    // Sanitize messages before sending (tiger_cowork: sanitizeMessages)
    let messages = sanitize_messages(messages);

    let is_anthropic = is_anthropic_api(api_url);

    let (body, url, headers) = if is_anthropic {
        let (system, anthropic_msgs) = to_anthropic_messages(&messages);
        let mut body = json!({
            "model": model,
            "messages": anthropic_msgs,
            "temperature": temperature,
            "max_tokens": max_tokens,
        });
        if let Some(sys) = system {
            body["system"] = json!(sys);
        }
        if let Some(t) = tools {
            if !t.is_empty() {
                body["tools"] = json!(to_anthropic_tools(t));
                body["tool_choice"] = json!({"type": "auto"});
            }
        }
        let mut hdrs = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("x-api-key".to_string(), api_key.to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ];
        // Support beta features if needed
        let _ = &mut hdrs;
        (body, api_url.to_string(), hdrs)
    } else {
        let mut body = json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
        });
        if let Some(t) = tools {
            if !t.is_empty() {
                body["tools"] = json!(t);
                body["tool_choice"] = json!("auto");
            }
        }
        let mut hdrs = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
        ];
        // Kimi Code API requires Claude Code identity headers
        if api_url.contains("api.kimi.com") {
            hdrs.push(("User-Agent".to_string(), "claude-code/1.0.6".to_string()));
            hdrs.push(("X-Client-Name".to_string(), "claude-code".to_string()));
            hdrs.push(("X-Client-Version".to_string(), "1.0.6".to_string()));
            hdrs.push(("HTTP-Referer".to_string(), "https://claude.ai".to_string()));
            hdrs.push(("X-Traffic-Source".to_string(), "claude-code".to_string()));
        }
        (body, api_url.to_string(), hdrs)
    };

    let mut req = client.post(&url);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let response = req.json(&body).send().await.map_err(|e| format!("API request failed: {e}"))?;
    let resp_json: Value = response.json().await.map_err(|e| format!("Failed to parse API response: {e}"))?;

    // Normalize Anthropic response to OpenAI format
    if is_anthropic {
        return Ok(normalize_anthropic_response(&resp_json));
    }

    Ok(resp_json)
}

/// Convert Anthropic response format to OpenAI format for unified processing
fn normalize_anthropic_response(resp: &Value) -> Value {
    // Check for Anthropic error
    if let Some(err) = resp.get("error") {
        return json!({"error": err});
    }

    let content_blocks = resp["content"].as_array();
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(blocks) = content_blocks {
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        text_parts.push(t.to_string());
                    }
                }
                Some("tool_use") => {
                    tool_calls.push(json!({
                        "id": block["id"],
                        "type": "function",
                        "function": {
                            "name": block["name"],
                            "arguments": serde_json::to_string(&block["input"]).unwrap_or_else(|_| "{}".to_string()),
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let stop_reason = resp["stop_reason"].as_str().unwrap_or("stop");
    let finish_reason = if stop_reason == "tool_use" { "tool_calls" } else { "stop" };

    let mut message = json!({
        "role": "assistant",
        "content": text_parts.join(""),
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    json!({
        "choices": [{
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": resp.get("usage").cloned().unwrap_or(json!({})),
    })
}

// ---------------------------------------------------------------------------
// Tool argument truncation (tiger_cowork: truncate >4000 char args)
// ---------------------------------------------------------------------------

fn truncate_tool_call_args(tool_calls: &[Value]) -> Vec<Value> {
    tool_calls.iter().map(|tc| {
        let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
        if args_str.len() <= ARG_TRUNCATE_THRESHOLD {
            return tc.clone();
        }
        // Build valid JSON summary instead of slicing mid-string
        let truncated_args = match serde_json::from_str::<Value>(args_str) {
            Ok(parsed) => {
                if let Some(obj) = parsed.as_object() {
                    let mut summary = serde_json::Map::new();
                    for (key, val) in obj {
                        if let Some(s) = val.as_str() {
                            if s.len() > ARG_VALUE_TRUNCATE {
                                summary.insert(key.clone(), json!(format!("{}...(truncated)", &s[..ARG_VALUE_TRUNCATE])));
                            } else {
                                summary.insert(key.clone(), val.clone());
                            }
                        } else {
                            summary.insert(key.clone(), val.clone());
                        }
                    }
                    serde_json::to_string(&Value::Object(summary)).unwrap_or_else(|_| args_str.to_string())
                } else {
                    args_str[..ARG_TRUNCATE_THRESHOLD].to_string()
                }
            }
            Err(_) => {
                serde_json::to_string(&json!({"_truncated": &args_str[..3000.min(args_str.len())]})).unwrap_or_default()
            }
        };
        let mut tc = tc.clone();
        tc["function"]["arguments"] = json!(truncated_args);
        tc
    }).collect()
}

// ---------------------------------------------------------------------------
// JSON parse recovery for malformed tool args (tiger_cowork)
// ---------------------------------------------------------------------------

fn recover_tool_args(tool_name: &str, raw_args: &str) -> Option<Value> {
    if !matches!(tool_name, "run_python" | "run_react" | "run_shell") {
        return None;
    }
    // Try to extract "code" value via regex
    let code_re = regex::Regex::new(r#""code"\s*:\s*"((?:[^"\\]|\\.)*)""#).ok()?;
    if let Some(cap) = code_re.captures(raw_args) {
        let code = cap[1].replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\");
        let mut args = serde_json::Map::new();
        args.insert("code".to_string(), json!(code));
        // Try to recover title
        let title_re = regex::Regex::new(r#""title"\s*:\s*"((?:[^"\\]|\\.)*)""#).ok()?;
        if let Some(tcap) = title_re.captures(raw_args) {
            args.insert("title".to_string(), json!(&tcap[1]));
        }
        return Some(Value::Object(args));
    }
    None
}

// ---------------------------------------------------------------------------
// Pending work check (tiger_cowork: prevent premature stop)
// ---------------------------------------------------------------------------

async fn check_pending_work(session_id: &str) -> (Vec<String>, Vec<(String, AgentResult)>, Vec<String>) {
    let mut working_agents = Vec::new();
    let mut pending_results = Vec::new();
    let mut pending_bb_tasks = Vec::new();

    // Check realtime session for working agents
    let map = realtime_sessions().lock().await;
    if let Some(session_arc) = map.get(session_id) {
        let session = session_arc.lock().await;
        for (id, handle) in &session.agents {
            let status = handle.status.lock().await.clone();
            if status == "working" {
                working_agents.push(id.clone());
            }
        }
        // Collect pending results
        let results = session.results.lock().await;
        for (id, result) in results.iter() {
            pending_results.push((id.clone(), result.clone()));
        }
    }
    drop(map);

    // Check blackboard for unfinished tasks
    let bb_tasks = protocols::blackboard_get_tasks(session_id, Some("awarded")).await;
    for t in &bb_tasks {
        pending_bb_tasks.push(t.task_id.clone());
    }
    let bb_open = protocols::blackboard_get_tasks(session_id, Some("open")).await;
    for t in &bb_open {
        pending_bb_tasks.push(t.task_id.clone());
    }

    (working_agents, pending_results, pending_bb_tasks)
}

// ---------------------------------------------------------------------------
// Checkpoint save/load (tiger_cowork: save every 5 rounds)
// ---------------------------------------------------------------------------

async fn save_checkpoint(
    session_id: &str,
    round: usize,
    all_messages: &[Value],
    tool_records: &[ToolCallRecord],
    total_tool_calls: usize,
    collected_files: &[String],
    tool_call_history: &[String],
    consecutive_errors: usize,
    early_content: &str,
) {
    if session_id.is_empty() { return; }
    let dir = "data/checkpoints";
    let _ = tokio::fs::create_dir_all(dir).await;
    let fp = format!("{}/{}.json", dir, session_id);

    // Compress checkpoint: only keep last 20 messages fully
    let compact_messages = if all_messages.len() > 30 {
        let mut msgs = Vec::new();
        msgs.extend_from_slice(&all_messages[..2.min(all_messages.len())]);
        msgs.push(json!({"role": "system", "content": format!("[Checkpoint: {} earlier messages omitted]", all_messages.len().saturating_sub(22))}));
        msgs.extend_from_slice(&all_messages[all_messages.len().saturating_sub(20)..]);
        msgs
    } else {
        all_messages.to_vec()
    };

    // Compact tool results
    let compact_records: Vec<Value> = tool_records.iter().map(|tr| {
        json!({
            "tool": tr.tool,
            "result": {
                "ok": tr.result.get("ok"),
                "exitCode": tr.result.get("exit_code"),
                "outputFiles": tr.result.get("output_files"),
                "stdout": tr.result.get("stdout").and_then(|v| v.as_str()).map(|s| &s[..s.len().min(2000)]),
                "stderr": tr.result.get("stderr").and_then(|v| v.as_str()).map(|s| &s[..s.len().min(1000)]),
                "error": tr.result.get("error"),
            }
        })
    }).collect();

    let checkpoint = json!({
        "round": round,
        "totalToolCalls": total_tool_calls,
        "messages": compact_messages,
        "toolRecords": compact_records,
        "files": collected_files,
        "toolCallHistory": tool_call_history,
        "consecutiveErrors": consecutive_errors,
        "earlyContent": if early_content.is_empty() { Value::Null } else { json!(early_content) },
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    let _ = tokio::fs::write(&fp, serde_json::to_string_pretty(&checkpoint).unwrap_or_default()).await;
}

/// Local checkpoint data matching what save_checkpoint writes
struct LocalCheckpoint {
    round: usize,
    messages: Vec<Value>,
    total_tool_calls: usize,
    files: Vec<String>,
    tool_call_history: Vec<String>,
    consecutive_errors: usize,
    early_content: Option<String>,
}

async fn load_checkpoint(session_id: &str) -> Option<LocalCheckpoint> {
    if session_id.is_empty() { return None; }
    let fp = format!("data/checkpoints/{}.json", session_id);
    let content = tokio::fs::read_to_string(&fp).await.ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    Some(LocalCheckpoint {
        round: v["round"].as_u64().unwrap_or(0) as usize,
        messages: v["messages"].as_array().cloned().unwrap_or_default(),
        total_tool_calls: v["totalToolCalls"].as_u64().unwrap_or(0) as usize,
        files: v["files"].as_array()
            .map(|arr| arr.iter().filter_map(|f| f.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default(),
        tool_call_history: v["toolCallHistory"].as_array()
            .map(|arr| arr.iter().filter_map(|f| f.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default(),
        consecutive_errors: v["consecutiveErrors"].as_u64().unwrap_or(0) as usize,
        early_content: v["earlyContent"].as_str().map(|s| s.to_string()),
    })
}

async fn clear_checkpoint(session_id: &str) {
    if session_id.is_empty() { return; }
    let fp = format!("data/checkpoints/{}.json", session_id);
    let _ = tokio::fs::remove_file(&fp).await;
}

// ---------------------------------------------------------------------------
// Main tool loop — faithful port of tiger_cowork callTigerBotWithTools()
// ---------------------------------------------------------------------------

async fn call_with_tools_inner(
    api_key: &str,
    api_url: &str,
    model: &str,
    messages: Vec<Value>,
    system_prompt: Option<String>,
    sandbox_dir: &str,
    on_update: impl Fn(ToolUpdate) + Send + Sync + 'static,
    sub_agent: SubAgentConfig,
    realtime: bool,
) -> ToolLoopResult {
    let on_update = std::sync::Arc::new(on_update);
    let client = Client::new();
    // Track whether a swarm/architecture has been activated this session
    let mut session_activated = realtime; // realtime mode is pre-activated
    let mut tools = if sub_agent.enabled {
        let t = tool_definitions_for_mode(&sub_agent, realtime, session_activated);
        info!("[call_with_tools] mode={}, enabled={}, agents={:?}, tools={}", sub_agent.mode, sub_agent.enabled, sub_agent.agent_ids, t.iter().filter_map(|td| td["function"]["name"].as_str()).collect::<Vec<_>>().join(", "));
        t
    } else {
        info!("[call_with_tools] sub_agent disabled, using default tools");
        tool_definitions()
    };

    // Inject MCP tools from connected servers
    let mcp_tools = mcp::get_mcp_tools().await;
    if !mcp_tools.is_empty() {
        info!("[call_with_tools] Adding {} MCP tool(s): {}", mcp_tools.len(),
            mcp_tools.iter().filter_map(|t| t["function"]["name"].as_str()).collect::<Vec<_>>().join(", "));
        tools.extend(mcp_tools);
    }

    // Orchestrators get reduced tools — research delegated to workers
    if sub_agent.agent_role == "orchestrator" {
        tools.retain(|t| {
            let name = t["function"]["name"].as_str().unwrap_or("");
            !matches!(name, "web_search" | "fetch_url")
        });
    }

    // Load settings — realtime agents get higher limits since orchestrators need many rounds
    let settings = load_agent_settings();
    let base_max_rounds = settings["agentMaxToolRounds"].as_u64().unwrap_or(DEFAULT_MAX_ROUNDS as u64) as usize;
    let base_max_tool_calls = settings["agentMaxToolCalls"].as_u64().unwrap_or(DEFAULT_MAX_TOOL_CALLS as u64) as usize;
    let max_rounds = if realtime { base_max_rounds.max(30) } else { base_max_rounds };
    let max_tool_calls = if realtime { base_max_tool_calls.max(60) } else { base_max_tool_calls };
    let max_consecutive_errors = settings["agentMaxConsecutiveErrors"].as_u64().unwrap_or(DEFAULT_MAX_CONSECUTIVE_ERRORS as u64) as usize;
    let max_error_recoveries = settings["agentMaxErrorRecoveries"].as_u64().unwrap_or(DEFAULT_MAX_ERROR_RECOVERIES as u64) as usize;
    let compression_interval = settings["agentCompressionInterval"].as_u64().unwrap_or(DEFAULT_COMPRESSION_INTERVAL as u64) as usize;
    let compression_window = settings["agentCompressionWindow"].as_u64().unwrap_or(DEFAULT_COMPRESSION_WINDOW as u64) as usize;
    let max_context_tokens = settings["agentMaxContextTokens"].as_u64().unwrap_or(DEFAULT_MAX_CONTEXT_TOKENS as u64) as usize;
    let temperature = settings["agentTemperature"].as_f64().unwrap_or(0.7);
    let max_tokens = settings["agentMaxTokens"].as_u64().unwrap_or(81920);
    let reflection_enabled = settings["agentReflectionEnabled"].as_bool().unwrap_or(false);
    let eval_threshold = settings["agentReflectionThreshold"].as_f64().unwrap_or(0.7);
    let max_reflection_retries = settings["agentMaxReflectionRetries"].as_u64().unwrap_or(2) as usize;
    let tool_result_max_len = settings["agentToolResultMaxLen"].as_u64().unwrap_or(6000).min(100_000) as usize;
    let checkpoint_enabled = settings["agentCheckpointEnabled"].as_bool().unwrap_or(true);

    // --- Checkpoint resume (tiger_cowork: try to resume from checkpoint) ---
    let mut all_messages = Vec::new();
    let mut tool_records: Vec<ToolCallRecord> = Vec::new();
    let mut collected_files: Vec<String> = Vec::new();
    let mut total_tool_calls: usize = 0;
    #[allow(unused_assignments)]
    let mut consecutive_errors: usize = 0;
    let mut error_recovery_attempts: usize = 0;
    let mut no_choices_retries: usize = 0;
    let mut _uses_skill = false;
    let mut early_content = String::new();
    let mut start_round: usize = 0;
    // For loop detection: track recent (tool_name, args_signature) tuples
    let mut recent_signatures: Vec<String> = Vec::new();
    // For tracking tool call history (loop detection)
    let mut tool_call_history: Vec<String> = Vec::new();

    if checkpoint_enabled && !sub_agent.session_id.is_empty() {
        if let Some(checkpoint) = load_checkpoint(&sub_agent.session_id).await {
            info!("[ToolLoop] Resuming from checkpoint at round {}", checkpoint.round);
            all_messages = checkpoint.messages;
            total_tool_calls = checkpoint.total_tool_calls;
            collected_files = checkpoint.files;
            tool_call_history = checkpoint.tool_call_history;
            consecutive_errors = checkpoint.consecutive_errors;
            if let Some(ec) = checkpoint.early_content {
                early_content = ec;
            }
            start_round = checkpoint.round;
        }
    }

    // Initialize messages if not resuming from checkpoint
    if all_messages.is_empty() {
        if let Some(sys) = &system_prompt {
            all_messages.push(json!({ "role": "system", "content": sys }));
        }
        all_messages.extend(messages);
    }

    // Extract user objective for reflection (first user message)
    let user_objective: String = all_messages.iter()
        .find(|m| m["role"].as_str() == Some("user"))
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .chars().take(2000)
        .collect();

    // Check if user wants output files (charts/graphs)
    let user_wants_output = {
        let lower = user_objective.to_lowercase();
        lower.contains("chart") || lower.contains("graph") || lower.contains("plot")
            || lower.contains("diagram") || lower.contains("visualiz")
            || lower.contains("figure") || lower.contains("draw")
    };

    for round in start_round..max_rounds {
        // --- Abort check: save checkpoint and return early (tiger_cowork: signal.aborted) ---
        if sub_agent.cancel_flag.load(Ordering::Relaxed) {
            info!("[ToolLoop] Abort signal received at round {} — saving checkpoint", round);
            if checkpoint_enabled {
                save_checkpoint(
                    &sub_agent.session_id, round, &all_messages, &tool_records,
                    total_tool_calls, &collected_files, &tool_call_history,
                    consecutive_errors, &early_content,
                ).await;
            }
            let content = if early_content.is_empty() {
                "Task was cancelled.".to_string()
            } else {
                early_content
            };
            return ToolLoopResult {
                content,
                tool_results: tool_records,
                files: collected_files,
            };
        }

        info!("Tool loop round {}/{}", round + 1, max_rounds);

        // --- Context compression ---
        // Periodic compression every N rounds
        if round > 0 && round % compression_interval == 0 {
            let compressed = compact::compress_older_messages(
                &all_messages, compression_window, api_key, api_url, model, None,
            ).await;
            if compressed.len() < all_messages.len() {
                info!("[ToolLoop] Periodic compression: {} -> {} messages", all_messages.len(), compressed.len());
                all_messages = compressed;
            }
        }

        // Proactive compaction: estimate tokens and compress if over budget
        let estimated_tokens = compact::estimate_messages_chars(&all_messages) / 4;
        if estimated_tokens > max_context_tokens {
            info!("[ToolLoop] Context ~{} tokens exceeds limit {} — compacting...", estimated_tokens, max_context_tokens);
            let compressed = compact::compress_older_messages(
                &all_messages, compression_window.min(6), api_key, api_url, model, None,
            ).await;
            if compressed.len() < all_messages.len() {
                all_messages = compressed;
                info!("[ToolLoop] Compacted to ~{} tokens ({} messages)",
                    compact::estimate_messages_chars(&all_messages) / 4, all_messages.len());
            }
        }

        // Safety fallback: naive trim if still over budget
        let trimmed = compact::trim_conversation_context(&all_messages, 6_000_000);
        if trimmed.len() < all_messages.len() {
            all_messages = trimmed;
        }

        // Validate message structure after trimming (tiger_cowork: tool-pair consistency)
        let validated = compact::validate_message_structure(&all_messages);
        if validated.messages.len() != all_messages.len() {
            all_messages = validated.messages;
        }

        // Periodic checkpoint save
        if round > 0 && round % CHECKPOINT_INTERVAL == 0 {
            save_checkpoint(
                &sub_agent.session_id, round, &all_messages, &tool_records,
                total_tool_calls, &collected_files, &tool_call_history,
                consecutive_errors, &early_content,
            ).await;
        }

        // --- LLM call with retry logic (tiger_cowork: 3 retries + overload backoff) ---
        // Note: sanitize_messages() is called inside llm_call() before sending to API
        let mut data: Option<Value> = None;
        let mut overload_retry_count: usize = 0;

        for llm_retry in 0..LLM_MAX_RETRIES {
            match llm_call(
                &client, api_key, api_url, model, &all_messages,
                Some(&tools), temperature, max_tokens,
            ).await {
                Ok(resp) => {
                    data = Some(resp);
                    break;
                }
                Err(err_msg) => {
                    // Context overflow: compress before retrying
                    if compact::is_context_overflow(&err_msg) && all_messages.len() > 3 {
                        info!("[ToolLoop] Context overflow detected — compressing before retry (attempt {}/{})...", llm_retry + 1, LLM_MAX_RETRIES);
                        let force_opts = compact::CompactOptions { force: true };
                        let compressed = compact::compress_older_messages(
                            &all_messages, compression_window.min(6), api_key, api_url, model, Some(&force_opts),
                        ).await;
                        if compressed.len() < all_messages.len() {
                            all_messages = compact::validate_message_structure(&compressed).messages;
                        } else if llm_retry >= 1 {
                            // Retries 2+: target single largest tool result (tiger_cowork)
                            let before_chars = compact::estimate_messages_chars(&all_messages);
                            compact::truncate_largest_tool_result(&mut all_messages, 4000);
                            let after_chars = compact::estimate_messages_chars(&all_messages);
                            if after_chars < before_chars {
                                all_messages = compact::validate_message_structure(&all_messages).messages;
                                info!("[ToolLoop] Truncated largest tool result: {} → {} chars", before_chars, after_chars);
                            } else {
                                // No tool message large enough — fall back to halving trim
                                let trimmed = compact::trim_conversation_context(&all_messages, before_chars / 2);
                                all_messages = compact::validate_message_structure(&trimmed).messages;
                                info!("[ToolLoop] Trimmed to {} messages ({} chars)", all_messages.len(), compact::estimate_messages_chars(&all_messages));
                            }
                        } else {
                            // First retry: halving trim path
                            let current_chars = compact::estimate_messages_chars(&all_messages);
                            let trimmed = compact::trim_conversation_context(&all_messages, current_chars / 2);
                            all_messages = compact::validate_message_structure(&trimmed).messages;
                            info!("[ToolLoop] Trimmed to {} messages ({} chars)", all_messages.len(), compact::estimate_messages_chars(&all_messages));
                        }
                        if llm_retry >= LLM_MAX_RETRIES - 1 {
                            on_update(ToolUpdate::Error(format!("Context overflow after {} retries", LLM_MAX_RETRIES)));
                            return ToolLoopResult {
                                content: format!("Context overflow after {} retries", LLM_MAX_RETRIES),
                                tool_results: tool_records,
                                files: collected_files,
                            };
                        }
                        continue;
                    }

                    // 529 overloaded: exponential backoff with jitter
                    let is_overloaded = err_msg.contains("529") || err_msg.to_lowercase().contains("overloaded");
                    if is_overloaded && overload_retry_count < OVERLOAD_MAX_RETRIES {
                        overload_retry_count += 1;
                        let base_delay = 3000u64.min(3000u64.saturating_mul(1u64 << (overload_retry_count - 1))).min(30000);
                        let jitter = rand_jitter(2000);
                        let delay = base_delay + jitter;
                        info!("[ToolLoop] API overloaded — backoff retry {}/{} in {}ms...",
                            overload_retry_count, OVERLOAD_MAX_RETRIES, delay);
                        on_update(ToolUpdate::Error(format!("API overloaded — retrying in {}ms...", delay)));
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        continue; // Don't count toward normal retries
                    }

                    // Normal retry: 2s, 4s backoff
                    if llm_retry < LLM_MAX_RETRIES - 1 {
                        let delay = ((llm_retry + 1) * 2000) as u64;
                        info!("[ToolLoop] LLM call failed (attempt {}/{}). Retrying in {}ms...",
                            llm_retry + 1, LLM_MAX_RETRIES, delay);
                        on_update(ToolUpdate::Error(format!("LLM call failed, retrying... ({})", err_msg)));
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    } else {
                        on_update(ToolUpdate::Error(format!("Connection error after {} retries: {}", LLM_MAX_RETRIES, err_msg)));
                        return ToolLoopResult {
                            content: format!("Connection error after {} retries: {}", LLM_MAX_RETRIES, err_msg),
                            tool_results: tool_records,
                            files: collected_files,
                        };
                    }
                }
            }
        }

        let resp_json = match data {
            Some(d) => d,
            None => continue,
        };

        // Reset consecutive errors on successful API response
        consecutive_errors = 0;

        // Check for API-level error in response body
        if let Some(err) = resp_json.get("error") {
            let err_msg = err.to_string();
            error!("API error: {}", err_msg);

            // Context overflow — try compression before giving up
            if compact::is_context_overflow(&err_msg) && all_messages.len() > 3 {
                info!("[ToolLoop] Context overflow in response — compressing and retrying...");
                let force_opts2 = compact::CompactOptions { force: true };
                let compressed = compact::compress_older_messages(
                    &all_messages, compression_window.min(6), api_key, api_url, model, Some(&force_opts2),
                ).await;
                if compressed.len() < all_messages.len() {
                    all_messages = compact::validate_message_structure(&compressed).messages;
                    continue;
                } else {
                    let current_chars = compact::estimate_messages_chars(&all_messages);
                    let trimmed = compact::trim_conversation_context(&all_messages, current_chars / 2);
                    all_messages = compact::validate_message_structure(&trimmed).messages;
                    continue;
                }
            }

            // Tool ID mismatch: remove orphaned tool results and retry
            if err_msg.contains("tool_id") || err_msg.contains("tool result") || err_msg.contains("tool_call_id") {
                let before_len = all_messages.len();
                let valid_ids: std::collections::HashSet<String> = all_messages.iter()
                    .filter_map(|m| m["tool_calls"].as_array())
                    .flatten()
                    .filter_map(|tc| tc["id"].as_str().map(|s| s.to_string()))
                    .collect();
                all_messages.retain(|m| {
                    if m["role"].as_str() == Some("tool") {
                        if let Some(id) = m["tool_call_id"].as_str() {
                            return valid_ids.contains(id);
                        }
                    }
                    true
                });
                if all_messages.len() < before_len {
                    info!("[ToolLoop] Removed {} orphaned tool results. Retrying...", before_len - all_messages.len());
                    continue;
                }
            }

            // "content is empty" error (MiniMax 2013) — re-sanitize and retry
            if (err_msg.contains("content is empty") || err_msg.contains("2013"))
                && all_messages.len() > 2
            {
                info!("[ToolLoop] Empty content error — re-sanitizing messages and retrying...");
                all_messages = sanitize_messages(&all_messages);
                // Also trim if too long
                let trimmed = compact::trim_conversation_context(&all_messages, 6_000_000);
                if trimmed.len() < all_messages.len() {
                    all_messages = compact::validate_message_structure(&trimmed).messages;
                }
                continue;
            }

            on_update(ToolUpdate::Error(format!("API error: {}", err_msg)));
            return ToolLoopResult {
                content: format!("Error: API error: {}", err_msg),
                tool_results: tool_records,
                files: collected_files,
            };
        }

        // --- Handle no-choices error ---
        let choice = &resp_json["choices"][0];
        if choice.is_null() {
            let api_error = resp_json.get("error")
                .map(|e| e.to_string())
                .unwrap_or_else(|| serde_json::to_string(&resp_json).unwrap_or_default().chars().take(500).collect());
            warn!("[ToolLoop] No response from API at round {}. Error: {}", round, api_error);

            no_choices_retries += 1;
            if no_choices_retries < 3 {
                tokio::time::sleep(Duration::from_millis(2000)).await;
                continue;
            }
            return ToolLoopResult {
                content: format!("API returned no choices: {}", api_error),
                tool_results: tool_records,
                files: collected_files,
            };
        }
        no_choices_retries = 0;

        let message = &choice["message"];
        let _finish_reason = choice["finish_reason"].as_str().unwrap_or("");

        // Capture early content (for abort/cancel scenarios)
        if let Some(text) = message["content"].as_str() {
            if !text.is_empty() {
                early_content = text.to_string();
            }
        }

        // Stream reasoning content to callback if present (extended thinking models)
        if let Some(reasoning) = message.get("reasoning_content").and_then(|r| r.as_str()) {
            if !reasoning.is_empty() {
                on_update(ToolUpdate::TextChunk(format!("[reasoning] {}", &reasoning[..reasoning.len().min(500)])));
            }
        }

        // Get tool calls and truncate large args before adding to context
        let tool_calls = message["tool_calls"].as_array();
        let mut truncated_message = if let Some(calls) = tool_calls {
            if !calls.is_empty() {
                let truncated = truncate_tool_call_args(calls);
                let mut msg = message.clone();
                msg["tool_calls"] = json!(truncated);
                msg
            } else {
                message.clone()
            }
        } else {
            message.clone()
        };

        // Preserve reasoning_content in the assistant message (tiger_cowork: reasoning support)
        if let Some(reasoning) = message.get("reasoning_content") {
            if reasoning.is_string() && !reasoning.as_str().unwrap_or("").is_empty() {
                truncated_message["reasoning_content"] = reasoning.clone();
                // Kimi API rejects empty content — fill from reasoning if content is empty
                if truncated_message["content"].as_str().unwrap_or("").is_empty() {
                    let r = reasoning.as_str().unwrap_or("");
                    truncated_message["content"] = json!(r[..r.len().min(200)]);
                }
            }
        }

        // Ensure content is never empty (MiniMax/Kimi APIs reject "chat content is empty")
        // This applies even when tool_calls are present — some APIs require non-empty content always
        if truncated_message["content"].as_str().unwrap_or("").is_empty() {
            truncated_message["content"] = json!("(thinking...)");
        }

        // Strip reasoning_content before appending — some APIs reject unknown fields
        if truncated_message.get("reasoning_content").is_some() {
            truncated_message.as_object_mut().map(|m| m.remove("reasoning_content"));
        }

        // Append the assistant message (with truncated args) to the conversation
        all_messages.push(truncated_message);

        // Check for tool calls
        if let Some(calls) = tool_calls {
            if calls.is_empty() {
                // No tool calls -- treat as final response
                let content = message["content"].as_str().unwrap_or("").to_string();
                on_update(ToolUpdate::TextChunk(content.clone()));
                clear_checkpoint(&sub_agent.session_id).await;
                return ToolLoopResult {
                    content,
                    tool_results: tool_records,
                    files: collected_files,
                };
            }

            // --- Parse tool calls ---
            let mut parsed_calls: Vec<(String, String, Value, String)> = Vec::new(); // (name, id, args, raw_args_str)
            for call in calls {
                let tool_name = call["function"]["name"].as_str().unwrap_or("unknown").to_string();
                let tool_args_str = call["function"]["arguments"].as_str().unwrap_or("{}").to_string();
                let tool_id = call["id"].as_str().unwrap_or("").to_string();

                let tool_args: Value = match serde_json::from_str(&tool_args_str) {
                    Ok(v) => v,
                    Err(_) => {
                        // JSON parse recovery for malformed args
                        if let Some(recovered) = recover_tool_args(&tool_name, &tool_args_str) {
                            info!("[ToolLoop] Recovered malformed JSON for {}", tool_name);
                            recovered
                        } else {
                            warn!("[ToolLoop] Failed to parse args for {}: {}", tool_name, &tool_args_str[..tool_args_str.len().min(200)]);
                            json!({})
                        }
                    }
                };
                parsed_calls.push((tool_name, tool_id, tool_args, tool_args_str));
            }

            // --- Execute tools: parallel for sub-agent tools, sequential for others ---
            let parallel_tool_names: std::collections::HashSet<&str> =
                ["spawn_subagent", "send_task", "wait_result"].iter().copied().collect();

            let (sequential_calls, parallel_calls): (Vec<_>, Vec<_>) = parsed_calls.iter()
                .partition(|(name, _, _, _)| !parallel_tool_names.contains(name.as_str()));

            // Execute sequential tools
            for (tool_name, tool_id, tool_args, _raw) in &sequential_calls {
                if total_tool_calls >= max_tool_calls {
                    warn!("Max total tool calls reached ({})", max_tool_calls);
                    let content = force_final_response(
                        &client, api_key, api_url, model, &all_messages, &tool_records,
                        total_tool_calls, temperature,
                    ).await;
                    on_update(ToolUpdate::TextChunk(content.clone()));
                    clear_checkpoint(&sub_agent.session_id).await;
                    return ToolLoopResult { content, tool_results: tool_records, files: collected_files };
                }

                if tool_name == "load_skill" { _uses_skill = true; }

                // Loop detection — skip for monitoring tools that are legitimately called repeatedly
                let is_monitoring_tool = matches!(tool_name.as_str(),
                    "check_agents" | "bb_read" | "proto_bb_read" | "proto_bus_history" | "proto_queue_peek"
                );
                let signature = format!("{}:{}", tool_name, tool_args);
                if !is_monitoring_tool {
                    recent_signatures.push(signature.clone());
                }
                tool_call_history.push(format!("{}:{}", tool_name, tool_args.to_string().chars().take(100).collect::<String>()));
                if recent_signatures.len() >= MAX_LOOP_REPEATS {
                    let tail = &recent_signatures[recent_signatures.len() - MAX_LOOP_REPEATS..];
                    if tail.iter().all(|s| s == &signature) {
                        warn!("Loop detected: same tool+args repeated {} times", MAX_LOOP_REPEATS);
                        on_update(ToolUpdate::Error("Loop detected: same tool call repeated".to_string()));
                        let content = force_final_response(
                            &client, api_key, api_url, model, &all_messages, &tool_records,
                            total_tool_calls, temperature,
                        ).await;
                        on_update(ToolUpdate::TextChunk(content.clone()));
                        clear_checkpoint(&sub_agent.session_id).await;
                        return ToolLoopResult { content, tool_results: tool_records, files: collected_files };
                    }
                }

                on_update(ToolUpdate::ToolCall { name: tool_name.clone(), args: tool_args.clone() });

                // Log tool call to agent history
                let args_preview: String = tool_args.to_string().chars().take(200).collect();
                write_agent_history(&sub_agent.session_id, "TOOL_CALL", json!({
                    "agent_id": sub_agent.agent_id,
                    "tool": tool_name,
                    "args_preview": args_preview,
                    "round": round,
                })).await;

                info!("Executing tool: {} (round {})", tool_name, round);
                let result = execute_tool_dispatch(
                    tool_name, tool_args, sandbox_dir, &sub_agent, on_update.clone(), realtime,
                ).await;

                // Track consecutive errors (skip agent timeouts)
                let is_agent_timeout = matches!(tool_name.as_str(), "wait_result" | "send_task")
                    && result.get("error").and_then(|v| v.as_str())
                        .map(|s| s.to_lowercase().contains("timeout")).unwrap_or(false);

                if (result.get("ok").and_then(|v| v.as_bool()) == Some(false)
                    || result.get("exit_code").and_then(|v| v.as_i64()) == Some(1))
                    && !is_agent_timeout
                {
                    consecutive_errors += 1;
                } else if !is_agent_timeout {
                    consecutive_errors = 0;
                }

                on_update(ToolUpdate::ToolResult { name: tool_name.clone(), result: result.clone() });

                // Log tool result to agent history
                {
                    let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                    let result_preview: String = result.to_string().chars().take(200).collect();
                    write_agent_history(&sub_agent.session_id, "TOOL_RESULT", json!({
                        "agent_id": sub_agent.agent_id,
                        "tool": tool_name,
                        "ok": ok,
                        "result_preview": result_preview,
                    })).await;
                }

                // Track file reads for post-compact restoration
                if tool_name == "read_file" {
                    if let (Some(path), Some(content)) = (
                        result.get("path").and_then(|v| v.as_str()),
                        result.get("content").and_then(|v| v.as_str()),
                    ) {
                        compact::track_file_read(path, content);
                    }
                }

                tool_records.push(ToolCallRecord { tool: tool_name.clone(), result: result.clone() });

                // After create_architecture or select_swarm: refresh tools to include realtime
                if (tool_name == "create_architecture" || tool_name == "select_swarm")
                    && result.get("ok").and_then(|v| v.as_bool()) == Some(true)
                    && !session_activated
                {
                    session_activated = true;
                    // Refresh tool set to include send_task/wait_result
                    tools = tool_definitions_for_mode(&sub_agent, true, true);
                    // Orchestrators get reduced tools — research delegated to workers
                    if sub_agent.agent_role == "orchestrator" {
                        tools.retain(|t| {
                            let name = t["function"]["name"].as_str().unwrap_or("");
                            !matches!(name, "web_search" | "fetch_url")
                        });
                    }
                    info!("[ToolLoop] Session activated via {}, tools refreshed with realtime tools", tool_name);
                }

                // Collect output files
                if let Some(files) = result.get("output_files").and_then(|v| v.as_array()) {
                    for f in files {
                        if let Some(s) = f.as_str() {
                            if !collected_files.contains(&s.to_string()) {
                                collected_files.push(s.to_string());
                            }
                        }
                    }
                }

                // Append tool result message (with smart compression)
                let max_len = if tool_name == "load_skill" { 3000 } else { tool_result_max_len };
                let result_str = compact::compress_tool_result(tool_name, &result, max_len);
                all_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_id,
                    "content": result_str,
                }));

                total_tool_calls += 1;
            }

            // Execute parallel sub-agent tools concurrently (tiger_cowork: Promise.all)
            if !parallel_calls.is_empty() && total_tool_calls < max_tool_calls {
                let mut handles = Vec::new();
                for (tool_name, tool_id, tool_args, _raw) in &parallel_calls {
                    let name = tool_name.clone();
                    let id = tool_id.clone();
                    let args = tool_args.clone();
                    let sa = sub_agent.clone();
                    let sd = sandbox_dir.to_string();
                    let upd = on_update.clone();
                    let rt = realtime;

                    handles.push(tokio::spawn(async move {
                        let result = execute_tool_dispatch(&name, &args, &sd, &sa, upd, rt).await;
                        (name, id, args, result)
                    }));
                }

                let results = futures_util::future::join_all(handles).await;
                for join_result in results {
                    if let Ok((tool_name, tool_id, tool_args, result)) = join_result {
                        on_update(ToolUpdate::ToolCall { name: tool_name.clone(), args: tool_args.clone() });
                        on_update(ToolUpdate::ToolResult { name: tool_name.clone(), result: result.clone() });

                        // Log
                        let args_preview: String = tool_args.to_string().chars().take(200).collect();
                        write_agent_history(&sub_agent.session_id, "TOOL_CALL", json!({
                            "agent_id": sub_agent.agent_id, "tool": &tool_name, "args_preview": args_preview, "round": round,
                        })).await;
                        let result_preview: String = result.to_string().chars().take(200).collect();
                        write_agent_history(&sub_agent.session_id, "TOOL_RESULT", json!({
                            "agent_id": sub_agent.agent_id, "tool": &tool_name,
                            "ok": result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
                            "result_preview": result_preview,
                        })).await;

                        tool_records.push(ToolCallRecord { tool: tool_name.clone(), result: result.clone() });

                        if let Some(files) = result.get("output_files").and_then(|v| v.as_array()) {
                            for f in files {
                                if let Some(s) = f.as_str() {
                                    if !collected_files.contains(&s.to_string()) {
                                        collected_files.push(s.to_string());
                                    }
                                }
                            }
                        }

                        let max_len = tool_result_max_len;
                        let result_str = compact::compress_tool_result(&tool_name, &result, max_len);
                        all_messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_id,
                            "content": result_str,
                        }));

                        total_tool_calls += 1;
                    }
                }
            }

            // --- Consecutive error recovery (tiger_cowork: nudge messages) ---
            if consecutive_errors >= max_consecutive_errors {
                if error_recovery_attempts < max_error_recoveries {
                    error_recovery_attempts += 1;
                    info!("[ToolLoop] {} consecutive errors. Recovery attempt {}/{}...",
                        max_consecutive_errors, error_recovery_attempts, max_error_recoveries);

                    let recovery_msg = if error_recovery_attempts <= 2 {
                        format!(
                            "⚠️ SYSTEM: You have had {} consecutive tool errors. Do NOT give up. \
                            Analyze the errors above and try a DIFFERENT approach:\n\
                            1. Check if paths/filenames are correct\n\
                            2. Try simpler commands\n\
                            3. Break complex operations into smaller steps\n\
                            4. If a tool keeps failing, try an alternative tool",
                            max_consecutive_errors
                        )
                    } else {
                        format!(
                            "🔴 SYSTEM CRITICAL: Recovery attempt {}/{}. Take a completely different strategy:\n\
                            1. Abandon the failing approach entirely\n\
                            2. List what you know works\n\
                            3. Use only the simplest possible tools\n\
                            4. If writing code, write minimal test code first",
                            error_recovery_attempts, max_error_recoveries
                        )
                    };

                    all_messages.push(json!({"role": "user", "content": recovery_msg}));
                    consecutive_errors = 0;
                } else {
                    // Final fallback: one more chance
                    all_messages.push(json!({
                        "role": "user",
                        "content": "🚨 SYSTEM FINAL FALLBACK: All recovery attempts exhausted. \
                            Provide a final answer with whatever partial results you have. \
                            Do NOT attempt any more tool calls."
                    }));
                    consecutive_errors = 0;
                    error_recovery_attempts = 0;
                }
            }

            // --- Max tool calls check ---
            if total_tool_calls >= max_tool_calls {
                info!("[ToolLoop] Reached max tool calls ({}). Ending loop.", max_tool_calls);
                break;
            }

            // --- Persistent errors after all recoveries ---
            if consecutive_errors >= max_consecutive_errors && error_recovery_attempts >= max_error_recoveries * 2 {
                info!("[ToolLoop] Persistent errors after all recovery cycles. Ending loop.");
                break;
            }

        } else {
            // No tool calls — agent wants to stop
            // But first: check for pending sub-agent work (tiger_cowork)
            if !sub_agent.session_id.is_empty() {
                let (working, pending, bb_tasks) = check_pending_work(&sub_agent.session_id).await;
                let has_pending = !working.is_empty() || !pending.is_empty() || !bb_tasks.is_empty();

                if has_pending {
                    let agent_names = working.join(", ");
                    let mut pending_info = String::new();
                    if !pending.is_empty() {
                        pending_info = format!("\n\nResults just arrived from your agents:\n{}",
                            pending.iter().map(|(id, r)| format!("**{}**: {}", id, &r.result[..r.result.len().min(3000)]))
                                .collect::<Vec<_>>().join("\n\n")
                        );
                    }

                    all_messages.push(json!({
                        "role": "user",
                        "content": format!(
                            "⚠️ SYSTEM: Do NOT stop yet — you have {} agent(s) still working [{}], \
                            {} pending result(s), and {} unfinished blackboard task(s). You MUST:\n\
                            1. Use bb_read to check bid status, then bb_award to assign open tasks\n\
                            2. Use send_task to deliver work to awarded agents\n\
                            3. Use wait_result to collect all agent results\n\
                            4. Integrate ALL results into your final answer\n\
                            5. Only finish AFTER all tasks are completed{}",
                            working.len(), agent_names,
                            pending.len(), bb_tasks.len(),
                            pending_info
                        )
                    }));
                    #[allow(unused_assignments)]
                    { consecutive_errors = 0; }
                    continue;
                }
            }

            // Truly done — return final response
            let content = message["content"].as_str().unwrap_or("").to_string();
            on_update(ToolUpdate::TextChunk(content.clone()));
            clear_checkpoint(&sub_agent.session_id).await;

            // --- Reflection loop (tiger_cowork: evaluate objective satisfaction) ---
            if reflection_enabled && total_tool_calls > 0 && !content.is_empty() {
                let mut reflection_content = content.clone();
                let records_snapshot = tool_records.clone();
                let reflection_result = run_reflection_loop(
                    &client, api_key, api_url, model, &mut all_messages, &user_objective,
                    &records_snapshot, eval_threshold, max_reflection_retries,
                    temperature, max_tokens,
                    &tools, &sub_agent, sandbox_dir, on_update.clone(), realtime,
                    &mut tool_records, &mut collected_files, &mut total_tool_calls,
                ).await;
                if let Some(improved) = reflection_result {
                    reflection_content = improved;
                }

                return ToolLoopResult {
                    content: reflection_content,
                    tool_results: tool_records,
                    files: collected_files,
                };
            }

            // --- Output file nudge (tiger_cowork: if user wants files but none generated) ---
            if user_wants_output && collected_files.is_empty() && total_tool_calls > 0 {
                let nudge_result = run_output_nudge(
                    &client, api_key, api_url, model, &mut all_messages,
                    &tools, temperature, max_tokens, &sub_agent, sandbox_dir,
                    on_update.clone(), realtime,
                    &mut tool_records, &mut collected_files, &mut total_tool_calls,
                ).await;
                if let Some(nudged_content) = nudge_result {
                    return ToolLoopResult {
                        content: nudged_content,
                        tool_results: tool_records,
                        files: collected_files,
                    };
                }
            }

            return ToolLoopResult {
                content,
                tool_results: tool_records,
                files: collected_files,
            };
        }
    }

    // Exhausted max rounds — force a final text response
    warn!("Tool loop exhausted max rounds ({})", max_rounds);
    clear_checkpoint(&sub_agent.session_id).await;
    let content = force_final_response(
        &client, api_key, api_url, model, &all_messages, &tool_records,
        total_tool_calls, temperature,
    ).await;
    // Fall back to early_content if force_final_response returned empty
    let content = if content.is_empty() && !early_content.is_empty() {
        early_content
    } else {
        content
    };
    on_update(ToolUpdate::TextChunk(content.clone()));

    ToolLoopResult {
        content,
        tool_results: tool_records,
        files: collected_files,
    }
}

// ---------------------------------------------------------------------------
// Helper: dispatch tool execution
// ---------------------------------------------------------------------------

async fn execute_tool_dispatch(
    tool_name: &str,
    tool_args: &Value,
    sandbox_dir: &str,
    sub_agent: &SubAgentConfig,
    on_update: Arc<dyn Fn(ToolUpdate) + Send + Sync + 'static>,
    _realtime: bool,
) -> Value {
    // Gate: require user approval for dangerous tools
    if tool_requires_approval(tool_name).await {
        let approved = request_tool_approval(tool_name, tool_args, &on_update).await;
        if !approved {
            return json!({
                "ok": false,
                "error": format!("User denied execution of '{}'", tool_name)
            });
        }
    }

    if tool_name == "spawn_subagent" {
        exec_spawn_subagent(tool_args, sub_agent, sandbox_dir, on_update).await
    } else if tool_name == "send_task" {
        exec_send_task_from(tool_args, &sub_agent.session_id, &sub_agent.agent_id).await
    } else if tool_name == "wait_result" {
        exec_wait_result(tool_args, &sub_agent.session_id).await
    } else if tool_name == "check_agents" {
        exec_check_agents(&sub_agent.session_id).await
    } else if tool_name == "create_architecture" {
        exec_create_architecture(tool_args, sub_agent).await
    } else if tool_name == "select_swarm" {
        exec_select_swarm(tool_args, sub_agent).await
    } else {
        execute_tool_with_context(
            tool_name, tool_args, sandbox_dir,
            &sub_agent.session_id, &sub_agent.agent_id,
        ).await
    }
}

// ---------------------------------------------------------------------------
// Reflection loop (tiger_cowork: post-loop evaluation with outer retry)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_reflection_loop(
    client: &Client,
    api_key: &str,
    api_url: &str,
    model: &str,
    all_messages: &mut Vec<Value>,
    user_objective: &str,
    tool_records: &[ToolCallRecord],
    eval_threshold: f64,
    max_reflection_retries: usize,
    temperature: f64,
    max_tokens: u64,
    tools: &[Value],
    sub_agent: &SubAgentConfig,
    sandbox_dir: &str,
    on_update: Arc<dyn Fn(ToolUpdate) + Send + Sync + 'static>,
    realtime: bool,
    tool_records_mut: &mut Vec<ToolCallRecord>,
    collected_files: &mut Vec<String>,
    total_tool_calls: &mut usize,
) -> Option<String> {
    // Outer retry loop (tiger_cowork: maxReflectionRetries, default 2)
    for retry_round in 0..max_reflection_retries {
        info!("[Reflection] Round {}/{} — evaluating objective satisfaction...", retry_round + 1, max_reflection_retries);

        // Build tool summary for evaluation (tiger_cowork format)
        let tool_summary: String = tool_records.iter().chain(tool_records_mut.iter()).map(|tr| {
            let r = &tr.result;
            if let Some(files) = r.get("output_files").and_then(|v| v.as_array()) {
                if !files.is_empty() {
                    let fnames: Vec<&str> = files.iter().filter_map(|f| f.as_str()).collect();
                    return format!("[{}] Generated: {}", tr.tool, fnames.join(", "));
                }
            }
            if r.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                return format!("[{}] Error: {}", tr.tool, r.get("error").and_then(|v| v.as_str()).unwrap_or("failed"));
            }
            if let Some(stdout) = r.get("stdout").and_then(|v| v.as_str()) {
                return format!("[{}] {}", tr.tool, &stdout[..stdout.len().min(300)]);
            }
            format!("[{}] {}", tr.tool, serde_json::to_string(r).unwrap_or_default().chars().take(300).collect::<String>())
        }).collect::<Vec<_>>().join("\n");

        let last_assistant = all_messages.iter().rev()
            .find(|m| m["role"].as_str() == Some("assistant"))
            .and_then(|m| m["content"].as_str())
            .unwrap_or("(none)");

        // Rich evaluation prompt matching tiger_cowork
        let eval_messages = vec![
            json!({"role": "system", "content": "You are an evaluation judge. Score how well the agent satisfied the user's objective."}),
            json!({"role": "user", "content": format!(
                "USER OBJECTIVE:\n{}\n\n\
                AGENT ACTIONS ({} tool calls):\n{}\n\n\
                LAST ASSISTANT MESSAGE:\n{}\n\n\
                Respond in EXACTLY this JSON format (no other text):\n\
                {{\"score\": <0.0-1.0>, \"satisfied\": <true/false>, \"missing\": \"<what is missing or incomplete, empty string if satisfied>\"}}\n\n\
                Scoring guide:\n\
                - 1.0: Fully satisfied, all parts addressed\n\
                - 0.7-0.9: Mostly satisfied, minor gaps\n\
                - 0.4-0.6: Partially satisfied, significant gaps\n\
                - 0.0-0.3: Not satisfied, major parts missing",
                user_objective, total_tool_calls, tool_summary, last_assistant
            )}),
        ];

        let eval_data = match llm_call(client, api_key, api_url, model, &eval_messages, None, temperature, max_tokens).await {
            Ok(d) => d,
            Err(e) => {
                error!("[Reflection] Eval call failed: {}", e);
                break;
            }
        };
        let eval_content = eval_data["choices"][0]["message"]["content"].as_str().unwrap_or("");
        info!("[Reflection] Raw eval: {}", &eval_content[..eval_content.len().min(300)]);

        // Parse JSON from response
        let json_str = eval_content.find('{')
            .and_then(|start| eval_content.rfind('}').map(|end| &eval_content[start..=end]));
        let parsed: Value = json_str
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(json!({"score": 1.0, "satisfied": true}));

        let score = parsed["score"].as_f64().unwrap_or(1.0);
        let satisfied = parsed["satisfied"].as_bool().unwrap_or(false);
        let missing = parsed["missing"].as_str().unwrap_or("").to_string();

        info!("[Reflection] Score: {:.2}, Satisfied: {}, Missing: {}", score, satisfied, &missing[..missing.len().min(200)]);

        if score >= eval_threshold || satisfied {
            info!("[Reflection] Score {:.2} >= threshold {:.2}. Objective satisfied.", score, eval_threshold);
            break;
        }

        // Score below threshold — re-enter agent loop to address gaps
        info!("[Reflection] Score {:.2} < threshold {:.2}. Re-entering agent loop...", score, eval_threshold);

        all_messages.push(json!({
            "role": "system",
            "content": format!(
                "REFLECTION CHECK: Your work scored {:.1}/1.0 (threshold: {:.1}). The evaluation found these gaps:\n{}\n\n\
                Please address what's missing to fully satisfy the user's objective. Use tools as needed.",
                score, eval_threshold, missing
            )
        }));

        // Run additional tool rounds to address the gaps (tiger_cowork: up to 5)
        for _extra_round in 0..5usize {
            let resp = match llm_call(client, api_key, api_url, model, all_messages, Some(tools), temperature, max_tokens).await {
                Ok(d) => d,
                Err(e) => {
                    error!("[Reflection retry] LLM call failed: {}", e);
                    break;
                }
            };

            let message = &resp["choices"][0]["message"];
            all_messages.push(message.clone());

            if let Some(calls) = message["tool_calls"].as_array() {
                if calls.is_empty() {
                    break; // LLM done, loop back to re-evaluate
                }
                for call in calls {
                    let name = call["function"]["name"].as_str().unwrap_or("unknown");
                    let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
                    let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                    let id = call["id"].as_str().unwrap_or("");

                    on_update(ToolUpdate::ToolCall { name: name.to_string(), args: args.clone() });
                    let result = execute_tool_dispatch(name, &args, sandbox_dir, sub_agent, on_update.clone(), realtime).await;
                    on_update(ToolUpdate::ToolResult { name: name.to_string(), result: result.clone() });
                    tool_records_mut.push(ToolCallRecord { tool: name.to_string(), result: result.clone() });

                    if let Some(files) = result.get("output_files").and_then(|v| v.as_array()) {
                        for f in files {
                            if let Some(s) = f.as_str() {
                                if !collected_files.contains(&s.to_string()) {
                                    collected_files.push(s.to_string());
                                }
                            }
                        }
                    }

                    let result_str = compact::compress_tool_result(name, &result, 6000);
                    all_messages.push(json!({"role": "tool", "tool_call_id": id, "content": result_str}));
                    *total_tool_calls += 1;
                }
            } else {
                break; // No tool calls, done with this round
            }
        }
        // Loop back to re-evaluate
    }

    // Return the last assistant message content (may be improved or original)
    all_messages.iter().rev()
        .find(|m| m["role"].as_str() == Some("assistant"))
        .and_then(|m| m["content"].as_str())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Output file nudge (tiger_cowork: nudge agent to generate files)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_output_nudge(
    client: &Client,
    api_key: &str,
    api_url: &str,
    model: &str,
    all_messages: &mut Vec<Value>,
    tools: &[Value],
    temperature: f64,
    max_tokens: u64,
    sub_agent: &SubAgentConfig,
    sandbox_dir: &str,
    on_update: Arc<dyn Fn(ToolUpdate) + Send + Sync + 'static>,
    realtime: bool,
    tool_records: &mut Vec<ToolCallRecord>,
    collected_files: &mut Vec<String>,
    total_tool_calls: &mut usize,
) -> Option<String> {
    all_messages.push(json!({
        "role": "system",
        "content": "IMPORTANT: The user asked for charts/graphs but you haven't generated output files yet. \
            Call run_python to create matplotlib charts and save as PNG. Use plt.show() which auto-saves."
    }));

    for _nudge_round in 0..3 {
        let resp = llm_call(client, api_key, api_url, model, all_messages, Some(tools), temperature, max_tokens).await.ok()?;

        let message = &resp["choices"][0]["message"];
        all_messages.push(message.clone());

        if let Some(calls) = message["tool_calls"].as_array() {
            if calls.is_empty() {
                return message["content"].as_str().map(|s| s.to_string());
            }
            for call in calls {
                let name = call["function"]["name"].as_str().unwrap_or("unknown");
                let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
                let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                let id = call["id"].as_str().unwrap_or("");

                let result = execute_tool_dispatch(name, &args, sandbox_dir, sub_agent, on_update.clone(), realtime).await;
                tool_records.push(ToolCallRecord { tool: name.to_string(), result: result.clone() });

                if let Some(files) = result.get("output_files").and_then(|v| v.as_array()) {
                    for f in files {
                        if let Some(s) = f.as_str() {
                            if !collected_files.contains(&s.to_string()) {
                                collected_files.push(s.to_string());
                            }
                        }
                    }
                }

                let result_str = compact::compress_tool_result(name, &result, 6000);
                all_messages.push(json!({"role": "tool", "tool_call_id": id, "content": result_str}));
                *total_tool_calls += 1;
            }

            if !collected_files.is_empty() {
                // Files generated — get final response
                let final_resp = llm_call(client, api_key, api_url, model, all_messages, None, temperature, max_tokens).await.ok()?;
                return final_resp["choices"][0]["message"]["content"].as_str().map(|s| s.to_string());
            }
        } else {
            return message["content"].as_str().map(|s| s.to_string());
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Jitter helper
// ---------------------------------------------------------------------------

fn rand_jitter(max_ms: u64) -> u64 {
    use std::time::SystemTime;
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    (seed as u64) % max_ms
}

// ---------------------------------------------------------------------------
// Force final response (unchanged logic, added temperature param)
// ---------------------------------------------------------------------------

async fn force_final_response(
    client: &Client,
    api_key: &str,
    api_url: &str,
    model: &str,
    all_messages: &[Value],
    tool_records: &[ToolCallRecord],
    total_tool_calls: usize,
    temperature: f64,
) -> String {
    let tool_summary = tool_records.iter().map(|tr| {
        let brief = if tr.result.get("output_files")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            let files: Vec<&str> = tr.result["output_files"]
                .as_array().unwrap()
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            format!("Generated: {}", files.join(", "))
        } else if tr.result.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            format!("Error: {}", tr.result.get("error").and_then(|v| v.as_str()).unwrap_or("failed"))
        } else if let Some(stdout) = tr.result.get("stdout").and_then(|v| v.as_str()) {
            stdout.chars().take(300).collect::<String>()
        } else if tr.result.is_string() {
            tr.result.as_str().unwrap_or("").chars().take(300).collect::<String>()
        } else {
            serde_json::to_string(&tr.result).unwrap_or_default().chars().take(300).collect::<String>()
        };
        format!("[{}]: {}", tr.tool, brief)
    }).collect::<Vec<_>>().join("\n");

    // Keep system prompt (first system msg) + all user messages
    let mut final_messages: Vec<Value> = vec![];
    let mut first_system_added = false;
    for m in all_messages {
        let role = m["role"].as_str().unwrap_or("");
        if role == "system" && !first_system_added {
            final_messages.push(m.clone());
            first_system_added = true;
        } else if role == "user" {
            final_messages.push(m.clone());
        }
    }

    final_messages.push(json!({
        "role": "user",
        "content": format!(
            "You executed {} tool calls. Summary:\n{}\n\nProvide a clear, helpful response to the user. \
            Mention any generated files. Do NOT call tools. \
            IMPORTANT: Do NOT include any internal tool call syntax, function names, parameter details, \
            or markers like [web_search], [fetch_url], etc. in your response. \
            The user should only see the final results, not the tools you used.",
            total_tool_calls, tool_summary
        )
    }));

    // Call LLM without tools (forces text-only response)
    match llm_call(client, api_key, api_url, model, &final_messages, None, temperature, 8192).await {
        Ok(data) => {
            let content = data["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
            if !content.is_empty() {
                return content;
            }
        }
        Err(e) => warn!("[FinalResponse] LLM call failed: {e}"),
    }

    // Absolute fallback
    let output_files: Vec<String> = tool_records.iter()
        .filter_map(|tr| tr.result.get("output_files").and_then(|v| v.as_array()).map(|a| {
            a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
        }))
        .filter(|s| !s.is_empty())
        .collect();

    let stdouts: Vec<String> = tool_records.iter()
        .filter_map(|tr| tr.result.get("stdout").and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
        .map(|s| s.chars().take(500).collect::<String>())
        .collect();

    let errors: Vec<String> = tool_records.iter()
        .filter_map(|tr| {
            if tr.result.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                tr.result.get("error").and_then(|v| v.as_str()).map(|s| format!("Error: {s}"))
            } else { None }
        })
        .collect();

    let fallback_parts: Vec<String> = output_files.into_iter()
        .chain(stdouts.into_iter())
        .chain(errors.into_iter())
        .collect();

    if !fallback_parts.is_empty() {
        fallback_parts.join("\n\n")
    } else {
        "Task completed. Check the output panel for results.".to_string()
    }
}
