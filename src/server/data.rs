use std::path::PathBuf;
use std::sync::{OnceLock, Mutex};

use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::fs;

// ---------------------------------------------------------------------------
// Remote Backend (transparent proxy for remote mode)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RemoteBackend {
    pub url: String,
    pub token: String,
}

static REMOTE_BACKEND: OnceLock<Mutex<Option<RemoteBackend>>> = OnceLock::new();

pub fn set_remote_backend(backend: Option<RemoteBackend>) {
    let lock = REMOTE_BACKEND.get_or_init(|| Mutex::new(None));
    // Clear cache when backend changes
    remote_cache_clear();
    // Pre-warm cache for the new backend
    if let Some(ref rb) = backend {
        eprintln!("[remote] Connecting to remote: {} (token={}...)", rb.url, crate::util::truncate_utf8(&rb.token, 4));
        let _ = std::fs::write("/tmp/tigrimos_remote.log", format!("CONNECT url={} token={}\n", rb.url, crate::util::truncate_utf8(&rb.token, 8)));
        remote_bg_fetch(rb, "/api/chat/sessions/bulk", 10);
        remote_bg_fetch(rb, "/api/settings", 15);
        remote_bg_fetch(rb, "/api/projects", 10);
        remote_bg_fetch(rb, "/api/tasks", 10);
        remote_bg_fetch(rb, "/api/skills", 15);
    } else {
        eprintln!("[remote] Disconnected from remote");
    }
    *lock.lock().unwrap() = backend;
}

pub fn get_remote_backend() -> Option<RemoteBackend> {
    REMOTE_BACKEND
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|g| g.clone())
}

// ---------------------------------------------------------------------------
// Remote cache — avoids repeated HTTP calls on every UI frame
// ---------------------------------------------------------------------------

use std::collections::HashMap;

struct CacheEntry {
    data: String, // JSON string
    expires: std::time::Instant,
}

static REMOTE_CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();

fn remote_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    REMOTE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remote_cache_get(key: &str) -> Option<String> {
    let cache = remote_cache().lock().ok()?;
    let entry = cache.get(key)?;
    if entry.expires > std::time::Instant::now() {
        Some(entry.data.clone())
    } else {
        None
    }
}

fn remote_cache_set(key: &str, data: String, ttl_secs: u64) {
    if let Ok(mut cache) = remote_cache().lock() {
        cache.insert(key.to_string(), CacheEntry {
            data,
            expires: std::time::Instant::now() + std::time::Duration::from_secs(ttl_secs),
        });
    }
}

pub fn remote_cache_clear() {
    if let Some(cache) = REMOTE_CACHE.get() {
        if let Ok(mut c) = cache.lock() {
            c.clear();
        }
    }
}

/// Invalidate a specific cache key (call after writes)
pub fn remote_cache_invalidate(key: &str) {
    if let Some(cache) = REMOTE_CACHE.get() {
        if let Ok(mut c) = cache.lock() {
            c.remove(key);
        }
    }
}

pub fn remote_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

/// Cached GET — returns cached JSON if fresh, otherwise fetches and caches.
/// Uses a sync fast-path for cache hits to avoid blocking the UI thread.
async fn remote_get_cached<T: serde::de::DeserializeOwned + Default>(
    rb: &RemoteBackend,
    path: &str,
    ttl_secs: u64,
) -> T {
    // Sync fast-path: return cached data without async overhead
    if let Some(cached) = remote_cache_get(path) {
        if let Ok(val) = serde_json::from_str::<T>(&cached) {
            return val;
        }
    }
    // Cache miss — fetch from remote
    remote_fetch_and_cache::<T>(rb, path, ttl_secs).await
}

/// Sync cache check — can be called from UI without block_on().
/// Returns Some(T) if cached, None if cache miss (caller should trigger background fetch).
pub fn remote_cache_try_get<T: serde::de::DeserializeOwned>(path: &str) -> Option<T> {
    let cached = remote_cache_get(path)?;
    serde_json::from_str::<T>(&cached).ok()
}

/// Trigger a background fetch for a cache key without blocking.
/// Spawns a tokio task to fetch and populate the cache.
pub fn remote_bg_fetch(rb: &RemoteBackend, path: &str, ttl_secs: u64) {
    let rb = rb.clone();
    let path = path.to_string();
    // Use tokio::spawn if we're inside a runtime, otherwise spawn a thread
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            remote_fetch_and_cache::<serde_json::Value>(&rb, &path, ttl_secs).await;
        });
    } else {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                remote_fetch_and_cache::<serde_json::Value>(&rb, &path, ttl_secs).await;
            });
        });
    }
}

