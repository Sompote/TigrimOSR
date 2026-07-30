use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Multipart, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::fs;
use tokio_util::io::ReaderStream;

use crate::server::data::*;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Query / body types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MountQuery {
    #[serde(rename = "mountId")]
    pub mount_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WriteBody {
    #[serde(rename = "mountId")]
    pub mount_id: Option<String>,
    pub path: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MkdirBody {
    #[serde(rename = "mountId")]
    pub mount_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ValidatePathBody {
    pub path: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve `requested` relative to `mount_path` and ensure it stays inside.
fn validate_local_path(mount_path: &str, requested: &str) -> Result<PathBuf, String> {
    let root = Path::new(mount_path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(mount_path));
    let joined = root.join(requested);
    let resolved = joined.canonicalize().unwrap_or(joined);
    if !resolved.starts_with(&root) {
        return Err("Access denied: path outside mounted directory".into());
    }
    Ok(resolved)
}

/// Look up an enabled mount by id; optionally require write permission.
async fn get_mount(
    mount_id: &str,
    require_write: bool,
) -> Result<LocalFileMount, (StatusCode, Json<Value>)> {
    let settings = get_settings().await;
    let mount = settings
        .local_file_mounts
        .unwrap_or_default()
        .into_iter()
        .find(|m| m.id == mount_id && m.enabled);

    match mount {
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Mount not found or disabled"})),
        )),
        Some(m) => {
            if require_write && m.permissions != "readwrite" {
                Err((
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": "Write permission denied for this mount"})),
                ))
            } else {
                Ok(m)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET / -- list all enabled mounts
async fn list_mounts() -> impl IntoResponse {
    let settings = get_settings().await;
    let mounts: Vec<Value> = settings
        .local_file_mounts
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.enabled)
        .map(|m| {
            json!({
                "id": m.id,
                "label": m.label,
                "path": m.path,
                "permissions": m.permissions,
            })
        })
        .collect();
    Json(json!(mounts))
}

/// GET /browse -- browse files in a mount directory
async fn browse(Query(q): Query<MountQuery>) -> impl IntoResponse {
    let mount_id = match &q.mount_id {
        Some(id) => id.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId required"})),
            )
                .into_response();
        }
    };

    let mount = match get_mount(&mount_id, false).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let sub_path = q.path.clone().unwrap_or_default();
    let resolved = match validate_local_path(&mount.path, &sub_path) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response(),
    };

    // If path doesn't exist, return empty list
    if fs::metadata(&resolved).await.is_err() {
        return Json(json!([])).into_response();
    }

    let mut entries_out: Vec<Value> = Vec::new();
    let mut dir = match fs::read_dir(&resolved).await {
        Ok(d) => d,
        Err(_) => return Json(json!([])).into_response(),
    };

    while let Ok(Some(entry)) = dir.next_entry().await {
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let entry_path = if sub_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", sub_path, name)
        };
        let modified = meta
            .modified()
            .ok()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.to_rfc3339()
            })
            .unwrap_or_default();

        entries_out.push(json!({
            "name": name,
            "path": entry_path,
            "isDirectory": meta.is_dir(),
            "size": if meta.is_dir() { 0 } else { meta.len() },
            "modified": modified,
        }));
    }

    Json(json!(entries_out)).into_response()
}

