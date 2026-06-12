//! Skill Synthesizer — faithful port of tiger_cowork/server/services/skill-synthesizer.ts
//!
//! Two-pass architecture:
//!   Pass 1: Remediation — sessions with thumbs-down feedback drive focused single-skill rewrites
//!   Pass 2: Synthesis — remaining sessions propose new skills or updates
//!
//! Features:
//!   - Rich session summaries with user queries, final assistant, feedback, subagent workflow
//!   - Chat log parsing for subagent workflow traces (AGENT_SPAWN/DONE/WORKING/COMPLETE)
//!   - Feedback-aware prompting (thumbs-up reinforces, thumbs-down blocks)
//!   - Source protection (never overwrite custom/clawhub/claude skills)
//!   - Robust JSON extraction with balanced brace parser
//!   - Configurable cron interval, max candidates, approval requirements

use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{error, info};
use uuid::Uuid;

use crate::server::data::{
    get_chat_history, get_settings, get_skills, save_settings, save_skills, ChatSession, Skill,
    SkillAutoMeta,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_NAME_LEN: usize = 64;
const MAX_DESC_LEN: usize = 1024;
const MAX_BODY_CHARS: usize = 100_000;
const EXISTING_SKILL_BODY_BUDGET: usize = 4000;
const CHAT_LOG_TAIL_BYTES: u64 = 256 * 1024;
const MAX_REMEDIATIONS_PER_RUN: usize = 5;
const REMEDIATION_MIN_SCORE: usize = 2;

fn name_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap())
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillProposal {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub content: String,
    pub rationale: String,
    pub based_on: Vec<String>,
    pub generated_at: String,
    pub model: String,
    pub review_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizerStatus {
    pub running: bool,
    pub last_run_at: Option<String>,
    pub last_run_summary: Option<String>,
    pub proposals: Vec<SkillProposal>,
}

impl Default for SynthesizerStatus {
    fn default() -> Self {
        Self {
            running: false,
            last_run_at: None,
            last_run_summary: None,
            proposals: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct Proposal {
    kind: String, // "create" or "update"
    name: String,
    description: String,
    content: String,
    based_on: Vec<String>,
    rationale: String,
}

#[derive(Debug, Clone)]
struct FeedbackEntry {
    role: String,
    rating: Option<String>,
    comment: Option<String>,
    excerpt: Option<String>,
}

#[derive(Debug, Clone)]
struct SubagentTrace {
    label: String,
    task: Option<String>,
    tools_used: Vec<String>,
    skills_loaded: Vec<String>,
    completed: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionSummary {
    session_id: String,
    title: String,
    updated_at: String,
    user_queries: Vec<String>,
    final_assistant: String,
    feedback: Vec<FeedbackEntry>,
    subagent_workflow: Vec<SubagentTrace>,
}

#[allow(dead_code)]
struct RunSummary {
    ok: bool,
    created: usize,
    updated: usize,
    skipped: usize,
    reasons: Vec<String>,
}

struct RemediationOutcome {
    updated: usize,
    skipped: usize,
    reasons: Vec<String>,
    consumed_session_ids: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

fn synth_status() -> &'static Arc<Mutex<SynthesizerStatus>> {
    static INSTANCE: OnceLock<Arc<Mutex<SynthesizerStatus>>> = OnceLock::new();
    INSTANCE.get_or_init(|| Arc::new(Mutex::new(SynthesizerStatus::default())))
}

static INFLIGHT: OnceLock<Arc<Mutex<bool>>> = OnceLock::new();
fn inflight() -> &'static Arc<Mutex<bool>> {
    INFLIGHT.get_or_init(|| Arc::new(Mutex::new(false)))
}

pub async fn get_synth_status() -> SynthesizerStatus {
    synth_status().lock().await.clone()
}

// ---------------------------------------------------------------------------
// Skill file helpers
// ---------------------------------------------------------------------------

fn skills_dir() -> PathBuf {
    // Must match where the rest of the app reads/writes skills. The old
    // current_dir()-based path broke the whole feature when launched from an
    // .app bundle (cwd = "/", so it silently targeted /data/skills).
    crate::server::data::data_dir().join("skills")
}

fn sanitize_slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

async fn read_auto_skill_content(name: &str) -> Option<String> {
    let slug = sanitize_slug(name);
    let path = skills_dir().join(slug).join("SKILL.md");
    tokio::fs::read_to_string(&path).await.ok()
}

async fn list_existing_skill_summaries() -> Vec<ExistingSkillInfo> {
    let skills = get_skills().await;
    let mut out = Vec::new();
    for s in &skills {
        // Include bodies for updatable skills so the LLM can propose
        // meaningful diffs (auto + custom; marketplace skills stay name-only).
        let body = if s.source == "auto" || s.source == "custom" {
            read_auto_skill_content(&s.name).await
        } else {
            None
        };
        out.push(ExistingSkillInfo {
            name: s.name.clone(),
            description: s.description.clone(),
            source: s.source.clone(),
            body,
        });
    }
    out
}

#[derive(Debug, Clone)]
struct ExistingSkillInfo {
    name: String,
    description: String,
    source: String,
    body: Option<String>,
}

async fn write_skill_file(name: &str, content: &str, proposed: bool) -> Result<(), String> {
    let dir = skills_dir().join(name);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create skill dir: {e}"))?;
    let filename = if proposed { "SKILL.md.proposed" } else { "SKILL.md" };
    tokio::fs::write(dir.join(filename), content)
        .await
        .map_err(|e| format!("Failed to write {filename}: {e}"))
}

// ---------------------------------------------------------------------------
// Error detection
// ---------------------------------------------------------------------------

fn is_error_reply(content: &str) -> bool {
    if content.is_empty() {
        return true;
    }
    let head: String = content.chars().take(200).collect::<String>().to_lowercase();
    head.starts_with("connection error:")
        || head.contains("api key not configured")
        || head.contains("unauthorized")
        || head.starts_with("tigerbot api key not configured")
        || head.starts_with("no response from tigerbot")
        || head.starts_with("context overflow")
        || head.starts_with("api error (")
        || head.starts_with("error:")
}

// ---------------------------------------------------------------------------
// Chat log parsing for subagent workflows
// ---------------------------------------------------------------------------

async fn read_chat_log_tail(session_id: &str) -> String {
    let file = crate::server::data::data_dir()
        .join("chat_logs")
        .join(format!("{}.log", session_id));
    match tokio::fs::metadata(&file).await {
        Ok(meta) => {
            let size = meta.len();
            let start = if size > CHAT_LOG_TAIL_BYTES {
                size - CHAT_LOG_TAIL_BYTES
            } else {
                0
            };
            if let Ok(content) = tokio::fs::read_to_string(&file).await {
                if start > 0 {
                    // Skip to a char boundary (byte slicing panics on UTF-8,
                    // e.g. Thai logs), then to the next full line.
                    let mut skip = (start as usize).min(content.len());
                    while skip < content.len() && !content.is_char_boundary(skip) {
                        skip += 1;
                    }
                    let tail = &content[skip..];
                    match tail.find('\n') {
                        Some(nl) => tail[nl + 1..].to_string(),
                        None => tail.to_string(),
                    }
                } else {
                    content
                }
            } else {
                String::new()
            }
        }
        Err(_) => String::new(),
    }
}

/// Parse the chat-log into per-agent traces.
/// Sees AGENT_SPAWN/DONE, AGENT_WORKING/COMPLETE, and tool calls.
/// Parse the chat-log into per-agent traces.
///
/// Handles the two formats this app actually writes:
///
/// Desktop / realtime agents (ui/chat.rs + toolbox.rs log closures):
///   [22:06:18] [design_orchestrator] TOOL CALL: send_task
///     args: {"to":"geotechnical_engineer","task":"..."}
///   [22:06:18] [design_orchestrator] TOOL RESULT: send_task
///     {"agentId":"geotechnical_engineer","ok":true,...}
///   [22:05:57] TOOL CALL: check_agents          <- no [agent] = main agent
///
/// Web chat (routes/chat.rs on_update):
///   🔧 Calling **run_shell** — {"command":"..."}
///   ✅ **run_shell** done — {"exit_code":0,"ok":true,...}
fn parse_subagent_workflow(log: &str) -> Vec<SubagentTrace> {
    if log.is_empty() {
        return Vec::new();
    }

    let mut traces: HashMap<String, SubagentTrace> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    macro_rules! ensure_trace {
        ($map:expr, $order:expr, $label:expr) => {{
            if !$map.contains_key($label) {
                $order.push($label.to_string());
            }
            $map.entry($label.to_string()).or_insert_with(|| SubagentTrace {
                label: $label.to_string(),
                task: None,
                tools_used: Vec::new(),
                skills_loaded: Vec::new(),
                completed: false,
                error: None,
            })
        }};
    }

    fn json_field<'a>(s: &'a str, key: &str) -> Option<String> {
        // Cheap extraction of "key":"value" from a (possibly truncated) JSON line
        let needle = format!("\"{}\":", key);
        let start = s.find(&needle)? + needle.len();
        let rest = s[start..].trim_start();
        let rest = rest.strip_prefix('"')?;
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => { if let Some(n) = chars.next() { out.push(n); } }
                '"' => break,
                _ => out.push(c),
            }
        }
        Some(out)
    }

    // Desktop format: "[ts] TOOL CALL: name" or "[ts] [agent] TOOL CALL: name",
    // with "  args: {...}" on the following line. Same shape for TOOL RESULT.
    let re_call = Regex::new(r"(?m)^\[[^\]]+\]\s*(?:\[([^\]]+)\]\s*)?TOOL (CALL|RESULT): (\S+)\s*$").unwrap();
    let lines: Vec<&str> = log.lines().collect();
    let mut line_starts: HashMap<usize, usize> = HashMap::new(); // byte offset -> line index
    {
        let mut off = 0usize;
        for (i, l) in lines.iter().enumerate() {
            line_starts.insert(off, i);
            off += l.len() + 1;
        }
    }

    for cap in re_call.captures_iter(log) {
        let label = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("main").to_string();
        let kind = &cap[2];
        let tool = cap[3].trim().to_string();
        // Payload is on the next line ("  args: {...}" for CALL, "  {...}" for RESULT)
        let payload: &str = cap.get(0)
            .and_then(|m| line_starts.get(&m.start()))
            .and_then(|i| lines.get(i + 1))
            .map(|l| l.trim().trim_start_matches("args:").trim())
            .unwrap_or("");

        let t = ensure_trace!(traces, order, label.as_str());

        if kind == "CALL" {
            if !t.tools_used.contains(&tool) {
                t.tools_used.push(tool.clone());
            }
            if tool == "load_skill" {
                if let Some(skill) = json_field(payload, "skill") {
                    if !t.skills_loaded.contains(&skill) {
                        t.skills_loaded.push(skill);
                    }
                }
            }
            // send_task: record the task on the RECEIVING agent
            if tool == "send_task" {
                let target = json_field(payload, "to")
                    .or_else(|| json_field(payload, "agent_name"))
                    .or_else(|| json_field(payload, "agentId"));
                let task = json_field(payload, "task")
                    .or_else(|| json_field(payload, "task_description"));
                if let (Some(target), Some(task)) = (target, task) {
                    let rt = ensure_trace!(traces, order, target.as_str());
                    if rt.task.is_none() {
                        rt.task = Some(task.chars().take(240).collect());
                    }
                }
            }
        } else {
            // RESULT: failures and completions
            if payload.contains("\"ok\":false") {
                if t.error.is_none() {
                    let err = json_field(payload, "error").unwrap_or_else(|| "tool failed".into());
                    t.error = Some(err.chars().take(200).collect());
                }
            }
            // wait_result returning ok marks the source agent as completed
            if tool == "wait_result" && payload.contains("\"ok\":true") {
                if let Some(agent) = json_field(payload, "agentId") {
                    let at = ensure_trace!(traces, order, agent.as_str());
                    at.completed = true;
                }
            }
        }
    }

    // Web format: 🔧 Calling **name** — {...} / ✅ **name** done — {...}
    let re_web_call = Regex::new(r"(?m)^🔧 Calling \*\*([^*]+)\*\* — (.*)$").unwrap();
    for cap in re_web_call.captures_iter(log) {
        let tool = cap[1].trim().to_string();
        let payload = cap[2].trim();
        let t = ensure_trace!(traces, order, "main");
        if !t.tools_used.contains(&tool) {
            t.tools_used.push(tool.clone());
        }
        if tool == "load_skill" {
            if let Some(skill) = json_field(payload, "skill") {
                if !t.skills_loaded.contains(&skill) {
                    t.skills_loaded.push(skill);
                }
            }
        }
        if tool == "send_task" {
            let target = json_field(payload, "to");
            let task = json_field(payload, "task");
            if let (Some(target), Some(task)) = (target, task) {
                let rt = ensure_trace!(traces, order, target.as_str());
                if rt.task.is_none() {
                    rt.task = Some(task.chars().take(240).collect());
                }
            }
        }
    }
    let re_web_result = Regex::new(r"(?m)^✅ \*\*([^*]+)\*\* done — (.*)$").unwrap();
    for cap in re_web_result.captures_iter(log) {
        let payload = cap[2].trim();
        if payload.contains("\"ok\":false") {
            let t = ensure_trace!(traces, order, "main");
            if t.error.is_none() {
                let err = json_field(payload, "error")
                    .or_else(|| json_field(payload, "stderr"))
                    .unwrap_or_else(|| "tool failed".into());
                t.error = Some(err.chars().take(200).collect());
            }
        }
    }

    // A finished session means the main agent completed
    if log.contains("=== Response complete ===") {
        if let Some(t) = traces.get_mut("main") {
            t.completed = true;
        }
    }

    // Preserve first-seen order (main first, then agents in spawn order)
    order.into_iter()
        .filter_map(|label| traces.remove(&label))
        .filter(|t| !t.tools_used.is_empty() || !t.skills_loaded.is_empty() || t.task.is_some() || t.error.is_some())
        .collect()
}

fn collect_loaded_skills(traces: &[SubagentTrace]) -> Vec<String> {
    let mut set = HashSet::new();
    for t in traces {
        for s in &t.skills_loaded {
            set.insert(s.clone());
        }
    }
    set.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Session summary
// ---------------------------------------------------------------------------

async fn summarise_session(s: &ChatSession) -> Option<SessionSummary> {
    let user_msgs: Vec<&crate::server::data::ChatMessage> =
        s.messages.iter().filter(|m| m.role == "user").collect();
    let assistant_msgs: Vec<&crate::server::data::ChatMessage> =
        s.messages.iter().filter(|m| m.role == "assistant").collect();

    if user_msgs.is_empty() || assistant_msgs.is_empty() {
        return None;
    }

    let last = assistant_msgs.last().unwrap();
    if is_error_reply(&last.content) {
        return None;
    }

    // Collect feedback
    let mut feedback = Vec::new();
    for m in &s.messages {
        if let Some(ref fb) = m.feedback {
            if fb.rating.is_none() && fb.comment.is_none() {
                continue;
            }
            let excerpt: String = m.content.chars().take(240).collect();
            feedback.push(FeedbackEntry {
                role: m.role.clone(),
                rating: fb.rating.clone(),
                comment: fb.comment.clone(),
                excerpt: Some(excerpt),
            });
        }
    }

    // Parse subagent workflow from chat log
    let log = read_chat_log_tail(&s.id).await;
    let subagent_workflow = parse_subagent_workflow(&log);

    let user_queries: Vec<String> = user_msgs
        .iter()
        .take(6)
        .map(|m| m.content.chars().take(600).collect())
        .collect();

    let final_assistant: String = last.content.chars().take(3000).collect();

    Some(SessionSummary {
        session_id: s.id.clone(),
        title: s.title.clone(),
        updated_at: s.updated_at.clone(),
        user_queries,
        final_assistant,
        feedback,
        subagent_workflow,
    })
}

// ---------------------------------------------------------------------------
// Prompt builders
// ---------------------------------------------------------------------------

fn build_prompt(
    candidates: &[SessionSummary],
    existing: &[ExistingSkillInfo],
    exclude_session_ids: &HashSet<String>,
) -> String {
    let candidates_block: String = candidates
        .iter()
        .filter(|c| !exclude_session_ids.contains(&c.session_id))
        .enumerate()
        .map(|(i, c)| {
            let feedback_block = if !c.feedback.is_empty() {
                let lines: Vec<String> = c
                    .feedback
                    .iter()
                    .map(|f| {
                        let tag = match f.rating.as_deref() {
                            Some("up") => "LIKED",
                            Some("down") => "DISLIKED",
                            _ => "NOTE",
                        };
                        let cmt = f
                            .comment
                            .as_ref()
                            .map(|c| format!(" — comment: {}", c.replace('\n', " ")))
                            .unwrap_or_default();
                        let ex = f
                            .excerpt
                            .as_ref()
                            .map(|e| format!(" (on: {})", e.replace('\n', " ")))
                            .unwrap_or_default();
                        format!("- {}{}{}", tag, cmt, ex)
                    })
                    .collect();
                format!("\nhuman_feedback:\n{}", lines.join("\n"))
            } else {
                String::new()
            };

            let workflow_block = if !c.subagent_workflow.is_empty() {
                let lines: Vec<String> = c
                    .subagent_workflow
                    .iter()
                    .take(10)
                    .map(|w| {
                        let mut parts = vec![format!("- {}", w.label)];
                        if let Some(ref task) = w.task {
                            parts.push(format!("task=\"{}\"", task.replace('"', "'")));
                        }
                        if !w.skills_loaded.is_empty() {
                            parts.push(format!("skills_loaded=[{}]", w.skills_loaded.join(", ")));
                        }
                        if !w.tools_used.is_empty() {
                            let tools: Vec<&str> = w.tools_used.iter().take(8).map(|s| s.as_str()).collect();
                            let suffix = if w.tools_used.len() > 8 { ", ..." } else { "" };
                            parts.push(format!("tools_used=[{}{}]", tools.join(", "), suffix));
                        }
                        if let Some(ref err) = w.error {
                            parts.push(format!("error=\"{}\"", err.replace('"', "'")));
                        } else if !w.completed {
                            parts.push("incomplete".to_string());
                        }
                        parts.join(" | ")
                    })
                    .collect();
                format!(
                    "\nsubagent_workflow (parsed from data/chat_logs/{}.log):\n{}",
                    c.session_id,
                    lines.join("\n")
                )
            } else {
                String::new()
            };

            let user_queries_block = c
                .user_queries
                .iter()
                .map(|q| format!("- {}", q.replace('\n', " ")))
                .collect::<Vec<_>>()
                .join("\n");

            format!(
                "## Session {}\nid: {}\ntitle: {}\nuser_queries:\n{}\nfinal_assistant_excerpt:\n{}{}{}",
                i + 1,
                c.session_id,
                c.title,
                user_queries_block,
                c.final_assistant,
                workflow_block,
                feedback_block
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    let existing_block = existing
        .iter()
        .map(|e| {
            let head = format!("- {} ({}): {}", e.name, e.source, e.description);
            match &e.body {
                Some(body) => {
                    let truncated = body.len() > EXISTING_SKILL_BODY_BUDGET;
                    let body_text: String = body.chars().take(EXISTING_SKILL_BODY_BUDGET).collect();
                    format!(
                        "{}\n  CURRENT SKILL.md{}:\n  ```\n{}\n  ```",
                        head,
                        if truncated { " (truncated)" } else { "" },
                        body_text
                    )
                }
                None => head,
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are the SkillSynthesiser for the cowork agent. Recent successful chats just completed. Decide whether any of them embed a *reusable procedure* worth capturing as a SKILL.md, and (optionally) propose updates to an existing skill where the new conversation extends or corrects it.

Return STRICT JSON in this exact shape, with NO surrounding prose, NO markdown fences:

{{"proposals":[{{"kind":"create"|"update","name":"kebab-name","description":"one line, <=200 chars","content":"---\nname: kebab-name\ndescription: ...\n---\n\n# Title\n\nbody...","basedOn":["session-id-1"],"rationale":"why this skill is useful"}}]}}

Rules:
- name must match [a-zA-Z0-9_-]+, max 64 chars.
- For update, name MUST exactly match an EXISTING skill below.
- Only propose updates to skills with source "auto" or "custom" (custom updates require human approval). NEVER propose updates to clawhub, claude, or openclaw skills.
- Skip casual chats / one-off Q&A that don't generalise. Quality > quantity.
- If nothing is worth capturing, return {{"proposals":[]}}.
- content is REQUIRED and MUST NOT be empty. It MUST be a complete SKILL.md including YAML frontmatter (name + description) followed by markdown body. The content field must start with "---\nname:". Body <= 100,000 chars. A proposal without content is INVALID.
- Treat human_feedback as authoritative: LIKED responses indicate workflows worth capturing or reinforcing; DISLIKED responses indicate the procedure failed or misled the user — DO NOT distil a skill from those, and if an existing auto-skill produced the disliked behaviour, propose an update that addresses the comment.
- If subagent_workflow is non-empty, the session ran a multi-agent topology. The final_assistant_excerpt is the orchestrator's merged summary and HIDES which sub-agent did what. Use the workflow block as ground truth for the actual procedure: capture the agent labels, the order they ran, the skills_loaded each used, and any errors. A skill body for a multi-agent workflow should prescribe the topology (which roles to spawn, in what order, with which skills loaded), not just the outcome. If skills_loaded names an existing auto-skill that produced a great result, prefer "update" to refine it over creating a near-duplicate "create".

EXISTING SKILLS:
{}

CANDIDATE CHAT SESSIONS:
{}"#,
        if existing_block.is_empty() {
            "(none)".to_string()
        } else {
            existing_block
        },
        candidates_block
    )
}

fn build_remediation_prompt(
    skill_name: &str,
    current_skill_md: &str,
    excerpt: &str,
    comment: &str,
    workflow: &[SubagentTrace],
) -> String {
    let workflow_hint = if !workflow.is_empty() {
        let lines: Vec<String> = workflow
            .iter()
            .take(8)
            .map(|w| {
                let skills = if !w.skills_loaded.is_empty() {
                    format!(" skills_loaded=[{}]", w.skills_loaded.join(", "))
                } else {
                    String::new()
                };
                let tools = if !w.tools_used.is_empty() {
                    let t: Vec<&str> = w.tools_used.iter().take(6).map(|s| s.as_str()).collect();
                    format!(" tools=[{}]", t.join(", "))
                } else {
                    String::new()
                };
                let status = if let Some(ref err) = w.error {
                    format!(" ERROR=\"{}\"", err.replace('"', "'"))
                } else if !w.completed {
                    " (incomplete)".to_string()
                } else {
                    String::new()
                };
                format!("- {}{}{}{}", w.label, skills, tools, status)
            })
            .collect();
        format!(
            "\nSUB-AGENT WORKFLOW:\n{}\n",
            lines.join("\n")
        )
    } else {
        String::new()
    };

    let body: String = current_skill_md.chars().take(30000).collect();

    format!(
        r#"You are fixing an existing SKILL.md because a user marked an assistant response that this skill produced as NOT HELPFUL and explained why.

Your job: rewrite the skill so the same failure does not recur. Preserve everything that still works. Address the user's complaint directly — change procedures, add a guard, remove a misleading instruction, or clarify scope as needed.

Return STRICT JSON, NO surrounding prose, NO markdown fences:
{{"name":"{skill_name}","description":"<=200 chars","content":"---\nname: {skill_name}\ndescription: ...\n---\n\n# Title\n\nbody...","rationale":"what you changed and why"}}

Rules:
- name MUST equal "{skill_name}" exactly.
- content MUST be a complete SKILL.md with YAML frontmatter (name + description) followed by markdown body. Body <= 100,000 chars.
- Do not delete the skill — only revise it.
- If the complaint is unrelated to this skill (wrong target), return {{"name":"{skill_name}","skip":true,"reason":"<short>"}} instead.
- If the failure was at the orchestration layer (wrong sub-agent loaded the skill, wrong order, missing prerequisite), describe the correction in the skill body.

CURRENT SKILL ({skill_name}):
```
{body}
```
{workflow_hint}
DISLIKED ASSISTANT RESPONSE EXCERPT:
{excerpt}

USER COMMENT (what went wrong / what to fix):
{comment}"#
    )
}

// ---------------------------------------------------------------------------
// JSON extraction (balanced brace parser)
// ---------------------------------------------------------------------------

fn strip_reasoning(text: &str) -> String {
    let mut t = text.to_string();
    // Strip all reasoning tag variants
    for tag in &["think", "thinking", "reasoning", "reflection"] {
        let open = format!("<{}>", tag);
        let close = format!("</{}>", tag);
        loop {
            if let Some(start) = t.to_lowercase().find(&open) {
                if let Some(end_rel) = t[start..].to_lowercase().find(&close) {
                    let end = start + end_rel + close.len();
                    t = format!("{}{}", &t[..start], &t[end..]);
                } else {
                    // Unclosed tag — remove from here to end
                    t = t[..start].to_string();
                    break;
                }
            } else {
                break;
            }
        }
    }
    t.trim().to_string()
}

fn find_balanced_json(text: &str) -> Result<Value, String> {
    // Strip markdown fences
    let t = if let Some(cap) = Regex::new(r"(?s)```(?:json)?\s*([\s\S]+?)\s*```")
        .ok()
        .and_then(|re| re.captures(text))
    {
        cap[1].trim().to_string()
    } else {
        text.to_string()
    };

    let start = t.find('{').ok_or("no JSON object found")?;

    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;

    let bytes = t.as_bytes();
    for i in start..bytes.len() {
        let ch = bytes[i] as char;
        if escape {
            escape = false;
            continue;
        }
        if in_str {
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        if ch == '"' {
            in_str = true;
            continue;
        }
        if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                let json_str = &t[start..=i];
                return serde_json::from_str(json_str)
                    .map_err(|e| format!("JSON parse error: {e}"));
            }
        }
    }

    Err("no balanced JSON object found".to_string())
}

fn extract_json(text: &str) -> Result<Value, String> {
    // Try 1: strip reasoning tags, then find JSON
    let stripped = strip_reasoning(text);
    let stripped = stripped.trim();
    info!("[SkillSynth] extract_json: input len={}, stripped len={}, stripped has '{{': {}",
        text.len(), stripped.len(), stripped.contains('{'));
    match find_balanced_json(stripped) {
        Ok(v) => return Ok(v),
        Err(e) => info!("[SkillSynth] extract_json: try1 (stripped) failed: {}", e),
    }

    // Try 2: search in the raw text (JSON might be inside <think> tags)
    match find_balanced_json(text) {
        Ok(v) => {
            info!("[SkillSynth] Found JSON in raw text (was inside reasoning tags)");
            return Ok(v);
        }
        Err(e) => info!("[SkillSynth] extract_json: try2 (raw) failed: {}", e),
    }

    // Try 3: just try serde_json directly on the stripped text
    if let Ok(v) = serde_json::from_str::<Value>(stripped) {
        info!("[SkillSynth] extract_json: direct serde_json parse worked");
        return Ok(v);
    }

    Err("no JSON object found".to_string())
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_proposal(
    raw: &Value,
    existing: &[(String, String)], // (name, source)
) -> Result<Proposal, String> {
    let kind = raw["kind"]
        .as_str()
        .ok_or("bad kind")?;
    if kind != "create" && kind != "update" {
        return Err(format!("bad kind: {kind}"));
    }

    let name = raw["name"]
        .as_str()
        .ok_or("missing name")?
        .to_string();
    if !name_re().is_match(&name) || name.len() > MAX_NAME_LEN {
        return Err(format!("bad name: {name}"));
    }

    let description = raw["description"]
        .as_str()
        .ok_or(format!("bad description for {name}"))?
        .to_string();
    if description.trim().is_empty() || description.len() > MAX_DESC_LEN {
        return Err(format!("bad description for {name}"));
    }

    let content = raw["content"]
        .as_str()
        .ok_or(format!("bad content for {name}"))?
        .to_string();
    if content.is_empty() || content.len() > MAX_BODY_CHARS {
        return Err(format!("bad content for {name}"));
    }

    // Validate YAML frontmatter
    if !content.starts_with("---") {
        return Err(format!("{name}: missing frontmatter"));
    }

    let matched = existing.iter().find(|(n, _)| n == &name);

    if kind == "create" && matched.is_some() {
        let src = &matched.unwrap().1;
        return Err(format!("{name}: name already exists (source={src})"));
    }
    if kind == "update" {
        match matched {
            None => return Err(format!("{name}: update target does not exist")),
            // auto + custom skills can receive updates (custom always goes
            // through the .proposed approval path); marketplace skills never.
            Some((_, src)) if src != "auto" && src != "custom" => {
                return Err(format!("{name}: refuse to update {src} skill (only auto/custom)"))
            }
            _ => {}
        }
    }

    let based_on: Vec<String> = raw["basedOn"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let rationale = raw["rationale"]
        .as_str()
        .unwrap_or("")
        .to_string();

    Ok(Proposal {
        kind: kind.to_string(),
        name,
        description,
        content,
        based_on,
        rationale,
    })
}

// ---------------------------------------------------------------------------
// Skill writing
// ---------------------------------------------------------------------------

async fn write_new_auto_skill(p: &Proposal, model: &str) {
    let slug = sanitize_slug(&p.name);
    if let Err(e) = write_skill_file(&slug, &p.content, false).await {
        error!("[SkillSynth] Failed to write new skill: {e}");
        return;
    }

    let settings = get_settings().await;
    let require_approval = settings
        .skill_auto_update_require_approval
        .unwrap_or(true);

    let skill = Skill {
        id: Uuid::new_v4().to_string(),
        name: p.name.clone(),
        description: p.description.clone(),
        source: "auto".to_string(),
        script: p.name.clone(),
        enabled: !require_approval,
        installed_at: chrono::Utc::now().to_rfc3339(),
        review_status: Some(if require_approval {
            "pending".to_string()
        } else {
            "approved".to_string()
        }),
        auto_meta: Some(SkillAutoMeta {
            kind: "create".to_string(),
            based_on: p.based_on.clone(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            model: model.to_string(),
            proposed_path: None,
            rationale: Some(p.rationale.clone()),
        }),
    };

    let mut skills = get_skills().await;
    skills.push(skill);
    save_skills(&skills).await;
}

async fn write_proposed_update(p: &Proposal, model: &str) {
    let slug = sanitize_slug(&p.name);
    let settings = get_settings().await;
    let mut require_approval = settings
        .skill_auto_update_require_approval
        .unwrap_or(true);

    let mut skills = get_skills().await;
    let idx = skills.iter().position(|s| {
        s.name == p.name && (s.source == "auto" || s.source == "custom")
    });

    if idx.is_none() {
        // Target doesn't exist — create instead
        write_new_auto_skill(p, model).await;
        return;
    }
    let idx = idx.unwrap();

    // User-authored (custom) skills are never overwritten silently
    if skills[idx].source == "custom" {
        require_approval = true;
    }

    if !require_approval {
        if let Err(e) = write_skill_file(&slug, &p.content, false).await {
            error!("[SkillSynth] Failed to write skill update: {e}");
            return;
        }
        skills[idx].description = p.description.clone();
        skills[idx].review_status = Some("approved".to_string());
        skills[idx].enabled = true;
        skills[idx].auto_meta = Some(SkillAutoMeta {
            kind: "update".to_string(),
            based_on: p.based_on.clone(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            model: model.to_string(),
            proposed_path: None,
            rationale: Some(p.rationale.clone()),
        });
    } else {
        if let Err(e) = write_skill_file(&slug, &p.content, true).await {
            error!("[SkillSynth] Failed to write proposed update: {e}");
            return;
        }
        skills[idx].review_status = Some("pending".to_string());
        skills[idx].auto_meta = Some(SkillAutoMeta {
            kind: "update".to_string(),
            based_on: p.based_on.clone(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            model: model.to_string(),
            proposed_path: Some("SKILL.md.proposed".to_string()),
            rationale: Some(p.rationale.clone()),
        });
    }
    save_skills(&skills).await;
}

// ---------------------------------------------------------------------------
// Approval / Rejection
// ---------------------------------------------------------------------------

pub async fn approve_proposal(proposal_id: &str) -> Result<(), String> {
    let proposal_name;
    let proposal_content;

    {
        let mut status = synth_status().lock().await;
        let proposal = status
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| "Proposal not found".to_string())?;
        proposal.review_status = "approved".to_string();
        proposal_name = proposal.name.clone();
        proposal_content = proposal.content.clone();
    }

    let slug = sanitize_slug(&proposal_name);
    let skill_dir = skills_dir().join(&slug);
    let proposed_path = skill_dir.join("SKILL.md.proposed");
    let live_path = skill_dir.join("SKILL.md");

    if proposed_path.exists() {
        tokio::fs::rename(&proposed_path, &live_path)
            .await
            .map_err(|e| format!("rename failed: {e}"))?;
    } else {
        write_skill_file(&slug, &proposal_content, false).await?;
    }

    let mut skills = get_skills().await;
    if let Some(skill) = skills.iter_mut().find(|s| s.name == proposal_name) {
        skill.enabled = true;
        skill.review_status = Some("approved".to_string());
        if let Some(ref mut meta) = skill.auto_meta {
            meta.proposed_path = None;
        }
    }
    save_skills(&skills).await;
    info!("[SkillSynth] Approved skill: {}", proposal_name);
    Ok(())
}

pub async fn reject_proposal(proposal_id: &str) -> Result<(), String> {
    let proposal_name;
    let proposal_kind;

    {
        let mut status = synth_status().lock().await;
        let proposal = status
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| "Proposal not found".to_string())?;
        proposal.review_status = "rejected".to_string();
        proposal_name = proposal.name.clone();
        proposal_kind = proposal.kind.clone();
    }

    let slug = sanitize_slug(&proposal_name);
    let skill_dir = skills_dir().join(&slug);

    // For updates: just remove the proposed file
    let proposed_path = skill_dir.join("SKILL.md.proposed");
    let _ = tokio::fs::remove_file(&proposed_path).await;

    if proposal_kind == "update" {
        // Keep existing skill, just reset status
        let mut skills = get_skills().await;
        if let Some(skill) = skills.iter_mut().find(|s| s.name == proposal_name) {
            skill.review_status = Some("approved".to_string());
            if let Some(ref mut meta) = skill.auto_meta {
                meta.proposed_path = None;
            }
        }
        save_skills(&skills).await;
    } else {
        // Create rejection: delete the whole folder + drop registry row
        let _ = tokio::fs::remove_dir_all(&skill_dir).await;
        let mut skills = get_skills().await;
        skills.retain(|s| s.name != proposal_name);
        save_skills(&skills).await;
    }

    info!("[SkillSynth] Rejected skill: {}", proposal_name);
    Ok(())
}

#[allow(dead_code)]
pub async fn get_proposed_diff(
    skill_id: &str,
) -> Result<(String, String), String> {
    let skills = get_skills().await;
    let skill = skills
        .iter()
        .find(|s| s.id == skill_id)
        .ok_or("not found")?;
    if skill.source != "auto" {
        return Err("not auto".to_string());
    }
    let slug = sanitize_slug(&skill.name);
    let skill_dir = skills_dir().join(&slug);

    let current = tokio::fs::read_to_string(skill_dir.join("SKILL.md"))
        .await
        .unwrap_or_default();

    let proposed = if let Some(ref pp) = skill.auto_meta.as_ref().and_then(|m| m.proposed_path.as_ref()) {
        tokio::fs::read_to_string(skill_dir.join(pp))
            .await
            .unwrap_or_default()
    } else if skill.auto_meta.as_ref().map(|m| m.kind.as_str()) == Some("create") {
        let p = current.clone();
        return Ok((String::new(), p));
    } else {
        String::new()
    };

    Ok((current, proposed))
}

// ---------------------------------------------------------------------------
// LLM call
// ---------------------------------------------------------------------------

async fn call_llm(api_key: &str, api_url: &str, model: &str, messages: Vec<Value>) -> Result<String, String> {
    let client = Client::new();
    let body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": 16384,
        "temperature": 0.3,
    });

    let mut req = client
        .post(api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json");

    // Only add Kimi-specific identity headers for Kimi API
    if api_url.contains("api.kimi.com") {
        req = req
            .header("User-Agent", "claude-code/1.0.6")
            .header("X-Client-Name", "claude-code")
            .header("X-Client-Version", "1.0.6")
            .header("HTTP-Referer", "https://claude.ai")
            .header("X-Traffic-Source", "claude-code");
    }

    let resp = req
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("API request failed (url={}, model={}): {e}", api_url, model))?;

    let resp_json: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {e}"))?;

    if let Some(err) = resp_json.get("error") {
        return Err(format!("API error: {}", err));
    }

    // Parse response — support OpenAI, Anthropic, OpenRouter, and other formats
    let content = resp_json["choices"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|c| {
            // Standard OpenAI: content is a string
            c["message"]["content"].as_str().map(|s| s.to_string())
                .or_else(|| {
                    // Some providers: content is array of blocks inside choices
                    c["message"]["content"].as_array().map(|blocks| {
                        blocks.iter()
                            .filter_map(|b| {
                                if b["type"] == "text" { b["text"].as_str().map(|s| s.to_string()) }
                                else { None }
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })
        })
        .or_else(|| {
            // Anthropic native format: content is array of blocks at root
            resp_json["content"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| {
                            if b["type"] == "text" {
                                b["text"].as_str().map(|s| s.to_string())
                            } else if b["type"] == "thinking" {
                                b["thinking"].as_str().map(|s| format!("<think>{}</think>", s))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
        })
        .or_else(|| {
            // Fallback: try "output" or "result" keys
            resp_json["output"].as_str().map(|s| s.to_string())
                .or_else(|| resp_json["result"].as_str().map(|s| s.to_string()))
        })
        .unwrap_or_default();

    let raw_str = resp_json.to_string();
    info!("[SkillSynth] Raw LLM response (first 1000): {}", crate::util::truncate_utf8(&raw_str, 1000));

    if content.is_empty() {
        error!("[SkillSynth] LLM returned empty content! Full raw response (first 2000): {}",
            crate::util::truncate_utf8(&raw_str, 2000));
    } else {
        info!("[SkillSynth] Extracted content length={}, first 300: {}",
            content.len(), crate::util::truncate_utf8(&content, 300));
    }

    Ok(content)
}

// ---------------------------------------------------------------------------
// Remediation — token overlap matching
// ---------------------------------------------------------------------------

fn tokenize(s: &str) -> HashSet<String> {
    let stop_words: HashSet<&str> = [
        "the", "and", "for", "with", "this", "that", "your", "from", "into", "have",
        "should", "would", "could", "about", "user", "agent", "skill", "code", "use", "using",
    ]
    .into_iter()
    .collect();

    let re = Regex::new(r"[a-z0-9_-]{3,}").unwrap();
    re.find_iter(&s.to_lowercase())
        .map(|m| m.as_str().to_string())
        .filter(|t| !stop_words.contains(t.as_str()))
        .collect()
}

fn select_remediation_target(
    excerpt: &str,
    comment: &str,
    auto_skills: &[(String, String)], // (name, description)
    contents: &HashMap<String, String>,
) -> Option<(String, usize)> {
    let needle = tokenize(&format!("{} {}", excerpt, comment));
    if needle.is_empty() {
        return None;
    }

    let mut best: Option<(String, usize)> = None;
    for (name, desc) in auto_skills {
        let hay_str = format!(
            "{} {} {}",
            name,
            desc,
            contents.get(name).map(|c| crate::util::truncate_utf8(&c, 4000)).unwrap_or("")
        );
        let hay_tokens = tokenize(&hay_str);
        let hits = needle.iter().filter(|t| hay_tokens.contains(*t)).count();
        if best.is_none() || hits > best.as_ref().unwrap().1 {
            best = Some((name.clone(), hits));
        }
    }

    best.filter(|(_, score)| *score >= REMEDIATION_MIN_SCORE)
}

async fn run_remediations(
    candidates: &[SessionSummary],
    api_key: &str,
    api_url: &str,
    model: &str,
) -> RemediationOutcome {
    let mut out = RemediationOutcome {
        updated: 0,
        skipped: 0,
        reasons: Vec::new(),
        consumed_session_ids: HashSet::new(),
    };

    // Find sessions with thumbs-down + comment on assistant messages
    struct Target {
        session_id: String,
        excerpt: String,
        comment: String,
        workflow: Vec<SubagentTrace>,
    }
    let mut targets: Vec<Target> = Vec::new();
    for c in candidates {
        for f in &c.feedback {
            if f.rating.as_deref() == Some("down")
                && f.comment.as_ref().map(|c| !c.trim().is_empty()).unwrap_or(false)
                && f.role == "assistant"
            {
                let excerpt: String = f
                    .excerpt
                    .as_ref()
                    .unwrap_or(&c.final_assistant)
                    .chars()
                    .take(1200)
                    .collect();
                targets.push(Target {
                    session_id: c.session_id.clone(),
                    excerpt,
                    comment: f.comment.as_ref().unwrap().trim().to_string(),
                    workflow: c.subagent_workflow.clone(),
                });
                break;
            }
        }
    }

    if targets.is_empty() {
        return out;
    }

    let all_skills = get_skills().await;
    // auto + custom skills are remediation targets (custom fixes go through approval)
    let auto_skills: Vec<(String, String)> = all_skills
        .iter()
        .filter(|s| s.source == "auto" || s.source == "custom")
        .map(|s| (s.name.clone(), s.description.clone()))
        .collect();

    if auto_skills.is_empty() {
        for t in &targets {
            out.skipped += 1;
            out.reasons
                .push(format!("remediation skipped ({}): no updatable (auto/custom) skill exists", t.session_id));
        }
        return out;
    }

    let auto_skill_names: HashSet<String> = auto_skills.iter().map(|(n, _)| n.clone()).collect();

    let mut contents: HashMap<String, String> = HashMap::new();
    for (name, _) in &auto_skills {
        if let Some(c) = read_auto_skill_content(name).await {
            contents.insert(name.clone(), c);
        }
    }

    let capped = &targets[..targets.len().min(MAX_REMEDIATIONS_PER_RUN)];
    if targets.len() > capped.len() {
        out.reasons.push(format!(
            "remediation cap reached: {} sessions deferred to next run",
            targets.len() - capped.len()
        ));
    }

    for t in capped {
        // Determine which auto-skill to remediate
        let loaded_auto: Vec<String> = collect_loaded_skills(&t.workflow)
            .into_iter()
            .filter(|n| auto_skill_names.contains(n))
            .collect();

        let (pick, strategy) = if loaded_auto.len() == 1 {
            (Some((loaded_auto[0].clone(), usize::MAX)), "chatlog-unique")
        } else if loaded_auto.len() > 1 {
            let subset: Vec<(String, String)> = auto_skills
                .iter()
                .filter(|(n, _)| loaded_auto.contains(n))
                .cloned()
                .collect();
            (
                select_remediation_target(&t.excerpt, &t.comment, &subset, &contents),
                "chatlog-subset",
            )
        } else {
            (
                select_remediation_target(&t.excerpt, &t.comment, &auto_skills, &contents),
                "token-overlap",
            )
        };

        let pick = match pick {
            Some(p) => p,
            None => {
                out.skipped += 1;
                out.reasons.push(format!(
                    "remediation skipped ({}): no auto skill matched the disliked turn",
                    t.session_id
                ));
                continue;
            }
        };

        let skill_md = match contents.get(&pick.0) {
            Some(md) => md.clone(),
            None => {
                out.skipped += 1;
                out.reasons.push(format!(
                    "remediation skipped ({}): could not read {}/SKILL.md",
                    t.session_id, pick.0
                ));
                continue;
            }
        };

        let prompt = build_remediation_prompt(&pick.0, &skill_md, &t.excerpt, &t.comment, &t.workflow);
        let reply = match call_llm(
            api_key,
            api_url,
            model,
            vec![
                json!({"role": "system", "content": "You are a careful skill remediator. You MUST output ONLY a JSON object. No markdown, no explanation, no prose. Output starts with { and ends with }."}),
                json!({"role": "user", "content": prompt}),
            ],
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                out.skipped += 1;
                out.reasons
                    .push(format!("remediation LLM error for {}: {}", pick.0, e));
                continue;
            }
        };

        if is_error_reply(&reply) {
            out.skipped += 1;
            let preview: String = reply.chars().take(120).collect();
            out.reasons
                .push(format!("remediation LLM error for {}: {}", pick.0, preview));
            continue;
        }

        let parsed = match extract_json(&reply) {
            Ok(v) => v,
            Err(e) => {
                out.skipped += 1;
                out.reasons
                    .push(format!("remediation parse failed for {}: {}", pick.0, e));
                continue;
            }
        };

        if parsed.get("skip").and_then(|v| v.as_bool()) == Some(true) {
            out.skipped += 1;
            let reason = parsed["reason"].as_str().unwrap_or("model said skip");
            out.reasons
                .push(format!("remediation declined for {}: {}", pick.0, reason));
            continue;
        }

        let proposal_raw = json!({
            "kind": "update",
            "name": pick.0,
            "description": parsed["description"],
            "content": parsed["content"],
            "basedOn": [t.session_id],
            "rationale": format!(
                "remediation from disliked comment: {}{}",
                crate::util::truncate_utf8(&t.comment, 160),
                parsed["rationale"]
                    .as_str()
                    .map(|r| format!(" | {}", crate::util::truncate_utf8(&r, 200)))
                    .unwrap_or_default()
            ),
        });

        let existing_for_collision: Vec<(String, String)> = all_skills
            .iter()
            .map(|s| (s.name.clone(), s.source.clone()))
            .collect();

        match validate_proposal(&proposal_raw, &existing_for_collision) {
            Ok(proposal) => {
                write_proposed_update(&proposal, model).await;
                out.updated += 1;
                out.consumed_session_ids.insert(t.session_id.clone());
                out.reasons.push(format!(
                    "remediated {} from disliked in {} (target picked via {})",
                    pick.0, t.session_id, strategy
                ));
            }
            Err(reason) => {
                out.skipped += 1;
                out.reasons
                    .push(format!("remediation invalid for {}: {}", pick.0, reason));
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Main synthesis
// ---------------------------------------------------------------------------

pub async fn run_synthesis_forced() -> Result<String, String> {
    run_synthesis_inner(true).await
}

pub async fn run_synthesis() -> Result<String, String> {
    run_synthesis_inner(false).await
}

async fn run_synthesis_inner(force: bool) -> Result<String, String> {
    // Inflight guard
    {
        let mut lock = inflight().lock().await;
        if *lock {
            return Err("already-running".to_string());
        }
        *lock = true;
    }

    let result = run_synthesis_core(force).await;

    {
        let mut lock = inflight().lock().await;
        *lock = false;
    }

    result
}

async fn run_synthesis_core(force: bool) -> Result<String, String> {
    info!("[SkillSynth] Starting synthesis run (force={})", force);

    {
        let mut status = synth_status().lock().await;
        status.running = true;
    }

    let settings = get_settings().await;
    if settings
        .skill_auto_update_enabled
        .map(|e| !e)
        .unwrap_or(false)
    {
        let mut status = synth_status().lock().await;
        status.running = false;
        return Err("disabled".to_string());
    }

    let api_key = settings.tiger_bot_api_key.clone();
    let api_url_raw = settings.tiger_bot_api_url.clone().unwrap_or_default();
    let model = if settings.tiger_bot_model.is_empty() {
        "unknown".to_string()
    } else {
        settings.tiger_bot_model.clone()
    };

    // CLI providers (claude-code, gemini-cli, codex-cli) are not supported for skill synthesis
    if api_url_raw.starts_with("claude-code") || api_url_raw.starts_with("gemini-cli") || api_url_raw.starts_with("codex-cli") {
        let mut status = synth_status().lock().await;
        status.running = false;
        return Err(format!("Skill auto-update requires an API provider (not {}). Please set an API URL in Settings (e.g. OpenAI, DeepSeek, MiniMax).", api_url_raw));
    }

    if api_key.is_empty() {
        let mut status = synth_status().lock().await;
        status.running = false;
        return Err("No API key configured".to_string());
    }

    let api_url = if api_url_raw.ends_with("/chat/completions") {
        api_url_raw
    } else {
        format!("{}/chat/completions", api_url_raw.trim_end_matches('/'))
    };

    // Cursor (last processed timestamp) — ignored when force=true
    let cursor = if force {
        String::new()
    } else {
        settings
            .extra
            .get("skillAutoUpdateCursor")
            .and_then(|v| v.as_str())
            .unwrap_or("1970-01-01T00:00:00.000Z")
            .to_string()
    };

    let max_candidates = settings
        .skill_auto_update_max_candidates
        .unwrap_or(10) as usize;

    // Load chat sessions sorted ascending by updatedAt
    let mut sessions = get_chat_history().await;
    sessions.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));

    let fresh: Vec<&ChatSession> = if force {
        sessions.iter().collect()
    } else {
        sessions.iter().filter(|s| s.updated_at.as_str() > cursor.as_str()).collect()
    };

    // Summarize sessions
    let mut candidates: Vec<SessionSummary> = Vec::new();
    for s in &fresh {
        if let Some(summary) = summarise_session(s).await {
            candidates.push(summary);
        }
    }
    // Take the last N
    if candidates.len() > max_candidates {
        candidates = candidates[candidates.len() - max_candidates..].to_vec();
    }

    if candidates.is_empty() {
        let summary_msg = if force {
            "no successful sessions found"
        } else {
            "no new successful sessions"
        };
        let mut settings = get_settings().await;
        settings.extra.insert(
            "skillAutoUpdateLastRunAt".to_string(),
            json!(chrono::Utc::now().to_rfc3339()),
        );
        settings.extra.insert(
            "skillAutoUpdateLastRunSummary".to_string(),
            json!(summary_msg),
        );
        save_settings(&settings).await;
        let mut status = synth_status().lock().await;
        status.running = false;
        status.last_run_at = Some(chrono::Utc::now().to_rfc3339());
        status.last_run_summary = Some(summary_msg.to_string());
        return Ok(summary_msg.to_string());
    }

    info!(
        "[SkillSynth] {} candidate sessions for synthesis",
        candidates.len()
    );

    // ── Pass 1: Remediation ──
    let remediation = run_remediations(&candidates, &api_key, &api_url, &model).await;

    // Filter out sessions consumed by remediation
    let remaining: Vec<&SessionSummary> = candidates
        .iter()
        .filter(|c| !remediation.consumed_session_ids.contains(&c.session_id))
        .collect();

    if remaining.is_empty() {
        let new_cursor = &candidates.last().unwrap().updated_at;
        let summary_msg = format!(
            "created=0 updated={} skipped={} candidates={} (all consumed by remediation)",
            remediation.updated, remediation.skipped, candidates.len()
        );
        update_settings_after_run(Some(new_cursor), &summary_msg, &remediation.reasons).await;
        finish_status(&summary_msg).await;
        return Ok(summary_msg);
    }

    // ── Pass 2: Synthesis ──
    let existing = list_existing_skill_summaries().await;
    let prompt = build_prompt(
        &candidates,
        &existing,
        &remediation.consumed_session_ids,
    );

    let reply = match call_llm(
        &api_key,
        &api_url,
        &model,
        vec![
            json!({"role": "system", "content": "You are a careful skill synthesiser. You MUST output ONLY a JSON object with a \"proposals\" array. No markdown, no explanation, no prose. Output starts with { and ends with }. Example: {\"proposals\":[]}"}),
            json!({"role": "user", "content": prompt}),
        ],
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            // Keep the cursor: these sessions should be retried next run
            let summary_msg = format!("LLM error: {}", crate::util::truncate_utf8(&e, 200));
            let mut reasons = remediation.reasons.clone();
            reasons.push(summary_msg.clone());
            update_settings_after_run(None, &summary_msg, &reasons).await;
            finish_status(&summary_msg).await;
            return Err(e);
        }
    };

    if is_error_reply(&reply) {
        let preview: String = reply.chars().take(200).collect();
        let summary_msg = format!("LLM error: {}", preview);
        let mut reasons = remediation.reasons.clone();
        reasons.push(summary_msg.clone());
        update_settings_after_run(None, &summary_msg, &reasons).await;
        finish_status(&summary_msg).await;
        return Err(summary_msg);
    }

    // Parse proposals — dump full reply for debugging
    info!("[SkillSynth] LLM reply length={}, has </think>: {}", reply.len(), reply.contains("</think>"));
    let stripped_preview = strip_reasoning(&reply);
    info!("[SkillSynth] after strip length={}", stripped_preview.len());
    // Write full reply to debug file
    let data = crate::server::data::data_dir();
    let _ = std::fs::write(data.join("debug_skill_llm_reply.txt"), &reply);
    let _ = std::fs::write(data.join("debug_skill_llm_stripped.txt"), &stripped_preview);
    let parsed = match extract_json(&reply) {
        Ok(v) => v,
        Err(e) => {
            // Check if reply is effectively "no proposals"
            let stripped = strip_reasoning(&reply);
            let head: String = stripped.trim().to_lowercase().chars().take(200).collect();
            let looks_empty = stripped.trim().is_empty()
                || head.contains("no proposals")
                || head.contains("nothing to propose")
                || head.contains("no skill")
                || head.contains("no worthwhile")
                || head.contains("not worth");

            if looks_empty {
                json!({"proposals": []})
            } else {
                let reply_preview: String = reply.chars().take(500).collect();
                error!("[SkillSynth] JSON parse failed. LLM reply (first 500 chars): {}", reply_preview);
                let summary_msg = format!("LLM JSON parse failed: {}", e);
                let mut reasons = remediation.reasons.clone();
                reasons.push(summary_msg.clone());
                update_settings_after_run(None, &summary_msg, &reasons).await;
                finish_status(&summary_msg).await;
                return Err(summary_msg);
            }
        }
    };

    // Try "proposals" key, fall back to root array
    let proposals: Vec<Value> = if let Some(arr) = parsed["proposals"].as_array() {
        arr.clone()
    } else if let Some(arr) = parsed.as_array() {
        arr.clone()
    } else {
        // Maybe the entire parsed object IS a single proposal
        if parsed["kind"].is_string() && parsed["name"].is_string() {
            vec![parsed.clone()]
        } else {
            vec![]
        }
    };
    info!("[SkillSynth] parsed JSON keys: {:?}, proposals count: {}",
        parsed.as_object().map(|o| o.keys().collect::<Vec<_>>()),
        proposals.len());

    let mut created = 0usize;
    let mut updated = remediation.updated;
    let mut skipped = remediation.skipped;
    let mut reasons = remediation.reasons;

    let mut existing_for_collision: Vec<(String, String)> = get_skills()
        .await
        .iter()
        .map(|s| (s.name.clone(), s.source.clone()))
        .collect();

    for (idx, raw) in proposals.iter().enumerate() {
        info!("[SkillSynth] proposal[{}] kind={:?} name={:?}",
            idx, raw["kind"].as_str(), raw["name"].as_str());
        match validate_proposal(raw, &existing_for_collision) {
            Ok(proposal) => {
                if proposal.kind == "create" {
                    write_new_auto_skill(&proposal, &model).await;
                    created += 1;
                    existing_for_collision.push((proposal.name.clone(), "auto".to_string()));

                    // Add to synth_status proposals
                    add_proposal_to_status(&proposal, &model, None).await;
                } else {
                    // Get existing content for diff
                    let existing_content = read_auto_skill_content(&proposal.name).await;
                    write_proposed_update(&proposal, &model).await;
                    updated += 1;

                    add_proposal_to_status(&proposal, &model, existing_content.as_deref()).await;
                }
            }
            Err(reason) => {
                info!("[SkillSynth] proposal[{}] REJECTED: {}", idx, reason);
                skipped += 1;
                reasons.push(reason);
            }
        }
    }

    let new_cursor = &candidates.last().unwrap().updated_at;
    let summary_msg = format!(
        "created={} updated={} skipped={} candidates={}",
        created, updated, skipped, candidates.len()
    );
    update_settings_after_run(Some(new_cursor), &summary_msg, &reasons).await;
    finish_status(&summary_msg).await;

    info!("[SkillSynth] {}", summary_msg);
    Ok(summary_msg)
}

async fn add_proposal_to_status(proposal: &Proposal, model: &str, existing_content: Option<&str>) {
    let mut status = synth_status().lock().await;
    status.proposals.push(SkillProposal {
        id: Uuid::new_v4().to_string(),
        name: proposal.name.clone(),
        description: proposal.description.clone(),
        kind: proposal.kind.clone(),
        content: proposal.content.clone(),
        rationale: proposal.rationale.clone(),
        based_on: proposal.based_on.clone(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        model: model.to_string(),
        review_status: "pending".to_string(),
        existing_content: existing_content.map(|s| s.to_string()),
    });
}

/// `cursor: None` leaves the cursor untouched — used on LLM/parse failures so
/// the affected sessions are retried on the next run instead of being skipped
/// forever.
async fn update_settings_after_run(cursor: Option<&str>, summary: &str, reasons: &[String]) {
    let mut settings = get_settings().await;
    if let Some(cursor) = cursor {
        settings
            .extra
            .insert("skillAutoUpdateCursor".to_string(), json!(cursor));
    }
    settings.extra.insert(
        "skillAutoUpdateLastRunAt".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    let reason_suffix = if !reasons.is_empty() {
        format!(
            " | {}",
            reasons.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
        )
    } else {
        String::new()
    };
    settings.extra.insert(
        "skillAutoUpdateLastRunSummary".to_string(),
        json!(format!("{}{}", summary, reason_suffix)),
    );
    save_settings(&settings).await;
}

async fn finish_status(summary: &str) {
    let mut status = synth_status().lock().await;
    status.running = false;
    status.last_run_at = Some(chrono::Utc::now().to_rfc3339());
    status.last_run_summary = Some(summary.to_string());
}

// ---------------------------------------------------------------------------
// Background cron
// ---------------------------------------------------------------------------

pub fn start_cron(runtime: tokio::runtime::Handle) {
    runtime.spawn(async move {
        // Wait 30 seconds before first run
        tokio::time::sleep(Duration::from_secs(30)).await;

        loop {
            let settings = get_settings().await;
            let enabled = settings.skill_auto_update_enabled.unwrap_or(true);
            let interval_mins = settings
                .skill_auto_update_interval_minutes
                .unwrap_or(60)
                .max(5)
                .min(1440);

            if enabled {
                info!("[SkillSynth] Cron triggered");
                match run_synthesis().await {
                    Ok(summary) => info!("[SkillSynth] Cron result: {}", summary),
                    Err(e) => error!("[SkillSynth] Cron error: {}", e),
                }
            }

            tokio::time::sleep(Duration::from_secs(interval_mins * 60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_desktop_log_format() {
        let log = r#"[2026-05-07 22:05:50 UTC] === Session: f7321748 ===
[2026-05-07 22:05:50 UTC] USER: Design shallow foundation
[22:05:57] TOOL CALL: check_agents
  args: {}
[22:05:57] TOOL RESULT: check_agents
  {"agents":[],"ok":true,"total":7}
[22:06:07] TOOL CALL: send_task
  args: {"task":"Design a shallow foundation for 10,000 kN","to":"design_orchestrator"}
[22:06:18] [design_orchestrator] TOOL CALL: send_task
  args: {"task":"Perform geotechnical analysis","to":"geotechnical_engineer"}
[22:06:21] [geotechnical_engineer] TOOL CALL: load_skill
  args: {"skill":"shallow_foundation"}
[22:06:21] [geotechnical_engineer] TOOL RESULT: load_skill
  {"error":"Skill not found","ok":false}
[22:09:00] TOOL CALL: wait_result
  args: {"from":"design_orchestrator"}
[22:09:00] TOOL RESULT: wait_result
  {"agentId":"design_orchestrator","ok":true,"result":"done"}
[22:09:01] === Response complete ===
"#;
        let traces = parse_subagent_workflow(log);
        let main = traces.iter().find(|t| t.label == "main").expect("main trace");
        assert!(main.tools_used.contains(&"check_agents".to_string()));
        assert!(main.tools_used.contains(&"send_task".to_string()));
        assert!(main.completed);

        let orch = traces.iter().find(|t| t.label == "design_orchestrator").expect("orch trace");
        assert!(orch.task.as_deref().unwrap().contains("shallow foundation"));
        assert!(orch.completed); // via wait_result ok

        let geo = traces.iter().find(|t| t.label == "geotechnical_engineer").expect("geo trace");
        assert_eq!(geo.skills_loaded, vec!["shallow_foundation".to_string()]);
        assert!(geo.error.as_deref().unwrap().contains("Skill not found"));
        assert!(geo.task.as_deref().unwrap().contains("geotechnical analysis"));
    }

    #[test]
    fn parses_web_log_format() {
        let log = r#"[10:57:38] === Web Chat Session ===
🔧 Calling **run_python** — {"code":"print(1)"}
✅ **run_python** done — {"exit_code":0,"ok":true,"stdout":"1"}
🔧 Calling **load_skill** — {"skill":"web-search"}
✅ **load_skill** done — {"ok":true}
🔧 Calling **run_shell** — {"command":"python x.py"}
✅ **run_shell** done — {"exit_code":127,"ok":false,"stderr":"python: not found"}
[11:00:04] === Response complete ===
"#;
        let traces = parse_subagent_workflow(log);
        assert_eq!(traces.len(), 1);
        let main = &traces[0];
        assert!(main.tools_used.contains(&"run_python".to_string()));
        assert!(main.skills_loaded.contains(&"web-search".to_string()));
        assert!(main.error.is_some());
        assert!(main.completed);
    }
}
