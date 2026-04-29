use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{delete, get, patch, post},
    Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server::data::*;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Log directories
// ---------------------------------------------------------------------------

fn activity_log_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("data").join("activity_logs")
}

fn chat_log_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("data").join("chat_logs")
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sessions", get(list_sessions).post(create_session))
        .route(
            "/sessions/{id}",
            get(get_session).delete(delete_session).patch(rename_session),
        )
        .route("/sessions/{id}/messages", post(send_message))
        .route(
            "/sessions/{id}/messages/{index}/feedback",
            post(message_feedback),
        )
        .route("/sessions/{id}/activity", get(get_activity_log))
        .route("/sessions/{id}/chatlog", get(get_chat_log))
}

// ---------------------------------------------------------------------------
// GET /sessions - list all chat sessions
// ---------------------------------------------------------------------------

async fn list_sessions() -> impl IntoResponse {
    let sessions = get_chat_history().await;
    let summaries: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "createdAt": s.created_at,
                "updatedAt": s.updated_at,
                "messageCount": s.messages.len(),
            })
        })
        .collect();
    Json(json!(summaries))
}

// ---------------------------------------------------------------------------
// GET /sessions/:id - get a single session
// ---------------------------------------------------------------------------

async fn get_session(Path(id): Path<String>) -> impl IntoResponse {
    let sessions = get_chat_history().await;
    let session = sessions.iter().find(|s| s.id == id);
    match session {
        Some(s) => {
            let mut val = serde_json::to_value(s).unwrap_or(json!({}));

            // Include auto-created architecture info if present
            let auto_arch_filename = get_auto_created_architecture(&id);
            if let Some(filename) = auto_arch_filename {
                let file_path =
                    std::path::PathBuf::from("data").join("agents").join(&filename);
                let system_name = if let Ok(content) =
                    tokio::fs::read_to_string(&file_path).await
                {
                    // Try to parse YAML and extract system.name
                    serde_yaml::from_str::<Value>(&content)
                        .ok()
                        .and_then(|parsed| {
                            parsed
                                .get("system")
                                .and_then(|sys| sys.get("name"))
                                .and_then(|n| n.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| {
                            filename
                                .trim_end_matches(".yml")
                                .trim_end_matches(".yaml")
                                .to_string()
                        })
                } else {
                    filename
                        .trim_end_matches(".yml")
                        .trim_end_matches(".yaml")
                        .to_string()
                };

                if let Some(obj) = val.as_object_mut() {
                    obj.insert(
                        "autoCreatedArch".to_string(),
                        json!({
                            "filename": filename,
                            "systemName": system_name,
                        }),
                    );
                }
            }

            (StatusCode::OK, Json(val)).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Session not found"})),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /sessions - create a new session
// ---------------------------------------------------------------------------

async fn create_session(Json(body): Json<Value>) -> impl IntoResponse {
    let mut sessions = get_chat_history().await;
    let title = body
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("New Chat")
        .to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let session = ChatSession {
        id: Uuid::new_v4().to_string(),
        title,
        messages: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
        skill_candidate: None,
        skill_feedback: None,
        project_id: None,
    };
    sessions.push(session.clone());
    save_chat_history(&sessions).await;
    Json(serde_json::to_value(&session).unwrap_or(json!({})))
}

// ---------------------------------------------------------------------------
// DELETE /sessions/:id - delete a session
// ---------------------------------------------------------------------------

async fn delete_session(Path(id): Path<String>) -> impl IntoResponse {
    let sessions = get_chat_history().await;
    let filtered: Vec<ChatSession> = sessions.into_iter().filter(|s| s.id != id).collect();
    save_chat_history(&filtered).await;
    // Clean up agent history folder for this session
    delete_agent_history(&id).await;
    Json(json!({"success": true}))
}

// ---------------------------------------------------------------------------
// PATCH /sessions/:id - rename a session
// ---------------------------------------------------------------------------

async fn rename_session(
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut sessions = get_chat_history().await;
    let session = sessions.iter_mut().find(|s| s.id == id);
    match session {
        Some(s) => {
            if let Some(title) = body.get("title").and_then(|v| v.as_str()) {
                s.title = title.to_string();
            }
            let updated = s.clone();
            save_chat_history(&sessions).await;
            (StatusCode::OK, Json(serde_json::to_value(&updated).unwrap_or(json!({})))).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Session not found"})),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /sessions/:id/messages/:index/feedback - save feedback on a message
// ---------------------------------------------------------------------------

async fn message_feedback(
    Path((session_id, index_str)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let index: usize = match index_str.parse() {
        Ok(i) => i,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "Invalid message index"})),
            )
                .into_response();
        }
    };

    let mut sessions = get_chat_history().await;
    let session = sessions.iter_mut().find(|s| s.id == session_id);
    let session = match session {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "error": "Session not found"})),
            )
                .into_response();
        }
    };

    if index >= session.messages.len() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Invalid message index"})),
        )
            .into_response();
    }

    let rating = body
        .get("rating")
        .and_then(|v| v.as_str())
        .filter(|r| *r == "up" || *r == "down")
        .map(|s| s.to_string());
    let comment = body
        .get("comment")
        .and_then(|v| v.as_str())
        .map(|s| s.chars().take(4000).collect::<String>());
    let clear = body
        .get("clear")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if rating.is_none() && comment.is_none() && !clear {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Provide rating, comment, or clear=true"})),
        )
            .into_response();
    }

    let msg = &mut session.messages[index];
    if clear {
        msg.feedback = None;
    } else {
        let existing = msg.feedback.take().unwrap_or(ChatMessageFeedback {
            rating: None,
            comment: None,
            submitted_at: None,
        });
        msg.feedback = Some(ChatMessageFeedback {
            rating: rating.or(existing.rating),
            comment: comment.or(existing.comment),
            submitted_at: Some(chrono::Utc::now().to_rfc3339()),
        });
    }

    session.updated_at = chrono::Utc::now().to_rfc3339();
    let feedback = msg.feedback.clone();
    save_chat_history(&sessions).await;

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "feedback": feedback,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST /sessions/:id/messages - send a message
// ---------------------------------------------------------------------------

