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

/// settings.json and .env are gitignored on purpose: project folders are
/// usually git repos, and either file can carry an API key — a committed key
/// is a leaked key. Force-add settings.json only if it holds no secrets.
const GITIGNORE: &str = "\
chat_history.json
cli_state.json
repl_history
settings.json
.env
";

const README: &str = "\
TigrimOS project folder (like Claude Code's .claude/).

The CLI is folder-local: it never reads the desktop app's global settings.
Setup — put your API credentials in `.env` here (or in the project root):
copy `.env.example` to `.env` and fill it in (the first interactive run does
this for you). NEVER commit a real key; `.env` and `settings.json` are
gitignored for exactly that reason.

YAML config here shadows the global data directory (YAML/skills only):
  agents/*.yaml       agent team definitions (example_team.yaml seeded)
  agent_loops/*.yaml  agent-loop profiles (default.yaml seeded — edit freely)
  graph/*.yaml        graph (judge panel) profiles (default.yaml seeded)
  settings.json       this folder's settings (written by /model etc. — NO keys)

State kept per folder (gitignored): chat_history.json, cli_state.json,
repl_history.
";

const ENV_EXAMPLE: &str = "\
# TigrimOS CLI setup — copy this file to `.env` (same folder, or the project
# root) and fill in your values. `.env` is gitignored; never commit real keys.
#
# The CLI uses ONLY this folder's configuration (never the desktop app's
# global settings), so every folder sets its own provider here.
TIGRIMOS_API_KEY=sk-your-key-here
TIGRIMOS_API_URL=https://api.deepseek.com/v1
TIGRIMOS_MODEL=deepseek-chat
";

const EXAMPLE_TEAM: &str = "\
# Example agent team — select it with `/agent example_team` (mode switches to
# `auto`). Edit roles/personas freely, or add more agents with unique ids.
system:
  name: Example Research Team
  orchestration_mode: hierarchical
  communication_protocol: structured_handoff
  context_passing: full_chain
agents:
  - id: agent_1
    name: Researcher
    role: worker
    persona: >-
      You are a meticulous researcher. You gather facts from the web and
      files, cite your sources, and clearly separate evidence from inference.
    responsibilities:
      - Search for and collect relevant information
      - Verify claims across at least two sources
      - Hand findings to the writer as structured notes
  - id: agent_2
    name: Writer
    role: orchestrator
    persona: >-
      You are a clear technical writer. You turn the researcher's notes into
      a concise, well-structured answer for the user.
    responsibilities:
      - Plan the work and delegate research tasks
      - Synthesize findings into the final answer
      - Flag any gaps or low-confidence claims
";

/// Ensure `<cwd>/.tigrimos` exists with its overlay subdirs and register it
/// as the process project dir. Returns the project dir path.
pub fn init_project(cwd: &Path) -> PathBuf {
    let proj = cwd.join(PROJECT_DIR_NAME);
    for sub in ["agents", "agent_loops", "graph"] {
        let _ = std::fs::create_dir_all(proj.join(sub));
    }
    // Always (re)write the gitignore — it protects secrets; older versions
    // missed settings.json/.env and must be upgraded in place.
    let _ = std::fs::write(proj.join(".gitignore"), GITIGNORE);
    let readme = proj.join("README.md");
    if !readme.exists() {
        let _ = std::fs::write(&readme, README);
    }
    let env_example = proj.join(".env.example");
    if !env_example.exists() {
        let _ = std::fs::write(&env_example, ENV_EXAMPLE);
    }
    crate::server::data::set_project_dir(proj.clone());
    proj
}

/// Seed editable starter YAMLs into empty project config dirs. Called AFTER
/// server bootstrap so the global defaults exist to copy — the project copy
/// is behavior-identical at seed time and shadows the global when edited.
pub fn seed_examples(proj: &Path) {
    let dir_is_empty = |d: &Path| {
        std::fs::read_dir(d)
            .map(|mut e| e.next().is_none())
            .unwrap_or(true)
    };
    let global = crate::server::data::data_dir();

    let loops = proj.join("agent_loops");
    if dir_is_empty(&loops) {
        let _ = std::fs::copy(global.join("agent_loops/default.yaml"), loops.join("default.yaml"));
    }
    let graph = proj.join("graph");
    if dir_is_empty(&graph) {
        let _ = std::fs::copy(global.join("graph/default.yaml"), graph.join("default.yaml"));
        let rules_src = global.join("graph/rules/default_rules.yaml");
        if rules_src.exists() {
            let rules_dst = graph.join("rules");
            let _ = std::fs::create_dir_all(&rules_dst);
            let _ = std::fs::copy(rules_src, rules_dst.join("default_rules.yaml"));
        }
    }
    let agents = proj.join("agents");
    if dir_is_empty(&agents) {
        let _ = std::fs::write(agents.join("example_team.yaml"), EXAMPLE_TEAM);
    }
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
