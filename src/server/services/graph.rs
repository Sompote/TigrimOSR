// ---------------------------------------------------------------------------
// Graph mode profiles — evaluator-optimizer agent graphs stored as YAML files
// in data_dir()/graph/*.yaml, with judge rule files in data_dir()/graph/rules/.
//
// A graph profile wires the evaluator-optimizer pattern: the worker node (the
// main agent loop, running in any existing sub-agent mode) produces the final
// answer, then a panel of one or more judge nodes reviews it against YAML rule
// files BEFORE it reaches the human. A failing aggregate verdict is fed back
// to the worker as structured YAML revision instructions; the cycle repeats
// until the panel passes or max_iterations is exhausted.
//
// Judges fail OPEN: a judge that errors is skipped, and if every judge errors
// the answer is released — a misconfigured judge must never trap an answer.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphProfile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Worker node: which sub-agent mode the main loop runs in under the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<WorkerNode>,
    /// Judge panel: one entry = single judge, several = multi-judge.
    #[serde(default)]
    pub judges: Vec<JudgeNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregation: Option<AggregationPolicy>,
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_: Option<GraphLoopKnobs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkerNode {
    /// "single" | "auto" | "manual" | "fully_auto" | "auto_swarm" | "router";
    /// "" = single.
    #[serde(default)]
    pub mode: String,
    /// Optional agent-loop profile filename applied to the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_loop_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JudgeNode {
    #[serde(default)]
    pub name: String,
    /// Dedicated judge model; "" = session model (avoids self-grading bias).
    #[serde(default)]
    pub model: String,
    /// "" = session api_url / api_key.
    #[serde(default)]
    pub api_url: String,
    #[serde(default)]
    pub api_key: String,
    /// Inline rules text — appended after rules_file content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    /// Filename in data/graph/rules/ (e.g. "default_rules.yaml").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_file: Option<String>,
    /// Weight for weighted_average aggregation (default 1.0; <= 0 = ignored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    /// Per-judge pass score override (default = aggregation.threshold).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// true (default) = judge may call read_file/list_files to verify claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_tools: Option<bool>,
    /// true also grants run_python/run_shell to the judge (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_execute: Option<bool>,
    /// Judge mini tool-loop rounds, clamped 1..=6 (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_judge_rounds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AggregationPolicy {
    /// "all_pass" (default) | "majority" | "weighted_average"
    #[serde(default)]
    pub policy: String,
    /// Pass score in 0.0..=1.0 (default 0.75). Used as the per-judge pass bar
    /// (unless a judge overrides it) and as the weighted-average bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphLoopKnobs {
    /// Judge→revise cycles, clamped 1..=5 (default 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u64>,
    /// Worker tool rounds per revision cycle, clamped 1..=10 (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fix_rounds: Option<u64>,
    /// true (default) = judge even answers produced without tool calls.
    /// Legacy evaluation skips those; graph mode gates everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_plain_answers: Option<bool>,
}

impl GraphProfile {
    pub fn worker_mode(&self) -> &str {
        match self.worker.as_ref().map(|w| w.mode.trim()) {
            Some("") | None => "single",
            Some(m) => m,
        }
    }
}

pub const WORKER_MODES: &[&str] = &[
    "single",
    "auto",
    "manual",
    "fully_auto",
    "auto_swarm",
    "router",
];
pub const AGGREGATION_POLICIES: &[&str] = &["all_pass", "majority", "weighted_average"];

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

pub const DEFAULT_PROFILE_FILE: &str = "default.yaml";
pub const DEFAULT_RULES_FILE: &str = "default_rules.yaml";

pub fn graph_dir() -> std::path::PathBuf {
    crate::server::data::data_dir().join("graph")
}

pub fn rules_dir() -> std::path::PathBuf {
    graph_dir().join("rules")
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

/// Reject anything that could escape the rules/profile directory.
pub fn is_safe_filename(name: &str) -> bool {
    let t = name.trim();
    !t.is_empty() && !t.contains('/') && !t.contains('\\') && !t.contains("..")
}

/// Load a graph profile by name or filename. None on missing/invalid file.
pub fn load_profile(name: &str) -> Option<GraphProfile> {
    let trimmed = name.trim();
    if trimmed.is_empty() || !is_safe_filename(trimmed) {
        return None;
    }
    let path = graph_dir().join(normalize_filename(trimmed));
    let content = std::fs::read_to_string(&path).ok()?;
    match serde_yaml::from_str::<GraphProfile>(&content) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!("[graph] Failed to parse profile {:?}: {}", path, e);
            None
        }
    }
}

/// Load a rule file's raw text from data/graph/rules/. None on missing file
/// or unsafe name (path traversal guarded).
pub fn load_rules_file(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || !is_safe_filename(trimmed) {
        return None;
    }
    let path = rules_dir().join(normalize_filename(trimmed));
    match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(_) => {
            tracing::warn!("[graph] Rules file not readable: {:?}", path);
            None
        }
    }
}