async fn send_message(
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut sessions = get_chat_history().await;
    let session = sessions.iter_mut().find(|s| s.id == id);
    let session = match session {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Session not found"})),
            )
                .into_response();
        }
    };

    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let now = chrono::Utc::now().to_rfc3339();

    // Save user message
    session.messages.push(ChatMessage {
        role: "user".to_string(),
        content: message,
        timestamp: now.clone(),
        files: None,
        feedback: None,
    });

    // Placeholder assistant response (tigerbot service will be added later)
    let assistant_content =
        "[Placeholder] TigerBot service not yet connected.".to_string();
    session.messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: assistant_content.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        files: None,
        feedback: None,
    });

    session.updated_at = chrono::Utc::now().to_rfc3339();
    save_chat_history(&sessions).await;

    (
        StatusCode::OK,
        Json(json!({
            "content": assistant_content,
            "usage": null,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /sessions/:id/activity - get activity log for a session
// ---------------------------------------------------------------------------

async fn get_activity_log(Path(id): Path<String>) -> impl IntoResponse {
    let log_path = activity_log_dir().join(format!("{}.log", id));
    let content = tokio::fs::read_to_string(&log_path)
        .await
        .unwrap_or_default();
    Json(json!({"ok": true, "content": content}))
}

// ---------------------------------------------------------------------------
// GET /sessions/:id/chatlog - get chat log for a session
// ---------------------------------------------------------------------------

async fn get_chat_log(Path(id): Path<String>) -> impl IntoResponse {
    let log_path = chat_log_dir().join(format!("{}.log", id));
    let content = tokio::fs::read_to_string(&log_path)
        .await
        .unwrap_or_default();
    Json(json!({"ok": true, "content": content}))
}

// ---------------------------------------------------------------------------
// Helper: check if a session has an auto-created architecture file
// ---------------------------------------------------------------------------

fn get_auto_created_architecture(session_id: &str) -> Option<String> {
    // Look for a YAML file in data/agents that matches the session ID
    let agents_dir = std::path::PathBuf::from("data").join("agents");
    let pattern = format!("{}.yml", session_id);
    let pattern_yaml = format!("{}.yaml", session_id);

    if agents_dir.join(&pattern).exists() {
        Some(pattern)
    } else if agents_dir.join(&pattern_yaml).exists() {
        Some(pattern_yaml)
    } else {
        None
    }
}
