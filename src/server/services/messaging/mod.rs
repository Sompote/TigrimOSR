//! Messaging-bot core shared by the Telegram and LINE transports.
//!
//! Owns everything that must behave identically on both platforms: per-chat
//! session state (persisted), the slash-command parser/executor, the bridge
//! into the web-chat agent pipeline (`chat::start_agent_run`), progress
//! coalescing/throttling, and UTF-8-safe message splitting. The transports
//! (`telegram.rs`, `line.rs`) only speak their platform's API.

pub mod line;
pub mod telegram;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

use crate::server::data::{data_dir, get_settings, read_json, save_settings, write_json};
use crate::server::routes::chat::{self, AgentRunRequest};
use crate::server::services::toolbox::ToolUpdate;
use crate::util::truncate_utf8_ellipsis;

const STATE_FILE: &str = "messaging_state.json";
/// Throttle for coalesced progress updates (Telegram edit-message cadence).
const PROGRESS_THROTTLE: Duration = Duration::from_secs(3);
/// Hard cap on a single agent run driven from a chat app.
const RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub const SUB_AGENT_MODES: &[&str] = &["single", "auto", "manual", "fully_auto", "router", "graph"];

// ---------------------------------------------------------------------------
// Per-chat state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatState {
    /// Chat-session id in chat_history.json, e.g. "tg_123456789".
    pub session_id: String,
    /// Per-chat sub-agent mode override (/mode). None = "single".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Per-chat agent-loop profile override (/loop). None = settings default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_profile: Option<String>,
}

#[derive(Default)]
struct MessagingState {
    /// Key: "tg:<chat_id>" | "line:<user_id>".
    chats: HashMap<String, ChatState>,
    /// Chats with a run in flight (one task at a time per chat) -> session_id.
    running: HashMap<String, String>,
}

static STATE: OnceLock<TokioMutex<MessagingState>> = OnceLock::new();

fn state() -> &'static TokioMutex<MessagingState> {
    STATE.get_or_init(|| TokioMutex::new(MessagingState::default()))
}

async fn persist_chats() {
    let chats = state().lock().await.chats.clone();
    write_json(STATE_FILE, &chats).await;
}

/// Default session id for a chat key: "tg:123" -> "tg_123".
fn default_session_id(chat_key: &str) -> String {
    chat_key.replace(':', "_")
}

async fn get_or_create_chat(chat_key: &str) -> ChatState {
    let (cs, created) = {
        let mut st = state().lock().await;
        let created = !st.chats.contains_key(chat_key);
        let cs = st
            .chats
            .entry(chat_key.to_string())
            .or_insert_with(|| ChatState {
                session_id: default_session_id(chat_key),
                mode: None,
                loop_profile: None,
            })
            .clone();
        (cs, created)
    };
    if created {
        persist_chats().await;
    }
    cs
}

/// Called from server startup: load persisted chat state, start transports.
pub fn init() {
    tokio::spawn(async {
        let chats: HashMap<String, ChatState> = read_json(STATE_FILE).await;
        state().lock().await.chats = chats;
        telegram::init();
        tracing::info!("[messaging] core initialized (Telegram supervisor spawned)");
    });
}

// ---------------------------------------------------------------------------
// Command parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum BotCommand {
    Agents,
    Model(Option<String>),
    Mode(Option<String>),
    Loop(Option<String>),
    New,
    Stop,
    Status,
    Help,
    Unknown(String),
    Chat(String),
}

