//! Project-local `.tigrimos/` state for the CLI.
//!
//! The `tigrim` binary treats the folder it is launched in as the workspace
//! (Claude Code semantics): `.tigrimos/` holds per-folder config overlays
//! (agents/, agent_loops/, graph/, settings.json) plus per-folder state
//! (chat history, CLI session state, REPL history).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PROJECT_DIR_NAME: &str = ".tigrimos";

/// Per-folder CLI session state, persisted to `.tigrimos/cli_state.json`
/// (routed there by `data::resolve_data_file`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliState {
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(rename = "loopProfile", skip_serializing_if = "Option::is_none")]
    pub loop_profile: Option<String>,
    #[serde(rename = "graphProfile", skip_serializing_if = "Option::is_none")]
    pub graph_profile: Option<String>,
    #[serde(rename = "configFile", skip_serializing_if = "Option::is_none")]
    pub config_file: Option<String>,
}

const GITIGNORE: &str = "chat_history.json\ncli_state.json\nrepl_history\n";

const README: &str = "\
TigrimOS project folder (like Claude Code's .claude/).

Config here overlays the global data directory — same-named files win:
  agents/*.yaml       agent team definitions
  agent_loops/*.yaml  agent-loop profiles
  graph/*.yaml        graph (judge panel) profiles
  settings.json       partial settings override (top-level keys only)

State kept per folder (gitignored): chat_history.json, cli_state.json,
repl_history.
";

/// Ensure `<cwd>/.tigrimos` exists with its overlay subdirs and register it
/// as the process project dir. Returns the project dir path.
pub fn init_project(cwd: &Path) -> PathBuf {
    let proj = cwd.join(PROJECT_DIR_NAME);
    for sub in ["agents", "agent_loops", "graph"] {
        let _ = std::fs::create_dir_all(proj.join(sub));
    }
    let gitignore = proj.join(".gitignore");
    if !gitignore.exists() {
        let _ = std::fs::write(&gitignore, GITIGNORE);
    }
    let readme = proj.join("README.md");
    if !readme.exists() {
        let _ = std::fs::write(&readme, README);
    }
    crate::server::data::set_project_dir(proj.clone());
    proj
}

pub async fn load_state() -> CliState {
    crate::server::data::read_json("cli_state.json").await
}

pub async fn save_state(state: &CliState) {
    crate::server::data::write_json("cli_state.json", state).await;
}

/// New per-folder session id.
pub fn new_session_id() -> String {
    format!("cli_{}", chrono::Utc::now().timestamp_millis())
}
