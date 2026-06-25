use std::sync::Arc;

use axum::{
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::Value;

use crate::server::services::vpn;
use crate::server::AppState;

/// /api/vpn/* — control + status for the optional Tailscale VPN used as a
/// private alternative to the Cloudflare tunnel for remote connect.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/status", get(status))
        .route("/start", post(start))
        .route("/stop", post(stop))
}

fn port() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3001)
}

async fn status() -> Json<Value> {
    Json(vpn::get_vpn_state().await)
}

async fn start() -> Json<Value> {
    Json(vpn::start_vpn(port()).await)
}

async fn stop() -> Json<Value> {
    Json(vpn::stop_vpn().await)
}