pub fn parse_command(text: &str) -> BotCommand {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return BotCommand::Chat(trimmed.to_string());
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let arg = parts
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // Telegram group syntax: "/model@MyBot deepseek-chat"
    let cmd = head.split('@').next().unwrap_or(head).to_ascii_lowercase();
    match cmd.as_str() {
        "/agents" => BotCommand::Agents,
        "/model" => BotCommand::Model(arg),
        "/mode" => BotCommand::Mode(arg),
        "/loop" => BotCommand::Loop(arg),
        "/new" => BotCommand::New,
        "/stop" => BotCommand::Stop,
        "/status" => BotCommand::Status,
        "/help" | "/start" => BotCommand::Help,
        other => BotCommand::Unknown(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Command execution
// ---------------------------------------------------------------------------

pub enum CommandOutcome {
    /// Immediate text reply (command handled).
    Reply(String),
    /// Non-command text: run it through the agent.
    RunChat(String),
}

fn help_text() -> String {
    "TigrimOS bot commands:\n\
     /agents — list agent team configs\n\
     /model [id] — show or switch the model (switch applies to ALL sessions)\n\
     /mode [single|auto|manual|fully_auto|router|graph] — sub-agent mode for this chat\n\
     /loop [profile|off] — agent-loop profile for this chat\n\
     /new — start a fresh conversation\n\
     /stop — cancel the running task\n\
     /status — current model/mode/profile and run state\n\
     /help — this message\n\n\
     Anything else is sent to the agent as a chat message."
        .to_string()
}

async fn list_agents_text() -> String {
    let dir = data_dir().join("agents");
    let mut names: Vec<String> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".yaml") || name.ends_with(".yml") {
                names.push(name);
            }
        }
    }
    names.sort();
    if names.is_empty() {
        return "No agent team configs found. Create one in Settings or let fully_auto/router mode generate a team.".to_string();
    }
    let mut lines = vec![format!("Agent team configs ({}):", names.len())];
    for name in names {
        match crate::server::services::toolbox::load_agent_yaml(&name) {
            Some((yaml, ids)) => {
                let team = yaml
                    .get("system")
                    .and_then(|s| s.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if team.is_empty() {
                    lines.push(format!("• {} — {} agents", name, ids.len()));
                } else {
                    lines.push(format!("• {} — \"{}\", {} agents", name, team, ids.len()));
                }
            }
            None => lines.push(format!("• {} (unreadable)", name)),
        }
    }
    lines.push("\nUse /mode auto (or manual) to run with a team from Settings.".to_string());
    lines.join("\n")
}

async fn list_loop_profiles_text(current: Option<&str>) -> String {
    let dir = crate::server::services::agent_loop::agent_loops_dir();
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".yaml") || name.ends_with(".yml") {
                names.push(name);
            }
        }
    }
    names.sort();
    let mut lines = vec![format!(
        "Agent-loop profile for this chat: {}",
        current.unwrap_or("(settings default)")
    )];
    if names.is_empty() {
        lines.push("No profiles found in data/agent_loops/.".to_string());
    } else {
        lines.push("Available:".to_string());
        for n in &names {
            lines.push(format!("• {}", n));
        }
        lines.push("\nUse /loop <profile> to set, /loop off to clear.".to_string());
    }
    lines.join("\n")
}

