use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{Multipart, Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post, put},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server::data::*;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the sandbox directory from settings or environment.
fn resolve_sandbox_dir(configured: &str) -> String {
    if !configured.is_empty() && Path::new(configured).exists() {
        return configured.to_string();
    }
    if let Ok(env_dir) = std::env::var("SANDBOX_DIR") {
        if Path::new(&env_dir).exists() {
            return env_dir;
        }
    }
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());
    if Path::new(&cwd).exists() {
        return cwd;
    }
    let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let fallback = format!("{}/tigrimos_sandbox", tmp);
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

/// Resolve a project's working folder to an absolute path.
async fn resolve_working_folder(project: &Project) -> String {
    if project.working_folder.is_empty() {
        return String::new();
    }

    let settings = get_settings().await;
    let sandbox = resolve_sandbox_dir(&settings.sandbox_dir);

    let resolved = if Path::new(&project.working_folder).is_absolute() {
        PathBuf::from(&project.working_folder)
    } else {
        PathBuf::from(&sandbox).join(&project.working_folder)
    };

    let resolved_str = resolved.to_string_lossy().to_string();

    if !resolved.exists() {
        if Path::new(&project.working_folder).is_absolute() {
            let _ = std::fs::create_dir_all(&resolved);
        }
        if !resolved.exists() {
            let basename = resolved
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let fallback = PathBuf::from(&sandbox).join(&basename);
            let _ = std::fs::create_dir_all(&fallback);
            return fallback.to_string_lossy().to_string();
        }
    }

    resolved_str
}

/// Compute the sandbox-relative path for a file inside a resolved project folder.
async fn project_file_rel_path(resolved_folder: &str, sub_file_path: &str) -> String {
    let settings = get_settings().await;
    let sandbox = resolve_sandbox_dir(&settings.sandbox_dir);
    let full = PathBuf::from(resolved_folder).join(sub_file_path);
    let sandbox_path = PathBuf::from(&sandbox);
    full.strip_prefix(&sandbox_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| full.to_string_lossy().to_string())
}

/// Find a project by ID from the projects list, returning (index, project).
fn find_project(projects: &[Project], id: &str) -> Option<(usize, Project)> {
    projects
        .iter()
        .enumerate()
        .find(|(_, p)| p.id == id)
        .map(|(i, p)| (i, p.clone()))
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct FilePathQuery {
    path: Option<String>,
}

#[derive(Deserialize)]
struct MkdirBody {
    name: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize)]
struct MemoryBody {
    content: Option<String>,
}

#[derive(Deserialize)]
struct TigerMdBody {
    content: Option<String>,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_projects).post(create_project))
        .route("/bulk", put(put_all_projects))
        .route(
            "/{id}",
            get(get_project).patch(update_project).delete(delete_project),
        )
        .route("/{id}/memory", get(get_memory).put(put_memory))
        .route("/{id}/memory/generate", post(generate_memory))
        .route("/{id}/tiger-md", get(get_tiger_md).put(put_tiger_md))
        .route("/{id}/files", get(list_project_files).delete(delete_project_file))
        .route("/{id}/files/mkdir", post(mkdir_project))
        .route("/{id}/files/download", get(download_project_file))
        .route("/{id}/files/upload", post(upload_project_file))
        .route("/{id}/files/sandbox-path", get(sandbox_path))
}

// ---------------------------------------------------------------------------
// GET / -- list all projects
// ---------------------------------------------------------------------------

async fn list_projects() -> Json<Value> {
    let projects = get_projects().await;
    Json(serde_json::to_value(&projects).unwrap_or(json!([])))
}

/// PUT /bulk - save all projects (for remote sync)
async fn put_all_projects(Json(projects): Json<Vec<Project>>) -> StatusCode {
    save_projects(&projects).await;
    StatusCode::OK
}

// ---------------------------------------------------------------------------
// GET /:id -- get single project
// ---------------------------------------------------------------------------

async fn get_project(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    let projects = get_projects().await;
    match find_project(&projects, &id) {
        Some((_, project)) => (
            StatusCode::OK,
            Json(serde_json::to_value(&project).unwrap_or(json!({}))),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Project not found"})),
        ),
    }
}

// ---------------------------------------------------------------------------
// POST / -- create project
// ---------------------------------------------------------------------------

