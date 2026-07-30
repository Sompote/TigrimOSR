//! Workflow-graph routes.
//!
//! Exposes the built-in orchestration patterns and lets a caller preview a
//! pattern's topology or validate a custom graph before running it. Preview and
//! validation are deliberately separate from execution so a UI can draw the
//! graph, and catch a cycle or a bad node reference, without spending a single
//! model call.

use std::sync::Arc;

use axum::{
    extract::{Path, Query},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::services::workflow::{
    build_pattern, pattern_catalog, WorkflowProfile,
};
use crate::server::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/patterns", get(list_patterns))
        .route("/patterns/{name}", get(preview_pattern))
        .route("/validate", post(validate_profile))
}

/// Every built-in pattern, for a mode picker.
async fn list_patterns() -> Json<Value> {
    let patterns: Vec<Value> = pattern_catalog()
        .into_iter()
        .map(|(id, description)| {
            // Build at a representative width so the picker can show shape
            // without the caller having to ask separately.
            let built = build_pattern(id, 3).ok();
            json!({
                "id": id,
                "description": description,
                "name": built.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
                "nodeCount": built.as_ref().map(|p| p.nodes.len()).unwrap_or(0),
                "supportsWidth": id != "loop_until_done",
            })
        })
        .collect();
    Json(json!({ "success": true, "patterns": patterns }))
}

#[derive(Deserialize)]
struct WidthQuery {
    width: Option<usize>,
}

/// Build a pattern and return its topology, including the concurrency levels
/// so a UI can render it as a graph.
async fn preview_pattern(Path(name): Path<String>, Query(q): Query<WidthQuery>) -> Json<Value> {
    let width = q.width.unwrap_or(3);
    match build_pattern(&name, width) {
        Ok(profile) => Json(json!({ "success": true, "profile": describe(&profile) })),
        Err(e) => Json(json!({ "success": false, "error": e })),
    }
}

/// Validate a custom graph: catches cycles, unknown inputs and duplicate names
/// before any model call is made.
async fn validate_profile(Json(profile): Json<WorkflowProfile>) -> Json<Value> {
    match profile.levels() {
        Ok(_) => Json(json!({ "success": true, "profile": describe(&profile) })),
        Err(e) => Json(json!({ "success": false, "error": e })),
    }
}

/// Shared shape: the profile plus its derived execution plan.
fn describe(profile: &WorkflowProfile) -> Value {
    let levels: Vec<Vec<String>> = profile
        .levels()
        .map(|ls| {
            ls.into_iter()
                .map(|l| l.into_iter().map(|i| profile.nodes[i].name.clone()).collect())
                .collect()
        })
        .unwrap_or_default();
    json!({
        "name": profile.name,
        "description": profile.description,
        "pattern": profile.pattern,
        "nodes": profile.nodes,
        "terminals": profile.terminal_nodes(),
        // Each inner list runs concurrently; lists run in order.
        "levels": levels,
    })
}