pub fn default_profile() -> GraphProfile {
    GraphProfile {
        name: "default".to_string(),
        description:
            "Evaluator-optimizer graph — a judge panel reviews the final answer before delivery."
                .to_string(),
        worker: Some(WorkerNode {
            mode: "single".to_string(),
            agent_loop_profile: None,
        }),
        judges: vec![JudgeNode {
            name: "quality".to_string(),
            model: String::new(),
            api_url: String::new(),
            api_key: String::new(),
            rules: None,
            rules_file: Some(DEFAULT_RULES_FILE.to_string()),
            weight: Some(1.0),
            threshold: None,
            use_tools: Some(true),
            allow_execute: Some(false),
            max_judge_rounds: Some(3),
        }],
        aggregation: Some(AggregationPolicy {
            policy: "all_pass".to_string(),
            threshold: Some(0.75),
        }),
        loop_: Some(GraphLoopKnobs {
            max_iterations: Some(2),
            max_fix_rounds: Some(5),
            judge_plain_answers: Some(true),
        }),
    }
}

const DEFAULT_RULES_CONTENT: &str = "\
# Judge rules — rendered verbatim into the judge's system prompt. The judge
# is asked to echo each rule id in its verdict's rule_results list.
# severity: blocker = failing this rule fails the verdict; warn = noted only.
rules:
  - id: answers-all-parts
    severity: blocker
    description: Every distinct part of the user's request is answered in the final answer itself.
  - id: no-fabrication
    severity: blocker
    description: Claims must be backed by tool evidence; no invented data, numbers, or files.
  - id: artifacts-exist
    severity: warn
    description: Files or charts the answer claims to have produced must exist on disk.
";

/// Seed data/graph/default.yaml and data/graph/rules/default_rules.yaml if
/// missing. Never overwrites existing files. Returns true when the default
/// profile exists after the call.
pub fn ensure_default_profile() -> bool {
    let dir = graph_dir();
    if let Err(e) = std::fs::create_dir_all(rules_dir()) {
        tracing::warn!("[graph] Failed to create {:?}: {}", rules_dir(), e);
        return false;
    }
    let rules_path = rules_dir().join(DEFAULT_RULES_FILE);
    if !rules_path.exists() {
        if let Err(e) = std::fs::write(&rules_path, DEFAULT_RULES_CONTENT) {
            tracing::warn!("[graph] Failed to write {:?}: {}", rules_path, e);
        }
    }
    let path = dir.join(DEFAULT_PROFILE_FILE);
    if path.exists() {
        return true;
    }
    match serde_yaml::to_string(&default_profile()) {
        Ok(yaml) => match std::fs::write(&path, yaml) {
            Ok(()) => {
                tracing::info!("[graph] Seeded default graph profile at {:?}", path);
                true
            }
            Err(e) => {
                tracing::warn!("[graph] Failed to write {:?}: {}", path, e);
                false
            }
        },
        Err(e) => {
            tracing::warn!("[graph] Failed to serialize default profile: {}", e);
            false
        }
    }
}

/// Resolve the active graph profile: request override wins over the project
/// override, which wins over the global setting. Empty names and unreadable
/// files resolve to None.
pub fn resolve_active_profile(
    settings_profile: Option<&str>,
    project_profile: Option<&str>,
    request_profile: Option<&str>,
) -> Option<GraphProfile> {
    request_profile
        .filter(|s| !s.trim().is_empty())
        .or(project_profile.filter(|s| !s.trim().is_empty()))
        .or(settings_profile.filter(|s| !s.trim().is_empty()))
        .and_then(load_profile)
}

