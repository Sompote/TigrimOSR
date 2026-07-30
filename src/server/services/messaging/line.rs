//! LINE Messaging API transport.
//!
//! Inbound events arrive on the (signature-verified) webhook in
//! routes/messaging.rs, which calls handle_events(). Outbound uses the reply
//! token where possible (free, single-use, ~1 min expiry) and push messages
//! for everything after — progress is limited to ONE push per run because
//! push messages count against LINE's monthly quota (~200 on the free plan).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use super::{parse_command, split_message, start_run, user_allowed, CommandOutcome, ProgressEvent};
use crate::server::data::{get_settings, remote_client};

/// LINE text-message limit is 5000 chars; 4900 bytes is always under it.
const LINE_CHUNK_BYTES: usize = 4900;
/// Max text objects per reply/push call.
const MAX_MSGS_PER_CALL: usize = 5;

// ---------------------------------------------------------------------------
// Webhook event dedupe (LINE retries deliveries on slow/failed responses)
// ---------------------------------------------------------------------------

static SEEN_EVENTS: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn seen_before(event_id: &str) -> bool {
    if event_id.is_empty() {
        return false;
    }
    let mut seen = SEEN_EVENTS
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .unwrap();
    if seen.iter().any(|e| e == event_id) {
        return true;
    }
    seen.push_back(event_id.to_string());
    if seen.len() > 256 {
        seen.pop_front();
    }
    false
}

// ---------------------------------------------------------------------------
// Messaging API client
// ---------------------------------------------------------------------------

