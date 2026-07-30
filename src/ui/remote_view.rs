use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::Value;

use crate::server::data::{get_settings, RemoteInstance};
use crate::util::floor_char_boundary;

// ---------------------------------------------------------------------------
// Remote sub-tab
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteTab {
    Chat,
    Files,
    Terminal,
    Agents,
    Tasks,
    Settings,
}

// ---------------------------------------------------------------------------
// Async result wrapper
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AsyncResult<T: Clone> {
    data: Arc<Mutex<Option<T>>>,
    loading: Arc<Mutex<bool>>,
}

impl<T: Clone> AsyncResult<T> {
    fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(None)),
            loading: Arc::new(Mutex::new(false)),
        }
    }

    fn is_loading(&self) -> bool {
        *self.loading.lock().unwrap()
    }

    fn take(&self) -> Option<T> {
        self.data.lock().unwrap().clone()
    }

    fn set_loading(&self) {
        *self.loading.lock().unwrap() = true;
    }

    fn set_done(&self, val: T) {
        *self.data.lock().unwrap() = Some(val);
        *self.loading.lock().unwrap() = false;
    }
}

// ---------------------------------------------------------------------------
// Main view
// ---------------------------------------------------------------------------

pub struct RemoteView {
    instances: Vec<RemoteInstance>,
    selected_idx: usize,
    instances_loaded: bool,

    tab: RemoteTab,

    // Connection status
    connected: AsyncResult<bool>,
    last_ping: Option<Instant>,
    latency_ms: Option<u64>,

    // Remote tasks
    remote_tasks: AsyncResult<Vec<Value>>,
    task_input: String,
    expanded_task_id: Option<String>,

    // WebSocket live updates for tasks
    ws_tasks: Arc<Mutex<Vec<Value>>>,
    ws_connected: Arc<std::sync::atomic::AtomicBool>,
    ws_url_connected: Mutex<String>, // track which URL the WS is connected to

    // Remote files
    remote_files: AsyncResult<Vec<Value>>,
    remote_file_path: String,
    remote_file_content: AsyncResult<String>,

    // Remote chat
    chat_sessions: AsyncResult<Vec<Value>>,
    chat_selected_session: Option<String>,
    chat_messages: AsyncResult<Vec<Value>>,
    chat_input: String,
    chat_response: AsyncResult<String>,

    // Remote settings
    remote_settings: AsyncResult<Value>,

    // Remote terminal
    term_input: String,
    term_output: Vec<Value>,
    term_result: AsyncResult<Value>,

    // Remote agents
    remote_agents: AsyncResult<Vec<Value>>,
}

