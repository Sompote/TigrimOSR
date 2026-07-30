use crate::server::AppState;
use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::sync::Arc;

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
