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
  settings.json       EVERY system setting, seeded with defaults — same
                      camelCase keys as the desktop Settings UI (approvals,
                      browser control, MCP servers, model pool, sub-agent
                      mode, loop knobs, …). NO keys here — use .env.
  SOUL.md             optional agent persona for THIS folder (global one is
  IDENTITY.md         never used in CLI mode; omit both for a neutral agent)

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

/// Seed a COMPLETE `.tigrimos/settings.json` — every setting the desktop
/// Settings UI can write, materialized with its default value, so the folder
/// has one visible, editable file covering the whole system. Same camelCase
/// keys as the desktop app. Credentials are deliberately blanked: they belong
/// in `.env` (gitignored), and env vars override this file at load time.
pub async fn seed_settings_file(proj: &Path) {
    let fp = proj.join("settings.json");
    if fp.exists() {
        return;
    }
    let settings = crate::server::data::get_settings().await;
    let mut map = match serde_json::to_value(&settings) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => return,
    };
    let fill: &[(&str, serde_json::Value)] = &[
        // Provider (prefer .env for the key/url/model — env wins over this file)
        ("tigerBotApiKey", serde_json::json!("")),
        ("tigerBotApiUrl", serde_json::json!("")),
        ("tigerBotModel", serde_json::json!("")),
        ("modelPool", serde_json::json!([])),
        // Sub-agents / orchestration
        ("subAgentMode", serde_json::json!("single")),
        ("subAgentConfigFile", serde_json::json!("")),
        ("graphEnabled", serde_json::json!(false)),
        // Tools & integrations
        ("mcpTools", serde_json::json!([])),
        ("webSearchEnabled", serde_json::json!(false)),
        ("sandboxDir", serde_json::json!("")),
        // Agent-loop knobs (same keys the desktop Settings UI writes; the
        // folder's agent_loops/default.yaml takes precedence when set)
        ("agentMaxToolRounds", serde_json::json!(15)),
        ("agentMaxToolCalls", serde_json::json!(25)),
        ("agentTemperature", serde_json::json!(0.7)),
        ("agentMaxTokens", serde_json::json!(81920)),
        ("agentReflectionEnabled", serde_json::json!(false)),
        ("agentReflectionThreshold", serde_json::json!(0.7)),
        ("agentMaxReflectionRetries", serde_json::json!(2)),
        ("agentCheckpointEnabled", serde_json::json!(true)),
        ("agentMaxConsecutiveErrors", serde_json::json!(3)),
        ("agentMaxErrorRecoveries", serde_json::json!(5)),
        ("agentCompressionInterval", serde_json::json!(5)),
        ("agentCompressionWindow", serde_json::json!(10)),
        ("agentMaxContextTokens", serde_json::json!(100000)),
        ("agentToolResultMaxLen", serde_json::json!(6000)),
        ("agentEvaluationEnabled", serde_json::json!(false)),
        ("agentEvaluationThreshold", serde_json::json!(0.75)),
        ("agentEvaluationMaxJudgeRounds", serde_json::json!(3)),
        ("agentAllowUnsandboxedExec", serde_json::json!(false)),
    ];
    for (k, default) in fill {
        map.entry(k.to_string()).or_insert(default.clone());
    }
    // Never write credentials here, even when .env already supplied them —
    // this file documents settings; .env holds secrets.
    map.insert("tigerBotApiKey".to_string(), serde_json::json!(""));
    let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default();
    let _ = std::fs::write(&fp, pretty);
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
