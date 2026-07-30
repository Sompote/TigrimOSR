//! Telegram transport: long-polling supervisor + Bot API client.
//!
//! No public URL required — getUpdates long-polling reaches api.telegram.org
//! outbound. The supervisor re-reads settings every poll cycle, so enabling/
//! disabling the bot or changing the token applies without a server restart.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Mutex as TokioMutex;

use super::{parse_command, split_message, start_run, user_allowed, CommandOutcome, ProgressEvent};
use crate::server::data::get_settings;

/// Telegram's limit is 4096 UTF-16 code units; 4000 *bytes* is always under it.
const TG_CHUNK_BYTES: usize = 4000;
/// Long-poll wait passed to getUpdates.
const POLL_TIMEOUT_SECS: u64 = 50;
/// Ignore backlog messages older than this on the first poll after boot.
const STALE_SECS: i64 = 120;

// ---------------------------------------------------------------------------
// Status (surfaced via GET /api/messaging/status)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct TelegramStatus {
    pub connected: bool,
    pub bot_username: Option<String>,
    pub error: Option<String>,
}

static TG_STATUS: OnceLock<TokioMutex<TelegramStatus>> = OnceLock::new();

fn status_slot() -> &'static TokioMutex<TelegramStatus> {
    TG_STATUS.get_or_init(|| TokioMutex::new(TelegramStatus::default()))
}

pub async fn get_status() -> TelegramStatus {
    status_slot().lock().await.clone()
}

async fn set_status(connected: bool, bot_username: Option<String>, error: Option<String>) {
    let mut s = status_slot().lock().await;
    s.connected = connected;
    s.bot_username = bot_username;
    s.error = error;
}

// ---------------------------------------------------------------------------
// Bot API client
// ---------------------------------------------------------------------------

/// Client for sends (30s timeout is fine).
fn send_client() -> reqwest::Client {
    crate::server::data::remote_client()
}

/// Client for getUpdates — must outlive the 50s long-poll.
fn poll_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(POLL_TIMEOUT_SECS + 20))
        .build()
        .unwrap_or_default()
}

async fn tg_call(client: &reqwest::Client, token: &str, method: &str, body: Value) -> Result<Value, String> {
    let url = format!("https://api.telegram.org/bot{}/{}", token, method);
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("network: {}", e))?;
    let status = resp.status().as_u16();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("bad response: {}", e))?;
    if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        let desc = v
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("unknown error");
        Err(format!("HTTP {}: {}", status, desc))
    }
}

async fn send_text(client: &reqwest::Client, token: &str, chat_id: i64, text: &str) -> Option<i64> {
    // Plain text (no parse_mode) — immune to MarkdownV2 escaping issues.
    let result = tg_call(
        client,
        token,
        "sendMessage",
        json!({ "chat_id": chat_id, "text": text }),
    )
    .await;
    match result {
        Ok(v) => v.get("message_id").and_then(|m| m.as_i64()),
        Err(e) => {
            tracing::warn!("[telegram] sendMessage failed: {}", e);
            None
        }
    }
}