async fn create_project(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut projects = get_projects().await;
    let settings = get_settings().await;
    let sandbox = resolve_sandbox_dir(&settings.sandbox_dir);

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Project")
        .to_string();
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let working_folder_raw = body
        .get("workingFolder")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let skills: Vec<String> = body
        .get("skills")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // Resolve working folder relative to sandbox if not absolute
    let resolved_folder = if working_folder_raw.is_empty() {
        String::new()
    } else if Path::new(&working_folder_raw).is_absolute() {
        working_folder_raw
    } else {
        PathBuf::from(&sandbox)
            .join(&working_folder_raw)
            .to_string_lossy()
            .to_string()
    };

    let now = chrono::Utc::now().to_rfc3339();
    let project = Project {
        id: Uuid::new_v4().to_string(),
        name,
        description,
        working_folder: resolved_folder.clone(),
        memory: String::new(),
        skills,
        system_prompt: None,
        agent_override: None,
        created_at: now.clone(),
        updated_at: now,
    };

    // Create working folder if specified and doesn't exist
    if !resolved_folder.is_empty() && !Path::new(&resolved_folder).exists() {
        let _ = std::fs::create_dir_all(&resolved_folder);
    }

    projects.push(project.clone());
    save_projects(&projects).await;

    Json(serde_json::to_value(&project).unwrap_or(json!({})))
}

// ---------------------------------------------------------------------------
// PATCH /:id -- update project
// ---------------------------------------------------------------------------

