use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Multipart, Path},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, patch, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server::data::*;
use crate::server::services::skill_synthesizer;
#[allow(unused_imports)]
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateSkillBody {
    name: Option<String>,
    description: Option<String>,
    source: Option<String>,
    script: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateSkillBody {
    name: Option<String>,
    description: Option<String>,
    source: Option<String>,
    script: Option<String>,
    enabled: Option<bool>,
    #[serde(rename = "reviewStatus")]
    review_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FeedbackBody {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "skillCandidate")]
    skill_candidate: Option<bool>,
    #[serde(rename = "skillFeedback")]
    skill_feedback: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse YAML frontmatter from a SKILL.md file and return (name, description).
fn parse_frontmatter(content: &str) -> (String, String) {
    // Look for --- delimited frontmatter block
    if !content.starts_with("---") {
        return (String::new(), String::new());
    }

    // Find the closing ---
    let rest = &content[3..];
    let end = match rest.find("\n---") {
        Some(pos) => pos,
        None => return (String::new(), String::new()),
    };

    let fm_text = &rest[..end];

    // Try serde_yaml first, fall back to simple parsing
    let mut name = String::new();
    let mut description = String::new();

    if let Ok(parsed) = serde_yaml::from_str::<serde_json::Value>(fm_text) {
        if let Some(n) = parsed.get("name").and_then(|v| v.as_str()) {
            name = n.to_string();
        }
        if let Some(d) = parsed.get("description").and_then(|v| v.as_str()) {
            description = d.to_string();
        }
    } else {
        // Fallback: simple key: value line parsing
        for line in fm_text.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("name:") {
                name = val.trim().trim_matches('"').trim_matches('\'').to_string();
            } else if let Some(val) = line.strip_prefix("description:") {
                description = val.trim().trim_matches('"').trim_matches('\'').to_string();
            }
        }
    }

    // Normalize whitespace
    let normalize = |s: String| -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    };

    (normalize(name), normalize(description))
}

