pub mod auth;
pub mod chat;
pub mod files;
pub mod tasks;
pub mod skills;
pub mod settings;
pub mod projects;
pub mod agents;
pub mod agent_loops;
pub mod custom_tools;
pub mod terminal;
pub mod local_files;
pub mod python;
pub mod tools;
pub mod clawhub;
pub mod google;
pub mod remote;
pub mod vpn;
pub mod plugins;
pub mod messaging;
pub mod web_ui;

use std::sync::Arc;
use axum::Router;
use crate::server::AppState;

pub fn build_api_routes(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .nest("/chat", chat::router())
        .nest("/files", files::router())
        .nest("/tasks", tasks::router())
        .nest("/skills", skills::router())
        .nest("/settings", settings::router())
        .nest("/projects", projects::router())
        .nest("/agents", agents::router())
        .nest("/agent-loops", agent_loops::router())
        .nest("/custom-tools", custom_tools::router())
        .nest("/terminal", terminal::router())
        .nest("/local-files", local_files::router())
        .nest("/python", python::router())
        .nest("/tools", tools::router())
        .nest("/clawhub", clawhub::router())
        .nest("/google", google::router())
        .nest("/remote", remote::router())
        .nest("/vpn", vpn::router())
        .nest("/plugins", plugins::router())
        .nest("/messaging", messaging::api_router())
}
