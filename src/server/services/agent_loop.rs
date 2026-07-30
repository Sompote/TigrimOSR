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
// - Approval gates follow tool_requires_approval unless
//   tools.config.<name>.require_approval overrides them; protected
//   coordination tools are never approval-gated while orchestration is active.
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
    /// Outer evaluation loop: tool-using judge that runs ONCE after the whole
    /// job finishes (top-level main agent only, never sub-agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<EvaluationKnobs>,
    /// Graph gate: judge panel (data/graph/ profiles) reviewing the final
    /// answer before delivery. `enabled` overrides the global graphEnabled
    /// setting either way; omitted = follow settings (default off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphKnobs>,
}

impl AgentLoopProfile {
    /// Per-tool config entry for `name`, if the profile carries one.
    pub fn tool_config(&self, name: &str) -> Option<&ToolConfig> {
        self.tools.as_ref().and_then(|t| t.config.get(name))
    }
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
    /// Per-tool overrides keyed by tool name (built-in or MCP).
    /// BTreeMap so serialized YAML has a stable key order.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub config: std::collections::BTreeMap<String, ToolConfig>,
}

impl ToolFilter {
    pub fn allows(&self, name: &str) -> bool {
        if self.config.get(name).and_then(|c| c.enabled) == Some(false) {
            return false;
        }
        match self.mode.as_str() {
            "allowlist" => self.list.iter().any(|t| t == name),
            "denylist" => !self.list.iter().any(|t| t == name),
            _ => true,
        }
    }
}

/// Per-tool overrides. Every field is optional; absent = inherit current
/// behavior. Map keys are tool names — built-in (see toolbox::tool_catalog)
/// or MCP tool names — so one mechanism covers both uniformly.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolConfig {
    /// false = tool removed from the model's tool list and hard-denied at
    /// dispatch (sugar for denylisting). Protected coordination tools exempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Some(true) = always approval-gate this tool; Some(false) = never gate;
    /// None = inherit the global tool_requires_approval logic. Who approves is
    /// unchanged: main agent gets the UI modal, background sub-agents follow
    /// auto_approve_subagent_tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_approval: Option<bool>,
    /// Replaces the tool description the model sees in the tool spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default argument values, injected only when the model omits the key
    /// (shallow, top-level keys of the args object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Map<String, Value>>,
    /// Hard overrides that ALWAYS overwrite what the model sends
    /// (shallow, top-level keys of the args object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_params: Option<serde_json::Map<String, Value>>,
    /// Truncate the tool result (bare string, or every top-level string field
    /// of an object result) to this many bytes, UTF-8 safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_result_len: Option<u64>,
    /// Wall-clock cap in seconds on tool execution. Ignored for protected
    /// coordination tools, which keep their own timeout machinery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Merge a tool-call's arguments with a ToolConfig: `params` fill in keys the