/// Sanitize a name to a filesystem-safe slug.
fn slugify(name: &str) -> String {
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

/// Try to find a skill's SKILL.md file on disk.
fn find_skill_file(skill: &Skill) -> Option<PathBuf> {
    let slug = if skill.script.is_empty() {
        slugify(&skill.name)
    } else {
        skill.script.clone()
    };

    let candidates = [
        PathBuf::from("skills").join(&slug).join("SKILL.md"),
        PathBuf::from("Tiger_bot")
            .join("skills")
            .join(&slug)
            .join("SKILL.md"),
    ];

    for path in &candidates {
        if path.exists() {
            return Some(path.clone());
        }
    }

    // Retry with name-based slug if different from script
    let name_slug = slugify(&skill.name);
    if name_slug != slug {
        let candidates2 = [
            PathBuf::from("skills").join(&name_slug).join("SKILL.md"),
            PathBuf::from("Tiger_bot")
                .join("skills")
                .join(&name_slug)
                .join("SKILL.md"),
        ];
        for path in &candidates2 {
            if path.exists() {
                return Some(path.clone());
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_skills).post(install_skill))
        .route("/upload", post(upload_skill))
        .route("/catalog", get(catalog))
        .route("/auto/run-now", post(auto_run_now))
        .route("/auto/status", get(auto_status))
        .route("/feedback", post(set_feedback))
        .route("/feedback/{sessionId}", get(get_feedback))
        .route("/{id}", patch(update_skill).delete(uninstall_skill))
        .route("/{id}/content", get(skill_content))
        .route("/{id}/download", get(skill_download))
        .route("/{id}/approve", post(approve_skill))
        .route("/{id}/reject", post(reject_skill))
        .route("/{id}/proposed-diff", get(proposed_diff))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET / - list all installed skills
async fn list_skills() -> Json<Vec<Skill>> {
    let skills = get_skills().await;
    // NOTE: ClawHub merge logic would go here; skipped for now.
    Json(skills)
}

/// POST / - install a new skill
async fn install_skill(Json(body): Json<CreateSkillBody>) -> impl IntoResponse {
    let mut skills = get_skills().await;

    let skill = Skill {
        id: Uuid::new_v4().to_string(),
        name: body.name.unwrap_or_else(|| "Untitled Skill".to_string()),
        description: body.description.unwrap_or_default(),
        source: body.source.unwrap_or_else(|| "custom".to_string()),
        script: body.script.unwrap_or_default(),
        enabled: true,
        installed_at: chrono::Utc::now().to_rfc3339(),
        review_status: None,
        auto_meta: None,
    };

    skills.push(skill.clone());
    save_skills(&skills).await;

    (StatusCode::CREATED, Json(serde_json::to_value(&skill).unwrap()))
}

/// PATCH /:id - update / toggle a skill
async fn update_skill(
    Path(id): Path<String>,
    Json(body): Json<UpdateSkillBody>,
) -> impl IntoResponse {
    let mut skills = get_skills().await;

    let idx = match skills.iter().position(|s| s.id == id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Not found"})),
            );
        }
    };

    if let Some(name) = body.name {
        skills[idx].name = name;
    }
    if let Some(description) = body.description {
        skills[idx].description = description;
    }
    if let Some(source) = body.source {
        skills[idx].source = source;
    }
    if let Some(script) = body.script {
        skills[idx].script = script;
    }
    if let Some(enabled) = body.enabled {
        skills[idx].enabled = enabled;
    }
    if let Some(review_status) = body.review_status {
        skills[idx].review_status = Some(review_status);
    }

    save_skills(&skills).await;

    (StatusCode::OK, Json(serde_json::to_value(&skills[idx]).unwrap()))
}

/// DELETE /:id - uninstall a skill
async fn uninstall_skill(Path(id): Path<String>) -> Json<Value> {
    let skills = get_skills().await;
    let skills: Vec<_> = skills.into_iter().filter(|s| s.id != id).collect();
    save_skills(&skills).await;
    Json(json!({"success": true}))
}

/// POST /upload - upload a SKILL.md file (or .zip, simplified to single-file for now)
async fn upload_skill(mut multipart: Multipart) -> impl IntoResponse {
    // Read the first file field from the multipart form
    let field = match multipart.next_field().await {
        Ok(Some(f)) => f,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "No file uploaded"})),
            );
        }
    };

    let original_name = field.file_name().unwrap_or("SKILL.md").to_string();
    let bytes = match field.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            );
        }
    };

    let ext = original_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();

    if ext == "zip" {
        // ZIP upload support requires the `zip` crate; return a stub for now.
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "ZIP upload not yet implemented in Rust backend. Upload a single SKILL.md instead."})),
        );
    }

    // --- Single SKILL.md file upload ---
    let content = String::from_utf8_lossy(&bytes).to_string();
    let (mut name, description) = parse_frontmatter(&content);

    if name.is_empty() {
        // Derive name from filename without extension
        name = original_name
            .rsplit('/')
            .next()
            .unwrap_or(&original_name)
            .to_string();
        if let Some(pos) = name.rfind('.') {
            name.truncate(pos);
        }
    }

    let sanitized = slugify(&name);
    let skill_dir = PathBuf::from("skills").join(&sanitized);
    let _ = tokio::fs::create_dir_all(&skill_dir).await;
    let skill_file = skill_dir.join("SKILL.md");
    let _ = tokio::fs::write(&skill_file, &content).await;

    // Register in skills.json
    let mut skills = get_skills().await;

    // Check if a custom skill with this name already exists
    if let Some(idx) = skills.iter().position(|s| s.name == name && s.source == "custom") {
        skills[idx].script = name.clone();
        if !description.is_empty() {
            skills[idx].description = description;
        }
        let result = serde_json::to_value(&skills[idx]).unwrap();
        save_skills(&skills).await;
        return (StatusCode::OK, Json(result));
    }

    let skill = Skill {
        id: Uuid::new_v4().to_string(),
        name: name.clone(),
        description: if description.is_empty() {
            format!("Custom skill from {}", original_name)
        } else {
            description
        },
        source: "custom".to_string(),
        script: name,
        enabled: true,
        installed_at: chrono::Utc::now().to_rfc3339(),
        review_status: None,
        auto_meta: None,
    };

    skills.push(skill.clone());
    save_skills(&skills).await;

    (StatusCode::OK, Json(serde_json::to_value(&skill).unwrap()))
}