async fn update_project(
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut projects = get_projects().await;
    let idx = projects.iter().position(|p| p.id == id);
    let idx = match idx {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    // Apply updates via JSON merge
    let mut current_val = serde_json::to_value(&projects[idx]).unwrap_or(json!({}));
    if let (Some(base), Some(updates)) = (current_val.as_object_mut(), body.as_object()) {
        for (k, v) in updates {
            // Skip legacy fields
            if k == "folderLocation" || k == "folderAccess" {
                continue;
            }
            base.insert(k.clone(), v.clone());
        }
    }

    // Resolve relative working folder
    if let Some(wf) = body.get("workingFolder").and_then(|v| v.as_str()) {
        if !wf.is_empty() && !Path::new(wf).is_absolute() {
            let settings = get_settings().await;
            let sandbox = resolve_sandbox_dir(&settings.sandbox_dir);
            let resolved = PathBuf::from(&sandbox).join(wf);
            current_val["workingFolder"] = json!(resolved.to_string_lossy().to_string());
        }
    }

    current_val["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());

    match serde_json::from_value::<Project>(current_val.clone()) {
        Ok(updated_project) => {
            // Create working folder if needed
            if !updated_project.working_folder.is_empty()
                && !Path::new(&updated_project.working_folder).exists()
            {
                let _ = std::fs::create_dir_all(&updated_project.working_folder);
            }
            projects[idx] = updated_project;
            save_projects(&projects).await;
            (
                StatusCode::OK,
                Json(serde_json::to_value(&projects[idx]).unwrap_or(json!({}))),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

// ---------------------------------------------------------------------------
// DELETE /:id -- delete project
// ---------------------------------------------------------------------------

async fn delete_project(AxumPath(id): AxumPath<String>) -> Json<Value> {
    let projects = get_projects().await;
    let filtered: Vec<Project> = projects.into_iter().filter(|p| p.id != id).collect();
    save_projects(&filtered).await;
    Json(json!({"ok": true}))
}

// ---------------------------------------------------------------------------
// GET /:id/memory -- read memory.md from working folder
// ---------------------------------------------------------------------------

async fn get_memory(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    let mut content = String::new();

    if !project.working_folder.is_empty() {
        let resolved = resolve_working_folder(&project).await;
        let memory_path = PathBuf::from(&resolved).join("memory.md");
        if memory_path.exists() {
            if let Ok(data) = std::fs::read_to_string(&memory_path) {
                content = data;
            }
        }
    }

    // Fallback to stored memory
    if content.is_empty() && !project.memory.is_empty() {
        content = project.memory.clone();
    }

    (StatusCode::OK, Json(json!({"content": content})))
}

// ---------------------------------------------------------------------------
// PUT /:id/memory -- save memory.md
// ---------------------------------------------------------------------------

async fn put_memory(
    AxumPath(id): AxumPath<String>,
    Json(body): Json<MemoryBody>,
) -> impl IntoResponse {
    let mut projects = get_projects().await;
    let idx = projects.iter().position(|p| p.id == id);
    let idx = match idx {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    let content = body.content.unwrap_or_default();

    // Write to memory.md in the working folder
    if !projects[idx].working_folder.is_empty() {
        let resolved = resolve_working_folder(&projects[idx]).await;
        let memory_path = PathBuf::from(&resolved).join("memory.md");
        if !Path::new(&resolved).exists() {
            let _ = std::fs::create_dir_all(&resolved);
        }
        if let Err(e) = std::fs::write(&memory_path, &content) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to write memory.md: {}", e)})),
            );
        }
    }

    // Also keep in project JSON as backup
    projects[idx].memory = content;
    projects[idx].updated_at = chrono::Utc::now().to_rfc3339();
    save_projects(&projects).await;

    (StatusCode::OK, Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// POST /:id/memory/generate -- stub
// ---------------------------------------------------------------------------

async fn generate_memory(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    let projects = get_projects().await;
    match find_project(&projects, &id) {
        Some(_) => (
            StatusCode::OK,
            Json(json!({"content": "", "message": "Memory generation not yet implemented in Rust backend"})),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Project not found"})),
        ),
    }
}

// ---------------------------------------------------------------------------
// GET /:id/tiger-md -- read tiger.md from working folder
// ---------------------------------------------------------------------------

async fn get_tiger_md(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    let mut content = String::new();
    if !project.working_folder.is_empty() {
        let resolved = resolve_working_folder(&project).await;
        let tiger_path = PathBuf::from(&resolved).join("tiger.md");
        if tiger_path.exists() {
            if let Ok(data) = std::fs::read_to_string(&tiger_path) {
                content = data;
            }
        }
    }

    (StatusCode::OK, Json(json!({"content": content})))
}

// ---------------------------------------------------------------------------
// PUT /:id/tiger-md -- save tiger.md
// ---------------------------------------------------------------------------

async fn put_tiger_md(
    AxumPath(id): AxumPath<String>,
    Json(body): Json<TigerMdBody>,
) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    let content = body.content.unwrap_or_default();

    if project.working_folder.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No working folder set -- tiger.md requires a working folder"})),
        );
    }

    let resolved = resolve_working_folder(&project).await;
    let tiger_path = PathBuf::from(&resolved).join("tiger.md");
    if !Path::new(&resolved).exists() {
        let _ = std::fs::create_dir_all(&resolved);
    }
    match std::fs::write(&tiger_path, &content) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write tiger.md: {}", e)})),
        ),
    }
}

// ---------------------------------------------------------------------------
// GET /:id/files -- list files in project working folder
// ---------------------------------------------------------------------------

async fn list_project_files(
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilePathQuery>,
) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    if project.working_folder.is_empty() {
        return (StatusCode::OK, Json(json!({"files": []})));
    }

    let resolved = resolve_working_folder(&project).await;
    let sub_path = query.path.unwrap_or_default();
    let full_path = if sub_path.is_empty() {
        PathBuf::from(&resolved)
    } else {
        PathBuf::from(&resolved).join(&sub_path)
    };

    if !full_path.exists() {
        return (StatusCode::OK, Json(json!({"files": []})));
    }

    let entries = match std::fs::read_dir(&full_path) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::OK,
                Json(json!({"files": [], "error": e.to_string()})),
            );
        }
    };

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(false);
        let size = if is_dir {
            0u64
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };
        let entry_path = if sub_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", sub_path, name)
        };

        files.push(json!({
            "name": name,
            "isDirectory": is_dir,
            "size": size,
            "path": entry_path,
        }));
    }

    (StatusCode::OK, Json(json!({"files": files})))
}

// ---------------------------------------------------------------------------
// POST /:id/files/upload -- upload file via multipart
// ---------------------------------------------------------------------------

async fn upload_project_file(
    AxumPath(id): AxumPath<String>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    if project.working_folder.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No working folder"})),
        );
    }

    let resolved = resolve_working_folder(&project).await;
    let mut sub_path = String::new();
    let mut file_name = String::new();
    let mut file_data: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name == "path" {
            sub_path = field.text().await.unwrap_or_default();
        } else if field_name == "file" {
            file_name = field
                .file_name()
                .unwrap_or("upload")
                .to_string();
            file_data = Some(field.bytes().await.unwrap_or_default().to_vec());
        }
    }

    let data = match file_data {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "No file"})),
            );
        }
    };

    let dest_dir = if sub_path.is_empty() {
        PathBuf::from(&resolved)
    } else {
        PathBuf::from(&resolved).join(&sub_path)
    };

    if !dest_dir.exists() {
        let _ = std::fs::create_dir_all(&dest_dir);
    }

    let dest_path = dest_dir.join(&file_name);
    match std::fs::write(&dest_path, &data) {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"success": true, "name": file_name})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

