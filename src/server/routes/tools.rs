use std::sync::Arc;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::data::*;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct WebSearchBody {
    query: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FetchBody {
    url: Option<String>,
    method: Option<String>,
    headers: Option<std::collections::HashMap<String, String>>,
    body: Option<Value>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /web-search — web search stub returning empty results
async fn web_search(Json(body): Json<WebSearchBody>) -> impl IntoResponse {
    let query = match &body.query {
        Some(q) if !q.is_empty() => q.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "query required"})),
            )
        }
    };

    let settings = get_settings().await;
    if !settings.web_search_enabled {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Web search not enabled. Configure in Settings."})),
        );
    }

    // Stub: return empty results
    // Full implementation would call Google Custom Search or DuckDuckGo
    (
        StatusCode::OK,
        Json(json!({
            "results": [],
            "query": query,
            "note": "Web search not yet fully implemented in Rust backend"
        })),
    )
}

/// POST /fetch — fetch a URL (internet access proxy)
async fn fetch_url(Json(body): Json<FetchBody>) -> impl IntoResponse {
    let url = match &body.url {
        Some(u) if !u.is_empty() => u.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "url required"})),
            )
        }
    };

    let method = body.method.as_deref().unwrap_or("GET");

    let client = reqwest::Client::new();
    let mut req_builder = match method.to_uppercase().as_str() {
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        "PATCH" => client.patch(&url),
        "HEAD" => client.head(&url),
        _ => client.get(&url),
    };

    // Add custom headers
    if let Some(ref headers) = body.headers {
        for (k, v) in headers {
            req_builder = req_builder.header(k.as_str(), v.as_str());
        }
    }

    // Add body if present
    if let Some(ref req_body) = body.body {
        req_builder = req_builder.json(req_body);
    }

    match req_builder.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let ok = response.status().is_success();
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            if content_type.contains("json") {
                match response.json::<Value>().await {
                    Ok(data) => (
                        StatusCode::OK,
                        Json(json!({"ok": ok, "status": status, "data": data})),
                    ),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    ),
                }
            } else {
                match response.text().await {
                    Ok(mut text) => {
                        // Truncate very large responses
                        if text.len() > 50000 {
                            text.truncate(50000);
                            text.push_str("\n...(truncated)");
                        }
                        (
                            StatusCode::OK,
                            Json(json!({"ok": ok, "status": status, "data": text})),
                        )
                    }
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    ),
                }
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// POST /mcp/:toolName — MCP tool proxy
async fn mcp_proxy(
    Path(tool_name): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let settings = get_settings().await;

    let tool = settings
        .mcp_tools
        .iter()
        .find(|t| t.name == tool_name && t.enabled);

    let tool = match tool {
        Some(t) => t.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Tool not found or disabled"})),
            )
        }
    };

    let client = reqwest::Client::new();
    match client
        .post(&tool.url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(response) => match response.json::<Value>().await {
            Ok(data) => (StatusCode::OK, Json(data)),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            ),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/web-search", post(web_search))
        .route("/fetch", post(fetch_url))
        .route("/mcp/{toolName}", post(mcp_proxy))
}
