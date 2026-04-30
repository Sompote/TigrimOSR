pub mod auth;
pub mod chat;
pub mod files;
pub mod tasks;
pub mod skills;
pub mod settings;
pub mod projects;
pub mod agents;
pub mod terminal;
pub mod local_files;
pub mod python;
pub mod tools;
pub mod clawhub;
pub mod remote;

use std::sync::Arc;
use axum::Router;
use crate::server::AppState;

pub fn build_api_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .nest("/chat", chat::router())
        .nest("/files", files::router())
        .nest("/tasks", tasks::router())
        .nest("/skills", skills::router())
        .nest("/settings", settings::router())
        .nest("/projects", projects::router())
        .nest("/agents", agents::router())
        .nest("/terminal", terminal::router())
        .nest("/local-files", local_files::router())
        .nest("/python", python::router())
        .nest("/tools", tools::router())
        .nest("/clawhub", clawhub::router())
        .nest("/remote", remote::router())
}