/// GET /read -- read file content
async fn read_file(Query(q): Query<MountQuery>) -> impl IntoResponse {
    let mount_id = match &q.mount_id {
        Some(id) => id.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };
    let file_path = match &q.path {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };

    let mount = match get_mount(&mount_id, false).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let resolved = match validate_local_path(&mount.path, &file_path) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response(),
    };

    if fs::metadata(&resolved).await.is_err() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "File not found"})),
        )
            .into_response();
    }

    match fs::read_to_string(&resolved).await {
        Ok(content) => Json(json!({"content": content, "path": file_path, "mountId": mount_id}))
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /write -- write file content (requires readwrite)
async fn write_file(Json(body): Json<WriteBody>) -> impl IntoResponse {
    let mount_id = match &body.mount_id {
        Some(id) => id.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };
    let file_path = match &body.path {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };

    let mount = match get_mount(&mount_id, true).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let resolved = match validate_local_path(&mount.path, &file_path) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response(),
    };

    // Ensure parent directory exists
    if let Some(parent) = resolved.parent() {
        let _ = fs::create_dir_all(parent).await;
    }

    let content = body.content.clone().unwrap_or_default();
    match fs::write(&resolved, content).await {
        Ok(_) => Json(json!({"success": true, "path": file_path})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE / -- delete file or directory (requires readwrite)
async fn delete_file(Query(q): Query<MountQuery>) -> impl IntoResponse {
    let mount_id = match &q.mount_id {
        Some(id) => id.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };
    let file_path = match &q.path {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };

    let mount = match get_mount(&mount_id, true).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let resolved = match validate_local_path(&mount.path, &file_path) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response(),
    };

    let meta = match fs::metadata(&resolved).await {
        Ok(m) => m,
        Err(e) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))).into_response()
        }
    };

    let result = if meta.is_dir() {
        fs::remove_dir_all(&resolved).await
    } else {
        fs::remove_file(&resolved).await
    };

    match result {
        Ok(_) => Json(json!({"success": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /mkdir -- create directory (requires readwrite)
async fn mkdir(Json(body): Json<MkdirBody>) -> impl IntoResponse {
    let mount_id = match &body.mount_id {
        Some(id) => id.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };
    let dir_path = match &body.path {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };

    let mount = match get_mount(&mount_id, true).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let resolved = match validate_local_path(&mount.path, &dir_path) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response(),
    };

    let _ = fs::create_dir_all(&resolved).await;
    Json(json!({"success": true, "path": dir_path})).into_response()
}

/// GET /download -- stream file download
async fn download(Query(q): Query<MountQuery>) -> impl IntoResponse {
    let mount_id = match &q.mount_id {
        Some(id) => id.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };
    let file_path = match &q.path {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId and path required"})),
            )
                .into_response();
        }
    };

    let mount = match get_mount(&mount_id, false).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let resolved = match validate_local_path(&mount.path, &file_path) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response(),
    };

    let file_name = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());

    let file = match tokio::fs::File::open(&resolved).await {
        Ok(f) => f,
        Err(e) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))).into_response()
        }
    };

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let disposition = format!("attachment; filename=\"{}\"", file_name);
    (StatusCode::OK, [("content-disposition", disposition)], body).into_response()
}

/// POST /upload -- multipart file upload (requires readwrite)
async fn upload(mut multipart: Multipart) -> impl IntoResponse {
    let mut mount_id: Option<String> = None;
    let mut dest_dir: Option<String> = None;
    let mut file_name: Option<String> = None;
    let mut file_data: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "mountId" => {
                mount_id = field.text().await.ok();
            }
            "path" => {
                dest_dir = field.text().await.ok();
            }
            "file" => {
                file_name = field.file_name().map(|s| s.to_string());
                file_data = field.bytes().await.ok().map(|b| b.to_vec());
            }
            _ => {
                // If the field has a filename, treat it as the file upload
                if field.file_name().is_some() {
                    file_name = field.file_name().map(|s| s.to_string());
                    file_data = field.bytes().await.ok().map(|b| b.to_vec());
                } else {
                    let _ = field.bytes().await; // consume
                }
            }
        }
    }

    let mount_id = match mount_id {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mountId required"})),
            )
                .into_response();
        }
    };

    let original_name = match file_name {
        Some(n) => n,
        None => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "No file"}))).into_response();
        }
    };

    let data = match file_data {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "No file data"})),
            )
                .into_response();
        }
    };

    let mount = match get_mount(&mount_id, true).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let dir_part = dest_dir.unwrap_or_default();
    let dest_path = if dir_part.is_empty() {
        original_name.clone()
    } else {
        format!("{}/{}", dir_part, original_name)
    };

    let resolved = match validate_local_path(&mount.path, &dest_path) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response(),
    };

    if let Some(parent) = resolved.parent() {
        let _ = fs::create_dir_all(parent).await;
    }

    match fs::write(&resolved, data).await {
        Ok(_) => Json(json!({"success": true, "path": dest_path})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /validate-path -- check if a path exists on the filesystem
async fn validate_path_handler(Json(body): Json<ValidatePathBody>) -> impl IntoResponse {
    let dir_path = match &body.path {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "path required"})),
            )
                .into_response();
        }
    };

    let resolved = Path::new(&dir_path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&dir_path));

    match fs::metadata(&resolved).await {
        Ok(meta) => Json(json!({
            "exists": true,
            "isDirectory": meta.is_dir(),
            "resolvedPath": resolved.to_string_lossy(),
        }))
        .into_response(),
        Err(_) => Json(json!({
            "exists": false,
            "isDirectory": false,
            "resolvedPath": resolved.to_string_lossy(),
        }))
        .into_response(),
    }
}

