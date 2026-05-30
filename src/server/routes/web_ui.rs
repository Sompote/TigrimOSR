use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
    Router,
    routing::get,
};
use std::sync::Arc;
use crate::server::AppState;

// Embed the SPA at compile time
const INDEX_HTML: &str = include_str!("../../../static/index.html");

async fn serve_index() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
        ],
        INDEX_HTML,
    )
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(serve_index))
        .route("/index.html", get(serve_index))
}