// ---------------------------------------------------------------------------
// POST /:id/files/mkdir -- create directory
// ---------------------------------------------------------------------------

async fn mkdir_project(
    AxumPath(id): AxumPath<String>,
    Json(body): Json<MkdirBody>,
) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    if project.working_folder.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No working folder"})),
        );
    }

    let dir_name = match &body.name {
        Some(n) if !n.is_empty() => n.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "name required"})),
            );
        }
    };

    let resolved = resolve_working_folder(&project).await;
    let sub_path = body.path.unwrap_or_default();
    let full_path = PathBuf::from(&resolved).join(&sub_path).join(&dir_name);

    // Prevent path traversal
    let resolved_canon = std::fs::canonicalize(&resolved)
        .unwrap_or_else(|_| PathBuf::from(&resolved));
    let full_canon = full_path
        .canonicalize()
        .unwrap_or_else(|_| full_path.clone());
    // For non-existent paths, check the parent or use string prefix
    if !full_canon
        .to_string_lossy()
        .starts_with(&*resolved_canon.to_string_lossy())
        && !full_path
            .to_string_lossy()
            .starts_with(&*resolved_canon.to_string_lossy())
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Invalid path"})),
        );
    }

    if !full_path.exists() {
        let _ = std::fs::create_dir_all(&full_path);
    }

    (StatusCode::OK, Json(json!({"success": true})))
}

// ---------------------------------------------------------------------------
// DELETE /:id/files -- delete file/directory (path in query param)
// ---------------------------------------------------------------------------

async fn delete_project_file(
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilePathQuery>,
) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    if project.working_folder.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No working folder"})),
        );
    }

    let file_path = match &query.path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "path required"})),
            );
        }
    };

    let resolved = resolve_working_folder(&project).await;
    let full_path = PathBuf::from(&resolved).join(&file_path);

    // Prevent path traversal
    let resolved_canon = std::fs::canonicalize(&resolved)
        .unwrap_or_else(|_| PathBuf::from(&resolved));
    if let Ok(full_canon) = std::fs::canonicalize(&full_path) {
        if !full_canon
            .to_string_lossy()
            .starts_with(&*resolved_canon.to_string_lossy())
        {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Invalid path"})),
            );
        }
    }

    if !full_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "File not found"})),
        );
    }

    let metadata = match std::fs::metadata(&full_path) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            );
        }
    };

    let result = if metadata.is_dir() {
        std::fs::remove_dir_all(&full_path)
    } else {
        std::fs::remove_file(&full_path)
    };

    match result {
        Ok(_) => (StatusCode::OK, Json(json!({"success": true}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

// ---------------------------------------------------------------------------
// GET /:id/files/download -- download a project file
// ---------------------------------------------------------------------------

async fn download_project_file(
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilePathQuery>,
) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            )
                .into_response();
        }
    };

    if project.working_folder.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No working folder"})),
        )
            .into_response();
    }

    let file_path = match &query.path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "path required"})),
            )
                .into_response();
        }
    };

    let resolved = resolve_working_folder(&project).await;
    let full_path = PathBuf::from(&resolved).join(&file_path);

    // Prevent path traversal
    let resolved_canon = std::fs::canonicalize(&resolved)
        .unwrap_or_else(|_| PathBuf::from(&resolved));
    if let Ok(full_canon) = std::fs::canonicalize(&full_path) {
        if !full_canon
            .to_string_lossy()
            .starts_with(&*resolved_canon.to_string_lossy())
        {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Invalid path"})),
            )
                .into_response();
        }
    }

    if !full_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "File not found"})),
        )
            .into_response();
    }

    let file_name = full_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());

    match std::fs::read(&full_path) {
        Ok(data) => {
            let headers = [
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", file_name),
                ),
                (
                    axum::http::header::CONTENT_TYPE,
                    "application/octet-stream".to_string(),
                ),
            ];
            (headers, data).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /:id/files/sandbox-path -- return sandbox-relative path
// ---------------------------------------------------------------------------

async fn sandbox_path(
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilePathQuery>,
) -> impl IntoResponse {
    let projects = get_projects().await;
    let (_, project) = match find_project(&projects, &id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Project not found"})),
            );
        }
    };

    if project.working_folder.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No working folder"})),
        );
    }

    let file_path = match &query.path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "path required"})),
            );
        }
    };

    let resolved = resolve_working_folder(&project).await;
    let rel_path = project_file_rel_path(&resolved, &file_path).await;

    (StatusCode::OK, Json(json!({"sandboxPath": rel_path})))
}