impl RemoteView {
    pub fn new() -> Self {
        Self {
            instances: vec![],
            selected_idx: 0,
            instances_loaded: false,
            tab: RemoteTab::Chat,
            connected: AsyncResult::new(),
            last_ping: None,
            latency_ms: None,
            remote_tasks: AsyncResult::new(),
            task_input: String::new(),
            expanded_task_id: None,
            ws_tasks: Arc::new(Mutex::new(vec![])),
            ws_connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ws_url_connected: Mutex::new(String::new()),
            remote_files: AsyncResult::new(),
            remote_file_path: String::new(),
            remote_file_content: AsyncResult::new(),
            chat_sessions: AsyncResult::new(),
            chat_selected_session: None,
            chat_messages: AsyncResult::new(),
            chat_input: String::new(),
            chat_response: AsyncResult::new(),
            remote_settings: AsyncResult::new(),
            term_input: String::new(),
            term_output: vec![],
            term_result: AsyncResult::new(),
            remote_agents: AsyncResult::new(),
        }
    }

    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        self.connected.take().unwrap_or(false)
    }

    pub fn instance_name(&self) -> String {
        self.selected_instance()
            .map(|i| i.name.clone())
            .unwrap_or_default()
    }

    pub fn has_instances(&self) -> bool {
        !self.instances.is_empty()
    }

    pub fn get_instances(&self) -> &[crate::server::data::RemoteInstance] {
        &self.instances
    }

    pub fn selected_remote_backend(&self) -> Option<crate::server::data::RemoteBackend> {
        self.selected_instance()
            .map(|i| crate::server::data::RemoteBackend {
                url: i.url.trim_end_matches('/').to_string(),
                token: i.token.clone(),
            })
    }

    pub fn ensure_instances_loaded(&mut self, runtime: &tokio::runtime::Handle) {
        if !self.instances_loaded {
            self.load_instances(runtime);
        }
    }

    pub fn ensure_loaded(&mut self, runtime: &tokio::runtime::Handle) {
        if !self.instances_loaded {
            self.load_instances(runtime);
        }
    }

    fn selected_instance(&self) -> Option<&RemoteInstance> {
        self.instances.get(self.selected_idx)
    }

    fn base_url(&self) -> Option<String> {
        self.selected_instance().map(|i| {
            let url = i.url.trim_end_matches('/').to_string();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                format!("http://{}", url)
            } else {
                url
            }
        })
    }

    fn token(&self) -> Option<String> {
        self.selected_instance().map(|i| i.token.clone())
    }

    // HTTP GET helper
    fn remote_get(&self, runtime: &tokio::runtime::Handle, path: &str, result: AsyncResult<Value>) {
        let Some(base) = self.base_url() else { return };
        let Some(token) = self.token() else { return };
        let url = format!("{}{}", base, path);
        result.set_loading();

        runtime.spawn(async move {
            let client = reqwest::Client::new();
            let resp = client
                .get(&url)
                .header("Authorization", format!("Bearer {}", token))
                .timeout(Duration::from_secs(15))
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let json = r.json::<Value>().await.unwrap_or(Value::Null);
                    result.set_done(json);
                }
                Err(_) => {
                    result.set_done(Value::Null);
                }
            }
        });
    }

    // HTTP POST helper
    fn remote_post(
        &self,
        runtime: &tokio::runtime::Handle,
        path: &str,
        body: Value,
        result: AsyncResult<Value>,
    ) {
        let Some(base) = self.base_url() else { return };
        let Some(token) = self.token() else { return };
        let url = format!("{}{}", base, path);
        result.set_loading();

        runtime.spawn(async move {
            let client = reqwest::Client::new();
            let resp = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", token))
                .json(&body)
                .timeout(Duration::from_secs(120))
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let json = r.json::<Value>().await.unwrap_or(Value::Null);
                    result.set_done(json);
                }
                Err(_) => {
                    result.set_done(Value::Null);
                }
            }
        });
    }

    // Ping the remote server
    fn check_connection(&mut self, runtime: &tokio::runtime::Handle) {
        let Some(base) = self.base_url() else { return };
        let Some(token) = self.token() else { return };
        let result = self.connected.clone();
        result.set_loading();
        let start = Instant::now();

        let latency_holder: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let lat = latency_holder.clone();

        runtime.spawn(async move {
            let client = reqwest::Client::new();
            let resp = client
                .post(format!("{}/api/auth/verify", base))
                .json(&serde_json::json!({ "token": token }))
                .timeout(Duration::from_secs(10))
                .send()
                .await;
            let elapsed = start.elapsed().as_millis() as u64;
            *lat.lock().unwrap() = Some(elapsed);
            match resp {
                Ok(r) => result.set_done(r.status().is_success()),
                Err(_) => result.set_done(false),
            }
        });

        self.last_ping = Some(start);
    }

    fn load_instances(&mut self, runtime: &tokio::runtime::Handle) {
        let settings = runtime.block_on(get_settings());
        self.instances = settings.remote_instances.unwrap_or_default();
        self.instances_loaded = true;
        if !self.instances.is_empty() {
            self.check_connection(runtime);
        }
    }

    // Fetch remote tasks
    fn fetch_tasks(&self, runtime: &tokio::runtime::Handle) {
        let Some(base) = self.base_url() else { return };
        let Some(token) = self.token() else { return };
        let result = self.remote_tasks.clone();
        result.set_loading();

        runtime.spawn(async move {
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("{}/api/remote/tasks", base))
                .header("Authorization", format!("Bearer {}", token))
                .timeout(Duration::from_secs(15))
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let json = r.json::<Value>().await.unwrap_or(Value::Null);
                    let tasks = json["tasks"]
                        .as_array()
                        .cloned()
                        .or_else(|| json.as_array().cloned())
                        .unwrap_or_default();
                    result.set_done(tasks);
                }
                Err(_) => result.set_done(vec![]),
            }
        });
    }

    // Connect WebSocket for live task updates
    fn connect_task_ws(&self, runtime: &tokio::runtime::Handle, ctx: &egui::Context) {
        let Some(base) = self.base_url() else { return };
        let Some(token) = self.token() else { return };

        // Already connected to this URL?
        {
            let connected_url = self.ws_url_connected.lock().unwrap();
            if *connected_url == base
                && self.ws_connected.load(std::sync::atomic::Ordering::Relaxed)
            {
                return;
            }
        }

        // Mark as connecting
        {
            let mut connected_url = self.ws_url_connected.lock().unwrap();
            *connected_url = base.clone();
        }

        let ws_url = base
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        let ws_url = format!(
            "{}/api/remote/ws?token={}",
            ws_url,
            urlencoding::encode(&token)
        );
        let ws_tasks = self.ws_tasks.clone();
        let ws_connected = self.ws_connected.clone();
        let ctx = ctx.clone();

        runtime.spawn(async move {
            use futures_util::StreamExt;

            let connect_result = tokio_tungstenite::connect_async(&ws_url).await;
            let (ws_stream, _) = match connect_result {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("[WS] Failed to connect to {}: {}", ws_url, e);
                    ws_connected.store(false, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
            };

            ws_connected.store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("[WS] Connected to {}", ws_url);

            let (_write, mut read) = ws_stream.split();

            while let Some(msg) = read.next().await {
                let msg = match msg {
                    Ok(m) => m,
                    Err(_) => break,
                };

                if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                    if let Ok(json) = serde_json::from_str::<Value>(&text) {
                        let msg_type = json["type"].as_str().unwrap_or("");
                        match msg_type {
                            "snapshot" => {
                                // Full task list replacement
                                if let Some(tasks) = json["tasks"].as_array() {
                                    let mut guard = ws_tasks.lock().unwrap();
                                    *guard = tasks.clone();
                                    ctx.request_repaint();
                                }
                            }
                            "task_update" => {
                                // Upsert a single task
                                if let Some(task) = json.get("task") {
                                    let tid = task["id"].as_str().unwrap_or("");
                                    let mut guard = ws_tasks.lock().unwrap();
                                    if let Some(existing) =
                                        guard.iter_mut().find(|t| t["id"].as_str() == Some(tid))
                                    {
                                        *existing = task.clone();
                                    } else {
                                        guard.insert(0, task.clone());
                                    }
                                    ctx.request_repaint();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            ws_connected.store(false, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("[WS] Disconnected from {}", ws_url);
            ctx.request_repaint(); // Trigger UI to reconnect on next frame
        });
    }

    // Fetch remote files
    fn fetch_files(&self, runtime: &tokio::runtime::Handle) {
        let Some(base) = self.base_url() else { return };
        let Some(token) = self.token() else { return };
        let path = if self.remote_file_path.is_empty() {
            "/".to_string()
        } else {
            self.remote_file_path.clone()
        };
        let result = self.remote_files.clone();
        result.set_loading();

        runtime.spawn(async move {
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("{}/api/files/{}", base, path))
                .header("Authorization", format!("Bearer {}", token))
                .timeout(Duration::from_secs(15))
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let json = r.json::<Value>().await.unwrap_or(Value::Null);
                    let files = json["files"]
                        .as_array()
                        .cloned()
                        .or_else(|| json.as_array().cloned())
                        .unwrap_or_default();
                    result.set_done(files);
                }
                Err(_) => result.set_done(vec![]),
            }
        });
    }

    // Fetch remote chat sessions
    fn fetch_chat_sessions(&self, runtime: &tokio::runtime::Handle) {
        let Some(base) = self.base_url() else { return };
        let Some(token) = self.token() else { return };
        let result = self.chat_sessions.clone();
        result.set_loading();

        runtime.spawn(async move {
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("{}/api/chat/sessions", base))
                .header("Authorization", format!("Bearer {}", token))
                .timeout(Duration::from_secs(15))
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let json = r.json::<Value>().await.unwrap_or(Value::Null);
                    let sessions = json.as_array().cloned().unwrap_or_default();
                    result.set_done(sessions);
                }
                Err(_) => result.set_done(vec![]),
            }
        });
    }

    pub fn show(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        if !self.instances_loaded {
            self.load_instances(runtime);
        }

        let accent = crate::ui::theme::accent_color();
        let text_dim = egui::Color32::from_rgb(139, 148, 158);

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Remote Server")
                    .size(18.0)
                    .strong()
                    .color(egui::Color32::from_rgb(36, 41, 47)),
            );
        });

        ui.add_space(8.0);

        // Instance selector
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Server:").size(13.0).color(text_dim));

            if self.instances.is_empty() {
                ui.label(
                    egui::RichText::new("No remote instances configured. Add one in Settings.")
                        .size(13.0)
                        .color(egui::Color32::from_rgb(220, 38, 38)),
                );
                return;
            }

            let prev_idx = self.selected_idx;
            egui::ComboBox::from_id_salt("remote_instance_selector")
                .width(300.0)
                .selected_text(
                    self.selected_instance()
                        .map(|i| format!("{} ({})", i.name, i.url))
                        .unwrap_or_default(),
                )
                .show_ui(ui, |ui| {
                    for (idx, inst) in self.instances.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.selected_idx,
                            idx,
                            format!("{} — {}", inst.name, inst.url),
                        );
                    }
                });

            if prev_idx != self.selected_idx {
                self.check_connection(runtime);
            }

            // Connection status indicator
            if self.connected.is_loading() {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Connecting...")
                        .size(12.0)
                        .color(text_dim),
                );
            } else if let Some(ok) = self.connected.take() {
                if ok {
                    ui.label(
                        egui::RichText::new("\u{25CF}")
                            .size(14.0)
                            .color(egui::Color32::from_rgb(74, 222, 128)),
                    );
                    let lat_text = self
                        .latency_ms
                        .map(|ms| format!("Connected ({}ms)", ms))
                        .unwrap_or_else(|| "Connected".to_string());
                    ui.label(
                        egui::RichText::new(lat_text)
                            .size(12.0)
                            .color(egui::Color32::from_rgb(74, 222, 128)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("\u{25CF}")
                            .size(14.0)
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    );
                    ui.label(
                        egui::RichText::new("Disconnected")
                            .size(12.0)
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    );
                }
            }

            if ui.button("Reconnect").clicked() {
                self.check_connection(runtime);
            }

            if ui.button("Reload Instances").clicked() {
                self.load_instances(runtime);
            }
        });

        ui.add_space(8.0);
        ui.separator();

        if self.instances.is_empty() {
            return;
        }

        // Sub-tabs
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            for &(tab, label) in &[
                (RemoteTab::Chat, "Chat"),
                (RemoteTab::Files, "Files"),
                (RemoteTab::Terminal, "Terminal"),
                (RemoteTab::Agents, "Agents"),
                (RemoteTab::Tasks, "Tasks"),
                (RemoteTab::Settings, "Settings"),
            ] {
                let is_active = self.tab == tab;
                let color = if is_active { accent } else { text_dim };
                let fill = if is_active {
                    egui::Color32::from_rgba_unmultiplied(88, 166, 255, 15)
                } else {
                    egui::Color32::TRANSPARENT
                };
                let btn = egui::Button::new(egui::RichText::new(label).size(13.0).color(color))
                    .fill(fill)
                    .corner_radius(6.0);
                if ui.add(btn).clicked() {
                    self.tab = tab;
                }
            }
        });

        ui.add_space(8.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(4.0);
            ui.set_min_width(ui.available_width());

            match self.tab {
                RemoteTab::Chat => self.show_chat(ui, runtime),
                RemoteTab::Files => self.show_files(ui, runtime),
                RemoteTab::Terminal => self.show_terminal(ui, runtime),
                RemoteTab::Agents => self.show_agents(ui, runtime),
                RemoteTab::Tasks => self.show_tasks(ui, runtime),
                RemoteTab::Settings => self.show_settings(ui, runtime),
            }
        });

        // Connect WebSocket for live task updates (replaces 5s polling)
        self.connect_task_ws(runtime, ui.ctx());

        // Fallback: only poll every 30s if WebSocket is not connected
        if !self.ws_connected.load(std::sync::atomic::Ordering::Relaxed) {
            ui.ctx().request_repaint_after(Duration::from_secs(5));
        }
    }

    // -----------------------------------------------------------------------
    // Tasks tab
    // -----------------------------------------------------------------------

    fn show_tasks(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Submit Task:").size(13.0));
        });

        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.task_input)
                    .desired_width(ui.available_width() - 120.0)
                    .hint_text("Enter task for remote agent..."),
            );

            let submit = ui.button("Submit");
            if (submit.clicked()
                || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                && !self.task_input.trim().is_empty()
            {
                let task = self.task_input.trim().to_string();
                self.task_input.clear();
                let result = AsyncResult::<Value>::new();
                self.remote_post(
                    runtime,
                    "/api/remote/task",
                    serde_json::json!({ "task": task }),
                    result,
                );
                // Refresh after short delay
                self.fetch_tasks(runtime);
            }
        });

        ui.add_space(12.0);

        // Use WebSocket data if available, otherwise fallback to HTTP polling
        let ws_active = self.ws_connected.load(std::sync::atomic::Ordering::Relaxed);
        if ws_active {
            // WebSocket is live — push ws_tasks into remote_tasks for the renderer
            let ws_data = self.ws_tasks.lock().unwrap().clone();
            if !ws_data.is_empty() {
                self.remote_tasks.set_done(ws_data);
            }
        } else {
            // Fallback: HTTP polling
            if self.remote_tasks.take().is_none() && !self.remote_tasks.is_loading() {
                self.fetch_tasks(runtime);
            }
            if let Some(ping) = self.last_ping {
                if ping.elapsed() > Duration::from_secs(5) && !self.remote_tasks.is_loading() {
                    self.fetch_tasks(runtime);
                    self.last_ping = Some(Instant::now());
                }
            }
        }

        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Remote Tasks").size(15.0).strong());
            if ui.button("Refresh").clicked() {
                self.fetch_tasks(runtime);
            }
            if self.remote_tasks.is_loading() {
                ui.spinner();
            }
        });

        ui.add_space(4.0);

        if let Some(tasks) = self.remote_tasks.take() {
            if tasks.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("No remote tasks")
                            .size(13.0)
                            .color(egui::Color32::from_rgb(139, 148, 158)),
                    );
                });
            }
            for task in &tasks {
                let id = task["id"].as_str().unwrap_or("");
                let status = task["status"].as_str().unwrap_or("unknown");
                let task_text = task["task"].as_str().unwrap_or("");
                let created = task["createdAt"].as_str().unwrap_or("");
                let progress_count = task["progressCount"].as_u64().unwrap_or(0);
                let is_expanded = self.expanded_task_id.as_deref() == Some(id);

                let status_color = match status {
                    "completed" => egui::Color32::from_rgb(74, 222, 128),
                    "running" => crate::ui::theme::accent_color(),
                    "pending" => egui::Color32::from_rgb(250, 204, 21),
                    "failed" => egui::Color32::from_rgb(239, 68, 68),
                    "killed" => egui::Color32::from_rgb(156, 163, 175),
                    _ => egui::Color32::from_rgb(139, 148, 158),
                };

                ui.horizontal(|ui| {
                    ui.add_space(16.0);

                    let frame = egui::Frame::new()
                        .fill(crate::ui::theme::card_color())
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgb(208, 215, 222),
                        ))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::same(12));

                    frame.show(ui, |ui| {
                        ui.set_min_width(ui.available_width() - 40.0);

                        ui.horizontal(|ui| {
                            // Status badge
                            let badge = egui::Frame::new()
                                .fill(status_color)
                                .corner_radius(4.0)
                                .inner_margin(egui::Margin::symmetric(6, 2));
                            badge.show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(status)
                                        .size(11.0)
                                        .color(egui::Color32::WHITE)
                                        .strong(),
                                );
                            });

                            ui.label(
                                egui::RichText::new(if task_text.len() > 80 {
                                    format!(
                                        "{}...",
                                        &task_text[..floor_char_boundary(&task_text, 80)]
                                    )
                                } else {
                                    task_text.to_string()
                                })
                                .size(13.0)
                                .color(egui::Color32::from_rgb(36, 41, 47)),
                            );
                        });

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "ID: {}  |  Created: {}  |  Progress: {}",
                                    &id[..8.min(id.len())],
                                    created,
                                    progress_count
                                ))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(139, 148, 158)),
                            );

                            if ui
                                .small_button(if is_expanded { "Collapse" } else { "Expand" })
                                .clicked()
                            {
                                if is_expanded {
                                    self.expanded_task_id = None;
                                } else {
                                    self.expanded_task_id = Some(id.to_string());
                                    // Fetch full task details
                                    let detail = AsyncResult::<Value>::new();
                                    self.remote_get(
                                        runtime,
                                        &format!("/api/remote/task/{}", id),
                                        detail,
                                    );
                                }
                            }

                            if status == "running" || status == "pending" {
                                let kill_btn = egui::Button::new(
                                    egui::RichText::new("Kill")
                                        .size(11.0)
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(220, 38, 38))
                                .corner_radius(4.0);
                                if ui.add(kill_btn).clicked() {
                                    let result = AsyncResult::<Value>::new();
                                    self.remote_post(
                                        runtime,
                                        &format!("/api/remote/task/{}/kill", id),
                                        serde_json::json!({}),
                                        result,
                                    );
                                }
                            }
                        });

                        if is_expanded {
                            if let Some(result) = task.get("result").and_then(|r| r.as_str()) {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new("Result:").size(12.0).strong());
                                ui.label(
                                    egui::RichText::new(result)
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(36, 41, 47)),
                                );
                            }
                        }
                    });
                });

                ui.add_space(4.0);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Files tab
    // -----------------------------------------------------------------------

    fn show_files(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Path:").size(13.0));
            let path_edit = ui.add(
                egui::TextEdit::singleline(&mut self.remote_file_path)
                    .desired_width(400.0)
                    .hint_text("/"),
            );
            if ui.button("Browse").clicked()
                || (path_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                self.fetch_files(runtime);
            }
            if ui.button("Root").clicked() {
                self.remote_file_path.clear();
                self.fetch_files(runtime);
            }
            if self.remote_files.is_loading() {
                ui.spinner();
            }
        });

        // Auto-fetch on first show
        if self.remote_files.take().is_none() && !self.remote_files.is_loading() {
            self.fetch_files(runtime);
        }

        ui.add_space(8.0);

        if let Some(files) = self.remote_files.take() {
            // Parent directory link
            if !self.remote_file_path.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    if ui
                        .link(egui::RichText::new("\u{2190} Parent directory").size(13.0))
                        .clicked()
                    {
                        if let Some(pos) = self.remote_file_path.rfind('/') {
                            self.remote_file_path = self.remote_file_path[..pos].to_string();
                        } else {
                            self.remote_file_path.clear();
                        }
                        self.fetch_files(runtime);
                    }
                });
            }

            for file in &files {
                let name = file["name"].as_str().or(file.as_str()).unwrap_or("?");
                let is_dir = file["isDirectory"].as_bool().unwrap_or(false) || name.ends_with('/');

                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    let icon = if is_dir { "\u{1F4C1}" } else { "\u{1F4C4}" };
                    if ui
                        .link(egui::RichText::new(format!("{} {}", icon, name)).size(13.0))
                        .clicked()
                    {
                        if is_dir {
                            let clean = name.trim_end_matches('/');
                            if self.remote_file_path.is_empty() {
                                self.remote_file_path = clean.to_string();
                            } else {
                                self.remote_file_path =
                                    format!("{}/{}", self.remote_file_path, clean);
                            }
                            self.fetch_files(runtime);
                        } else {
                            // Fetch file content
                            let path = if self.remote_file_path.is_empty() {
                                name.to_string()
                            } else {
                                format!("{}/{}", self.remote_file_path, name)
                            };
                            let Some(base) = self.base_url() else {
                                return;
                            };
                            let Some(token) = self.token() else {
                                return;
                            };
                            let result = self.remote_file_content.clone();
                            result.set_loading();
                            runtime.spawn(async move {
                                let client = reqwest::Client::new();
                                let resp = client
                                    .get(format!("{}/api/files/{}/read", base, path))
                                    .header("Authorization", format!("Bearer {}", token))
                                    .timeout(Duration::from_secs(15))
                                    .send()
                                    .await;
                                match resp {
                                    Ok(r) => {
                                        let json = r.json::<Value>().await.unwrap_or(Value::Null);
                                        let content = json["content"]
                                            .as_str()
                                            .unwrap_or("(could not read)")
                                            .to_string();
                                        result.set_done(content);
                                    }
                                    Err(e) => {
                                        result.set_done(format!("Error: {}", e));
                                    }
                                }
                            });
                        }
                    }
                });
            }
        }

        // Show file content if loaded
        if let Some(content) = self.remote_file_content.take() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(egui::RichText::new("File Content:").size(13.0).strong());
                if ui.small_button("Close").clicked() {
                    *self.remote_file_content.data.lock().unwrap() = None;
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                let frame = egui::Frame::new()
                    .fill(crate::ui::theme::card_color())
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(208, 215, 222),
                    ))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(8));
                frame.show(ui, |ui| {
                    ui.set_min_width(ui.available_width() - 40.0);
                    egui::ScrollArea::vertical()
                        .max_height(400.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut content.as_str())
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                });
            });
        }
    }

    // -----------------------------------------------------------------------
    // Chat tab
    // -----------------------------------------------------------------------

    fn show_chat(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // Fetch sessions on first load
        if self.chat_sessions.take().is_none() && !self.chat_sessions.is_loading() {
            self.fetch_chat_sessions(runtime);
        }

        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Remote Chat Sessions")
                    .size(15.0)
                    .strong(),
            );
            if ui.button("Refresh").clicked() {
                self.fetch_chat_sessions(runtime);
            }
            if ui.button("New Session").clicked() {
                let result = AsyncResult::<Value>::new();
                self.remote_post(
                    runtime,
                    "/api/chat/sessions",
                    serde_json::json!({ "title": "Remote Chat" }),
                    result,
                );
                self.fetch_chat_sessions(runtime);
            }
            if self.chat_sessions.is_loading() {
                ui.spinner();
            }
        });

        ui.add_space(4.0);

        // Session list
        if let Some(sessions) = self.chat_sessions.take() {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                egui::ComboBox::from_id_salt("remote_chat_session")
                    .width(400.0)
                    .selected_text(
                        self.chat_selected_session
                            .as_ref()
                            .and_then(|id| {
                                sessions
                                    .iter()
                                    .find(|s| s["id"].as_str() == Some(id.as_str()))
                                    .map(|s| {
                                        format!(
                                            "{} ({})",
                                            s["title"].as_str().unwrap_or("Untitled"),
                                            s["messageCount"].as_u64().unwrap_or(0)
                                        )
                                    })
                            })
                            .unwrap_or_else(|| "Select a session...".to_string()),
                    )
                    .show_ui(ui, |ui| {
                        for s in &sessions {
                            let id = s["id"].as_str().unwrap_or("").to_string();
                            let title = s["title"].as_str().unwrap_or("Untitled");
                            let count = s["messageCount"].as_u64().unwrap_or(0);
                            let label = format!("{} ({} msgs)", title, count);
                            if ui
                                .selectable_label(
                                    self.chat_selected_session.as_deref() == Some(&id),
                                    label,
                                )
                                .clicked()
                            {
                                self.chat_selected_session = Some(id.clone());
                                // Fetch messages
                                let Some(base) = self.base_url() else { return };
                                let Some(token) = self.token() else { return };
                                let result = self.chat_messages.clone();
                                result.set_loading();
                                runtime.spawn(async move {
                                    let client = reqwest::Client::new();
                                    let resp = client
                                        .get(format!("{}/api/chat/sessions/{}", base, id))
                                        .header("Authorization", format!("Bearer {}", token))
                                        .timeout(Duration::from_secs(15))
                                        .send()
                                        .await;
                                    match resp {
                                        Ok(r) => {
                                            let json =
                                                r.json::<Value>().await.unwrap_or(Value::Null);
                                            let msgs = json["messages"]
                                                .as_array()
                                                .cloned()
                                                .unwrap_or_default();
                                            result.set_done(msgs);
                                        }
                                        Err(_) => result.set_done(vec![]),
                                    }
                                });
                            }
                        }
                    });
            });
        }

        ui.add_space(8.0);

        // Messages display
        if let Some(messages) = self.chat_messages.take() {
            let frame = egui::Frame::new()
                .fill(crate::ui::theme::card_color())
                .stroke(egui::Stroke::new(
                    1.0,
                    egui::Color32::from_rgb(208, 215, 222),
                ))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(8));

            ui.horizontal(|ui| {
                ui.add_space(16.0);
                frame.show(ui, |ui| {
                    ui.set_min_width(ui.available_width() - 40.0);
                    egui::ScrollArea::vertical()
                        .max_height(350.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            if messages.is_empty() {
                                ui.label(
                                    egui::RichText::new("No messages yet")
                                        .size(13.0)
                                        .color(egui::Color32::from_rgb(139, 148, 158)),
                                );
                            }
                            for msg in &messages {
                                let role = msg["role"].as_str().unwrap_or("user");
                                let content = msg["content"].as_str().unwrap_or("");
                                let color = if role == "assistant" {
                                    crate::ui::theme::accent_color()
                                } else {
                                    egui::Color32::from_rgb(36, 41, 47)
                                };
                                ui.label(
                                    egui::RichText::new(format!("[{}] {}", role, content))
                                        .size(12.0)
                                        .color(color),
                                );
                                ui.add_space(4.0);
                            }
                        });
                });
            });
        }

        ui.add_space(8.0);

        // Chat input
        if self.chat_selected_session.is_some() {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.chat_input)
                        .desired_width(ui.available_width() - 120.0)
                        .hint_text("Type message..."),
                );

                let sending = self.chat_response.is_loading();
                let send_btn = ui.add_enabled(!sending, egui::Button::new("Send"));

                if (send_btn.clicked()
                    || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                    && !self.chat_input.trim().is_empty()
                    && !sending
                {
                    let msg = self.chat_input.trim().to_string();
                    self.chat_input.clear();
                    let session_id = self.chat_selected_session.clone().unwrap();
                    let Some(base) = self.base_url() else { return };
                    let Some(token) = self.token() else { return };
                    let result = self.chat_response.clone();
                    result.set_loading();
                    let msgs_result = self.chat_messages.clone();

                    runtime.spawn(async move {
                        let client = reqwest::Client::new();
                        let _resp = client
                            .post(format!(
                                "{}/api/chat/sessions/{}/messages",
                                base, session_id
                            ))
                            .header("Authorization", format!("Bearer {}", token))
                            .json(&serde_json::json!({ "message": msg }))
                            .timeout(Duration::from_secs(120))
                            .send()
                            .await;

                        // Refresh messages
                        let resp2 = client
                            .get(format!("{}/api/chat/sessions/{}", base, session_id))
                            .header("Authorization", format!("Bearer {}", token))
                            .timeout(Duration::from_secs(15))
                            .send()
                            .await;
                        if let Ok(r) = resp2 {
                            let json = r.json::<Value>().await.unwrap_or(Value::Null);
                            let msgs = json["messages"].as_array().cloned().unwrap_or_default();
                            msgs_result.set_done(msgs);
                        }
                        result.set_done("done".to_string());
                    });
                }

                if sending {
                    ui.spinner();
                    ui.label(egui::RichText::new("Waiting for response...").size(12.0));
                }
            });
        }
    }

    // -----------------------------------------------------------------------
    // Settings tab
    // -----------------------------------------------------------------------

    fn show_settings(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        if self.remote_settings.take().is_none() && !self.remote_settings.is_loading() {
            let result = AsyncResult::<Value>::new();
            let r2 = result.clone();
            self.remote_get(runtime, "/api/settings", r2);
            *self.remote_settings.data.lock().unwrap() = result.take();
            // re-fetch properly
            let Some(base) = self.base_url() else { return };
            let Some(token) = self.token() else { return };
            let rs = self.remote_settings.clone();
            rs.set_loading();
            runtime.spawn(async move {
                let client = reqwest::Client::new();
                let resp = client
                    .get(format!("{}/api/settings", base))
                    .header("Authorization", format!("Bearer {}", token))
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await;
                match resp {
                    Ok(r) => {
                        let json = r.json::<Value>().await.unwrap_or(Value::Null);
                        rs.set_done(json);
                    }
                    Err(_) => rs.set_done(Value::Null),
                }
            });
        }

        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Remote Server Settings (read-only)")
                    .size(15.0)
                    .strong(),
            );
            if ui.button("Refresh").clicked() {
                *self.remote_settings.data.lock().unwrap() = None;
            }
            if self.remote_settings.is_loading() {
                ui.spinner();
            }
        });

        ui.add_space(8.0);

        if let Some(settings) = self.remote_settings.take() {
            if settings.is_null() {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("Could not fetch remote settings")
                            .color(egui::Color32::from_rgb(220, 38, 38)),
                    );
                });
                return;
            }

            let items = [
                ("Model", "tigerBotModel"),
                ("API URL", "tigerBotApiUrl"),
                ("Sandbox Dir", "sandboxDir"),
                ("Remote Enabled", "remoteEnabled"),
            ];

            for (label, key) in &items {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(format!("{}:", label))
                            .size(13.0)
                            .strong(),
                    );
                    let val = match &settings[key] {
                        Value::String(s) => s.clone(),
                        Value::Bool(b) => b.to_string(),
                        Value::Number(n) => n.to_string(),
                        Value::Null => "(not set)".to_string(),
                        other => other.to_string(),
                    };
                    ui.label(egui::RichText::new(val).size(13.0));
                });
            }

            // MCP tools
            if let Some(tools) = settings["mcpTools"].as_array() {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(format!("MCP Tools ({})", tools.len()))
                            .size(13.0)
                            .strong(),
                    );
                });
                for tool in tools {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        let name = tool["name"].as_str().unwrap_or("?");
                        let enabled = tool["enabled"].as_bool().unwrap_or(false);
                        let dot_color = if enabled {
                            egui::Color32::from_rgb(74, 222, 128)
                        } else {
                            egui::Color32::from_rgb(239, 68, 68)
                        };
                        ui.label(egui::RichText::new("\u{25CF}").color(dot_color));
                        ui.label(egui::RichText::new(name).size(12.0));
                    });
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Terminal tab
    // -----------------------------------------------------------------------

    fn show_terminal(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Remote Terminal").size(15.0).strong());
        });

        ui.add_space(8.0);

        // Output area
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let frame = egui::Frame::new()
                .fill(crate::ui::theme::canvas_color())
                .stroke(crate::ui::theme::hairline_stroke())
                .corner_radius(crate::ui::theme::RADIUS_SMALL)
                .inner_margin(egui::Margin::same(8));

            frame.show(ui, |ui| {
                ui.set_min_width(ui.available_width() - 40.0);
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if self.term_output.is_empty() {
                            ui.label(
                                egui::RichText::new("$ _")
                                    .size(13.0)
                                    .color(egui::Color32::from_rgb(74, 222, 128))
                                    .monospace(),
                            );
                        }
                        for entry in &self.term_output {
                            let cmd = entry["cmd"].as_str().unwrap_or("");
                            let stdout = entry["stdout"].as_str().unwrap_or("");
                            let stderr = entry["stderr"].as_str().unwrap_or("");

                            ui.label(
                                egui::RichText::new(format!("$ {}", cmd))
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(74, 222, 128))
                                    .monospace(),
                            );
                            if !stdout.is_empty() {
                                ui.label(
                                    egui::RichText::new(stdout)
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(224, 224, 224))
                                        .monospace(),
                                );
                            }
                            if !stderr.is_empty() {
                                ui.label(
                                    egui::RichText::new(stderr)
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(239, 68, 68))
                                        .monospace(),
                                );
                            }
                            ui.add_space(4.0);
                        }
                    });
            });
        });

        ui.add_space(8.0);

        // Input
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("$")
                    .size(14.0)
                    .color(egui::Color32::from_rgb(74, 222, 128))
                    .monospace(),
            );
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.term_input)
                    .desired_width(ui.available_width() - 100.0)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("command..."),
            );

            let running = self.term_result.is_loading();
            let run_btn = ui.add_enabled(!running, egui::Button::new("Run"));

            if (run_btn.clicked()
                || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                && !self.term_input.trim().is_empty()
                && !running
            {
                let cmd = self.term_input.trim().to_string();
                self.term_input.clear();
                let Some(base) = self.base_url() else { return };
                let Some(token) = self.token() else { return };
                let result = self.term_result.clone();
                result.set_loading();
                let cmd_clone = cmd.clone();

                runtime.spawn(async move {
                    let client = reqwest::Client::new();
                    let resp = client
                        .post(format!("{}/api/terminal/exec", base))
                        .header("Authorization", format!("Bearer {}", token))
                        .json(&serde_json::json!({ "command": cmd_clone }))
                        .timeout(Duration::from_secs(60))
                        .send()
                        .await;
                    match resp {
                        Ok(r) => {
                            let json = r.json::<Value>().await.unwrap_or(Value::Null);
                            result.set_done(json);
                        }
                        Err(e) => {
                            result.set_done(serde_json::json!({
                                "stderr": format!("Error: {}", e)
                            }));
                        }
                    }
                });

                // Add pending entry
                self.term_output.push(serde_json::json!({
                    "cmd": cmd,
                    "stdout": "",
                    "stderr": "(running...)"
                }));
            }

            if running {
                ui.spinner();
            }
        });

        // Check for completed terminal results
        if let Some(result) = self.term_result.take() {
            if !self.term_result.is_loading() {
                // Update last entry with actual result
                if let Some(last) = self.term_output.last_mut() {
                    let stdout = result["stdout"].as_str().unwrap_or("").to_string();
                    let stderr = result["stderr"].as_str().unwrap_or("").to_string();
                    *last = serde_json::json!({
                        "cmd": last["cmd"].as_str().unwrap_or(""),
                        "stdout": stdout,
                        "stderr": stderr,
                    });
                }
            }
        }

        // Clear button
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            if ui.small_button("Clear").clicked() {
                self.term_output.clear();
            }
        });
    }

    // -----------------------------------------------------------------------
    // Agents tab
    // -----------------------------------------------------------------------

    fn show_agents(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Remote Agent Configs")
                    .size(15.0)
                    .strong(),
            );
            if ui.button("Refresh").clicked() {
                *self.remote_agents.data.lock().unwrap() = None;
            }
            if self.remote_agents.is_loading() {
                ui.spinner();
            }
        });

        // Fetch agents
        if self.remote_agents.take().is_none() && !self.remote_agents.is_loading() {
            let Some(base) = self.base_url() else { return };
            let Some(token) = self.token() else { return };
            let result = self.remote_agents.clone();
            result.set_loading();
            runtime.spawn(async move {
                let client = reqwest::Client::new();
                let resp = client
                    .get(format!("{}/api/agents", base))
                    .header("Authorization", format!("Bearer {}", token))
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await;
                match resp {
                    Ok(r) => {
                        let json = r.json::<Value>().await.unwrap_or(Value::Null);
                        let agents = json.as_array().cloned().unwrap_or_default();
                        result.set_done(agents);
                    }
                    Err(_) => result.set_done(vec![]),
                }
            });
        }

        ui.add_space(8.0);

        if let Some(agents) = self.remote_agents.take() {
            if agents.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("No agent configs on remote server")
                            .size(13.0)
                            .color(egui::Color32::from_rgb(139, 148, 158)),
                    );
                });
            }
            for agent in &agents {
                let name = agent
                    .as_str()
                    .unwrap_or(agent["name"].as_str().unwrap_or("?"));
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    let frame = egui::Frame::new()
                        .fill(crate::ui::theme::card_color())
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgb(208, 215, 222),
                        ))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::same(10));

                    frame.show(ui, |ui| {
                        ui.set_min_width(ui.available_width() - 40.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("\u{1F4C4}").size(14.0));
                            ui.label(
                                egui::RichText::new(name)
                                    .size(13.0)
                                    .color(egui::Color32::from_rgb(36, 41, 47)),
                            );
                        });
                    });
                });
                ui.add_space(2.0);
            }
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        // Submit task with agent config
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Submit Remote Task")
                    .size(15.0)
                    .strong(),
            );
        });

        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.task_input)
                    .desired_width(ui.available_width() - 120.0)
                    .hint_text("Enter task for remote agent..."),
            );

            let submit = ui.button("Submit");
            if (submit.clicked()
                || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                && !self.task_input.trim().is_empty()
            {
                let task = self.task_input.trim().to_string();
                self.task_input.clear();
                let result = AsyncResult::<Value>::new();
                self.remote_post(
                    runtime,
                    "/api/remote/task",
                    serde_json::json!({ "task": task }),
                    result,
                );
            }
        });
    }
}
