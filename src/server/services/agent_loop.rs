// ---------------------------------------------------------------------------
// Agent loop profiles — user-configurable agent loop settings stored as YAML
// files in data_dir()/agent_loops/*.yaml.
//
// A profile controls which built-in tools the agent may use, which MCP
// servers' tools are exposed, which skills are offered in the system prompt,
// model/system-prompt overrides, loop knobs (rounds, temperature, ...) and
// context-compaction knobs. Every omitted section means "inherit current
// behavior" — an empty profile is a no-op.
//
// Semantics guaranteed elsewhere in the codebase (do not weaken here):
// - The profile tool filter is intersected AFTER tool_definitions_for_mode,
//   and coordination tools are protected (is_protected_tool in toolbox.rs)
//   so a profile can never brick swarm/router orchestration.
// - Approval gates (tool_requires_approval) are NOT bypassed by a profile.
// - compaction.enabled=false disables only PERIODIC compression; proactive
//   over-budget and emergency overflow compaction stay always-on.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentLoopProfile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<SystemPromptOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillFilter>,
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_: Option<LoopKnobs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionKnobs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelOverride {
    #[serde(default)]
    pub model: String, // "" = inherit
    #[serde(default)]
    pub api_url: String, // "" = inherit
    #[serde(default)]
    pub api_key: String, // "" = inherit
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemPromptOverride {
    #[serde(default)]
    pub text: String,
    /// false (default) = text is appended to the built-in base prompt;
    /// true = text replaces the base (skills/SOUL/project prompts still apply).
    #[serde(default)]
    pub replace_base: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolFilter {
    /// "allowlist" | "denylist" | anything else = all
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub list: Vec<String>,
}

impl ToolFilter {
    pub fn allows(&self, name: &str) -> bool {
        match self.mode.as_str() {
            "allowlist" => self.list.iter().any(|t| t == name),
            "denylist" => !self.list.iter().any(|t| t == name),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpFilter {
    /// "all" (default) | "selected" | "none"
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillFilter {
    /// "all" (default) | "selected" | "none"
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub list: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoopKnobs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflection_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflection_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reflection_retries: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_consecutive_errors: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_error_recoveries: Option<u64>,
    /// Clamped to 1..=5 (built-in default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_depth: Option<u64>,
    /// Realtime per-step judge (verify each team agent's finished step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_verification: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompactionKnobs {
    /// false disables PERIODIC compression only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_max_len: Option<u64>,
    /// Dedicated (usually cheaper) summarization model; "" = session model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

pub const DEFAULT_PROFILE_FILE: &str = "default.yaml";

pub fn agent_loops_dir() -> std::path::PathBuf {
    crate::server::data::data_dir().join("agent_loops")
}

/// Normalize "foo" / "foo.yaml" / "foo.yml" to an on-disk filename.
pub fn normalize_filename(name: &str) -> String {
    let base = name.trim();
    if base.ends_with(".yaml") || base.ends_with(".yml") {
        base.to_string()
    } else {
        format!("{}.yaml", base)
    }
}

/// Load a profile by name or filename. Returns None on missing/invalid file.
pub fn load_profile(name: &str) -> Option<AgentLoopProfile> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = agent_loops_dir().join(normalize_filename(trimmed));
    let content = std::fs::read_to_string(&path).ok()?;
    match serde_yaml::from_str::<AgentLoopProfile>(&content) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!("[agent_loop] Failed to parse profile {:?}: {}", path, e);
            None
        }
    }
}

/// Build the profile that mirrors CURRENT loop behavior: everything enabled,
/// knobs taken from live settings.json values (same keys/defaults as the
/// toolbox loop reads).
pub fn default_profile_from_settings(settings: &Value) -> AgentLoopProfile {
    let u = |key: &str, def: u64| settings.get(key).and_then(|v| v.as_u64()).unwrap_or(def);
    let b = |key: &str, def: bool| settings.get(key).and_then(|v| v.as_bool()).unwrap_or(def);
    let f = |key: &str, def: f64| settings.get(key).and_then(|v| v.as_f64()).unwrap_or(def);
    AgentLoopProfile {
        name: "default".to_string(),
        description: "Seeded from current settings — mirrors the built-in agent loop. Edit freely or clone via New.".to_string(),
        model: None,
        system_prompt: None,
        tools: Some(ToolFilter { mode: "all".to_string(), list: Vec::new() }),
        mcp: Some(McpFilter { mode: "all".to_string(), servers: Vec::new() }),
        skills: Some(SkillFilter { mode: "all".to_string(), list: Vec::new() }),
        loop_: Some(LoopKnobs {
            max_rounds: Some(u("agentMaxToolRounds", 15)),
            max_tool_calls: Some(u("agentMaxToolCalls", 25)),
            temperature: Some(f("agentTemperature", 0.7)),
            max_tokens: Some(u("agentMaxTokens", 81920)),
            reflection_enabled: Some(b("agentReflectionEnabled", false)),
            reflection_threshold: Some(f("agentReflectionThreshold", 0.7)),
            max_reflection_retries: Some(u("agentMaxReflectionRetries", 2)),
            checkpoint_enabled: Some(b("agentCheckpointEnabled", true)),
            max_consecutive_errors: Some(u("agentMaxConsecutiveErrors", 3)),
            max_error_recoveries: Some(u("agentMaxErrorRecoveries", 5)),
            max_spawn_depth: Some(3),
            step_verification: Some(true),
        }),
        compaction: Some(CompactionKnobs {
            enabled: Some(true),
            interval: Some(u("agentCompressionInterval", 5)),
            window: Some(u("agentCompressionWindow", 10)),
            max_context_tokens: Some(u("agentMaxContextTokens", 100_000)),
            tool_result_max_len: Some(u("agentToolResultMaxLen", 6000)),
            model: settings
                .get("agentCompressionModel")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }),
    }
}

/// Seed data_dir()/agent_loops/default.yaml if missing. Never overwrites an
/// existing file. Returns true if the file exists after the call.
pub fn ensure_default_profile() -> bool {
    let dir = agent_loops_dir();
    let path = dir.join(DEFAULT_PROFILE_FILE);
    if path.exists() {
        return true;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("[agent_loop] Failed to create {:?}: {}", dir, e);
        return false;
    }
    let settings = std::fs::read_to_string(crate::server::data::data_dir().join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let profile = default_profile_from_settings(&settings);
    match serde_yaml::to_string(&profile) {
        Ok(yaml) => match std::fs::write(&path, yaml) {
            Ok(()) => {
                tracing::info!("[agent_loop] Seeded default profile at {:?}", path);
                true
            }
            Err(e) => {
                tracing::warn!("[agent_loop] Failed to write {:?}: {}", path, e);
                false
            }
        },
        Err(e) => {
            tracing::warn!("[agent_loop] Failed to serialize default profile: {}", e);
            false
        }
    }
}

/// Resolve the active profile: project override wins over the global setting.
/// Empty/missing names and unreadable files resolve to None (built-in behavior).
pub fn resolve_active_profile(
    settings_profile: Option<&str>,
    project_profile: Option<&str>,
) -> Option<AgentLoopProfile> {
    project_profile
        .filter(|s| !s.trim().is_empty())
        .or(settings_profile.filter(|s| !s.trim().is_empty()))
        .and_then(load_profile)
}

// ---------------------------------------------------------------------------
// Per-agent profiles from team YAML agent definitions
// ---------------------------------------------------------------------------

/// Build a profile from a team-YAML agent definition's optional fields:
///   tools: {mode, list}
///   mcp_servers: [..]   — shorthand: present = selected, [] = none, absent = all
///   skills: {mode, list}
///   loop: {..}
///   compaction: {..}
///   system_prompt: "..."  — appended to the generated agent prompt
/// Returns None when the definition carries none of these fields.
pub fn profile_from_agent_def(agent_def: &Value) -> Option<AgentLoopProfile> {
    let tools = agent_def
        .get("tools")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<ToolFilter>(v.clone()).ok());
    let mcp = agent_def.get("mcp_servers").and_then(|v| v.as_array()).map(|arr| {
        let servers: Vec<String> = arr
            .iter()
            .filter_map(|s| s.as_str())
            .map(|s| s.to_string())
            .collect();
        McpFilter {
            mode: if servers.is_empty() { "none".to_string() } else { "selected".to_string() },
            servers,
        }
    });
    let skills = agent_def
        .get("skills")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<SkillFilter>(v.clone()).ok());
    let loop_ = agent_def
        .get("loop")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<LoopKnobs>(v.clone()).ok());
    let compaction = agent_def
        .get("compaction")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<CompactionKnobs>(v.clone()).ok());
    let system_prompt = agent_def
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| SystemPromptOverride { text: s.to_string(), replace_base: false });

    if tools.is_none()
        && mcp.is_none()
        && skills.is_none()
        && loop_.is_none()
        && compaction.is_none()
        && system_prompt.is_none()
    {
        return None;
    }
    Some(AgentLoopProfile {
        name: agent_def.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        description: String::new(),
        model: None, // per-agent model/api_url/api_key are handled by the existing YAML fields
        system_prompt,
        tools,
        mcp,
        skills,
        loop_,
        compaction,
    })
}