/// Full judge rules text: rules_file content first, inline rules appended.
pub fn resolve_judge_rules(judge: &JudgeNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(file) = judge.rules_file.as_deref() {
        if !file.trim().is_empty() {
            if let Some(text) = load_rules_file(file) {
                parts.push(text);
            }
        }
    }
    if let Some(inline) = judge.rules.as_deref() {
        if !inline.trim().is_empty() {
            parts.push(inline.to_string());
        }
    }
    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Parse + validate a graph profile YAML document.
/// Hard errors abort the save; warnings ride along.
pub fn validate_graph_yaml(content: &str) -> Result<(GraphProfile, Vec<String>), String> {
    let profile: GraphProfile =
        serde_yaml::from_str(content).map_err(|e| format!("Invalid YAML: {e}"))?;
    let mut warnings = Vec::new();

    if profile.judges.is_empty() {
        return Err("A graph profile needs at least one judge (judges: [...])".to_string());
    }
    let mode = profile.worker_mode();
    if !WORKER_MODES.contains(&mode) {
        return Err(format!(
            "Unknown worker.mode '{}' (expected one of: {})",
            mode,
            WORKER_MODES.join(", ")
        ));
    }
    if let Some(agg) = &profile.aggregation {
        let p = agg.policy.trim();
        if !p.is_empty() && !AGGREGATION_POLICIES.contains(&p) {
            return Err(format!(
                "Unknown aggregation.policy '{}' (expected one of: {})",
                p,
                AGGREGATION_POLICIES.join(", ")
            ));
        }
        if let Some(t) = agg.threshold {
            if !(0.0..=1.0).contains(&t) {
                return Err(format!("aggregation.threshold {} outside 0.0..=1.0", t));
            }
        }
    }
    for (i, judge) in profile.judges.iter().enumerate() {
        let label = if judge.name.trim().is_empty() {
            format!("judges[{i}]")
        } else {
            format!("judge '{}'", judge.name)
        };
        if let Some(t) = judge.threshold {
            if !(0.0..=1.0).contains(&t) {
                return Err(format!("{label}: threshold {} outside 0.0..=1.0", t));
            }
        }
        if let Some(w) = judge.weight {
            if w <= 0.0 {
                warnings.push(format!("{label}: weight {} <= 0 — judge is ignored", w));
            }
        }
        if judge.allow_execute == Some(true) {
            warnings.push(format!(
                "{label}: allow_execute grants run_python/run_shell to the judge"
            ));
        }
        if let Some(file) = judge.rules_file.as_deref() {
            let f = file.trim();
            if !f.is_empty() {
                if !is_safe_filename(f) {
                    return Err(format!("{label}: unsafe rules_file name '{f}'"));
                }
                if !rules_dir().join(normalize_filename(f)).exists() {
                    warnings.push(format!(
                        "{label}: rules_file '{f}' not found in data/graph/rules/"
                    ));
                }
            }
        }
        if let Some(r) = judge.max_judge_rounds {
            if r > 6 {
                warnings.push(format!(
                    "{label}: max_judge_rounds {} will be clamped to 6",
                    r
                ));
            }
        }
    }
    if let Some(knobs) = &profile.loop_ {
        if let Some(i) = knobs.max_iterations {
            if i > 5 {
                warnings.push(format!("loop.max_iterations {} will be clamped to 5", i));
            }
        }
        if let Some(r) = knobs.max_fix_rounds {
            if r > 10 {
                warnings.push(format!("loop.max_fix_rounds {} will be clamped to 10", r));
            }
        }
    }
    Ok((profile, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_profile_parses_with_defaults() {
        let yaml = "name: g\njudges:\n  - name: q\n";
        let (p, warnings) = validate_graph_yaml(yaml).unwrap();
        assert_eq!(p.worker_mode(), "single");
        assert_eq!(p.judges.len(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn empty_judges_rejected() {
        assert!(validate_graph_yaml("name: g\njudges: []\n").is_err());
        assert!(validate_graph_yaml("name: g\n").is_err());
    }

    #[test]
    fn bad_policy_and_mode_rejected() {
        let yaml = "judges: [{name: q}]\naggregation: {policy: consensus}\n";
        assert!(validate_graph_yaml(yaml).is_err());
        let yaml = "judges: [{name: q}]\nworker: {mode: warp}\n";
        assert!(validate_graph_yaml(yaml).is_err());
    }

    #[test]
    fn thresholds_validated() {
        assert!(validate_graph_yaml("judges: [{name: q, threshold: 1.5}]\n").is_err());
        assert!(
            validate_graph_yaml("judges: [{name: q}]\naggregation: {threshold: -0.1}\n").is_err()
        );
        assert!(validate_graph_yaml("judges: [{name: q, threshold: 0.9}]\n").is_ok());
    }

    #[test]
    fn unsafe_filenames_rejected() {
        assert!(!is_safe_filename("../escape.yaml"));
        assert!(!is_safe_filename("a/b.yaml"));
        assert!(!is_safe_filename("a\\b.yaml"));
        assert!(!is_safe_filename(""));
        assert!(is_safe_filename("rules-1.yaml"));
        let yaml = "judges: [{name: q, rules_file: \"../x.yaml\"}]\n";
        assert!(validate_graph_yaml(yaml).is_err());
    }

    #[test]
    fn clamp_warnings_emitted() {
        let yaml = "judges: [{name: q, max_judge_rounds: 9, weight: 0}]\nloop: {max_iterations: 8, max_fix_rounds: 20}\n";
        let (_, warnings) = validate_graph_yaml(yaml).unwrap();
        assert_eq!(warnings.len(), 4);
    }

    #[test]
    fn default_profile_round_trips() {
        let yaml = serde_yaml::to_string(&default_profile()).unwrap();
        let (p, warnings) = validate_graph_yaml(&yaml).unwrap();
        assert_eq!(p.name, "default");
        assert_eq!(p.judges[0].rules_file.as_deref(), Some(DEFAULT_RULES_FILE));
        // rules_file existence warning is environment-dependent; policy/mode clean.
        assert!(warnings.iter().all(|w| w.contains("rules_file")));
    }
}