async fn remote_fetch_and_cache<T: serde::de::DeserializeOwned + Default>(
    rb: &RemoteBackend,
    path: &str,
    ttl_secs: u64,
) -> T {
    let url = format!("{}{}", rb.url, path);
    eprintln!("[remote] GET {}", url);
    let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/tigrimos_remote.log")
        .and_then(|mut f| { use std::io::Write; writeln!(f, "GET {}", url) });
    match remote_client()
        .get(&url)
        .bearer_auth(&rb.token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            eprintln!("[remote] GET {} → {}", path, status);
            let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/tigrimos_remote.log")
                .and_then(|mut f| { use std::io::Write; writeln!(f, "RESULT {} → {}", path, status) });
            if !status.is_success() {
                // Cache a short-lived empty marker to avoid hammering the server
                remote_cache_set(path, String::new(), 5);
                return T::default();
            }
            if let Ok(text) = resp.text().await {
                remote_cache_set(path, text.clone(), ttl_secs);
                serde_json::from_str::<T>(&text).unwrap_or_default()
            } else {
                T::default()
            }
        }
        Err(e) => {
            eprintln!("[remote] GET {} FAILED: {}", path, e);
            let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/tigrimos_remote.log")
                .and_then(|mut f| { use std::io::Write; writeln!(f, "FAILED {} → {}", path, e) });
            T::default()
        }
    }
}

#[allow(dead_code)]
async fn remote_get_json<T: serde::de::DeserializeOwned + Default>(rb: &RemoteBackend, path: &str) -> T {
    match remote_client()
        .get(format!("{}{}", rb.url, path))
        .bearer_auth(&rb.token)
        .send()
        .await
    {
        Ok(resp) => resp.json::<T>().await.unwrap_or_default(),
        Err(_) => T::default(),
    }
}

async fn remote_put_json<T: Serialize>(rb: &RemoteBackend, path: &str, body: &T) {
    let _ = remote_client()
        .put(format!("{}{}", rb.url, path))
        .bearer_auth(&rb.token)
        .json(body)
        .send()
        .await;
}

async fn remote_post_json<T: Serialize>(rb: &RemoteBackend, path: &str, body: &T) {
    let _ = remote_client()
        .post(format!("{}{}", rb.url, path))
        .bearer_auth(&rb.token)
        .json(body)
        .send()
        .await;
}

async fn remote_delete(rb: &RemoteBackend, path: &str) {
    let _ = remote_client()
        .delete(format!("{}{}", rb.url, path))
        .bearer_auth(&rb.token)
        .send()
        .await;
}