/// model omitted, `pinned_params` always overwrite. Shallow — top-level keys
/// only. Non-object args are replaced by an object when the config has any
/// params to apply (tool args are always JSON objects in practice).
pub fn merge_tool_args(args: &Value, cfg: &ToolConfig) -> Value {
    if cfg.params.is_none() && cfg.pinned_params.is_none() {
        return args.clone();
    }
    let mut obj = args.as_object().cloned().unwrap_or_default();
    if let Some(defaults) = &cfg.params {
        for (k, v) in defaults {
            obj.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    if let Some(pins) = &cfg.pinned_params {
        for (k, v) in pins {
            obj.insert(k.clone(), v.clone());
        }
    }
    Value::Object(obj)
}

/// Commented example appended to seeded/reset default.yaml files. serde_yaml
/// cannot emit comments, so this rides along as a trailing block (ignored on
/// parse; lost only if the user re-saves from the form editor).
pub const TOOL_CONFIG_EXAMPLE: &str = "\
# Per-tool overrides — uncomment and nest under tools:
#   config:
#     run_shell:
#       require_approval: false   # true=always ask, false=never ask, absent=global default
#       timeout_secs: 120         # wall-clock cap on execution
#       max_result_len: 4000      # truncate result strings (UTF-8 safe)
#       pinned_params: { cwd: \".\" }   # always overwrite model-sent values
#       params: { }                     # defaults used when the model omits them
#     web_search:
#       description: \"Override the description the model sees.\"
#     some_tool:
#       enabled: false            # remove the tool from the model entirely
";

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

/// Outer evaluation loop knobs. The judge verifies the FINAL job result
/// against the user objective (and optional rubric), and may call read-only
/// tools (read_file/list_files) to check that claimed artifacts exist.
/// Runs only at top level (depth 0, agent "main"); on a failing score the
/// gap list is injected back so the orchestrator can delegate targeted fixes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EvaluationKnobs {
    /// Default false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Pass score in 0.0..=1.0 (default 0.75).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// Outer judge→fix cycles, clamped 1..=5 (default 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u64>,
    /// Worker tool rounds per fix cycle, clamped 1..=10 (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fix_rounds: Option<u64>,
    /// Judge mini tool-loop rounds, clamped 1..=6 (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_judge_rounds: Option<u64>,
    /// Dedicated judge model; "" = session model (avoids self-grading bias).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Judge API URL; "" = session api_url.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    /// Judge API key; "" = session api_key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// User-defined success criteria appended to the judge prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rubric: Option<String>,
    /// true also grants run_python/run_shell to the judge (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_execute: Option<bool>,
}

/// Graph-gate knobs inside an agent-loop profile:
///   graph:
///     enabled: true              # turn the judge panel on/off for this profile
///     profile: strict.yaml       # optional graph profile in data/graph/ (default: active/global)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphKnobs {
    /// Some(true/false) overrides the global graphEnabled setting; None = follow it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Graph profile filename in data/graph/ used when the gate is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
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
        tools: Some(ToolFilter {
            mode: "all".to_string(),
            list: Vec::new(),
            config: Default::default(),
        }),
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
        evaluation: Some(EvaluationKnobs {
            enabled: Some(b("agentEvaluationEnabled", false)),
            threshold: Some(f("agentEvaluationThreshold", 0.75)),
            max_retries: Some(u("agentEvaluationMaxRetries", 2)),
            max_fix_rounds: Some(5),
            max_judge_rounds: Some(u("agentEvaluationMaxJudgeRounds", 3)),
            model: settings
                .get("agentEvaluationModel")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            api_url: None,
            api_key: None,
            rubric: None,
            allow_execute: Some(false),
        }),
        // Graph gate deliberately unset: omitted = follow the global
        // graphEnabled setting, which defaults to OFF.
        graph: None,
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
        Ok(yaml) => match std::fs::write(&path, format!("{yaml}\n{TOOL_CONFIG_EXAMPLE}")) {
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
    let mcp = agent_def
        .get("mcp_servers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            let servers: Vec<String> = arr
                .iter()
                .filter_map(|s| s.as_str())
                .map(|s| s.to_string())
                .collect();
            McpFilter {
                mode: if servers.is_empty() {
                    "none".to_string()
                } else {
                    "selected".to_string()
                },
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
        .map(|s| SystemPromptOverride {
            text: s.to_string(),
            replace_base: false,
        });

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
        name: agent_def
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: String::new(),
        model: None, // per-agent model/api_url/api_key are handled by the existing YAML fields
        system_prompt,
        tools,
        mcp,
        skills,
        loop_,
        compaction,
        // Per-agent evaluation is meaningless: the outer eval loop only runs
        // for the top-level main agent, never for team/sub-agents.
        evaluation: None,
        // Same reasoning: the graph gate is top-level only.
        graph: None,
    })
}

/// Layer a per-profile ToolConfig over a global (data/tools file) one:
/// profile fields win where set, global fills the gaps.
pub fn merge_tool_config(
    global: Option<ToolConfig>,
    profile: Option<&ToolConfig>,
) -> Option<ToolConfig> {
    match (global, profile) {
        (None, None) => None,
        (Some(g), None) => Some(g),
        (None, Some(p)) => Some(p.clone()),
        (Some(g), Some(p)) => Some(ToolConfig {
            enabled: p.enabled.or(g.enabled),
            require_approval: p.require_approval.or(g.require_approval),
            description: p.description.clone().or(g.description),
            params: p.params.clone().or(g.params),
            pinned_params: p.pinned_params.clone().or(g.pinned_params),
            timeout_secs: p.timeout_secs.or(g.timeout_secs),
            max_result_len: p.max_result_len.or(g.max_result_len),
        }),
    }
}

// ---------------------------------------------------------------------------
// Per-tool config editor documents (Settings > Tools > Configure).
//
// Built-in tools edit like custom-tool YAML files: the editor shows a full
// document with the tool's ACTUAL current values (override if set, else the
// built-in default), and save stores only the fields that differ from the
// defaults back into tools.config.<name>. Shared by the desktop UI and the
// /api/agent-loops/{profile}/tool-config/{tool} REST endpoints.
// ---------------------------------------------------------------------------

/// The editable document shape. `name`/`kind` are informational and ignored
/// on save; unknown keys are ignored so stray comments/typos don't hard-fail.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolEditorDoc {
    pub enabled: Option<bool>,
    pub require_approval: Option<bool>,
    pub description: Option<String>,
    pub params: Option<serde_json::Map<String, Value>>,
    pub pinned_params: Option<serde_json::Map<String, Value>>,
    pub timeout_secs: Option<u64>,
    pub max_result_len: Option<u64>,
}