/// GET /catalog - return hardcoded skill catalog
async fn catalog() -> Json<Value> {
    Json(json!([
        { "name": "Web Search", "description": "Search the web using configured search engine", "source": "claude", "script": "web-search" },
        { "name": "Code Review", "description": "Review code for quality and security issues", "source": "claude", "script": "code-review" },
        { "name": "File Converter", "description": "Convert between file formats (PDF, DOCX, CSV)", "source": "claude", "script": "file-converter" },
        { "name": "Data Analyzer", "description": "Analyze CSV/JSON data and generate charts", "source": "openclaw", "script": "data-analyzer" },
        { "name": "API Tester", "description": "Test REST APIs with custom requests", "source": "openclaw", "script": "api-tester" },
        { "name": "Markdown Renderer", "description": "Render markdown to HTML/PDF", "source": "openclaw", "script": "markdown-renderer" },
        { "name": "Git Helper", "description": "Git operations within sandbox", "source": "claude", "script": "git-helper" },
        { "name": "Image Processor", "description": "Resize, crop, and convert images", "source": "openclaw", "script": "image-processor" },
    ]))
}

/// GET /:id/content - read skill SKILL.md content from disk
async fn skill_content(Path(id): Path<String>) -> impl IntoResponse {
    let skills = get_skills().await;
    let skill = match skills.iter().find(|s| s.id == id) {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Skill not found"})),
            );
        }
    };

    match find_skill_file(skill) {
        Some(file_path) => {
            let content = tokio::fs::read_to_string(&file_path)
                .await
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "name": skill.name,
                    "content": content,
                    "source": skill.source,
                    "hasFile": true,
                    "path": file_path.to_string_lossy(),
                })),
            )
        }
        None => {
            // For custom skills with inline script, return script as content
            if !skill.script.is_empty() && !skill.script.contains('/') {
                return (
                    StatusCode::OK,
                    Json(json!({
                        "ok": true,
                        "name": skill.name,
                        "content": skill.script,
                        "source": skill.source,
                        "hasFile": false,
                    })),
                );
            }
            (
                StatusCode::OK,
                Json(json!({"ok": false, "error": "SKILL.md file not found on disk"})),
            )
        }
    }
}

/// GET /:id/download - download skill as SKILL.md file
async fn skill_download(Path(id): Path<String>) -> impl IntoResponse {
    let skills = get_skills().await;
    let skill = match skills.iter().find(|s| s.id == id) {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "application/json".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        String::new(),
                    ),
                ],
                json!({"error": "Skill not found"}).to_string(),
            );
        }
    };

    let slug = if skill.script.is_empty() {
        slugify(&skill.name)
    } else {
        skill.script.clone()
    };

    let content = match find_skill_file(skill) {
        Some(file_path) => tokio::fs::read_to_string(&file_path)
            .await
            .unwrap_or_default(),
        None => {
            // Generate a SKILL.md on the fly for inline skills
            format!(
                "---\nname: {}\ndescription: {}\n---\n\n{}",
                skill.name,
                skill.description,
                if skill.script.is_empty() {
                    ""
                } else {
                    &skill.script
                }
            )
        }
    };

    let filename = format!("{}.SKILL.md", slug);
    (
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/markdown".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        content,
    )
}

/// POST /auto/run-now - trigger skill synthesis (forced, ignores cursor)
async fn auto_run_now() -> Json<Value> {
    tokio::spawn(async {
        match skill_synthesizer::run_synthesis_forced().await {
            Ok(summary) => tracing::info!("[SkillSynth] Manual run: {}", summary),
            Err(e) => tracing::error!("[SkillSynth] Manual run error: {}", e),
        }
    });
    Json(json!({"ok": true, "message": "Synthesis started"}))
}