async fn remote_get_result<T: serde::de::DeserializeOwned>(rb: &RemoteBackend, path: &str) -> Result<T, String> {
    let resp = remote_client()
        .get(format!("{}{}", rb.url, path))
        .bearer_auth(&rb.token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<T>().await.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// JSON file helpers
// ---------------------------------------------------------------------------

pub fn data_dir() -> PathBuf {
    // When running as a .app bundle or from any directory, use a stable location.
    // If a local "data" folder exists (dev mode), use it. Otherwise use ~/Library/Application Support/TigrimOS/data (macOS)
    // or ~/.local/share/TigrimOS/data (Linux) or %APPDATA%/TigrimOS/data (Windows).
    let local = PathBuf::from("data");
    if local.exists() {
        return local;
    }
    let app_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("TigrimOS")
        .join("data");
    let _ = std::fs::create_dir_all(&app_dir);
    app_dir
}

pub async fn read_json<T: serde::de::DeserializeOwned + Default>(file: &str) -> T {
    let fp = data_dir().join(file);
    match fs::read_to_string(&fp).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => T::default(),
    }
}

pub async fn write_json<T: Serialize>(file: &str, data: &T) {
    let fp = data_dir().join(file);
    let json = serde_json::to_string_pretty(data).expect("serialize");
    let _ = fs::write(&fp, json).await;
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageFeedback {
    pub rating: Option<String>,
    pub comment: Option<String>,
    #[serde(rename = "submittedAt")]
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<ChatMessageFeedback>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub messages: Vec<ChatMessage>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "skillCandidate", skip_serializing_if = "Option::is_none")]
    pub skill_candidate: Option<bool>,
    #[serde(rename = "skillFeedback", skip_serializing_if = "Option::is_none")]
    pub skill_feedback: Option<String>,
    #[serde(rename = "projectId", skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// Create a single chat session on the remote server. Returns the created session.
pub async fn remote_create_chat_session(title: &str, project_id: Option<&str>) -> Option<ChatSession> {
    let rb = get_remote_backend()?;
    let mut body = serde_json::json!({ "title": title });
    if let Some(pid) = project_id {
        body["projectId"] = serde_json::Value::String(pid.to_string());
    }
    let resp = remote_client()
        .post(format!("{}/api/chat/sessions", rb.url))
        .bearer_auth(&rb.token)
        .json(&body)
        .send()
        .await
        .ok()?;
    resp.json::<ChatSession>().await.ok()
}

pub async fn get_chat_history() -> Vec<ChatSession> {
    if let Some(rb) = get_remote_backend() {
        // Sync fast-path: return cached data immediately without HTTP
        if let Some(bulk) = remote_cache_try_get::<Vec<ChatSession>>("/api/chat/sessions/bulk") {
            if !bulk.is_empty() {
                return bulk;
            }
        }
        // Cache miss — fetch (this path is only hit on first load or cache expiry)
        let bulk: Vec<ChatSession> = remote_get_cached(&rb, "/api/chat/sessions/bulk", 10).await;
        if !bulk.is_empty() {
            return bulk;
        }
        // Fallback for old servers without bulk endpoint
        let summaries: Vec<serde_json::Value> = remote_get_cached(&rb, "/api/chat/sessions", 10).await;
        let mut sessions = Vec::new();
        for s in &summaries {
            if let Some(id) = s.get("id").and_then(|v| v.as_str()) {
                let path = format!("/api/chat/sessions/{}", id);
                let full: ChatSession = remote_get_cached(&rb, &path, 30).await;
                if !full.id.is_empty() {
                    sessions.push(full);
                }
            }
        }
        return sessions;
    }
    read_json("chat_history.json").await
}

pub async fn save_chat_history(sessions: &[ChatSession]) {
    if let Some(rb) = get_remote_backend() {
        remote_cache_invalidate("/api/chat/sessions/bulk");
        remote_cache_invalidate("/api/chat/sessions");
        let _ = remote_client()
            .put(format!("{}/api/chat/sessions/bulk", rb.url))
            .bearer_auth(&rb.token)
            .json(&sessions.to_vec())
            .send()
            .await;
        return;
    }
    write_json("chat_history.json", &sessions.to_vec()).await;
}

// ---------------------------------------------------------------------------
// Tasks (scheduled/cron)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub command: String,
    pub enabled: bool,
    #[serde(rename = "lastRun", skip_serializing_if = "Option::is_none")]
    pub last_run: Option<String>,
    #[serde(rename = "lastResult", skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// "chat" when created by the AI via the cron_create tool; None for manual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

pub async fn get_tasks() -> Vec<ScheduledTask> {
    if let Some(rb) = get_remote_backend() {
        // Sync fast-path
        if let Some(cached) = remote_cache_try_get::<Vec<ScheduledTask>>("/api/tasks") {
            return cached;
        }
        return remote_get_cached(&rb, "/api/tasks", 10).await;
    }
    read_json("tasks.json").await
}

pub async fn save_tasks(tasks: &[ScheduledTask]) {
    if let Some(rb) = get_remote_backend() {
        remote_cache_invalidate("/api/tasks");
        remote_put_json(&rb, "/api/tasks/bulk", &tasks.to_vec()).await;
        return;
    }
    write_json("tasks.json", &tasks.to_vec()).await;
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub url: String,
    pub enabled: bool,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteInstance {
    pub id: String,
    pub name: String,
    pub url: String,
    pub token: String,
}

/// One entry in the "router" mode model pool — a single, self-contained LLM
/// endpoint (its own model id, URL and key) that the orchestrator can route an
/// agent to. Different providers coexist because each entry carries its own
/// `api_url`/`api_key`; empty values inherit the main session's.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelPoolEntry {
    pub id: String,        // stable key, e.g. "claude_opus"
    pub label: String,     // display name
    pub model: String,     // model id passed to llm_call
    #[serde(default)]
    pub api_url: String,   // empty -> inherit session api_url
    #[serde(default)]
    pub api_key: String,   // empty -> inherit session api_key
    #[serde(default)]
    pub tier: String,      // "fast" | "balanced" | "deep"
    #[serde(default)]
    pub strengths: String, // freeform hint for the designer LLM
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalFileMount {
    pub id: String,
    pub path: String,
    pub label: String,
    pub permissions: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(rename = "sandboxDir", default)]
    pub sandbox_dir: String,
    #[serde(rename = "tigerBotApiKey", default)]
    pub tiger_bot_api_key: String,
    #[serde(rename = "tigerBotModel", default)]
    pub tiger_bot_model: String,
    #[serde(rename = "tigerBotApiUrl", skip_serializing_if = "Option::is_none")]
    pub tiger_bot_api_url: Option<String>,
    #[serde(rename = "mcpTools", default)]
    pub mcp_tools: Vec<McpTool>,
    #[serde(rename = "webSearchEnabled", default)]
    pub web_search_enabled: bool,
    #[serde(rename = "webSearchApiKey", skip_serializing_if = "Option::is_none")]
    pub web_search_api_key: Option<String>,
    #[serde(rename = "pythonPath", skip_serializing_if = "Option::is_none")]
    pub python_path: Option<String>,
    #[serde(rename = "subAgentEnabled", skip_serializing_if = "Option::is_none")]
    pub sub_agent_enabled: Option<bool>,
    #[serde(rename = "subAgentMode", skip_serializing_if = "Option::is_none")]
    pub sub_agent_mode: Option<String>,
    #[serde(rename = "subAgentModel", skip_serializing_if = "Option::is_none")]
    pub sub_agent_model: Option<String>,
    #[serde(rename = "subAgentConfigFile", skip_serializing_if = "Option::is_none")]
    pub sub_agent_config_file: Option<String>,
    /// Active agent-loop profile filename in data_dir()/agent_loops/.
    /// None/"" = built-in loop behavior.
    #[serde(rename = "agentLoopProfile", skip_serializing_if = "Option::is_none")]
    pub agent_loop_profile: Option<String>,
    #[serde(rename = "modelPool", skip_serializing_if = "Option::is_none")]
    pub model_pool: Option<Vec<ModelPoolEntry>>,
    #[serde(rename = "routerTier", skip_serializing_if = "Option::is_none")]
    pub router_tier: Option<String>,
    /// Which pool model the router ORCHESTRATOR runs on (triage + dispatch +
    /// merge). Stores the entry's `model` id; empty/unset = use the main model.
    #[serde(rename = "routerOrchestratorModel", skip_serializing_if = "Option::is_none")]
    pub router_orchestrator_model: Option<String>,
    #[serde(rename = "remoteEnabled", skip_serializing_if = "Option::is_none")]
    pub remote_enabled: Option<bool>,
    #[serde(rename = "remoteToken", skip_serializing_if = "Option::is_none")]
    pub remote_token: Option<String>,
    #[serde(rename = "remoteTaskMaxRetries", skip_serializing_if = "Option::is_none")]
    pub remote_task_max_retries: Option<u64>,
    #[serde(rename = "remoteInstances", skip_serializing_if = "Option::is_none")]
    pub remote_instances: Option<Vec<RemoteInstance>>,
    /// When true, reach this host remotely over a Tailscale VPN instead of a
    /// public Cloudflare tunnel (mutually exclusive "remote connection method").
    /// Off by default — behavior is unchanged unless the user opts in.
    #[serde(rename = "vpnEnabled", skip_serializing_if = "Option::is_none")]
    pub vpn_enabled: Option<bool>,
    #[serde(rename = "localFileMounts", skip_serializing_if = "Option::is_none")]
    pub local_file_mounts: Option<Vec<LocalFileMount>>,
    #[serde(rename = "skillAutoUpdateEnabled", skip_serializing_if = "Option::is_none")]
    pub skill_auto_update_enabled: Option<bool>,
    #[serde(rename = "skillAutoUpdateIntervalMinutes", skip_serializing_if = "Option::is_none")]
    pub skill_auto_update_interval_minutes: Option<u64>,
    #[serde(rename = "skillAutoUpdateRequireApproval", skip_serializing_if = "Option::is_none")]
    pub skill_auto_update_require_approval: Option<bool>,
    #[serde(rename = "skillAutoUpdateHumanFeedbackEnabled", skip_serializing_if = "Option::is_none")]
    pub skill_auto_update_human_feedback_enabled: Option<bool>,
    #[serde(rename = "skillAutoUpdateMaxCandidates", skip_serializing_if = "Option::is_none")]
    pub skill_auto_update_max_candidates: Option<u64>,
    // Tool approval security settings
    #[serde(rename = "approvalRequiredForShell", skip_serializing_if = "Option::is_none")]
    pub approval_required_for_shell: Option<bool>,
    #[serde(rename = "approvalRequiredForPython", skip_serializing_if = "Option::is_none")]
    pub approval_required_for_python: Option<bool>,
    #[serde(rename = "approvalRequiredForFileWrite", skip_serializing_if = "Option::is_none")]
    pub approval_required_for_file_write: Option<bool>,
    #[serde(rename = "approvalRequiredForFileDelete", skip_serializing_if = "Option::is_none")]
    pub approval_required_for_file_delete: Option<bool>,
    #[serde(rename = "approvalRequiredForAgentSpawn", skip_serializing_if = "Option::is_none")]
    pub approval_required_for_agent_spawn: Option<bool>,
    /// Whether background swarm sub-agents may auto-run tools that would otherwise
    /// require interactive approval. Background agents have no UI to prompt, so the
    /// interactive approval channel cannot serve them; this toggle governs them instead.
    /// Defaults to true so auto-swarm runs don't stall. Set false to make sub-agents
    /// refuse approval-gated tools (run_shell/run_python/etc.) rather than run them.
    #[serde(rename = "autoApproveSubagentTools", skip_serializing_if = "Option::is_none")]
    pub auto_approve_subagent_tools: Option<bool>,
    /// Browser control: when true, a Playwright browser MCP server is
    /// auto-registered so the agent can drive a real web browser (navigate,
    /// click, type, screenshot). Off by default for safety — once on, the agent
    /// can act in the browser (submit forms, click buttons) as the user.
    #[serde(rename = "browserControlEnabled", skip_serializing_if = "Option::is_none")]
    pub browser_control_enabled: Option<bool>,
    /// Which engine browser control drives: "chromium" (Playwright's bundled
    /// build — portable, always available) or "chrome" (the user's installed
    /// Google Chrome). Defaults to "chrome" — real Chrome trips far fewer bot
    /// blocks (e.g. Google search) than bundled Chromium. Falls back to nothing
    /// if Chrome isn't installed, so set this to "chromium" on hosts without it.
    #[serde(rename = "browserEngine", skip_serializing_if = "Option::is_none")]
    pub browser_engine: Option<String>,
    /// Whether browser control runs the browser headless (no visible window).
    /// When unset, follows the process `--headless` flag (legacy behaviour: a
    /// UI-less server also ran the browser headless). Set to `false` to force a
    /// real, headful browser even on a UI-less server — required to beat
    /// headless-detection blocking (Google etc.). On a server with no display
    /// you must provide a virtual one (e.g. run under `xvfb-run`).
    #[serde(rename = "browserHeadless", skip_serializing_if = "Option::is_none")]
    pub browser_headless: Option<bool>,
    // Catch-all for unknown fields
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Sync cache-only accessor — returns cached settings without blocking.
pub fn get_settings_cached() -> Option<Settings> {
    remote_cache_try_get::<Settings>("/api/settings")
}

/// Sync cache-only accessor — returns cached projects without blocking.
pub fn get_projects_cached() -> Option<Vec<Project>> {
    remote_cache_try_get::<Vec<Project>>("/api/projects")
}

pub async fn get_settings() -> Settings {
    if let Some(rb) = get_remote_backend() {
        // Sync fast-path
        if let Some(cached) = remote_cache_try_get::<Settings>("/api/settings") {
            return cached;
        }
        return remote_get_cached(&rb, "/api/settings", 15).await;
    }
    let mut settings: Settings = read_json("settings.json").await;
    if settings.skill_auto_update_enabled.is_none() {
        settings.skill_auto_update_enabled = Some(true);
    }
    if settings.skill_auto_update_interval_minutes.is_none() {
        settings.skill_auto_update_interval_minutes = Some(5);
    }
    if settings.skill_auto_update_require_approval.is_none() {
        settings.skill_auto_update_require_approval = Some(true);
    }
    if settings.skill_auto_update_human_feedback_enabled.is_none() {
        settings.skill_auto_update_human_feedback_enabled = Some(true);
    }
    if settings.skill_auto_update_max_candidates.is_none() {
        settings.skill_auto_update_max_candidates = Some(10);
    }
    if settings.approval_required_for_shell.is_none() {
        settings.approval_required_for_shell = Some(true);
    }
    if settings.approval_required_for_python.is_none() {
        settings.approval_required_for_python = Some(true);
    }
    if settings.approval_required_for_file_write.is_none() {
        settings.approval_required_for_file_write = Some(false);
    }
    if settings.approval_required_for_file_delete.is_none() {
        settings.approval_required_for_file_delete = Some(true);
    }
    if settings.approval_required_for_agent_spawn.is_none() {
        settings.approval_required_for_agent_spawn = Some(false);
    }
    if settings.auto_approve_subagent_tools.is_none() {
        settings.auto_approve_subagent_tools = Some(true);
    }
    if settings.browser_control_enabled.is_none() {
        settings.browser_control_enabled = Some(false);
    }
    if settings.browser_engine.is_none() {
        settings.browser_engine = Some("chrome".to_string());
    }
    // Default to the seeded agent-loop profile (behavior-identical mirror of
    // the built-in loop). "" stays as an explicit "no profile" user choice.
    if settings.agent_loop_profile.is_none() {
        settings.agent_loop_profile = Some("default.yaml".to_string());
    }
    // browser_headless intentionally left None by default: None = follow the
    // process --headless flag (legacy). Only an explicit Some(false)/Some(true)
    // overrides the browser's headless mode independently of the server UI.
    settings
}

pub async fn save_settings(settings: &Settings) {
    if let Some(rb) = get_remote_backend() {
        remote_cache_invalidate("/api/settings");
        remote_put_json(&rb, "/api/settings", settings).await;
        return;
    }
    write_json("settings.json", settings).await;
}

/// Synchronous sandbox dir accessor for UI code.
pub fn get_sandbox_dir_sync() -> String {
    // Read settings from the correct data_dir (works from .app bundle too)
    let settings_path = data_dir().join("settings.json");
    if let Ok(content) = std::fs::read_to_string(&settings_path) {
        if let Ok(settings) = serde_json::from_str::<Settings>(&content) {
            if !settings.sandbox_dir.is_empty() {
                // Resolve relative sandbox_dir to absolute under app data
                let raw = &settings.sandbox_dir;
                if std::path::Path::new(raw).is_absolute() {
                    return raw.clone();
                }
            }
        }
    }
    // Default: sandbox dir next to data dir (e.g. ~/Library/Application Support/TigrimOS/sandbox)
    let sandbox = data_dir()
        .parent()
        .unwrap_or(&std::path::PathBuf::from("."))
        .join("sandbox");
    let _ = std::fs::create_dir_all(&sandbox);
    sandbox.to_string_lossy().to_string()
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOverride {
    pub enabled: Option<bool>,
    #[serde(rename = "subAgentMode", skip_serializing_if = "Option::is_none")]
    pub sub_agent_mode: Option<String>,
    #[serde(rename = "subAgentConfigFile", skip_serializing_if = "Option::is_none")]
    pub sub_agent_config_file: Option<String>,
    #[serde(rename = "agentLoopProfile", skip_serializing_if = "Option::is_none")]
    pub agent_loop_profile: Option<String>,
    #[serde(rename = "autoArchitectureType", skip_serializing_if = "Option::is_none")]
    pub auto_architecture_type: Option<String>,
    #[serde(rename = "autoAgentCount", skip_serializing_if = "Option::is_none")]
    pub auto_agent_count: Option<serde_json::Value>,
    #[serde(rename = "autoProtocols", skip_serializing_if = "Option::is_none")]
    pub auto_protocols: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "workingFolder")]
    pub working_folder: String,
    pub memory: String,
    pub skills: Vec<String>,
    #[serde(rename = "systemPrompt", skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(rename = "agentOverride", skip_serializing_if = "Option::is_none")]
    pub agent_override: Option<AgentOverride>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

pub async fn get_projects() -> Vec<Project> {
    let mut projects: Vec<Project> = if let Some(rb) = get_remote_backend() {
        if let Some(cached) = remote_cache_try_get::<Vec<Project>>("/api/projects") {
            cached
        } else {
            remote_get_cached(&rb, "/api/projects", 10).await
        }
    } else {
        read_json("projects.json").await
    };
    // Resolve any relative working_folder paths to absolute under sandbox
    let sandbox = get_sandbox_dir_sync();
    for p in &mut projects {
        if !p.working_folder.is_empty() && !std::path::Path::new(&p.working_folder).is_absolute() {
            p.working_folder = std::path::PathBuf::from(&sandbox)
                .join(&p.working_folder)
                .to_string_lossy()
                .to_string();
        }
    }
    projects
}

pub async fn save_projects(projects: &[Project]) {
    if let Some(rb) = get_remote_backend() {
        remote_cache_invalidate("/api/projects");
        remote_put_json(&rb, "/api/projects/bulk", &projects.to_vec()).await;
        return;
    }
    write_json("projects.json", &projects.to_vec()).await;
}

// ---------------------------------------------------------------------------
// File Access Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileToken {
    pub id: String,
    pub name: String,
    pub token: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

pub async fn get_file_tokens() -> Vec<FileToken> {
    if let Some(rb) = get_remote_backend() {
        if let Some(cached) = remote_cache_try_get::<Vec<FileToken>>("/api/settings/file-tokens") {
            return cached;
        }
        return remote_get_cached(&rb, "/api/settings/file-tokens", 15).await;
    }
    read_json("file_tokens.json").await
}

pub async fn save_file_tokens(tokens: &[FileToken]) {
    if let Some(rb) = get_remote_backend() {
        remote_cache_invalidate("/api/settings/file-tokens");
        remote_put_json(&rb, "/api/settings/file-tokens/bulk", &tokens.to_vec()).await;
        return;
    }
    write_json("file_tokens.json", &tokens.to_vec()).await;
}

pub fn generate_token() -> String {
    let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
        .chars()
        .collect();
    let mut rng = rand::thread_rng();
    (0..48).map(|_| chars[rng.gen_range(0..chars.len())]).collect()
}

pub async fn is_valid_file_token(token: &str) -> bool {
    let tokens = get_file_tokens().await;
    tokens.iter().any(|t| t.token == token)
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillAutoMeta {
    pub kind: String,
    #[serde(rename = "basedOn")]
    pub based_on: Vec<String>,
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    pub model: String,
    #[serde(rename = "proposedPath", skip_serializing_if = "Option::is_none")]
    pub proposed_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub script: String,
    pub enabled: bool,
    #[serde(rename = "installedAt")]
    pub installed_at: String,
    #[serde(rename = "reviewStatus", skip_serializing_if = "Option::is_none")]
    pub review_status: Option<String>,
    #[serde(rename = "autoMeta", skip_serializing_if = "Option::is_none")]
    pub auto_meta: Option<SkillAutoMeta>,
}

pub async fn get_skills() -> Vec<Skill> {
    if let Some(rb) = get_remote_backend() {
        // Sync fast-path
        if let Some(cached) = remote_cache_try_get::<Vec<Skill>>("/api/skills") {
            return cached;
        }
        return remote_get_cached(&rb, "/api/skills", 15).await;
    }
    read_json("skills.json").await
}

pub async fn save_skills(skills: &[Skill]) {
    if let Some(rb) = get_remote_backend() {
        remote_cache_invalidate("/api/skills");
        remote_put_json(&rb, "/api/skills/bulk", &skills.to_vec()).await;
        return;
    }
    write_json("skills.json", &skills.to_vec()).await;
}

// ---------------------------------------------------------------------------
// Agent History (JSONL)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub async fn ensure_agent_history_dir(session_id: &str) -> PathBuf {
    let dir = data_dir().join("agent_history").join(session_id);
    let _ = fs::create_dir_all(&dir).await;
    dir
}

#[allow(dead_code)]
pub async fn append_agent_history(session_id: &str, file: &str, entry: &serde_json::Value) {
    let dir = ensure_agent_history_dir(session_id).await;
    let fp = dir.join(file);
    let line = format!("{}\n", serde_json::to_string(entry).unwrap_or_default());
    if let Ok(mut f) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&fp)
        .await
    {
        use tokio::io::AsyncWriteExt;
        let _ = f.write_all(line.as_bytes()).await;
    }
}

#[allow(dead_code)]
pub async fn read_agent_history(session_id: &str, file: &str) -> Vec<serde_json::Value> {
    let fp = data_dir()
        .join("agent_history")
        .join(session_id)
        .join(file);
    match fs::read_to_string(&fp).await {
        Ok(content) => content
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

pub async fn delete_agent_history(session_id: &str) {
    if get_remote_backend().is_some() {
        return; // agent history is server-local
    }
    let dir = data_dir().join("agent_history").join(session_id);
    let _ = fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// Sandbox file operations
// ---------------------------------------------------------------------------

pub fn validate_path(sandbox_dir: &str, requested: &str) -> Result<PathBuf, String> {
    let root = std::path::Path::new(sandbox_dir).canonicalize().unwrap_or_else(|_| PathBuf::from(sandbox_dir));
    let resolved = root.join(requested);
    let resolved = resolved.canonicalize().unwrap_or(resolved);
    if !resolved.starts_with(&root) {
        return Err("Access denied: path outside workspace".to_string());
    }
    Ok(resolved)
}

pub async fn list_files(sandbox_dir: &str, sub_path: &str) -> Result<Vec<serde_json::Value>, String> {
    if let Some(rb) = get_remote_backend() {
        let encoded = urlencoding::encode(sub_path);
        let path = format!("/api/files?path={}", encoded);
        if let Some(cached) = remote_cache_try_get::<Vec<serde_json::Value>>(&path) {
            return Ok(cached);
        }
        let result: Vec<serde_json::Value> = remote_get_cached(&rb, &path, 10).await;
        return Ok(result);
    }

    let dir = if sub_path.is_empty() {
        PathBuf::from(sandbox_dir)
    } else {
        validate_path(sandbox_dir, sub_path)?
    };

    let mut entries = match fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };

    let mut results = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let metadata = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let entry_path = if sub_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", sub_path, name)
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| {
                let dt: DateTime<Utc> = t.into();
                Some(dt.to_rfc3339())
            })
            .unwrap_or_default();

        results.push(serde_json::json!({
            "name": name,
            "path": entry_path,
            "isDirectory": metadata.is_dir(),
            "size": if metadata.is_dir() { 0 } else { metadata.len() },
            "modified": modified,
        }));
    }
    Ok(results)
}

pub async fn read_file_content(sandbox_dir: &str, file_path: &str) -> Result<String, String> {
    if let Some(rb) = get_remote_backend() {
        let encoded = urlencoding::encode(file_path);
        return remote_get_result(&rb, &format!("/api/files/read?path={}", encoded)).await;
    }
    let resolved = validate_path(sandbox_dir, file_path)?;
    fs::read_to_string(&resolved)
        .await
        .map_err(|e| e.to_string())
}

pub async fn write_file_content(
    sandbox_dir: &str,
    file_path: &str,
    content: &str,
) -> Result<(), String> {
    if let Some(rb) = get_remote_backend() {
        let body = serde_json::json!({ "path": file_path, "content": content });
        remote_post_json(&rb, "/api/files/write", &body).await;
        return Ok(());
    }
    let resolved = validate_path(sandbox_dir, file_path)?;
    if let Some(parent) = resolved.parent() {
        let _ = fs::create_dir_all(parent).await;
    }
    fs::write(&resolved, content)
        .await
        .map_err(|e| e.to_string())
}

/// Write raw bytes into the sandbox — remote-aware (multipart upload to the
/// connected server). `file_path` is sandbox-relative including the filename.
pub async fn write_file_bytes(
    sandbox_dir: &str,
    file_path: &str,
    bytes: Vec<u8>,
) -> Result<(), String> {
    if let Some(rb) = get_remote_backend() {
        let (dir, name) = match file_path.rsplit_once('/') {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), file_path.to_string()),
        };
        let part = reqwest::multipart::Part::bytes(bytes).file_name(name);
        let form = reqwest::multipart::Form::new()
            .text("path", dir)
            .part("file", part);
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/api/files/upload", rb.url))
            .bearer_auth(&rb.token)
            .timeout(std::time::Duration::from_secs(300))
            .multipart(form)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("Remote upload failed: HTTP {}", resp.status()));
        }
        return Ok(());
    }
    let resolved = validate_path(sandbox_dir, file_path)?;
    if let Some(parent) = resolved.parent() {
        let _ = fs::create_dir_all(parent).await;
    }
    fs::write(&resolved, &bytes).await.map_err(|e| e.to_string())
}

