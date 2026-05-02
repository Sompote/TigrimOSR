use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{Multipart, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::fs;
use tokio_util::io::ReaderStream;

use crate::server::data::*;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Query param structs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    pub path: String,
}

// ---------------------------------------------------------------------------
// Request body structs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct WriteBody {
    pub path: String,
    #[serde(default)]
    pub content: String,
}

#[derive(Deserialize)]
pub struct MkdirBody {
    pub path: String,
}

// ---------------------------------------------------------------------------
// Response helper structs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct UploadedFile {
    name: String,
    path: String,
    size: usize,
    #[serde(rename = "type")]
    mime_type: String,
}

// ---------------------------------------------------------------------------
// Allowed extensions for chat-upload
// ---------------------------------------------------------------------------

const ALLOWED_CHAT_EXTENSIONS: &[&str] = &[
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".csv", ".txt", ".json", ".xml",
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".svg",
    ".py", ".js", ".ts", ".html", ".css", ".md", ".yaml", ".yml",
    ".zip", ".tar", ".gz",
];

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_handler).delete(delete_handler))
        .route("/read", get(read_handler))
        .route("/write", post(write_handler))
        .route("/mkdir", post(mkdir_handler))
        .route("/upload", post(upload_handler))
        .route("/chat-upload", post(chat_upload_handler))
        .route("/preview", get(preview_handler))
        .route("/download", get(download_handler))
}

// ---------------------------------------------------------------------------
// GET / -- list files
// ---------------------------------------------------------------------------

async fn list_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PathQuery>,
) -> Response {
    match list_files(&state.sandbox_dir, &params.path).await {
        Ok(files) => Json(json!(files)).into_response(),
        Err(e) => (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /read -- read file content
// ---------------------------------------------------------------------------

async fn read_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PathQuery>,
) -> Response {
    if params.path.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "path required"}))).into_response();
    }
    match read_file_content(&state.sandbox_dir, &params.path).await {
        Ok(content) => Json(json!({"content": content, "path": params.path})).into_response(),
        Err(e) => (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /write -- write file content
// ---------------------------------------------------------------------------

async fn write_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WriteBody>,
) -> Response {
    if body.path.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "path required"}))).into_response();
    }
    match write_file_content(&state.sandbox_dir, &body.path, &body.content).await {
        Ok(_) => Json(json!({"success": true, "path": body.path})).into_response(),
        Err(e) => (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// DELETE / -- delete file or directory
// ---------------------------------------------------------------------------

async fn delete_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PathQuery>,
) -> Response {
    if params.path.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "path required"}))).into_response();
    }
    match delete_file_or_dir(&state.sandbox_dir, &params.path).await {
        Ok(_) => Json(json!({"success": true})).into_response(),
        Err(e) => (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /mkdir -- create directory
// ---------------------------------------------------------------------------

async fn mkdir_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MkdirBody>,
) -> Response {
    if body.path.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "path required"}))).into_response();
    }
    match validate_path(&state.sandbox_dir, &body.path) {
        Ok(resolved) => {
            if let Err(e) = fs::create_dir_all(&resolved).await {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response();
            }
            Json(json!({"success": true, "path": body.path})).into_response()
        }
        Err(e) => (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /upload -- single file upload
// ---------------------------------------------------------------------------

async fn upload_handler(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Response {
    let mut file_data: Option<(String, Vec<u8>)> = None;
    let mut dest_dir = String::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "path" {
            dest_dir = field.text().await.unwrap_or_default();
        } else if name == "file" {
            let filename = field.file_name().unwrap_or("upload").to_string();
            match field.bytes().await {
                Ok(bytes) => file_data = Some((filename, bytes.to_vec())),
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": e.to_string()})),
                    )
                        .into_response()
                }
            }
        }
    }

    let (filename, bytes) = match file_data {
        Some(d) => d,
        None => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "No file"}))).into_response()
        }
    };

    let dest_path = if dest_dir.is_empty() {
        filename.clone()
    } else {
        format!("{}/{}", dest_dir, filename)
    };

    match validate_path(&state.sandbox_dir, &dest_path) {
        Ok(resolved) => {
            // Ensure parent directory exists
            if let Some(parent) = resolved.parent() {
                let _ = fs::create_dir_all(parent).await;
            }
            match fs::write(&resolved, &bytes).await {
                Ok(_) => Json(json!({"success": true, "path": dest_path})).into_response(),
                Err(e) => (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        Err(e) => (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /chat-upload -- multiple file upload for chat
// ---------------------------------------------------------------------------

async fn chat_upload_handler(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Response {
    let uploads_dir = Path::new(&state.sandbox_dir).join("uploads");
    if let Err(e) = fs::create_dir_all(&uploads_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response();
    }

    let mut uploaded: Vec<UploadedFile> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = match field.file_name() {
            Some(name) => name.to_string(),
            None => continue,
        };
        let content_type = field.content_type().unwrap_or("application/octet-stream").to_string();

        // Check extension
        let ext = Path::new(&filename)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
            .unwrap_or_default();

        if !ALLOWED_CHAT_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        let bytes = match field.bytes().await {
            Ok(b) => b,
            Err(_) => continue,
        };

        // Sanitize filename
        let safe_name: String = filename
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' })
            .collect();

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let dest_name = format!("{}_{}", timestamp, safe_name);
        let dest_path = uploads_dir.join(&dest_name);

        if let Err(_) = fs::write(&dest_path, &bytes).await {
            continue;
        }

        uploaded.push(UploadedFile {
            name: filename,
            path: format!("uploads/{}", dest_name),
            size: bytes.len(),
            mime_type: content_type,
        });
    }

    if uploaded.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "No files"}))).into_response();
    }

    Json(json!({"success": true, "files": uploaded})).into_response()
}