/// GET /auto/status - get auto-update status from settings
async fn auto_status() -> Json<Value> {
    let settings = get_settings().await;
    let synth_status = skill_synthesizer::get_synth_status().await;

    let cursor = settings
        .extra
        .get("skillAutoUpdateCursor")
        .cloned()
        .unwrap_or(Value::Null);

    let last_run_at = synth_status
        .last_run_at
        .clone()
        .or_else(|| {
            settings
                .extra
                .get("skillAutoUpdateLastRunAt")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let last_run_summary = synth_status
        .last_run_summary
        .clone()
        .or_else(|| {
            settings
                .extra
                .get("skillAutoUpdateLastRunSummary")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let pending: Vec<Value> = synth_status
        .proposals
        .iter()
        .filter(|p| p.review_status == "pending")
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "description": p.description,
                "kind": p.kind,
                "rationale": p.rationale,
                "generatedAt": p.generated_at,
            })
        })
        .collect();

    Json(json!({
        "enabled": settings.skill_auto_update_enabled.unwrap_or(true),
        "intervalMinutes": settings.skill_auto_update_interval_minutes.unwrap_or(60),
        "requireApproval": settings.skill_auto_update_require_approval.unwrap_or(true),
        "running": synth_status.running,
        "cursor": cursor,
        "lastRunAt": last_run_at,
        "lastRunSummary": last_run_summary,
        "pendingProposals": pending,
    }))
}

/// POST /:id/approve - approve pending skill
async fn approve_skill(Path(id): Path<String>) -> Json<Value> {
    match skill_synthesizer::approve_proposal(&id).await {
        Ok(()) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e})),
    }
}

/// POST /:id/reject - reject pending skill
async fn reject_skill(Path(id): Path<String>) -> Json<Value> {
    match skill_synthesizer::reject_proposal(&id).await {
        Ok(()) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e})),
    }
}

/// GET /:id/proposed-diff - get proposed diff
async fn proposed_diff(Path(id): Path<String>) -> impl IntoResponse {
    let status = skill_synthesizer::get_synth_status().await;
    if let Some(proposal) = status.proposals.iter().find(|p| p.id == id) {
        (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "id": id,
                "name": proposal.name,
                "kind": proposal.kind,
                "proposed": proposal.content,
                "existing": proposal.existing_content,
                "rationale": proposal.rationale,
            })),
        )
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "No proposed diff found", "id": id})),
        )
    }
}

/// POST /feedback - mark/unmark a chat session as skill candidate
async fn set_feedback(Json(body): Json<FeedbackBody>) -> impl IntoResponse {
    let session_id = match body.session_id {
        Some(ref id) if !id.is_empty() => id.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Missing sessionId"})),
            );
        }
    };

    let mut sessions = get_chat_history().await;
    let idx = match sessions.iter().position(|s| s.id == session_id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Session not found"})),
            );
        }
    };

    if let Some(candidate) = body.skill_candidate {
        sessions[idx].skill_candidate = Some(candidate);
    }
    if let Some(ref feedback) = body.skill_feedback {
        sessions[idx].skill_feedback = Some(feedback.clone());
    }
    sessions[idx].updated_at = chrono::Utc::now().to_rfc3339();

    let sc = sessions[idx].skill_candidate;
    let sf = sessions[idx].skill_feedback.clone();
    save_chat_history(&sessions).await;

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "sessionId": session_id,
            "skillCandidate": sc.unwrap_or(false),
            "skillFeedback": sf,
        })),
    )
}

/// GET /feedback/:sessionId - get feedback status for a session
async fn get_feedback(Path(session_id): Path<String>) -> impl IntoResponse {
    let sessions = get_chat_history().await;
    let session = match sessions.iter().find(|s| s.id == session_id) {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Session not found"})),
            );
        }
    };

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "sessionId": session_id,
            "skillCandidate": session.skill_candidate.unwrap_or(false),
            "skillFeedback": session.skill_feedback,
        })),
    )
}