/// Read raw bytes from the sandbox — remote-aware (downloads from the
/// connected server).
pub async fn read_file_bytes(sandbox_dir: &str, file_path: &str) -> Result<Vec<u8>, String> {
    if let Some(rb) = get_remote_backend() {
        let encoded = urlencoding::encode(file_path);
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/api/files/download?path={}", rb.url, encoded))
            .bearer_auth(&rb.token)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("Remote download failed: HTTP {}", resp.status()));
        }
        return resp.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string());
    }
    let resolved = validate_path(sandbox_dir, file_path)?;
    fs::read(&resolved).await.map_err(|e| e.to_string())
}

pub async fn delete_file_or_dir(sandbox_dir: &str, file_path: &str) -> Result<(), String> {
    if let Some(rb) = get_remote_backend() {
        let encoded = urlencoding::encode(file_path);
        remote_delete(&rb, &format!("/api/files?path={}", encoded)).await;
        return Ok(());
    }
    let resolved = validate_path(sandbox_dir, file_path)?;
    let meta = fs::metadata(&resolved)
        .await
        .map_err(|e| e.to_string())?;
    if meta.is_dir() {
        fs::remove_dir_all(&resolved)
            .await
            .map_err(|e| e.to_string())
    } else {
        fs::remove_file(&resolved)
            .await
            .map_err(|e| e.to_string())
    }
}