// ---------------------------------------------------------------------------
// GET /preview -- preview PDF/DOCX/Excel (placeholder)
// ---------------------------------------------------------------------------

async fn preview_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PathQuery>,
) -> Response {
    if params.path.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "path required"}))).into_response();
    }

    let resolved = match validate_path(&state.sandbox_dir, &params.path) {
        Ok(r) => r,
        Err(e) => return (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
    };

    if fs::metadata(&resolved).await.is_err() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "File not found"}))).into_response();
    }

    let ext = resolved
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "pdf" => {
            // Placeholder: PDF parsing library will be added later
            Json(json!({
                "type": "pdf",
                "pages": 0,
                "html": "<p><em>PDF preview not yet implemented in Rust build. Please download the file to view.</em></p>"
            }))
            .into_response()
        }
        "docx" | "doc" => {
            // Placeholder: DOCX parsing library will be added later
            Json(json!({
                "type": "docx",
                "html": "<p><em>DOCX preview not yet implemented in Rust build. Please download the file to view.</em></p>"
            }))
            .into_response()
        }
        "xlsx" | "xls" => {
            // Placeholder: Excel parsing library will be added later
            Json(json!({
                "type": "excel",
                "sheets": 0,
                "html": "<p><em>Excel preview not yet implemented in Rust build. Please download the file to view.</em></p>"
            }))
            .into_response()
        }
        "md" => {
            match fs::read_to_string(&resolved).await {
                Ok(content) => Json(json!({"type": "markdown", "html": content})).into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        "js" | "jsx" | "tsx" => {
            // Check if it's a React component (has __REACT_META__ or JSX patterns)
            match fs::read_to_string(&resolved).await {
                Ok(content) => {
                    if content.contains("__REACT_META__") || content.contains("React") {
                        // Extract renderTarget and title from __REACT_META__
                        let (title, render_target) = {
                            let mut t = "React Preview".to_string();
                            let mut rt = "App()".to_string();
                            if let Some(meta_start) = content.find("__REACT_META__=") {
                                let json_start = content[meta_start..].find('{').map(|i| meta_start + i);
                                let json_end = json_start.and_then(|s| content[s..].find('}').map(|i| s + i + 1));
                                if let (Some(s), Some(e)) = (json_start, json_end) {
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content[s..e]) {
                                        if let Some(s) = v.get("title").and_then(|v| v.as_str()) { t = s.to_string(); }
                                        if let Some(s) = v.get("renderTarget").and_then(|v| v.as_str()) { rt = s.to_string(); }
                                    }
                                }
                            }
                            (t, rt)
                        };

                        // Strip boilerplate lines and trailing return statement
                        let clean_jsx: String = content
                            .lines()
                            .filter(|l| {
                                let trimmed = l.trim();
                                !l.contains("__REACT_META__")
                                && !l.contains("} = React")
                                && !l.contains("typeof Recharts")
                                && !l.contains("} = _Recharts")
                                && !(trimmed.starts_with("return ") && trimmed.ends_with("();"))
                            })
                            .collect::<Vec<&str>>()
                            .join("\n");

                        let html = format!(
                            r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title}</title>
<script src="https://unpkg.com/react@18/umd/react.development.js" crossorigin></script>
<script src="https://unpkg.com/react-dom@18/umd/react-dom.development.js" crossorigin></script>
<script src="https://unpkg.com/@babel/standalone/babel.min.js"></script>
<script src="https://unpkg.com/recharts@2/umd/Recharts.min.js" crossorigin></script>
<link href="https://cdn.jsdelivr.net/npm/tailwindcss@2.2.19/dist/tailwind.min.css" rel="stylesheet">
<style>
  body {{ margin: 0; padding: 20px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; background: #f8f9fa; }}
  #root {{ max-width: 1200px; margin: 0 auto; }}
</style>
</head>
<body>
<div id="root"></div>
<script type="text/babel">
  const React = window.React;
  const {{ useState, useEffect, useRef, useMemo, useCallback, useReducer, useContext, createContext, Fragment, memo, forwardRef, lazy, Suspense }} = React;
  const _Recharts = typeof Recharts !== 'undefined' ? Recharts : {{}};
  const {{ LineChart, Line, BarChart, Bar, AreaChart, Area, PieChart, Pie, Cell, XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer, ScatterChart, Scatter, RadarChart, Radar, PolarGrid, PolarAngleAxis, PolarRadiusAxis, ComposedChart, Treemap }} = _Recharts;

  {clean_jsx}

  try {{
    const element = {render_target};
    ReactDOM.createRoot(document.getElementById('root')).render(element);
  }} catch(e) {{
    document.getElementById('root').innerHTML = '<pre style="color:red">' + e.message + '\\n' + e.stack + '</pre>';
  }}
</script>
</body>
</html>"#,
                            title = title,
                            clean_jsx = clean_jsx,
                            render_target = render_target,
                        );

                        Response::builder()
                            .status(200)
                            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                            .body(Body::from(html))
                            .unwrap()
                    } else {
                        // Plain JS file — return as text
                        Json(json!({"type": "text", "content": content})).into_response()
                    }
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Unsupported file type for preview"})),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /download -- download file
// ---------------------------------------------------------------------------

async fn download_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PathQuery>,
) -> Response {
    if params.path.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "path required"}))).into_response();
    }

    let resolved = match validate_path(&state.sandbox_dir, &params.path) {
        Ok(r) => r,
        Err(e) => return (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
    };

    let file = match tokio::fs::File::open(&resolved).await {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    let filename = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Response::builder()
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(body)
        .unwrap()
        .into_response()
}