async fn edit_text(client: &reqwest::Client, token: &str, chat_id: i64, message_id: i64, text: &str) {
    let r = tg_call(
        client,
        token,
        "editMessageText",
        json!({ "chat_id": chat_id, "message_id": message_id, "text": text }),
    )
    .await;
    // "message is not modified" is expected when the status line repeats.
    if let Err(e) = r {
        if !e.contains("not modified") {
            tracing::debug!("[telegram] editMessageText: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Supervisor + poll loop
// ---------------------------------------------------------------------------

/// Spawned once from messaging::init(). Never returns.
pub fn init() {
    tokio::spawn(async {
        loop {
            let s = get_settings().await;
            let enabled = s.telegram_enabled == Some(true);
            let token = s
                .telegram_bot_token
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string();
            if enabled && !token.is_empty() {
                poll_generation(&token).await;
                // Returns when settings changed or on auth failure (after its
                // own backoff) — loop around and re-read settings.
            } else {
                set_status(false, None, None).await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    });
}

/// One polling "generation" for a fixed token. Returns when the token/enabled
/// settings change or when the token is rejected (after a 60s backoff).
async fn poll_generation(token: &str) {
    let poller = poll_client();
    let sender = send_client();

    match tg_call(&sender, token, "getMe", json!({})).await {
        Ok(me) => {
            let username = me
                .get("username")
                .and_then(|u| u.as_str())
                .map(|u| format!("@{}", u));
            tracing::info!("[telegram] connected as {}", username.as_deref().unwrap_or("?"));
            set_status(true, username, None).await;
        }
        Err(e) => {
            tracing::warn!("[telegram] getMe failed: {}", e);
            set_status(false, None, Some(format!("getMe failed: {}", e))).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
            return;
        }
    }

    let boot_ts = chrono::Utc::now().timestamp();
    let mut first_batch = true;
    let mut offset: i64 = 0;
    let mut backoff: u64 = 1;

    loop {
        // Apply settings changes without restart: bail out of this generation
        // when the token changed or the bot was disabled.
        let s = get_settings().await;
        if s.telegram_enabled != Some(true)
            || s.telegram_bot_token.as_deref().map(str::trim) != Some(token)
        {
            tracing::info!("[telegram] settings changed — restarting poll loop");
            set_status(false, None, None).await;
            return;
        }

        let body = json!({
            "timeout": POLL_TIMEOUT_SECS,
            "offset": offset,
            "allowed_updates": ["message", "callback_query"],
        });
        match tg_call(&poller, token, "getUpdates", body).await {
            Ok(result) => {
                backoff = 1;
                let updates = result.as_array().cloned().unwrap_or_default();
                for upd in updates {
                    if let Some(id) = upd.get("update_id").and_then(|v| v.as_i64()) {
                        offset = offset.max(id + 1);
                    }
                    if first_batch {
                        // Don't replay commands that queued up while offline.
                        let msg_date = upd
                            .pointer("/message/date")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(boot_ts);
                        if msg_date < boot_ts - STALE_SECS {
                            continue;
                        }
                    }
                    dispatch_update(&sender, token, &s, upd);
                }
                first_batch = false;
            }
            Err(e) => {
                if e.contains("HTTP 401") || e.contains("HTTP 404") {
                    tracing::warn!("[telegram] token rejected: {}", e);
                    set_status(false, None, Some(format!("token rejected: {}", e))).await;
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    return;
                }
                if e.contains("HTTP 409") {
                    tracing::warn!("[telegram] 409 conflict — another poller is running for this bot");
                    set_status(false, None, Some("409 conflict: another getUpdates poller is active for this bot token".to_string())).await;
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    continue;
                }
                tracing::debug!("[telegram] getUpdates error: {} (backoff {}s)", e, backoff);
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Update dispatch
// ---------------------------------------------------------------------------

fn dispatch_update(
    sender: &reqwest::Client,
    token: &str,
    settings: &crate::server::data::Settings,
    upd: Value,
) {
    let allow = settings.telegram_allowed_user_ids.clone();
    let token = token.to_string();
    let sender = sender.clone();

    if let Some(cq) = upd.get("callback_query").cloned() {
        tokio::spawn(async move {
            handle_callback_query(&sender, &token, allow.as_ref(), cq).await;
        });
        return;
    }

    let Some(msg) = upd.get("message").cloned() else {
        return;
    };
    let Some(text) = msg.pointer("/text").and_then(|t| t.as_str()).map(String::from) else {
        return; // ignore stickers/photos/etc.
    };
    let Some(chat_id) = msg.pointer("/chat/id").and_then(|v| v.as_i64()) else {
        return;
    };
    let from_id = msg.pointer("/from/id").and_then(|v| v.as_i64()).unwrap_or(0);

    tokio::spawn(async move {
        if !user_allowed(allow.as_ref(), &from_id.to_string()) {
            send_text(
                &sender,
                &token,
                chat_id,
                &format!(
                    "Unauthorized. Your Telegram user id is {} — add it to telegramAllowedUserIds in AndrewOS Settings.",
                    from_id
                ),
            )
            .await;
            return;
        }
        handle_text_message(&sender, &token, chat_id, &text).await;
    });
}

async fn handle_text_message(client: &reqwest::Client, token: &str, chat_id: i64, text: &str) {
    let chat_key = format!("tg:{}", chat_id);
    match super::execute_command(&chat_key, parse_command(text)).await {
        CommandOutcome::Reply(reply) => {
            for chunk in split_message(&reply, TG_CHUNK_BYTES) {
                send_text(client, token, chat_id, &chunk).await;
            }
        }
        CommandOutcome::RunChat(msg) => {
            run_and_stream(client, token, chat_id, &chat_key, &msg).await;
        }
    }
}

/// Drive one agent run: a single "Working…" message edited in place for
/// progress (sidesteps the ~1 msg/s per-chat rate limit), approval prompts as
/// separate messages with inline buttons, final answer as split messages.
async fn run_and_stream(client: &reqwest::Client, token: &str, chat_id: i64, chat_key: &str, msg: &str) {
    let events = match start_run(chat_key, msg).await {
        Ok(ev) => ev,
        Err(reply) => {
            send_text(client, token, chat_id, &reply).await;
            return;
        }
    };

    let progress_id = send_text(client, token, chat_id, "🤖 Working…").await;

    let pump = {
        let client = client.clone();
        let token = token.to_string();
        let mut progress_rx = events.progress_rx;
        tokio::spawn(async move {
            while let Some(ev) = progress_rx.recv().await {
                match ev {
                    ProgressEvent::Status(s) => {
                        if let Some(mid) = progress_id {
                            edit_text(&client, &token, chat_id, mid, &s).await;
                        }
                    }
                    ProgressEvent::ApprovalNeeded { name, preview } => {
                        send_approval_prompt(&client, &token, chat_id, &name, &preview).await;
                    }
                }
            }
        })
    };

    let content = events
        .done_rx
        .await
        .unwrap_or_else(|_| "⚠️ The run ended unexpectedly.".to_string());
    let _ = pump.await;

    if let Some(mid) = progress_id {
        edit_text(client, token, chat_id, mid, "✅ Done").await;
    }
    for chunk in split_message(&content, TG_CHUNK_BYTES) {
        send_text(client, token, chat_id, &chunk).await;
        // Stay under Telegram's per-chat rate limit on multi-part answers.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

// ---------------------------------------------------------------------------
// Approvals (inline keyboard + callback_query)
// ---------------------------------------------------------------------------

async fn send_approval_prompt(client: &reqwest::Client, token: &str, chat_id: i64, name: &str, preview: &str) {
    let body = json!({
        "chat_id": chat_id,
        "text": format!("⚠️ Approve tool {}?\n{}\n\n(Denied automatically after 120s.)", name, preview),
        "reply_markup": {
            "inline_keyboard": [[
                { "text": "✅ Approve", "callback_data": "appr:1" },
                { "text": "❌ Deny", "callback_data": "appr:0" },
            ]]
        }
    });
    if let Err(e) = tg_call(client, token, "sendMessage", body).await {
        tracing::warn!("[telegram] approval prompt failed: {}", e);
    }
}

async fn handle_callback_query(
    client: &reqwest::Client,
    token: &str,
    allow: Option<&Vec<String>>,
    cq: Value,
) {
    let cq_id = cq.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let from_id = cq.pointer("/from/id").and_then(|v| v.as_i64()).unwrap_or(0);
    let data = cq.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = cq.pointer("/message/chat/id").and_then(|v| v.as_i64());
    let message_id = cq.pointer("/message/message_id").and_then(|v| v.as_i64());

    if !user_allowed(allow, &from_id.to_string()) {
        let _ = tg_call(
            client,
            token,
            "answerCallbackQuery",
            json!({ "callback_query_id": cq_id, "text": "Unauthorized" }),
        )
        .await;
        return;
    }

    let approved = data == "appr:1";
    if data == "appr:1" || data == "appr:0" {
        crate::server::services::toolbox::respond_tool_approval(approved).await;
        let _ = tg_call(
            client,
            token,
            "answerCallbackQuery",
            json!({ "callback_query_id": cq_id, "text": if approved { "Approved" } else { "Denied" } }),
        )
        .await;
        // Replace the buttons so the prompt can't be tapped twice.
        if let (Some(cid), Some(mid)) = (chat_id, message_id) {
            edit_text(
                client,
                token,
                cid,
                mid,
                if approved { "✅ Tool approved" } else { "❌ Tool denied" },
            )
            .await;
        }
    } else {
        let _ = tg_call(
            client,
            token,
            "answerCallbackQuery",
            json!({ "callback_query_id": cq_id }),
        )
        .await;
    }
}