pub async fn execute_command(chat_key: &str, cmd: BotCommand) -> CommandOutcome {
    use CommandOutcome::*;
    match cmd {
        BotCommand::Chat(text) => RunChat(text),
        BotCommand::Help => Reply(help_text()),
        BotCommand::Unknown(c) => Reply(format!("Unknown command {} — send /help for the list.", c)),
        BotCommand::Agents => Reply(list_agents_text().await),
        BotCommand::Model(None) => {
            let s = get_settings().await;
            let mut lines = vec![format!("Current model: {}", s.tiger_bot_model)];
            if let Some(pool) = s.model_pool.as_ref().filter(|p| !p.is_empty()) {
                lines.push("Model pool:".to_string());
                for e in pool {
                    lines.push(format!("• {} ({})", e.label, e.model));
                }
            }
            lines.push("\nUse /model <model-id> to switch — applies to ALL sessions.".to_string());
            Reply(lines.join("\n"))
        }
        BotCommand::Model(Some(m)) => {
            let mut s = get_settings().await;
            s.tiger_bot_model = m.clone();
            save_settings(&s).await;
            Reply(format!("Model set to {} (applies to all sessions).", m))
        }
        BotCommand::Mode(None) => {
            let cs = get_or_create_chat(chat_key).await;
            Reply(format!(
                "Sub-agent mode for this chat: {}\nValid: {}\nUse /mode <mode> to switch.",
                cs.mode.as_deref().unwrap_or("single"),
                SUB_AGENT_MODES.join(", ")
            ))
        }
        BotCommand::Mode(Some(m)) => {
            let m = m.to_ascii_lowercase();
            if !SUB_AGENT_MODES.contains(&m.as_str()) {
                return Reply(format!(
                    "Unknown mode '{}'. Valid: {}",
                    m,
                    SUB_AGENT_MODES.join(", ")
                ));
            }
            get_or_create_chat(chat_key).await;
            {
                let mut st = state().lock().await;
                if let Some(cs) = st.chats.get_mut(chat_key) {
                    cs.mode = if m == "single" { None } else { Some(m.clone()) };
                }
            }
            persist_chats().await;
            Reply(format!("Sub-agent mode for this chat set to {}.", m))
        }
        BotCommand::Loop(None) => {
            let cs = get_or_create_chat(chat_key).await;
            Reply(list_loop_profiles_text(cs.loop_profile.as_deref()).await)
        }
        BotCommand::Loop(Some(name)) => {
            if name.eq_ignore_ascii_case("off") {
                get_or_create_chat(chat_key).await;
                {
                    let mut st = state().lock().await;
                    if let Some(cs) = st.chats.get_mut(chat_key) {
                        cs.loop_profile = None;
                    }
                }
                persist_chats().await;
                return Reply("Agent-loop profile cleared — using the settings default.".to_string());
            }
            if crate::server::services::agent_loop::load_profile(&name).is_none() {
                return Reply(format!(
                    "Profile '{}' not found or invalid. Send /loop to list available profiles.",
                    name
                ));
            }
            get_or_create_chat(chat_key).await;
            {
                let mut st = state().lock().await;
                if let Some(cs) = st.chats.get_mut(chat_key) {
                    cs.loop_profile = Some(name.clone());
                }
            }
            persist_chats().await;
            Reply(format!("Agent-loop profile for this chat set to {}.", name))
        }
        BotCommand::New => {
            get_or_create_chat(chat_key).await;
            let new_id = format!(
                "{}_{}",
                default_session_id(chat_key),
                chrono::Utc::now().timestamp()
            );
            {
                let mut st = state().lock().await;
                if let Some(cs) = st.chats.get_mut(chat_key) {
                    cs.session_id = new_id.clone();
                }
            }
            persist_chats().await;
            Reply("Started a fresh session. Previous conversation is kept in the web UI history.".to_string())
        }
        BotCommand::Stop => {
            let cs = get_or_create_chat(chat_key).await;
            let running_session = {
                let mut st = state().lock().await;
                st.running.remove(chat_key)
            };
            let target = running_session.unwrap_or(cs.session_id);
            let killed = chat::kill_session_by_id(&target).await;
            Reply(if killed {
                "🛑 Stopped the running task (its processes were terminated).".to_string()
            } else {
                "Nothing is running for this chat.".to_string()
            })
        }
        BotCommand::Status => {
            let s = get_settings().await;
            let cs = get_or_create_chat(chat_key).await;
            let running = state().lock().await.running.contains_key(chat_key);
            let loop_display = cs
                .loop_profile
                .clone()
                .or(s.agent_loop_profile.clone().filter(|p| !p.is_empty()))
                .unwrap_or_else(|| "(built-in)".to_string());
            Reply(format!(
                "Model: {} (global)\nMode: {} (this chat)\nLoop profile: {}\nSession: {}\nRunning: {}",
                s.tiger_bot_model,
                cs.mode.as_deref().unwrap_or("single"),
                loop_display,
                cs.session_id,
                if running { "yes — /stop to cancel" } else { "no" }
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Agent-run bridge
// ---------------------------------------------------------------------------

/// Coalesced events a transport renders while a run is in flight.
pub enum ProgressEvent {
    /// Throttled one-line status ("🔧 web_search → run_python").
    Status(String),
    /// A tool needs user approval — render approve/deny buttons.
    ApprovalNeeded { name: String, preview: String },
}

pub struct RunEvents {
    pub progress_rx: mpsc::UnboundedReceiver<ProgressEvent>,
    /// Resolves with the final assistant text (after persistence).
    pub done_rx: oneshot::Receiver<String>,
}

/// Start an agent run for a chat message. Returns Err(reply_text) when the
/// run cannot start (busy, or a startup error already persisted to the
/// session) — the transport should just send that text.
pub async fn start_run(chat_key: &str, text: &str) -> Result<RunEvents, String> {
    let cs = get_or_create_chat(chat_key).await;

    {
        let mut st = state().lock().await;
        if st.running.contains_key(chat_key) {
            return Err(
                "⏳ Still working on the previous message — send /stop to cancel it first."
                    .to_string(),
            );
        }
        st.running
            .insert(chat_key.to_string(), cs.session_id.clone());
    }

    let title = if chat_key.starts_with("line:") {
        "LINE Chat".to_string()
    } else {
        "Telegram Chat".to_string()
    };

    let (raw_tx, raw_rx) = mpsc::unbounded_channel::<ToolUpdate>();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel::<ProgressEvent>();
    let (inner_done_tx, inner_done_rx) = oneshot::channel::<String>();
    let (outer_done_tx, outer_done_rx) = oneshot::channel::<String>();

    spawn_progress_pump(raw_rx, evt_tx);

    // Completion wrapper: clears the busy marker, enforces the run timeout.
    let ck = chat_key.to_string();
    tokio::spawn(async move {
        let content = match tokio::time::timeout(RUN_TIMEOUT, inner_done_rx).await {
            Ok(Ok(c)) => c,
            Ok(Err(_)) => "⚠️ The run ended without producing a result.".to_string(),
            Err(_) => "⚠️ The task did not finish within 30 minutes. Send /stop to kill it, or /status to check.".to_string(),
        };
        state().lock().await.running.remove(&ck);
        let _ = outer_done_tx.send(content);
    });

    let req = AgentRunRequest {
        session_id: cs.session_id.clone(),
        message: text.to_string(),
        session_title: Some(title),
        agent_mode: cs.mode.clone(),
        agent_loop_profile: cs.loop_profile.clone(),
        graph_profile: None,
        config_file: None,
        project_id: None,
    };
    let cb: Arc<dyn Fn(ToolUpdate) + Send + Sync> = Arc::new(move |u| {
        let _ = raw_tx.send(u);
    });

    if let Err(e) = chat::start_agent_run(req, Some(cb), Some(inner_done_tx)).await {
        state().lock().await.running.remove(chat_key);
        return Err(e);
    }

    Ok(RunEvents {
        progress_rx: evt_rx,
        done_rx: outer_done_rx,
    })
}

/// Drains raw ToolUpdates into coalesced, throttled ProgressEvents.
/// Approval requests pass through immediately; tool-call activity is batched
/// into one status line emitted at most every PROGRESS_THROTTLE.
fn spawn_progress_pump(
    mut raw_rx: mpsc::UnboundedReceiver<ToolUpdate>,
    evt_tx: mpsc::UnboundedSender<ProgressEvent>,
) {
    tokio::spawn(async move {
        let mut last_emit = Instant::now() - PROGRESS_THROTTLE;
        let mut pending: Option<String> = None;
        let mut recent_tools: Vec<String> = Vec::new();
        loop {
            let update = if pending.is_some() {
                // Flush the pending status once the throttle window elapses
                // even when no new updates arrive.
                let wait = PROGRESS_THROTTLE.saturating_sub(last_emit.elapsed());
                match tokio::time::timeout(wait, raw_rx.recv()).await {
                    Ok(u) => u,
                    Err(_) => {
                        if let Some(s) = pending.take() {
                            let _ = evt_tx.send(ProgressEvent::Status(s));
                            last_emit = Instant::now();
                        }
                        continue;
                    }
                }
            } else {
                raw_rx.recv().await
            };

            let Some(update) = update else {
                // Run finished — channel closed. Drop any pending status;
                // the final answer supersedes it.
                break;
            };

            match update {
                ToolUpdate::ApprovalRequired { name, args } => {
                    let preview = truncate_utf8_ellipsis(&args.to_string(), 300);
                    let _ = evt_tx.send(ProgressEvent::ApprovalNeeded { name, preview });
                }
                ToolUpdate::ToolCall { name, .. } => {
                    if recent_tools.last() != Some(&name) {
                        recent_tools.push(name);
                    }
                    if recent_tools.len() > 4 {
                        recent_tools.remove(0);
                    }
                    pending = Some(format!("🔧 {}", recent_tools.join(" → ")));
                }
                ToolUpdate::Error(e) => {
                    pending = Some(format!("❌ {}", truncate_utf8_ellipsis(&e, 200)));
                }
                ToolUpdate::ToolResult { .. } | ToolUpdate::TextChunk(_) => {}
            }

            if pending.is_some() && last_emit.elapsed() >= PROGRESS_THROTTLE {
                let _ = evt_tx.send(ProgressEvent::Status(pending.take().unwrap()));
                last_emit = Instant::now();
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Message splitting (UTF-8 safe — Thai text must never be byte-sliced)
// ---------------------------------------------------------------------------

/// Split `text` into chunks of at most `max_bytes` bytes, preferring newline
/// then space boundaries, always cutting on UTF-8 char boundaries.
pub fn split_message(text: &str, max_bytes: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        if rest.len() <= max_bytes {
            out.push(rest.to_string());
            break;
        }
        let cut = crate::util::floor_char_boundary(rest, max_bytes);
        let window = &rest[..cut];
        // Prefer a natural boundary, but not so early the chunk is tiny.
        let split_at = window
            .rfind('\n')
            .or_else(|| window.rfind(' '))
            .filter(|&i| i > max_bytes / 2)
            .map(|i| i + 1)
            .unwrap_or(cut);
        let split_at = split_at.max(1); // guarantee progress
        out.push(rest[..split_at].trim_end().to_string());
        rest = rest[split_at..].trim_start();
    }
    out.retain(|c| !c.is_empty());
    out
}

// ---------------------------------------------------------------------------
// Allow-list helper shared by both transports
// ---------------------------------------------------------------------------

/// Fail closed: an empty/unset allow-list rejects everyone.
pub fn user_allowed(allow_list: Option<&Vec<String>>, user_id: &str) -> bool {
    allow_list
        .map(|l| l.iter().any(|s| s.trim() == user_id))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commands() {
        assert_eq!(parse_command("/agents"), BotCommand::Agents);
        assert_eq!(parse_command("/model"), BotCommand::Model(None));
        assert_eq!(
            parse_command("/model deepseek-chat"),
            BotCommand::Model(Some("deepseek-chat".to_string()))
        );
        assert_eq!(
            parse_command("/model@TigrimBot deepseek-chat"),
            BotCommand::Model(Some("deepseek-chat".to_string()))
        );
        assert_eq!(parse_command("/MODE router"), BotCommand::Mode(Some("router".to_string())));
        assert_eq!(parse_command("/start"), BotCommand::Help);
        assert_eq!(
            parse_command("hello there"),
            BotCommand::Chat("hello there".to_string())
        );
        assert_eq!(
            parse_command("/frobnicate now"),
            BotCommand::Unknown("/frobnicate".to_string())
        );
    }

    #[test]
    fn split_message_thai_never_panics() {
        let thai = "สวัสดีครับ ผลการวิเคราะห์ข้อมูลเสร็จสมบูรณ์แล้ว และนี่คือรายละเอียดเพิ่มเติม\n".repeat(100);
        for max in [50, 100, 4000] {
            let chunks = split_message(&thai, max);
            assert!(!chunks.is_empty());
            for c in &chunks {
                assert!(c.len() <= max, "chunk {} bytes > {}", c.len(), max);
                assert!(!c.is_empty());
                // Valid UTF-8 by construction (String), but re-check boundaries:
                assert!(std::str::from_utf8(c.as_bytes()).is_ok());
            }
        }
    }

    #[test]
    fn split_message_prefers_newlines() {
        let text = format!("{}\n{}", "a".repeat(30), "b".repeat(30));
        let chunks = split_message(&text, 40);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "a".repeat(30));
        assert_eq!(chunks[1], "b".repeat(30));
    }

    #[test]
    fn split_message_short_passthrough() {
        assert_eq!(split_message("hello", 4000), vec!["hello".to_string()]);
        assert!(split_message("   ", 4000).is_empty());
    }

    #[test]
    fn allow_list_fails_closed() {
        assert!(!user_allowed(None, "123"));
        assert!(!user_allowed(Some(&vec![]), "123"));
        assert!(user_allowed(Some(&vec!["123".to_string()]), "123"));
        assert!(user_allowed(Some(&vec![" 123 ".to_string()]), "123"));
        assert!(!user_allowed(Some(&vec!["456".to_string()]), "123"));
    }
}
