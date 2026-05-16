//! Compact Context — LLM-based conversation compression system.
//!
//! Ports the full 9-step compaction pipeline from TypeScript tigerbot.ts:
//!  1. Pre-compact hooks
//!  2. Structured summarization prompt (9 sections)
//!  3. Strip images/docs
//!  4. Send to model (with prompt-too-long retry)
//!  5. Group-based dropping on overflow
//!  6. Format summary (strip <analysis>, extract <summary>)
//!  7. Restore critical context (files, plan, skills)
//!  8. Build new message history
//!  9. Cleanup (transcript)

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_COMPACT_FAILURES: u32 = 3;
const COMPACT_COOLDOWN_MS: u64 = 60_000; // 60s minimum between compactions
const DEFAULT_COMPRESSION_INTERVAL: usize = 5;
const DEFAULT_COMPRESSION_WINDOW: usize = 10;
const DEFAULT_MAX_CONTEXT_TOKENS: usize = 100_000;
const MAX_RECENT_FILES: usize = 10;
const MAX_PROMPT_RETRIES: usize = 3;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Metadata about a compaction operation.
#[derive(Debug, Clone, Serialize)]
pub struct CompactMetadata {
    pub compaction_id: String,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub messages_before: usize,
    pub messages_after: usize,
    pub timestamp: String,
    pub transcript_path: Option<String>,
}

/// A hook that runs before or after compaction.
/// Receives the message array, optionally returns a string to inject into the summarization prompt.
pub type CompactHook =
    Box<dyn Fn(&[Value]) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// Checkpoint for resuming a tool loop session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolLoopCheckpoint {
    pub session_id: String,
    pub checkpoint_round: usize,
    pub timestamp: String,
    pub all_messages: Vec<Value>,
    pub tool_results: Vec<CheckpointToolResult>,
    pub tool_call_history: Vec<String>,
    pub total_tool_calls: usize,
    pub consecutive_errors: usize,
    pub early_content: Option<String>,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointToolResult {
    pub tool: String,
    pub result: Value,
}

