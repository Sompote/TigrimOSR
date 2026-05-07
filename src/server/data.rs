use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::fs;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

pub async fn get_chat_history() -> Vec<ChatSession> {
    read_json("chat_history.json").await
}

pub async fn save_chat_history(sessions: &[ChatSession]) {
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
}

pub async fn get_tasks() -> Vec<ScheduledTask> {
    read_json("tasks.json").await
}

pub async fn save_tasks(tasks: &[ScheduledTask]) {
    write_json("tasks.json", &tasks.to_vec()).await;
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpTool {
    pub name: String,
    pub url: String,
    pub enabled: bool,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<String>,
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
    #[serde(rename = "remoteEnabled", skip_serializing_if = "Option::is_none")]
    pub remote_enabled: Option<bool>,
    #[serde(rename = "remoteToken", skip_serializing_if = "Option::is_none")]
    pub remote_token: Option<String>,
    #[serde(rename = "remoteTaskMaxRetries", skip_serializing_if = "Option::is_none")]
    pub remote_task_max_retries: Option<u64>,
    #[serde(rename = "remoteInstances", skip_serializing_if = "Option::is_none")]
    pub remote_instances: Option<Vec<RemoteInstance>>,
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
    // Catch-all for unknown fields
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

pub async fn get_settings() -> Settings {
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
    settings
}

pub async fn save_settings(settings: &Settings) {
    write_json("settings.json", settings).await;
}

/// Synchronous sandbox dir accessor for UI code.
pub fn get_sandbox_dir_sync() -> String {
    let path = std::path::Path::new("data/settings.json");
    if let Ok(content) = std::fs::read_to_string(path) {
        if let Ok(settings) = serde_json::from_str::<Settings>(&content) {
            if !settings.sandbox_dir.is_empty() {
                return settings.sandbox_dir;
            }
        }
    }
    "sandbox".to_string()
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
    read_json("projects.json").await
}

pub async fn save_projects(projects: &[Project]) {
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
    read_json("file_tokens.json").await
}

pub async fn save_file_tokens(tokens: &[FileToken]) {
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
    read_json("skills.json").await
}

pub async fn save_skills(skills: &[Skill]) {
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
    let resolved = validate_path(sandbox_dir, file_path)?;
    if let Some(parent) = resolved.parent() {
        let _ = fs::create_dir_all(parent).await;
    }
    fs::write(&resolved, content)
        .await
        .map_err(|e| e.to_string())
}

pub async fn delete_file_or_dir(sandbox_dir: &str, file_path: &str) -> Result<(), String> {
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