async fn line_api(access_token: &str, endpoint: &str, body: Value) -> Result<(), String> {
    let resp = remote_client()
        .post(format!("https://api.line.me/v2/bot/message/{}", endpoint))
        .bearer_auth(access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("network: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Err(format!("HTTP {}: {}", status, crate::util::truncate_utf8(&text, 300)))
    }
}

fn text_objects(text: &str) -> Vec<Value> {
    split_message(text, LINE_CHUNK_BYTES)
        .into_iter()
        .map(|c| json!({ "type": "text", "text": c }))
        .collect()
}

/// Reply with the (single-use) reply token. Falls back to push when the text
/// needs more than one call's worth of chunks.
async fn reply_text(access_token: &str, reply_token: &str, user_id: &str, text: &str) {
    let mut objs = text_objects(text);
    if objs.is_empty() {
        return;
    }
    let rest = if objs.len() > MAX_MSGS_PER_CALL {
        objs.split_off(MAX_MSGS_PER_CALL)
    } else {
        Vec::new()
    };
    if let Err(e) = line_api(
        access_token,
        "reply",
        json!({ "replyToken": reply_token, "messages": objs }),
    )
    .await
    {
        tracing::warn!("[line] reply failed: {}", e);
        // Reply token may have expired — push instead so the user still hears back.
        push_objects(access_token, user_id, text_objects(text)).await;
        return;
    }
    push_objects(access_token, user_id, rest).await;
}

async fn push_text(access_token: &str, user_id: &str, text: &str) {
    push_objects(access_token, user_id, text_objects(text)).await;
}

async fn push_objects(access_token: &str, user_id: &str, objs: Vec<Value>) {
    for batch in objs.chunks(MAX_MSGS_PER_CALL) {
        if let Err(e) = line_api(
            access_token,
            "push",
            json!({ "to": user_id, "messages": batch }),
        )
        .await
        {
            tracing::warn!("[line] push failed: {}", e);
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Event handling (spawned from the webhook handler after signature check)
// ---------------------------------------------------------------------------

pub async fn handle_events(payload: Value) {
    let settings = get_settings().await;
    if settings.line_enabled != Some(true) {
        return;
    }
    let access_token = settings
        .line_channel_access_token
        .clone()
        .unwrap_or_default();
    if access_token.is_empty() {
        tracing::warn!("[line] event received but lineChannelAccessToken is not set");
        return;
    }

    let events = payload
        .get("events")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();

    for ev in events {
        let event_id = ev
            .get("webhookEventId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if seen_before(event_id) {
            continue;
        }
        // 1:1 user chats only — group/room support would need per-room keys.
        if ev.pointer("/source/type").and_then(|v| v.as_str()) != Some("user") {
            continue;
        }
        let Some(user_id) = ev
            .pointer("/source/userId")
            .and_then(|v| v.as_str())
            .map(String::from)
        else {
            continue;
        };
        let reply_token = ev
            .get("replyToken")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !user_allowed(settings.line_allowed_user_ids.as_ref(), &user_id) {
            if !reply_token.is_empty() {
                reply_text(
                    &access_token,
                    &reply_token,
                    &user_id,
                    &format!(
                        "Unauthorized. Your LINE user id is {} — add it to lineAllowedUserIds in AndrewOS Settings.",
                        user_id
                    ),
                )
                .await;
            }
            continue;
        }

        match ev.get("type").and_then(|v| v.as_str()) {
            Some("message") if ev.pointer("/message/type").and_then(|v| v.as_str()) == Some("text") => {
                let Some(text) = ev
                    .pointer("/message/text")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                else {
                    continue;
                };
                let tok = access_token.clone();
                tokio::spawn(async move {
                    handle_text(&tok, &user_id, &reply_token, &text).await;
                });
            }
            Some("postback") => {
                let data = ev
                    .pointer("/postback/data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if data == "appr:1" || data == "appr:0" {
                    let approved = data == "appr:1";
                    crate::server::services::toolbox::respond_tool_approval(approved).await;
                    reply_text(
                        &access_token,
                        &reply_token,
                        &user_id,
                        if approved { "✅ Tool approved" } else { "❌ Tool denied" },
                    )
                    .await;
                }
            }
            _ => {}
        }
    }
}

async fn handle_text(access_token: &str, user_id: &str, reply_token: &str, text: &str) {
    let chat_key = format!("line:{}", user_id);
    match super::execute_command(&chat_key, parse_command(text)).await {
        CommandOutcome::Reply(reply) => {
            reply_text(access_token, reply_token, user_id, &reply).await;
        }
        CommandOutcome::RunChat(msg) => {
            run_and_stream(access_token, user_id, reply_token, &chat_key, &msg).await;
        }
    }
}

/// Drive one agent run over LINE. The "Working…" ack uses the free reply
/// token; at most ONE progress push per run (push quota); the final answer is
/// always pushed.
async fn run_and_stream(access_token: &str, user_id: &str, reply_token: &str, chat_key: &str, msg: &str) {
    let events = match start_run(chat_key, msg).await {
        Ok(ev) => ev,
        Err(reply) => {
            reply_text(access_token, reply_token, user_id, &reply).await;
            return;
        }
    };

    reply_text(
        access_token,
        reply_token,
        user_id,
        "🤖 Working… I'll send the answer when done.",
    )
    .await;

    let pump = {
        let access_token = access_token.to_string();
        let user_id = user_id.to_string();
        let mut progress_rx = events.progress_rx;
        tokio::spawn(async move {
            let mut progress_sent = false;
            while let Some(ev) = progress_rx.recv().await {
                match ev {
                    ProgressEvent::Status(s) => {
                        if !progress_sent {
                            push_text(&access_token, &user_id, &s).await;
                            progress_sent = true;
                        }
                    }
                    ProgressEvent::ApprovalNeeded { name, preview } => {
                        send_approval_confirm(&access_token, &user_id, &name, &preview).await;
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

    push_text(access_token, user_id, &content).await;
}

// ---------------------------------------------------------------------------
// Approvals: confirm template with postback actions (unlike quick replies,
// the buttons survive if the user sends other messages first)
// ---------------------------------------------------------------------------

async fn send_approval_confirm(access_token: &str, user_id: &str, name: &str, preview: &str) {
    // Confirm-template text is capped at 240 chars — keep the preview short.
    let text = crate::util::truncate_utf8_ellipsis(
        &format!("⚠️ Approve tool {}?\n{}", name, preview),
        200,
    );
    let body = json!({
        "to": user_id,
        "messages": [{
            "type": "template",
            "altText": format!("Approve tool {}?", name),
            "template": {
                "type": "confirm",
                "text": text,
                "actions": [
                    { "type": "postback", "label": "Approve", "data": "appr:1" },
                    { "type": "postback", "label": "Deny", "data": "appr:0" },
                ]
            }
        }]
    });
    if let Err(e) = line_api(access_token, "push", body).await {
        tracing::warn!("[line] approval confirm failed: {}", e);
    }
}