/// Render the tool's parameter schema as comment lines so users know which
/// argument keys params/pinned_params can target (e.g. `command` for
/// run_shell). One line per parameter: name, type, required flag, description.
fn param_schema_comments(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(props) = schema.pointer("/properties").and_then(|p| p.as_object()) else {
        return out;
    };
    if props.is_empty() {
        return out;
    }
    let required: Vec<&str> = schema
        .pointer("/required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    out.push("#".to_string());
    out.push("# Arguments this tool accepts (keys usable in params / pinned_params):".to_string());
    for (name, p) in props {
        let ty = p.get("type").and_then(|t| t.as_str()).unwrap_or("any");
        let req = if required.contains(&name.as_str()) {
            ", required"
        } else {
            ""
        };
        let desc = p
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .replace('\n', " ");
        let desc = crate::util::truncate_utf8_ellipsis(&desc, 110);
        if desc.is_empty() {
            out.push(format!("#   {name} ({ty}{req})"));
        } else {
            out.push(format!("#   {name} ({ty}{req}) — {desc}"));
        }
    }
    out
}

/// Render the editor YAML for one tool: current effective values plus
/// commented hints for the optional fields that aren't set.
pub fn tool_editor_yaml(
    tool: &str,
    cfg: Option<&ToolConfig>,
    builtin_desc: &str,
    default_approval: bool,
    param_schema: Option<&Value>,
) -> String {
    let doc = ToolEditorDoc {
        enabled: Some(cfg.and_then(|c| c.enabled).unwrap_or(true)),
        require_approval: Some(
            cfg.and_then(|c| c.require_approval)
                .unwrap_or(default_approval),
        ),
        description: Some(
            cfg.and_then(|c| c.description.clone())
                .unwrap_or_else(|| builtin_desc.to_string()),
        ),
        params: cfg.and_then(|c| c.params.clone()),
        pinned_params: cfg.and_then(|c| c.pinned_params.clone()),
        timeout_secs: cfg.and_then(|c| c.timeout_secs),
        max_result_len: cfg.and_then(|c| c.max_result_len),
    };
    let body = serde_yaml::to_string(&doc).unwrap_or_default();
    // serde skips None fields via Option — but our struct serializes them as
    // nulls; strip `key: null` lines and add commented hints instead.
    let mut lines: Vec<String> = body
        .lines()
        .filter(|l| !l.trim_end().ends_with(": null"))
        .map(|l| l.to_string())
        .collect();
    if doc.params.is_none() {
        lines.push("# params: {}             # default args when the model omits them".into());
    }
    if doc.pinned_params.is_none() {
        lines.push("# pinned_params: {}      # forced args the model cannot override".into());
    }
    if doc.timeout_secs.is_none() {
        lines.push("# timeout_secs: 60       # wall-clock cap".into());
    }
    if doc.max_result_len.is_none() {
        lines.push("# max_result_len: 4000   # truncate the result".into());
    }
    let mut header: Vec<String> = vec![
        format!("# Built-in tool '{tool}' — saved into the active profile's tools.config."),
        "# Save stores only values that differ from the defaults; matching the".into(),
        "# defaults (or an empty file) removes the override entirely.".into(),
    ];
    if let Some(schema) = param_schema {
        header.extend(param_schema_comments(schema));
    }
    format!("{}\n{}\n", header.join("\n"), lines.join("\n"))
}

/// Parse an editor document back into an override. Returns Ok(None) when the
/// values match the defaults (= remove the override). Err on invalid YAML.
pub fn tool_editor_to_config(
    content: &str,
    builtin_desc: &str,
    default_approval: bool,
) -> Result<Option<ToolConfig>, String> {
    // Comments/blank-only = explicit clear.
    let has_content = content.lines().any(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    });
    if !has_content {
        return Ok(None);
    }
    let doc: ToolEditorDoc =
        serde_yaml::from_str(content).map_err(|e| format!("Invalid tool-config YAML: {e}"))?;
    let cfg = ToolConfig {
        enabled: match doc.enabled {
            Some(false) => Some(false),
            _ => None,
        },
        require_approval: doc.require_approval.filter(|v| *v != default_approval),
        description: doc
            .description
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty() && d != builtin_desc.trim()),
        params: doc.params.filter(|m| !m.is_empty()),
        pinned_params: doc.pinned_params.filter(|m| !m.is_empty()),
        timeout_secs: doc.timeout_secs,
        max_result_len: doc.max_result_len,
    };
    let empty = cfg.enabled.is_none()
        && cfg.require_approval.is_none()
        && cfg.description.is_none()
        && cfg.params.is_none()
        && cfg.pinned_params.is_none()
        && cfg.timeout_secs.is_none()
        && cfg.max_result_len.is_none();
    Ok(if empty { None } else { Some(cfg) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn graph_knobs_parse_and_round_trip() {
        let yaml = "name: g\ngraph:\n  enabled: true\n  profile: strict.yaml\n";
        let p: AgentLoopProfile = serde_yaml::from_str(yaml).unwrap();
        let g = p.graph.as_ref().unwrap();
        assert_eq!(g.enabled, Some(true));
        assert_eq!(g.profile.as_deref(), Some("strict.yaml"));
        let out = serde_yaml::to_string(&p).unwrap();
        let p2: AgentLoopProfile = serde_yaml::from_str(&out).unwrap();
        assert_eq!(p2.graph.unwrap().enabled, Some(true));
        // Omitted section stays None (= follow the global toggle, default off).
        let p3: AgentLoopProfile = serde_yaml::from_str("name: plain\n").unwrap();
        assert!(p3.graph.is_none());
        // enabled: false explicitly overrides the settings toggle.
        let p4: AgentLoopProfile = serde_yaml::from_str("graph:\n  enabled: false\n").unwrap();
        assert_eq!(p4.graph.unwrap().enabled, Some(false));
    }

    #[test]
    fn config_less_profile_still_parses() {
        let yaml = "name: legacy\ntools:\n  mode: allowlist\n  list: [run_shell]\n";
        let p: AgentLoopProfile = serde_yaml::from_str(yaml).unwrap();
        let tf = p.tools.unwrap();
        assert!(tf.config.is_empty());
        assert!(tf.allows("run_shell"));
        assert!(!tf.allows("web_search"));
    }

    #[test]
    fn tool_config_yaml_round_trip() {
        let yaml = r#"
name: tooltest
tools:
  mode: all
  list: []
  config:
    run_shell:
      require_approval: false
      timeout_secs: 5
      pinned_params: { cwd: "." }
    read_file:
      max_result_len: 300
    web_search:
      enabled: false
      description: "custom"
      params: { region: "th-th" }
"#;
        let p: AgentLoopProfile = serde_yaml::from_str(yaml).unwrap();
        let sh = p.tool_config("run_shell").unwrap();
        assert_eq!(sh.require_approval, Some(false));
        assert_eq!(sh.timeout_secs, Some(5));
        assert_eq!(sh.pinned_params.as_ref().unwrap()["cwd"], json!("."));
        assert_eq!(
            p.tool_config("read_file").unwrap().max_result_len,
            Some(300)
        );
        let ws = p.tool_config("web_search").unwrap();
        assert_eq!(ws.enabled, Some(false));
        assert_eq!(ws.description.as_deref(), Some("custom"));

        let out = serde_yaml::to_string(&p).unwrap();
        let p2: AgentLoopProfile = serde_yaml::from_str(&out).unwrap();
        assert_eq!(p2.tool_config("run_shell").unwrap().timeout_secs, Some(5));
        assert_eq!(p2.tool_config("web_search").unwrap().enabled, Some(false));
    }

    #[test]
    fn enabled_false_denies_in_every_mode() {
        for (mode, list) in [
            ("all", vec![]),
            ("allowlist", vec!["web_search".to_string()]),
            ("denylist", vec![]),
        ] {
            let mut config = std::collections::BTreeMap::new();
            config.insert(
                "web_search".to_string(),
                ToolConfig {
                    enabled: Some(false),
                    ..Default::default()
                },
            );
            let tf = ToolFilter {
                mode: mode.to_string(),
                list,
                config,
            };
            assert!(
                !tf.allows("web_search"),
                "mode {mode} should deny disabled tool"
            );
        }
    }

    #[test]
    fn merge_tool_args_defaults_and_pins() {
        let cfg = ToolConfig {
            params: serde_json::from_value(json!({"region": "th-th", "limit": 10})).unwrap(),
            pinned_params: serde_json::from_value(json!({"cwd": "."})).unwrap(),
            ..Default::default()
        };
        // Model-sent value survives a default but not a pin.
        let merged = merge_tool_args(&json!({"limit": 3, "cwd": "/tmp"}), &cfg);
        assert_eq!(merged["limit"], json!(3));
        assert_eq!(merged["region"], json!("th-th"));
        assert_eq!(merged["cwd"], json!("."));
        // Non-object args become an object carrying the config values.
        let merged = merge_tool_args(&Value::Null, &cfg);
        assert_eq!(merged["cwd"], json!("."));
        // No params configured -> args pass through untouched.
        let noop = ToolConfig {
            max_result_len: Some(100),
            ..Default::default()
        };
        assert_eq!(merge_tool_args(&json!({"a": 1}), &noop), json!({"a": 1}));
    }
}