/// Options for `compress_older_messages`.
#[derive(Debug, Default)]
pub struct CompactOptions {
    /// Bypass cooldown (used by emergency retry loop).
    pub force: bool,
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

struct CompactState {
    consecutive_failures: u32,
    last_compaction_time: u64,
    recent_file_reads: HashMap<String, FileReadEntry>,
    active_plan: Option<String>,
    invoked_skills: HashMap<String, String>,
    pre_compact_hooks: Vec<CompactHook>,
    post_compact_hooks: Vec<CompactHook>,
}

struct FileReadEntry {
    path: String,
    content: String,
    timestamp: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

static COMPACT_STATE: std::sync::OnceLock<Mutex<CompactState>> = std::sync::OnceLock::new();

fn compact_state() -> &'static Mutex<CompactState> {
    COMPACT_STATE.get_or_init(|| {
        Mutex::new(CompactState {
            consecutive_failures: 0,
            last_compaction_time: 0,
            recent_file_reads: HashMap::new(),
            active_plan: None,
            invoked_skills: HashMap::new(),
            pre_compact_hooks: Vec::new(),
            post_compact_hooks: Vec::new(),
        })
    })
}

// ---------------------------------------------------------------------------
// Public tracking functions (called from toolbox during tool execution)
// ---------------------------------------------------------------------------

/// Track a file read for post-compact context restoration.
pub fn track_file_read(file_path: &str, content: &str) {
    let mut state = compact_state().lock().unwrap();
    let entry = FileReadEntry {
        path: file_path.to_string(),
        content: content.chars().take(20_000).collect(),
        timestamp: now_ms(),
    };
    state.recent_file_reads.insert(file_path.to_string(), entry);

    // Evict oldest beyond limit
    if state.recent_file_reads.len() > MAX_RECENT_FILES {
        let mut entries: Vec<(String, u64)> = state
            .recent_file_reads
            .iter()
            .map(|(k, v)| (k.clone(), v.timestamp))
            .collect();
        entries.sort_by_key(|(_, ts)| *ts);
        let to_remove = entries.len() - MAX_RECENT_FILES;
        for (key, _) in entries.into_iter().take(to_remove) {
            state.recent_file_reads.remove(&key);
        }
    }
}

/// Track the active plan for post-compact restoration.
pub fn set_active_plan(plan: Option<String>) {
    compact_state().lock().unwrap().active_plan = plan;
}

/// Get the active plan.
pub fn get_active_plan() -> Option<String> {
    compact_state().lock().unwrap().active_plan.clone()
}

/// Track an invoked skill for post-compact restoration.
pub fn track_invoked_skill(name: &str, content: &str) {
    let truncated: String = content.chars().take(5000).collect();
    compact_state()
        .lock()
        .unwrap()
        .invoked_skills
        .insert(name.to_string(), truncated);
}

// ---------------------------------------------------------------------------
// Compact hooks
// ---------------------------------------------------------------------------

/// Register a hook to run before compaction.
/// The hook receives the message array and can return a string to inject into the prompt.
pub fn on_pre_compact(hook: CompactHook) {
    compact_state().lock().unwrap().pre_compact_hooks.push(hook);
}

/// Register a hook to run after compaction.
pub fn on_post_compact(hook: CompactHook) {
    compact_state()
        .lock()
        .unwrap()
        .post_compact_hooks.push(hook);
}

// ---------------------------------------------------------------------------
// Estimate message size
// ---------------------------------------------------------------------------

/// Estimate total character size of a messages array.
pub fn estimate_messages_chars(messages: &[Value]) -> usize {
    let mut total = 0usize;
    for m in messages {
        if let Some(s) = m["content"].as_str() {
            total += s.len();
        } else if let Some(arr) = m["content"].as_array() {
            for part in arr {
                if part["type"].as_str() == Some("text") {
                    total += part["text"].as_str().map(|t| t.len()).unwrap_or(0);
                } else if part["type"].as_str() == Some("image_url") {
                    total += 2000;
                }
            }
        }
        if let Some(tc) = m.get("tool_calls") {
            total += tc.to_string().len();
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Validate message structure
// ---------------------------------------------------------------------------

/// Result of message validation.
pub struct ValidateResult {
    pub valid: bool,
    pub messages: Vec<Value>,
    pub dropped: usize,
}

/// Validate the structural integrity of a message array against API tool-use rules.
/// Removes orphaned `tool` messages, strips dangling `tool_calls` ids from
/// assistant messages, and ensures the first non-system message is a user message.
pub fn validate_message_structure(messages: &[Value]) -> ValidateResult {
    let mut cleaned: Vec<Value> = Vec::new();
    let mut dropped: usize = 0;
    let mut seen_assistant_tool_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for (idx, m) in messages.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or("");

        if role == "assistant" {
            if let Some(tool_calls) = m["tool_calls"].as_array() {
                if !tool_calls.is_empty() {
                    // Look ahead for matching tool results
                    let mut responded_ids: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for j in (idx + 1)..messages.len() {
                        if messages[j]["role"].as_str() != Some("tool") {
                            break;
                        }
                        if let Some(tcid) = messages[j]["tool_call_id"].as_str() {
                            responded_ids.insert(tcid.to_string());
                        }
                    }

                    let kept_tool_calls: Vec<&Value> = tool_calls
                        .iter()
                        .filter(|tc| {
                            tc["id"]
                                .as_str()
                                .map(|id| responded_ids.contains(id))
                                .unwrap_or(false)
                        })
                        .collect();

                    if kept_tool_calls.len() == tool_calls.len() {
                        // All tool_calls have matching results
                        for tc in &kept_tool_calls {
                            if let Some(id) = tc["id"].as_str() {
                                seen_assistant_tool_ids.insert(id.to_string());
                            }
                        }
                        cleaned.push(m.clone());
                    } else if !kept_tool_calls.is_empty() {
                        // Some tool_calls survived
                        for tc in &kept_tool_calls {
                            if let Some(id) = tc["id"].as_str() {
                                seen_assistant_tool_ids.insert(id.to_string());
                            }
                        }
                        let kept_arr: Vec<Value> =
                            kept_tool_calls.into_iter().cloned().collect();
                        let mut trimmed = m.clone();
                        trimmed["tool_calls"] = json!(kept_arr);
                        cleaned.push(trimmed);
                        dropped += tool_calls.len() - kept_arr.len();
                    } else {
                        // No tool_calls survive — keep message only if it has text content
                        let text = m["content"].as_str().unwrap_or("");
                        if !text.trim().is_empty() {
                            let mut stripped = m.clone();
                            if let Some(obj) = stripped.as_object_mut() {
                                obj.remove("tool_calls");
                            }
                            cleaned.push(stripped);
                        }
                        dropped += tool_calls.len();
                    }
                    continue;
                }
            }
            // Assistant without tool_calls — pass through
            cleaned.push(m.clone());
        } else if role == "tool" {
            if let Some(tcid) = m["tool_call_id"].as_str() {
                if seen_assistant_tool_ids.contains(tcid) {
                    cleaned.push(m.clone());
                } else {
                    dropped += 1;
                }
            } else {
                dropped += 1;
            }
        } else {
            cleaned.push(m.clone());
        }
    }

    // Ensure the first non-system message is `user`
    let mut first_non_system = 0;
    while first_non_system < cleaned.len()
        && cleaned[first_non_system]["role"].as_str() == Some("system")
    {
        first_non_system += 1;
    }
    if first_non_system < cleaned.len()
        && cleaned[first_non_system]["role"].as_str() != Some("user")
    {
        cleaned.insert(
            first_non_system,
            json!({"role": "user", "content": "Continue."}),
        );
    }

    if dropped > 0 {
        info!("[ValidateMsg] Dropped {} malformed messages", dropped);
    }

    ValidateResult {
        valid: dropped == 0,
        messages: cleaned,
        dropped,
    }
}

// ---------------------------------------------------------------------------
// Truncate largest tool result
// ---------------------------------------------------------------------------

/// Truncate the largest `tool` message in-place so an oversized single tool
/// result doesn't keep blowing the context budget. Used by the emergency retry
/// loop on retries 2+ when compaction couldn't reduce size.
pub fn truncate_largest_tool_result(messages: &mut Vec<Value>, max_len: usize) {
    let default_max = if max_len == 0 { 4000 } else { max_len };
    let mut largest_idx: Option<usize> = None;
    let mut largest_len: usize = 0;

    for (i, m) in messages.iter().enumerate() {
        if m["role"].as_str() != Some("tool") {
            continue;
        }
        let len = m["content"].as_str().map(|s| s.len()).unwrap_or(0);
        if len > largest_len {
            largest_len = len;
            largest_idx = Some(i);
        }
    }

    if let Some(idx) = largest_idx {
        if largest_len > default_max {
            let original = messages[idx]["content"].as_str().unwrap_or("").to_string();
            let truncated_content: String = original.chars().take(default_max).collect();
            let truncated = format!(
                "{}\n\n[... truncated due to context overflow — original was {} chars ...]",
                truncated_content,
                original.len()
            );
            messages[idx]["content"] = json!(truncated);
            info!(
                "[ContextTrim] Truncated largest tool result at idx {}: {} -> {} chars",
                idx,
                original.len(),
                truncated.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Trim conversation context (naive fallback)
// ---------------------------------------------------------------------------

/// Trim conversation messages to fit within a character budget.
/// Keeps system messages from the start + most recent messages.
/// Tool-pair aware: groups assistant+tool messages atomically.
/// Default ~6M chars (~1.5M tokens).
pub fn trim_conversation_context(messages: &[Value], max_chars: usize) -> Vec<Value> {
    let total_chars = estimate_messages_chars(messages);
    if total_chars <= max_chars {
        return messages.to_vec();
    }

    let mut result: Vec<Value> = Vec::new();
    let mut used_chars = 0usize;

    // Keep system messages from the start
    let mut start_idx = 0;
    while start_idx < messages.len()
        && messages[start_idx]["role"].as_str() == Some("system")
    {
        let c = messages[start_idx]["content"]
            .as_str()
            .map(|s| s.len())
            .unwrap_or(500);
        used_chars += c;
        result.push(messages[start_idx].clone());
        start_idx += 1;
    }

    // Pre-group remaining messages into atomic units
    let units = group_into_atomic_units(&messages[start_idx..]);

    let unit_chars = |unit: &[Value]| -> usize {
        let mut n = 0usize;
        for m in unit {
            if let Some(s) = m["content"].as_str() {
                n += s.len();
            } else {
                n += 500;
            }
            if m.get("tool_calls").is_some() {
                n += m["tool_calls"].to_string().len();
            }
        }
        n
    };

    // Walk units from most recent backward, taking whole units while budget allows
    let mut taken: Vec<Vec<Value>> = Vec::new();
    let mut dropped_units = 0;
    for k in (0..units.len()).rev() {
        let c = unit_chars(&units[k]);
        if used_chars + c > max_chars {
            dropped_units = k + 1;
            break;
        }
        taken.push(units[k].clone());
        used_chars += c;
    }
    taken.reverse();

    if dropped_units > 0 {
        result.push(json!({
            "role": "system",
            "content": "[Earlier conversation history was trimmed to fit context window]"
        }));
    }

    for unit in &taken {
        result.extend(unit.iter().cloned());
    }

    // Final structural validation
    let validated = validate_message_structure(&result);

    info!(
        "[ContextTrim] Trimmed {} messages ({} chars) -> {} messages ({} chars)",
        messages.len(),
        total_chars,
        validated.messages.len(),
        used_chars
    );
    validated.messages
}

/// Group messages into atomic units where each assistant{tool_calls} is bundled
/// with its matching tool{tool_call_id} results.
fn group_into_atomic_units(messages: &[Value]) -> Vec<Vec<Value>> {
    let mut units: Vec<Vec<Value>> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        if msg["role"].as_str() == Some("assistant") {
            if let Some(tool_calls) = msg["tool_calls"].as_array() {
                if !tool_calls.is_empty() {
                    let ids: std::collections::HashSet<String> = tool_calls
                        .iter()
                        .filter_map(|tc| tc["id"].as_str().map(|s| s.to_string()))
                        .collect();
                    let mut unit = vec![msg.clone()];
                    let mut j = i + 1;
                    while j < messages.len()
                        && messages[j]["role"].as_str() == Some("tool")
                        && messages[j]["tool_call_id"]
                            .as_str()
                            .map(|id| ids.contains(id))
                            .unwrap_or(false)
                    {
                        unit.push(messages[j].clone());
                        j += 1;
                    }
                    units.push(unit);
                    i = j;
                    continue;
                }
            }
        }

        // Standalone message (user, system, orphan tool, assistant without tool_calls)
        units.push(vec![msg.clone()]);
        i += 1;
    }

    units
}

// ---------------------------------------------------------------------------
// Smart tool result compression
// ---------------------------------------------------------------------------

/// Compress a tool result intelligently based on tool type.
pub fn compress_tool_result(tool_name: &str, result: &Value, max_len: usize) -> String {
    let raw = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_string());
    if raw.len() <= max_len {
        return raw;
    }

    // For error results, keep full error info (usually small)
    if result["ok"].as_bool() == Some(false) || result["exitCode"].as_i64() == Some(1) {
        let mut compact = json!({"ok": false});
        if let Some(e) = result["error"].as_str() {
            compact["error"] = json!(&e[..e.len().min(2000)]);
        }
        if let Some(e) = result["stderr"].as_str() {
            compact["stderr"] = json!(&e[..e.len().min(2000)]);
        }
        if let Some(ec) = result.get("exitCode") {
            compact["exitCode"] = ec.clone();
        }
        if let Some(of) = result.get("outputFiles") {
            compact["outputFiles"] = of.clone();
        }
        return serde_json::to_string(&compact).unwrap_or(raw);
    }

    // run_python / run_shell: keep first+last lines of stdout
    if tool_name == "run_python" || tool_name == "run_shell" {
        if let Some(stdout) = result["stdout"].as_str() {
            let lines: Vec<&str> = stdout.lines().collect();
            let mut compact = json!({"exitCode": result["exitCode"].as_i64().unwrap_or(0)});
            if let Some(of) = result.get("outputFiles") {
                if of.is_array() && !of.as_array().unwrap().is_empty() {
                    compact["outputFiles"] = of.clone();
                }
            }
            if lines.len() <= 60 {
                compact["stdout"] = json!(&stdout[..stdout.len().min(max_len.saturating_sub(200))]);
            } else {
                let head: String = lines[..30].join("\n");
                let tail: String = lines[lines.len() - 20..].join("\n");
                compact["stdout"] = json!(format!(
                    "{}\n\n[...{} lines omitted...]\n\n{}",
                    head,
                    lines.len() - 50,
                    tail
                ));
            }
            if let Some(stderr) = result["stderr"].as_str() {
                compact["stderr"] = json!(&stderr[..stderr.len().min(1000)]);
            }
            return serde_json::to_string(&compact).unwrap_or(raw);
        }
    }

    // web_search: keep titles + URLs, truncate snippets
    if tool_name == "web_search" || tool_name == "openrouter_web_search" {
        if let Some(results) = result["results"].as_array() {
            let compact_results: Vec<Value> = results
                .iter()
                .map(|r| {
                    let snippet = r["snippet"]
                        .as_str()
                        .map(|s| &s[..s.len().min(150)])
                        .unwrap_or("");
                    json!({
                        "title": r["title"],
                        "url": r["url"],
                        "snippet": snippet,
                    })
                })
                .collect();
            let mut compact = result.clone();
            compact["results"] = json!(compact_results);
            return serde_json::to_string(&compact).unwrap_or(raw);
        }
    }

    // fetch_url: keep structure preview
    if tool_name == "fetch_url" {
        if let Some(content) = result["content"].as_str() {
            let lines: Vec<&str> = content.lines().collect();
            let mut compact = json!({"ok": true, "url": result["url"]});
            if lines.len() <= 50 {
                compact["content"] = json!(&content[..content.len().min(max_len.saturating_sub(200))]);
            } else {
                let head: String = lines[..30].join("\n");
                let tail: String = lines[lines.len() - 10..].join("\n");
                compact["content"] = json!(format!(
                    "{}\n[...{} lines omitted...]\n{}",
                    head,
                    lines.len() - 40,
                    tail
                ));
            }
            return serde_json::to_string(&compact).unwrap_or(raw);
        }
    }

    // read_file: keep first+last lines
    if tool_name == "read_file" {
        if let Some(content) = result["content"].as_str() {
            let lines: Vec<&str> = content.lines().collect();
            let mut compact = json!({"path": result["path"]});
            if lines.len() <= 50 {
                compact["content"] = json!(&content[..content.len().min(max_len.saturating_sub(100))]);
            } else {
                let head: String = lines[..30].join("\n");
                let tail: String = lines[lines.len() - 10..].join("\n");
                compact["content"] = json!(format!(
                    "{}\n[...{} lines omitted...]\n{}",
                    head,
                    lines.len() - 40,
                    tail
                ));
            }
            return serde_json::to_string(&compact).unwrap_or(raw);
        }
    }

    // list_files: cap entries
    if tool_name == "list_files" {
        if let Some(files) = result["files"].as_array() {
            if files.len() > 50 {
                let mut compact = result.clone();
                compact["files"] = json!(&files[..50]);
                compact["_note"] = json!(format!("Showing 50 of {} files", files.len()));
                return serde_json::to_string(&compact).unwrap_or(raw);
            }
        }
    }

    // Default: truncate with note
    if raw.len() > max_len {
        format!("{}...(truncated)", &raw[..max_len.saturating_sub(20)])
    } else {
        raw
    }
}

// ---------------------------------------------------------------------------
// Group messages by round
// ---------------------------------------------------------------------------

/// Group messages by API round: each round = user message + assistant response + tool results.
/// Used during prompt-too-long retry to intelligently drop message groups.
fn group_messages_by_round(messages: &[Value]) -> Vec<Vec<Value>> {
    let mut groups: Vec<Vec<Value>> = Vec::new();
    let mut current: Vec<Value> = Vec::new();

    for msg in messages {
        if msg["role"].as_str() == Some("user") && !current.is_empty() {
            groups.push(current);
            current = Vec::new();
        }
        current.push(msg.clone());
    }
    if !current.is_empty() {
        groups.push(current);
    }

    groups
}

// ---------------------------------------------------------------------------
// Compact artifact detection
// ---------------------------------------------------------------------------

/// Identify system messages that were injected by a prior compaction so we can
/// drop them on the next compaction instead of letting them accumulate.
fn is_compact_artifact(msg: &Value) -> bool {
    if msg["role"].as_str() != Some("system") {
        return false;
    }
    let c = msg["content"].as_str().unwrap_or("");
    c.starts_with("[COMPACT BOUNDARY")
        || c.starts_with("[Post-compact:")
        || c.starts_with("This session is continued from a previous conversation")
}

// ---------------------------------------------------------------------------
// Summarization prompt
// ---------------------------------------------------------------------------

fn build_summarization_prompt(
    tool_call_count: usize,
    conversation_parts: &[String],
    msg_count: usize,
) -> Vec<Value> {
    vec![
        json!({
            "role": "system",
            "content": r#"You are a conversation context compressor. You will receive a conversation history and must produce a structured summary.

RULES:
- Do NOT use any tools — respond with text only.
- First produce an <analysis> block where you think through what's important (this is your scratchpad and will be stripped).
- Then produce a <summary> block with EXACTLY these 9 sections:

<analysis>
(Your private reasoning about what to preserve and what to drop. Consider: what is the user trying to accomplish? What files were touched? What errors occurred? What decisions were made?)
</analysis>

<summary>
## a. Primary Request and Intent
(What the user originally asked for and what they're trying to achieve)

## b. Key Technical Concepts
(Important technical terms, algorithms, frameworks, or domain concepts discussed)

## c. Files and Code Sections
(File paths mentioned or modified, with relevant code snippets — preserve exact paths and line numbers)

## d. Errors and Fixes
(Errors encountered and how they were resolved, or unresolved errors)

## e. Problem Solving
(Key decisions made, approaches tried, reasoning about trade-offs)

## f. All User Messages
(Reproduce ALL non-tool-result user messages — preserve the user's exact words where possible)

## g. Pending Tasks
(Tasks mentioned but not yet completed, next steps discussed)

## h. Current Work
(What was actively being worked on at the end of this conversation segment)

## i. Optional Next Step
(If the conversation implies a clear next action, state it with verbatim quotes from the user if applicable)
</summary>

Be thorough but concise. Preserve factual details, file paths, exact error messages, and code snippets. Do NOT fabricate information."#
        }),
        json!({
            "role": "user",
            "content": format!(
                "Compress this conversation history ({} messages, {} tool calls):\n\n{}",
                msg_count, tool_call_count, conversation_parts.join("\n")
            )
        }),
    ]
}

// ---------------------------------------------------------------------------
// Strip images/docs for summarization
// ---------------------------------------------------------------------------

fn strip_for_summarization(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            if let Some(arr) = msg["content"].as_array() {
                let stripped: Vec<Value> = arr
                    .iter()
                    .map(|part| {
                        let ptype = part["type"].as_str().unwrap_or("");
                        if ptype == "image_url" || ptype == "image" {
                            json!({"type": "text", "text": "[image]"})
                        } else if ptype == "document" || ptype == "file" {
                            let name = part["name"]
                                .as_str()
                                .or(part["path"].as_str())
                                .unwrap_or("unknown");
                            json!({"type": "text", "text": format!("[document: {}]", name)})
                        } else {
                            part.clone()
                        }
                    })
                    .collect();
                let mut m = msg.clone();
                m["content"] = json!(stripped);
                m
            } else {
                msg.clone()
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Format compact summary
// ---------------------------------------------------------------------------

fn format_compact_summary(raw_summary: &str, metadata: &CompactMetadata) -> String {
    // Strip <analysis> block
    let re_analysis = regex::Regex::new(r"(?si)<analysis>.*?</analysis>").unwrap();
    let formatted = re_analysis.replace_all(raw_summary, "").trim().to_string();

    // Extract <summary> content if present
    let re_summary = regex::Regex::new(r"(?si)<summary>(.*?)</summary>").unwrap();
    let formatted = if let Some(caps) = re_summary.captures(&formatted) {
        caps[1].trim().to_string()
    } else {
        formatted
    };

    let header = format!(
        "This session is continued from a previous conversation that was compacted to save context space.\n\
         Compaction ID: {} | Messages: {} -> {} | Tokens saved: ~{}",
        metadata.compaction_id,
        metadata.messages_before,
        metadata.messages_after,
        metadata.tokens_before.saturating_sub(metadata.tokens_after)
    );

    let transcript_note = metadata
        .transcript_path
        .as_ref()
        .map(|p| {
            format!(
                "\nFull pre-compact transcript available at: {} (use read_file to access if needed)",
                p
            )
        })
        .unwrap_or_default();

    format!("{}{}\n\n---\n\n{}", header, transcript_note, formatted)
}

// ---------------------------------------------------------------------------
// Post-compact attachments
// ---------------------------------------------------------------------------

fn build_post_compact_attachments() -> Vec<Value> {
    let state = compact_state().lock().unwrap();
    let mut attachments = Vec::new();
    const MAX_TOTAL_FILE_CHARS: usize = 200_000;

    // 1. Top 5 recently-read files
    let mut recent_files: Vec<&FileReadEntry> = state.recent_file_reads.values().collect();
    recent_files.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    recent_files.truncate(5);

    if !recent_files.is_empty() {
        let mut total_chars = 0usize;
        let mut file_parts = Vec::new();
        for file in &recent_files {
            let content: String = file.content.chars().take(20_000).collect();
            if total_chars + content.len() > MAX_TOTAL_FILE_CHARS {
                break;
            }
            file_parts.push(format!("### {}\n```\n{}\n```", file.path, content));
            total_chars += content.len();
        }
        if !file_parts.is_empty() {
            attachments.push(json!({
                "role": "system",
                "content": format!(
                    "[Post-compact: Recently-read files ({} files)]\n\n{}",
                    file_parts.len(),
                    file_parts.join("\n\n")
                )
            }));
        }
    }

    // 2. Active plan
    if let Some(ref plan) = state.active_plan {
        attachments.push(json!({
            "role": "system",
            "content": format!("[Post-compact: Active Plan]\n\n{}", plan)
        }));
    }

    // 3. Invoked skills
    if !state.invoked_skills.is_empty() {
        let skill_parts: Vec<String> = state
            .invoked_skills
            .iter()
            .map(|(name, content)| format!("### Skill: {}\n{}", name, content))
            .collect();
        attachments.push(json!({
            "role": "system",
            "content": format!("[Post-compact: Invoked Skills]\n\n{}", skill_parts.join("\n\n"))
        }));
    }

    attachments
}

// ---------------------------------------------------------------------------
// Write pre-compact transcript
// ---------------------------------------------------------------------------

async fn write_compact_transcript(compaction_id: &str, messages: &[Value]) -> Option<String> {
    let transcript_dir = "data/transcripts";
    if let Err(e) = tokio::fs::create_dir_all(transcript_dir).await {
        error!("[Compact] Failed to create transcript dir: {}", e);
        return None;
    }

    let transcript_path = format!("{}/compact_{}.jsonl", transcript_dir, compaction_id);

    let lines: Vec<String> = messages
        .iter()
        .map(|msg| {
            let content = msg["content"]
                .as_str()
                .map(|s| {
                    let truncated: String = s.chars().take(10_000).collect();
                    truncated
                })
                .unwrap_or_else(|| "[multimodal]".to_string());
            let compact = json!({
                "role": msg["role"],
                "content": content,
                "tool_call_id": msg.get("tool_call_id"),
            });
            serde_json::to_string(&compact).unwrap_or_default()
        })
        .collect();

    match tokio::fs::write(&transcript_path, lines.join("\n")).await {
        Ok(_) => {
            info!(
                "[Compact] Transcript written: {} ({} messages)",
                transcript_path,
                lines.len()
            );
            Some(transcript_path)
        }
        Err(e) => {
            error!("[Compact] Failed to write transcript: {}", e);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// LLM call for summarization
// ---------------------------------------------------------------------------

async fn llm_summarize(
    api_key: &str,
    api_url: &str,
    model: &str,
    prompt_messages: Vec<Value>,
) -> Result<String, String> {
    let client = Client::new();
    let body = json!({
        "model": model,
        "messages": prompt_messages,
    });

    let resp = client
        .post(api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .header("User-Agent", "claude-code/1.0.6")
        .header("X-Client-Name", "claude-code")
        .header("X-Client-Version", "1.0.6")
        .header("HTTP-Referer", "https://claude.ai")
        .header("X-Traffic-Source", "claude-code")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("LLM request failed: {e}"))?;

    let status = resp.status();
    let resp_body: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse LLM response: {e}"))?;

    if !status.is_success() {
        let err_msg = resp_body["error"]["message"]
            .as_str()
            .or(resp_body["error"].as_str())
            .unwrap_or("unknown error");
        return Err(err_msg.to_string());
    }

    // Parse OpenAI-compatible response
    let content = resp_body["choices"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|c| c["message"]["content"].as_str())
        // Fallback: Anthropic native format
        .or_else(|| {
            resp_body["content"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|b| b["text"].as_str())
        })
        .unwrap_or("")
        .to_string();

    Ok(content)
}

// ---------------------------------------------------------------------------
// Main: compressOlderMessages
// ---------------------------------------------------------------------------

/// Full Compact Algorithm — structured 9-step compaction pipeline.
///
/// Replaces older messages with a structured LLM-generated summary,
/// restores critical context (files, plans, skills), and writes
/// a transcript of the pre-compact messages for later retrieval.
pub async fn compress_older_messages(
    all_messages: &[Value],
    window_size: usize,
    api_key: &str,
    api_url: &str,
    model: &str,
    options: Option<&CompactOptions>,
) -> Vec<Value> {
    let force = options.map(|o| o.force).unwrap_or(false);

    // Find system message boundary
    let mut system_end = 0;
    while system_end < all_messages.len()
        && all_messages[system_end]["role"].as_str() == Some("system")
    {
        system_end += 1;
    }

    // Preserve only the original system prompt(s); strip prior compaction artifacts
    // so they don't accumulate on every compaction cycle.
    let original_system: Vec<Value> = all_messages[..system_end]
        .iter()
        .filter(|m| !is_compact_artifact(m))
        .cloned()
        .collect();

    let non_system = &all_messages[system_end..];
    if non_system.len() <= window_size {
        // Nothing to compress, but dedupe accumulated artifacts if any slipped in
        if original_system.len() < system_end {
            let mut result = original_system;
            result.extend_from_slice(non_system);
            return result;
        }
        return all_messages.to_vec();
    }

    // Circuit breaker
    {
        let state = compact_state().lock().unwrap();
        if state.consecutive_failures >= MAX_COMPACT_FAILURES {
            warn!(
                "[Compact] Circuit breaker: {} consecutive failures. Skipping.",
                state.consecutive_failures
            );
            return all_messages.to_vec();
        }

        // Cooldown — bypassed when called with force from emergency retry loop
        let now = now_ms();
        if !force && now - state.last_compaction_time < COMPACT_COOLDOWN_MS {
            info!(
                "[Compact] Cooldown: last compaction was {}s ago (min {}s). Skipping.",
                (now - state.last_compaction_time) / 1000,
                COMPACT_COOLDOWN_MS / 1000
            );
            return all_messages.to_vec();
        }
    }

    let compaction_id = Uuid::new_v4().to_string()[..16].to_string();
    let tokens_before = estimate_messages_chars(all_messages) / 4;

    info!(
        "[Compact] Starting compaction {} — {} messages, ~{} tokens",
        compaction_id,
        all_messages.len(),
        tokens_before
    );

    // Step 1: Pre-compact hooks
    let pre_hook_futures: Vec<Pin<Box<dyn Future<Output = Option<String>> + Send>>> = {
        let state = compact_state().lock().unwrap();
        state
            .pre_compact_hooks
            .iter()
            .map(|hook| hook(all_messages))
            .collect()
    };
    let mut hook_injections: Vec<String> = Vec::new();
    for fut in pre_hook_futures {
        if let Some(result) = fut.await {
            hook_injections.push(result);
        }
    }

    // Step 2 & 3: Prepare messages for summarization
    let split_point = non_system.len() - window_size;
    let to_compress = &non_system[..split_point];
    let to_keep = &non_system[split_point..];

    // Step 3: Strip images/documents
    let stripped = strip_for_summarization(to_compress);

    // Build conversation parts for summarization prompt
    let mut summary_parts: Vec<String> = Vec::new();
    let mut tool_call_count = 0usize;

    for msg in &stripped {
        let role = msg["role"].as_str().unwrap_or("");
        match role {
            "user" => {
                let text = msg["content"]
                    .as_str()
                    .map(|s| s.chars().take(500).collect::<String>())
                    .unwrap_or_else(|| "(multimodal)".to_string());
                summary_parts.push(format!("USER: {}", text));
            }
            "assistant" => {
                if let Some(text) = msg["content"].as_str() {
                    if !text.is_empty() {
                        let truncated: String = text.chars().take(300).collect();
                        summary_parts.push(format!("ASSISTANT: {}", truncated));
                    }
                }
                if let Some(calls) = msg["tool_calls"].as_array() {
                    for tc in calls {
                        let name = tc["function"]["name"].as_str().unwrap_or("unknown");
                        let args = tc["function"]["arguments"]
                            .as_str()
                            .map(|s| s.chars().take(100).collect::<String>())
                            .unwrap_or_default();
                        summary_parts.push(format!("  -> Called {}({})", name, args));
                        tool_call_count += 1;
                    }
                }
            }
            "tool" => {
                let text = msg["content"]
                    .as_str()
                    .map(|s| s.chars().take(200).collect::<String>())
                    .unwrap_or_default();
                summary_parts.push(format!("  RESULT: {}", text));
            }
            _ => {}
        }
    }

    // Add hook injections to the prompt
    if !hook_injections.is_empty() {
        summary_parts.push("\n--- Pre-compact hook context ---".to_string());
        summary_parts.extend(hook_injections);
    }

    // Step 4 & 5: Send to model with retry logic
    let mut summary = String::new();
    let mut prompt_messages = summary_parts.clone();

    for retry in 0..MAX_PROMPT_RETRIES {
        let compression_prompt = build_summarization_prompt(
            tool_call_count,
            &prompt_messages,
            to_compress.len(),
        );

        info!(
            "[Compact] Sending summarization request (attempt {}/{}, {} chars)...",
            retry + 1,
            MAX_PROMPT_RETRIES,
            prompt_messages.join("\n").len()
        );

        match llm_summarize(api_key, api_url, model, compression_prompt).await {
            Ok(result) if !result.is_empty() => {
                summary = result;
                compact_state().lock().unwrap().consecutive_failures = 0;
                break;
            }
            Ok(_) => {
                info!("[Compact] LLM returned empty summary.");
            }
            Err(err_msg) => {
                let is_prompt_too_long = err_msg.contains("context window exceeds")
                    || err_msg.contains("context_length_exceeded")
                    || err_msg.contains("maximum context length")
                    || err_msg.contains("too many tokens");

                if is_prompt_too_long && retry < MAX_PROMPT_RETRIES - 1 {
                    // Step 5: Drop oldest message groups using round-based grouping
                    info!(
                        "[Compact] Prompt too long — dropping oldest message groups (retry {})...",
                        retry + 1
                    );

                    let keep_count =
                        stripped.len() - stripped.len() / (retry + 2);
                    let remaining_messages = &stripped[stripped.len().saturating_sub(keep_count)..];
                    let groups = group_messages_by_round(remaining_messages);
                    let remaining: Vec<Value> = groups.into_iter().flatten().collect();

                    prompt_messages = Vec::new();
                    for msg in &remaining {
                        let role = msg["role"].as_str().unwrap_or("");
                        match role {
                            "user" => {
                                let text = msg["content"]
                                    .as_str()
                                    .map(|s| s.chars().take(300).collect::<String>())
                                    .unwrap_or_else(|| "(multimodal)".to_string());
                                prompt_messages.push(format!("USER: {}", text));
                            }
                            "assistant" => {
                                if let Some(text) = msg["content"].as_str() {
                                    if !text.is_empty() {
                                        let truncated: String = text.chars().take(150).collect();
                                        prompt_messages
                                            .push(format!("ASSISTANT: {}", truncated));
                                    }
                                }
                                if let Some(calls) = msg["tool_calls"].as_array() {
                                    for tc in calls {
                                        let name = tc["function"]["name"]
                                            .as_str()
                                            .unwrap_or("unknown");
                                        prompt_messages
                                            .push(format!("  -> Called {}", name));
                                    }
                                }
                            }
                            "tool" => {
                                let text = msg["content"]
                                    .as_str()
                                    .map(|s| s.chars().take(100).collect::<String>())
                                    .unwrap_or_default();
                                prompt_messages.push(format!("  RESULT: {}", text));
                            }
                            _ => {}
                        }
                    }

                    info!(
                        "[Compact] Reduced to {} parts (dropped {} parts)",
                        prompt_messages.len(),
                        summary_parts.len().saturating_sub(prompt_messages.len())
                    );
                    continue;
                }

                error!("[Compact] Summarization failed: {}", err_msg);
                compact_state().lock().unwrap().consecutive_failures += 1;
                return all_messages.to_vec();
            }
        }
    }

    if summary.is_empty() {
        warn!("[Compact] All summarization attempts returned empty. Falling back.");
        compact_state().lock().unwrap().consecutive_failures += 1;
        return all_messages.to_vec();
    }

    // Step 6: Format the summary
    let transcript_path = write_compact_transcript(&compaction_id, to_compress).await;

    let mut metadata = CompactMetadata {
        compaction_id: compaction_id.clone(),
        tokens_before,
        tokens_after: 0, // computed after building final messages
        messages_before: all_messages.len(),
        messages_after: 0,
        timestamp: chrono::Utc::now().to_rfc3339(),
        transcript_path,
    };

    let formatted_summary = format_compact_summary(&summary, &metadata);

    // Step 7: Build post-compact attachments
    let post_compact_attachments = build_post_compact_attachments();

    // Step 8: Build the new message history
    let mut compressed: Vec<Value> = Vec::new();

    // Keep only the ORIGINAL system prompt(s); compaction artifacts from prior
    // compactions were filtered out above to prevent unbounded growth.
    compressed.extend(original_system);

    // Compact boundary marker
    compressed.push(json!({
        "role": "system",
        "content": format!(
            "[COMPACT BOUNDARY — id:{} | {} messages compacted, {} tool calls | tokens saved: ~{}]",
            compaction_id, to_compress.len(), tool_call_count, tokens_before
        )
    }));

    // Formatted summary
    compressed.push(json!({
        "role": "system",
        "content": formatted_summary
    }));

    // Post-compact attachments
    compressed.extend(post_compact_attachments);

    // Ensure a user message exists before assistant messages
    if to_keep.is_empty() || to_keep[0]["role"].as_str() != Some("user") {
        let first_user = to_compress
            .iter()
            .find(|m| m["role"].as_str() == Some("user"));
        let content = first_user
            .and_then(|m| m["content"].as_str())
            .unwrap_or("Continue with the task.");
        compressed.push(json!({"role": "user", "content": content}));
    }

    // Keep recent messages
    compressed.extend_from_slice(to_keep);

    // Update metadata with final token count
    metadata.tokens_after = estimate_messages_chars(&compressed) / 4;
    metadata.messages_after = compressed.len();

    info!(
        "[Compact] Compaction {} complete: {} -> {} messages, ~{} -> ~{} tokens",
        compaction_id,
        metadata.messages_before,
        metadata.messages_after,
        metadata.tokens_before,
        metadata.tokens_after
    );

    // Step 9: Cleanup
    {
        let mut state = compact_state().lock().unwrap();
        state.last_compaction_time = now_ms();
        state.recent_file_reads.clear();
        state.invoked_skills.clear();
    }

    // Execute post-compact hooks
    let post_hook_futures: Vec<Pin<Box<dyn Future<Output = Option<String>> + Send>>> = {
        let state = compact_state().lock().unwrap();
        state
            .post_compact_hooks
            .iter()
            .map(|hook| hook(&compressed))
            .collect()
    };
    for fut in post_hook_futures {
        let _ = fut.await;
    }

    // Final structural validation — make sure the compaction step itself didn't
    // leave any orphaned tool messages or break the user-first invariant.
    let validated = validate_message_structure(&compressed);
    validated.messages
}

// ---------------------------------------------------------------------------
// Checkpoint & Resume
// ---------------------------------------------------------------------------

/// Save a checkpoint for session resumption.
pub async fn save_checkpoint(session_id: &str, checkpoint: &ToolLoopCheckpoint) {
    let dir = "data/checkpoints";
    if let Err(e) = tokio::fs::create_dir_all(dir).await {
        error!("[Checkpoint] Failed to create checkpoint dir: {}", e);
        return;
    }

    let fp = format!("{}/{}.json", dir, session_id);

    // Compress tool results in checkpoint to keep file size reasonable
    let compact_tool_results: Vec<Value> = checkpoint
        .tool_results
        .iter()
        .map(|tr| {
            let mut compact_result = json!({"ok": tr.result.get("ok")});
            if let Some(ec) = tr.result.get("exitCode") {
                compact_result["exitCode"] = ec.clone();
            }
            if let Some(of) = tr.result.get("outputFiles") {
                compact_result["outputFiles"] = of.clone();
            }
            if let Some(stdout) = tr.result["stdout"].as_str() {
                compact_result["stdout"] = json!(&stdout[..stdout.len().min(2000)]);
            }
            if let Some(stderr) = tr.result["stderr"].as_str() {
                compact_result["stderr"] = json!(&stderr[..stderr.len().min(1000)]);
            }
            if let Some(err) = tr.result.get("error") {
                compact_result["error"] = err.clone();
            }
            json!({"tool": tr.tool, "result": compact_result})
        })
        .collect();

    // Compress allMessages — only keep last 20 messages fully, summarize earlier ones
    let compact_messages: Vec<Value> = if checkpoint.all_messages.len() > 30 {
        let mut msgs = Vec::new();
        // system prompt(s)
        msgs.extend_from_slice(&checkpoint.all_messages[..2.min(checkpoint.all_messages.len())]);
        msgs.push(json!({
            "role": "system",
            "content": format!(
                "[Checkpoint: {} earlier messages omitted]",
                checkpoint.all_messages.len().saturating_sub(22)
            )
        }));
        let start = checkpoint.all_messages.len().saturating_sub(20);
        msgs.extend_from_slice(&checkpoint.all_messages[start..]);
        msgs
    } else {
        checkpoint.all_messages.clone()
    };

    let compact_checkpoint = json!({
        "sessionId": checkpoint.session_id,
        "checkpointRound": checkpoint.checkpoint_round,
        "timestamp": checkpoint.timestamp,
        "allMessages": compact_messages,
        "toolResults": compact_tool_results,
        "toolCallHistory": checkpoint.tool_call_history,
        "totalToolCalls": checkpoint.total_tool_calls,
        "consecutiveErrors": checkpoint.consecutive_errors,
        "earlyContent": checkpoint.early_content,
        "systemPrompt": checkpoint.system_prompt,
    });

    let serialized = serde_json::to_string(&compact_checkpoint).unwrap_or_default();
    let size_kb = serialized.len() / 1024;

    match tokio::fs::write(&fp, &serialized).await {
        Ok(_) => {
            info!(
                "[Checkpoint] Saved round {} for session {} ({}KB)",
                checkpoint.checkpoint_round, session_id, size_kb
            );
        }
        Err(e) => {
            error!("[Checkpoint] Failed to save: {}", e);
        }
    }
}

/// Load a checkpoint for session resumption.
pub async fn load_checkpoint(session_id: &str) -> Option<ToolLoopCheckpoint> {
    let dir = "data/checkpoints";
    let fp = format!("{}/{}.json", dir, session_id);

    match tokio::fs::read_to_string(&fp).await {
        Ok(content) => match serde_json::from_str::<ToolLoopCheckpoint>(&content) {
            Ok(checkpoint) => {
                info!(
                    "[Checkpoint] Loaded checkpoint for session {} at round {}",
                    session_id, checkpoint.checkpoint_round
                );
                Some(checkpoint)
            }
            Err(e) => {
                error!("[Checkpoint] Failed to parse: {}", e);
                None
            }
        },
        Err(_) => None,
    }
}

/// Clear a checkpoint for a session.
pub async fn clear_checkpoint(session_id: &str) {
    let dir = "data/checkpoints";
    let fp = format!("{}/{}.json", dir, session_id);

    match tokio::fs::remove_file(&fp).await {
        Ok(_) => {
            info!("[Checkpoint] Cleared checkpoint for session {}", session_id);
        }
        Err(_) => {} // Ignore if doesn't exist
    }
}

// ---------------------------------------------------------------------------
// Compression settings helper
// ---------------------------------------------------------------------------

/// Get compression settings from the extra fields in Settings.
pub fn get_compression_settings(
    extra: &HashMap<String, Value>,
) -> (usize, usize, usize, Option<String>) {
    let interval = extra
        .get("agentCompressionInterval")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_COMPRESSION_INTERVAL as u64) as usize;
    let window = extra
        .get("agentCompressionWindowSize")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_COMPRESSION_WINDOW as u64) as usize;
    let max_tokens = extra
        .get("agentMaxContextTokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_CONTEXT_TOKENS as u64) as usize;
    let compression_model = extra
        .get("agentCompressionModel")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (interval, window, max_tokens, compression_model)
}

// ---------------------------------------------------------------------------
// Context overflow detection
// ---------------------------------------------------------------------------

/// Check if an error message indicates a context overflow.
pub fn is_context_overflow(err_msg: &str) -> bool {
    err_msg.contains("context window exceeds")
        || err_msg.contains("context_length_exceeded")
        || err_msg.contains("maximum context length")
        || err_msg.contains("too many tokens")
        || (err_msg.contains("invalid params") && err_msg.contains("2013"))
}