/// GET /detect-shares -- detect shared/mounted folders
async fn detect_shares() -> impl IntoResponse {
    let mut detected: Vec<Value> = Vec::new();

    // On macOS, check /Volumes and common paths
    // On Linux, parse /proc/mounts and scan /mnt, /media, etc.
    let is_macos = cfg!(target_os = "macos");

    if is_macos {
        // macOS: scan /Volumes for mounted shares
        let scan_roots = vec!["/Volumes"];
        let skip_names: std::collections::HashSet<&str> =
            ["Macintosh HD", "Recovery", ".timemachine"]
                .iter()
                .copied()
                .collect();

        for root in &scan_roots {
            let root_path = Path::new(root);
            if !root_path.exists() || !root_path.is_dir() {
                continue;
            }
            let mut dir = match fs::read_dir(root_path).await {
                Ok(d) => d,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = dir.next_entry().await {
                let meta = match entry.metadata().await {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if !meta.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if skip_names.contains(name.as_str()) || name.starts_with('.') {
                    continue;
                }
                let full_path = entry.path().to_string_lossy().to_string();
                if detected.iter().any(|d| d["path"] == full_path) {
                    continue;
                }
                // Test write permission
                let permissions = test_write_permission(&full_path).await;
                detected.push(json!({
                    "path": full_path,
                    "label": name,
                    "source": "Volumes",
                    "permissions": permissions,
                }));
            }
        }
    } else {
        // Linux: try /proc/mounts for VM share filesystems
        let vm_fs_types: std::collections::HashSet<&str> =
            ["9p", "virtiofs", "vboxsf", "fuse.vmhgfs-fuse"]
                .iter()
                .copied()
                .collect();

        if let Ok(content) = fs::read_to_string("/proc/mounts").await {
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 4 {
                    continue;
                }
                let (tag, mount_point, fs_type, options) = (parts[0], parts[1], parts[2], parts[3]);
                if !vm_fs_types.contains(fs_type) {
                    continue;
                }
                if detected.iter().any(|d| d["path"] == mount_point) {
                    continue;
                }
                let is_ro = options.split(',').any(|o| o == "ro");
                let label = if ["none", "share"].contains(&tag) || tag.parse::<u64>().is_ok() {
                    Path::new(mount_point)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| mount_point.to_string())
                } else {
                    tag.to_string()
                };
                detected.push(json!({
                    "path": mount_point,
                    "label": label,
                    "source": fs_type,
                    "permissions": if is_ro { "read" } else { "readwrite" },
                }));
            }
        }

        // Scan common mount roots
        let scan_roots = vec![
            "/mnt",
            "/media",
            "/media/share",
            "/media/psf",
            "/shared",
            "/host",
        ];
        let skip_names: std::collections::HashSet<&str> =
            ["cdrom", "floppy", "removable"].iter().copied().collect();

        for root in &scan_roots {
            let root_path = Path::new(root);
            if !root_path.exists() || !root_path.is_dir() {
                continue;
            }
            let mut dir = match fs::read_dir(root_path).await {
                Ok(d) => d,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = dir.next_entry().await {
                let meta = match entry.metadata().await {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if !meta.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if skip_names.contains(name.as_str()) || name.starts_with('.') {
                    continue;
                }
                let full_path = entry.path().to_string_lossy().to_string();
                if detected.iter().any(|d| d["path"] == full_path) {
                    continue;
                }
                let permissions = test_write_permission(&full_path).await;
                detected.push(json!({
                    "path": full_path,
                    "label": name,
                    "source": format!("Shared ({})", root),
                    "permissions": permissions,
                }));
            }
        }
    }

    Json(json!(detected))
}

/// Test if a directory is writable by attempting to create and remove a temp file.
async fn test_write_permission(dir_path: &str) -> &'static str {
    let test_file = format!(
        "{}/.andrewos_write_test_{}",
        dir_path,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    match fs::write(&test_file, b"").await {
        Ok(_) => {
            let _ = fs::remove_file(&test_file).await;
            "readwrite"
        }
        Err(_) => "read",
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_mounts))
        .route("/", delete(delete_file))
        .route("/browse", get(browse))
        .route("/read", get(read_file))
        .route("/write", post(write_file))
        .route("/mkdir", post(mkdir))
        .route("/download", get(download))
        .route("/upload", post(upload))
        .route("/validate-path", post(validate_path_handler))
        .route("/detect-shares", get(detect_shares))
}
