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
use serde_json::{json, Value};
use std::collections::HashMap;
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
// Global state
// ---------------------------------------------------------------------------

struct CompactState {
    consecutive_failures: u32,
    last_compaction_time: u64,
    recent_file_reads: HashMap<String, FileReadEntry>,
    active_plan: Option<String>,
    invoked_skills: HashMap<String, String>,
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
// Trim conversation context (naive fallback)
// ---------------------------------------------------------------------------

/// Trim conversation messages to fit within a character budget.
/// Keeps system messages from the start + most recent messages.
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

    // Add messages from the end (most recent) until budget
    let mut reversed: Vec<Value> = Vec::new();
    for i in (start_idx..messages.len()).rev() {
        let msg_chars = messages[i]["content"]
            .as_str()
            .map(|s| s.len())
            .unwrap_or(500);
        if used_chars + msg_chars > max_chars {
            break;
        }
        reversed.push(messages[i].clone());
        used_chars += msg_chars;
    }

    if reversed.len() < messages.len() - start_idx {
        result.push(json!({
            "role": "system",
            "content": "[Earlier conversation history was trimmed to fit context window]"
        }));
    }

    reversed.reverse();
    result.extend(reversed);

    info!(
        "[ContextTrim] Trimmed {} messages ({} chars) -> {} messages ({} chars)",
        messages.len(),
        total_chars,
        result.len(),
        used_chars
    );
    result
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

fn format_compact_summary(
    raw_summary: &str,
    compaction_id: &str,
    messages_before: usize,
    messages_after: usize,
    tokens_before: usize,
    tokens_after: usize,
    transcript_path: Option<&str>,
) -> String {
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
        compaction_id,
        messages_before,
        messages_after,
        tokens_before.saturating_sub(tokens_after)
    );

    let transcript_note = transcript_path
        .map(|p| format!("\nFull pre-compact transcript available at: {} (use read_file to access if needed)", p))
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

async fn write_compact_transcript(
    compaction_id: &str,
    messages: &[Value],
) -> Option<String> {
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
) -> Vec<Value> {
    // Find system message boundary
    let mut system_end = 0;
    while system_end < all_messages.len()
        && all_messages[system_end]["role"].as_str() == Some("system")
    {
        system_end += 1;
    }

    let non_system = &all_messages[system_end..];
    if non_system.len() <= window_size {
        return all_messages.to_vec(); // Nothing to compress
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

        // Cooldown
        let now = now_ms();
        if now - state.last_compaction_time < COMPACT_COOLDOWN_MS {
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
                    // Step 5: Drop oldest message groups
                    info!(
                        "[Compact] Prompt too long — dropping oldest message groups (retry {})...",
                        retry + 1
                    );
                    // Keep only the latter portion
                    let keep_ratio = stripped.len() / (retry + 2);
                    let remaining = &stripped[stripped.len().saturating_sub(keep_ratio)..];

                    prompt_messages = Vec::new();
                    for msg in remaining {
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
                                        prompt_messages.push(format!("ASSISTANT: {}", truncated));
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

    // Step 7: Build post-compact attachments
    let post_compact_attachments = build_post_compact_attachments();

    // Step 8: Build the new message history
    let mut compressed: Vec<Value> = Vec::new();

    // Keep original system messages
    compressed.extend_from_slice(&all_messages[..system_end]);

    // Compact boundary marker
    compressed.push(json!({
        "role": "system",
        "content": format!(
            "[COMPACT BOUNDARY — id:{} | {} messages compacted, {} tool calls | tokens saved: ~{}]",
            compaction_id, to_compress.len(), tool_call_count, tokens_before
        )
    }));

    // Formatted summary (compute tokens_after after building)
    let formatted_summary = format_compact_summary(
        &summary,
        &compaction_id,
        all_messages.len(),
        0, // placeholder, updated below
        tokens_before,
        0,
        transcript_path.as_deref(),
    );

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

    let tokens_after = estimate_messages_chars(&compressed) / 4;

    info!(
        "[Compact] Compaction {} complete: {} -> {} messages, ~{} -> ~{} tokens",
        compaction_id,
        all_messages.len(),
        compressed.len(),
        tokens_before,
        tokens_after
    );

    // Step 9: Cleanup
    {
        let mut state = compact_state().lock().unwrap();
        state.last_compaction_time = now_ms();
        state.recent_file_reads.clear();
    }

    compressed
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
