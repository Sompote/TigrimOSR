use axum::{
    extract::{Path, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::services::clawhub;

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct InstallBody {
    slug: String,
    force: Option<bool>,
}

/// GET /clawhub/skills — list installed skills
async fn list_skills() -> impl IntoResponse {
    let result = clawhub::list_installed_skills().await;
    Json(result)
}

/// GET /clawhub/skills/:name — read a skill's SKILL.md
async fn get_skill(Path(name): Path<String>) -> impl IntoResponse {
    let result = clawhub::read_skill(&name).await;
    let status = if result["ok"].as_bool().unwrap_or(false) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    (status, Json(result))
}

/// GET /clawhub/search?q=...&limit=... — search clawhub catalog
async fn search(Query(params): Query<SearchQuery>) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(10);
    let result = clawhub::clawhub_search(&params.q, limit).await;
    Json(result)
}

/// GET /clawhub/info/:slug — get skill info from clawhub
async fn info(Path(slug): Path<String>) -> impl IntoResponse {
    let result = clawhub::clawhub_info(&slug).await;
    Json(result)
}

/// POST /clawhub/install — install a skill from clawhub
async fn install(Json(body): Json<InstallBody>) -> impl IntoResponse {
    let result = clawhub::clawhub_install(&body.slug, body.force.unwrap_or(false)).await;
    let status = if result["ok"].as_bool().unwrap_or(false) {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, Json(result))
}

pub fn router() -> Router<std::sync::Arc<crate::server::AppState>> {
    Router::new()
        .route("/skills", get(list_skills))
        .route("/skills/{name}", get(get_skill))
        .route("/search", get(search))
        .route("/info/{slug}", get(info))
        .route("/install", post(install))
}
