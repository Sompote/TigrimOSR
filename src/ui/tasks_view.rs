use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::server::data::{self, ScheduledTask};

// ---------------------------------------------------------------------------
// Shared state for active chat sessions (set by ChatView, read by TasksView)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ActiveChatSession {
    pub session_id: String,
    pub title: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub agent_count: usize,
    pub tool_calls: usize,
}

static ACTIVE_CHATS: std::sync::OnceLock<Arc<Mutex<Vec<ActiveChatSession>>>> =
    std::sync::OnceLock::new();

pub fn active_chats() -> &'static Arc<Mutex<Vec<ActiveChatSession>>> {
    ACTIVE_CHATS.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

// ---------------------------------------------------------------------------
// Killed chat session IDs (set by TasksView, read by ChatView to cancel streams)
// ---------------------------------------------------------------------------

static KILLED_CHAT_IDS: std::sync::OnceLock<Arc<Mutex<Vec<String>>>> =
    std::sync::OnceLock::new();

pub fn take_killed_chat_ids() -> Vec<String> {
    let mut ids = KILLED_CHAT_IDS
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .lock()
        .unwrap();
    std::mem::take(&mut *ids)
}

pub fn killed_chat_ids() -> &'static Arc<Mutex<Vec<String>>> {
    KILLED_CHAT_IDS.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

fn push_killed_chat_id(id: &str) {
    KILLED_CHAT_IDS
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .lock()
        .unwrap()
        .push(id.to_string());
}

// ---------------------------------------------------------------------------
// Finished chat session tracking
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FinishedChatSession {
    pub session_id: String,
    pub title: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_secs: f64,
    pub agent_count: usize,
    pub tool_calls: usize,
}

static FINISHED_CHATS: std::sync::OnceLock<Arc<Mutex<Vec<FinishedChatSession>>>> =
    std::sync::OnceLock::new();

pub fn finished_chats() -> &'static Arc<Mutex<Vec<FinishedChatSession>>> {
    FINISHED_CHATS.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

/// Call this when a chat session completes to record it in the finished list.
pub fn mark_chat_finished(session: &ActiveChatSession) {
    let now = chrono::Utc::now();
    let duration = (now - session.started_at).num_milliseconds() as f64 / 1000.0;
    let finished = FinishedChatSession {
        session_id: session.session_id.clone(),
        title: session.title.clone(),
        started_at: session.started_at.to_rfc3339(),
        finished_at: now.to_rfc3339(),
        duration_secs: duration,
        agent_count: session.agent_count,
        tool_calls: session.tool_calls,
    };
    finished_chats().lock().unwrap().push(finished);
}

// ---------------------------------------------------------------------------
// Data structures for active and finished tasks
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ActiveTask {
    pub id: String,
    pub name: String,
    pub command: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub pid: Option<u32>,
    pub status: String, // "running", "waiting"
    pub output: Arc<Mutex<String>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FinishedTask {
    pub id: String,
    pub name: String,
    pub command: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_secs: f64,
    pub status: String, // "success", "error", "cancelled"
    pub exit_code: Option<i32>,
    pub output: String,
}

// ---------------------------------------------------------------------------
// Cron presets
// ---------------------------------------------------------------------------

const CRON_PRESETS: &[(&str, &str)] = &[
    ("Every minute", "*/1 * * * *"),
    ("Every 5 min", "*/5 * * * *"),
    ("Every hour", "0 * * * *"),
    ("Every day", "0 0 * * *"),
    ("Every week", "0 0 * * 0"),
];

// ---------------------------------------------------------------------------
// Sub-tab enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskTab {
    Scheduled,
    Active,
    Remote,
    Finished,
}

// ---------------------------------------------------------------------------
// TasksView
// ---------------------------------------------------------------------------

pub struct TasksView {
    // scheduled
    tasks: Vec<ScheduledTask>,
    needs_refresh: bool,

    // create dialog
    show_create: bool,
    new_name: String,
    new_cron: String,
    new_command: String,
    selected_preset: usize, // 0 = custom

    // edit dialog
    editing_task_id: Option<String>,
    edit_name: String,
    edit_cron: String,
    edit_command: String,
    edit_preset: usize,

    // delete confirmation
    confirm_delete_id: Option<String>,

    // tabs
    current_tab: TaskTab,

    // active tasks (shared with background executors)
    active_tasks: Arc<Mutex<Vec<ActiveTask>>>,

    // finished tasks
    finished_tasks: Vec<FinishedTask>,
    finished_needs_load: bool,
    expanded_finished_id: Option<String>,

    // remote tasks
    remote_tasks: Vec<serde_json::Value>,
    remote_needs_refresh: bool,
    expanded_remote_id: Option<String>,

    // navigation to chat
    pub navigate_to_chat: Option<String>, // session_id to jump to
}

impl TasksView {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            needs_refresh: true,
            show_create: false,
            new_name: String::new(),
            new_cron: String::new(),
            new_command: String::new(),
            selected_preset: 0,
            editing_task_id: None,
            edit_name: String::new(),
            edit_cron: String::new(),
            edit_command: String::new(),
            edit_preset: 0,
            confirm_delete_id: None,
            current_tab: TaskTab::Active,
            active_tasks: Arc::new(Mutex::new(Vec::new())),
            finished_tasks: Vec::new(),
            finished_needs_load: true,
            expanded_finished_id: None,
            remote_tasks: Vec::new(),
            remote_needs_refresh: true,
            expanded_remote_id: None,
            navigate_to_chat: None,
        }
    }

    // ----- execute a command (used by Run Now and scheduled) -----
    fn execute_task(
        &self,
        name: &str,
        command: &str,
        task_id: &str,
        runtime: &tokio::runtime::Handle,
    ) {
        let active_list = self.active_tasks.clone();
        let cmd = command.to_string();
        let task_name = name.to_string();
        let tid = task_id.to_string();

        let active_task = ActiveTask {
            id: tid.clone(),
            name: task_name.clone(),
            command: cmd.clone(),
            started_at: chrono::Utc::now(),
            pid: None,
            status: "running".to_string(),
            output: Arc::new(Mutex::new(String::new())),
        };
        let output_ref = active_task.output.clone();

        // Add to active list
        {
            let mut guard = active_list.lock().unwrap();
            guard.push(active_task);
        }

        let active_for_finish = active_list.clone();
        let tid_for_finish = tid.clone();
        let name_for_finish = task_name.clone();
        let cmd_for_finish = cmd.clone();
        let start_time = chrono::Utc::now();

        runtime.spawn(async move {
            let result = tokio::process::Command::new("bash")
                .arg("-c")
                .arg(&cmd)
                .output()
                .await;

            let finish_time = chrono::Utc::now();
            let duration = (finish_time - start_time).num_milliseconds() as f64 / 1000.0;

            let (exit_code, combined_output, status) = match result {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    let mut combined = stdout;
                    if !stderr.is_empty() {
                        if !combined.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str(&stderr);
                    }
                    let code = out.status.code();
                    let s = if out.status.success() {
                        "success"
                    } else {
                        "error"
                    };
                    (code, combined, s.to_string())
                }
                Err(e) => (None, format!("Error: {}", e), "error".to_string()),
            };

            // Update output on the active task
            {
                let mut guard = output_ref.lock().unwrap();
                *guard = combined_output.clone();
            }

            // Build finished task
            let finished = FinishedTask {
                id: uuid::Uuid::new_v4().to_string(),
                name: name_for_finish,
                command: cmd_for_finish,
                started_at: start_time.to_rfc3339(),
                finished_at: finish_time.to_rfc3339(),
                duration_secs: duration,
                status,
                exit_code,
                output: combined_output,
            };

            // Persist finished task
            let mut history: Vec<FinishedTask> =
                data::read_json("finished_tasks.json").await;
            history.push(finished);
            data::write_json("finished_tasks.json", &history).await;

            // Remove from active list
            {
                let mut guard = active_for_finish.lock().unwrap();
                guard.retain(|t| t.id != tid_for_finish);
            }
        });
    }

    // ----- main show -----
    pub fn show(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // ---------- refresh scheduled tasks from data layer ----------
        if self.needs_refresh {
            self.needs_refresh = false;
            let tasks = runtime.block_on(data::get_tasks());
            self.tasks = tasks;
        }

        // ---------- load finished tasks ----------
        if self.finished_needs_load {
            self.finished_needs_load = false;
            let history: Vec<FinishedTask> =
                runtime.block_on(data::read_json("finished_tasks.json"));
            self.finished_tasks = history;
        }

        // ---------- header ----------
        ui.horizontal(|ui| {
            ui.heading("Task Scheduler");
        });

        ui.separator();

        // ---------- three-tab bar ----------
        ui.horizontal(|ui| {
            let active_count = {
                let guard = self.active_tasks.lock().unwrap();
                guard.len()
            };

            let active_label = if active_count > 0 {
                format!("Active ({})", active_count)
            } else {
                "Active".to_string()
            };
            if ui
                .selectable_label(self.current_tab == TaskTab::Active, active_label)
                .clicked()
            {
                self.current_tab = TaskTab::Active;
            }

            if ui
                .selectable_label(self.current_tab == TaskTab::Scheduled, "Scheduled")
                .clicked()
            {
                self.current_tab = TaskTab::Scheduled;
                self.needs_refresh = true;
            }

            let remote_count = self.remote_tasks.iter()
                .filter(|t| t["status"].as_str() == Some("running"))
                .count();
            let remote_label = if remote_count > 0 {
                format!("Remote ({})", remote_count)
            } else {
                "Remote".to_string()
            };
            if ui
                .selectable_label(self.current_tab == TaskTab::Remote, remote_label)
                .clicked()
            {
                self.current_tab = TaskTab::Remote;
                self.remote_needs_refresh = true;
            }

            if ui
                .selectable_label(self.current_tab == TaskTab::Finished, "Finished")
                .clicked()
            {
                self.current_tab = TaskTab::Finished;
                self.finished_needs_load = true;
            }
        });

        ui.separator();

        // ---------- tab content ----------
        match self.current_tab {
            TaskTab::Scheduled => self.show_scheduled_tab(ui, runtime),
            TaskTab::Active => self.show_active_tab(ui, runtime),
            TaskTab::Remote => self.show_remote_tab(ui, runtime),
            TaskTab::Finished => self.show_finished_tab(ui, runtime),
        }
    }

    // ===================================================================
    // SCHEDULED TAB
    // ===================================================================
    fn show_scheduled_tab(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // header with create button
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} scheduled tasks", self.tasks.len()))
                    .weak()
                    .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+ Create Task").clicked() {
                    self.show_create = true;
                    self.new_name.clear();
                    self.new_cron.clear();
                    self.new_command.clear();
                    self.selected_preset = 0;
                }
            });
        });

        ui.add_space(4.0);

        // ---------- create-task dialog ----------
        if self.show_create {
            self.show_create_dialog(ui, runtime);
            ui.add_space(8.0);
        }

        // ---------- edit-task dialog ----------
        if self.editing_task_id.is_some() {
            self.show_edit_dialog(ui, runtime);
            ui.add_space(8.0);
        }

        // ---------- delete confirmation dialog ----------
        if let Some(ref del_id) = self.confirm_delete_id.clone() {
            let task_name = self
                .tasks
                .iter()
                .find(|t| t.id == *del_id)
                .map(|t| t.name.clone())
                .unwrap_or_default();

            egui::Frame::new()
                .fill(egui::Color32::from_rgb(60, 20, 20))
                .inner_margin(egui::Margin::same(12))
                .corner_radius(6.0)
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Are you sure you want to delete \"{}\"?",
                            task_name
                        ))
                        .color(egui::Color32::from_rgb(255, 200, 200)),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                egui::RichText::new("Yes, Delete")
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            )
                            .clicked()
                        {
                            let del = del_id.clone();
                            self.tasks.retain(|t| t.id != del);
                            let tasks = self.tasks.clone();
                            runtime.spawn(async move {
                                data::save_tasks(&tasks).await;
                            });
                            self.confirm_delete_id = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_delete_id = None;
                        }
                    });
                });
            ui.add_space(8.0);
        }

        // ---------- task list ----------
        if self.tasks.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("No tasks scheduled. Click \"+ Create Task\" to add one.");
            });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut toggled = false;
                let mut run_now_cmd: Option<(String, String, String)> = None;
                let mut delete_id: Option<String> = None;
                let mut edit_id: Option<String> = None;

                for task in self.tasks.iter_mut() {
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .inner_margin(egui::Margin::same(10))
                        .corner_radius(4.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // enabled toggle
                                if ui.checkbox(&mut task.enabled, "").changed() {
                                    toggled = true;
                                }

                                // status badge
                                let (badge_text, badge_color) = if task.enabled {
                                    (
                                        "Enabled",
                                        egui::Color32::from_rgb(34, 197, 94),
                                    )
                                } else {
                                    (
                                        "Disabled",
                                        egui::Color32::from_rgb(156, 163, 175),
                                    )
                                };
                                ui.label(
                                    egui::RichText::new(badge_text)
                                        .small()
                                        .color(badge_color),
                                );

                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.strong(&task.name);
                                        if task.source.as_deref() == Some("chat") {
                                            ui.label(
                                                egui::RichText::new("chat")
                                                    .small()
                                                    .color(egui::Color32::from_rgb(168, 85, 247)),
                                            );
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("cron: {}", task.cron))
                                                .monospace()
                                                .small(),
                                        );
                                        if let Some(ref lr) = task.last_run {
                                            ui.label(
                                                egui::RichText::new(format!("  last run: {}", lr))
                                                    .weak()
                                                    .small(),
                                            );
                                        }
                                        if let Some(ref result) = task.last_result {
                                            let color = if result == "success" {
                                                egui::Color32::from_rgb(34, 197, 94)
                                            } else {
                                                egui::Color32::from_rgb(239, 68, 68)
                                            };
                                            ui.label(
                                                egui::RichText::new(format!("  result: {}", result))
                                                    .small()
                                                    .color(color),
                                            );
                                        }
                                    });
                                    ui.label(
                                        egui::RichText::new(format!("$ {}", task.command))
                                            .monospace()
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    );
                                });

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        // Delete button
                                        if ui
                                            .button(
                                                egui::RichText::new("Delete")
                                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                                            )
                                            .clicked()
                                        {
                                            delete_id = Some(task.id.clone());
                                        }

                                        // Edit button
                                        if ui.button("Edit").clicked() {
                                            edit_id = Some(task.id.clone());
                                        }

                                        // Run Now button
                                        if ui
                                            .button(
                                                egui::RichText::new("Run Now").color(
                                                    egui::Color32::from_rgb(18, 154, 145),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            run_now_cmd = Some((
                                                task.id.clone(),
                                                task.name.clone(),
                                                task.command.clone(),
                                            ));
                                        }
                                    },
                                );
                            });
                        });

                    ui.add_space(4.0);
                }

                // persist after toggle
                if toggled {
                    let tasks = self.tasks.clone();
                    runtime.spawn(async move {
                        data::save_tasks(&tasks).await;
                    });
                }

                // handle run now
                if let Some((tid, name, cmd)) = run_now_cmd {
                    let run_id = format!("run-{}", uuid::Uuid::new_v4());
                    self.execute_task(&name, &cmd, &run_id, runtime);
                    // Update last_run on the scheduled task
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == tid) {
                        t.last_run = Some(chrono::Utc::now().to_rfc3339());
                    }
                    let tasks = self.tasks.clone();
                    runtime.spawn(async move {
                        data::save_tasks(&tasks).await;
                    });
                }

                // handle delete (show confirmation)
                if let Some(id) = delete_id {
                    self.confirm_delete_id = Some(id);
                }

                // handle edit
                if let Some(id) = edit_id {
                    if let Some(task) = self.tasks.iter().find(|t| t.id == id) {
                        self.edit_name = task.name.clone();
                        self.edit_cron = task.cron.clone();
                        self.edit_command = task.command.clone();
                        self.edit_preset = 0; // custom by default
                        // Check if cron matches a preset
                        for (i, (_label, expr)) in CRON_PRESETS.iter().enumerate() {
                            if task.cron == *expr {
                                self.edit_preset = i + 1;
                                break;
                            }
                        }
                        self.editing_task_id = Some(id);
                    }
                }
            });
    }

    // ----- create dialog -----
    fn show_create_dialog(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::same(12))
            .corner_radius(6.0)
            .show(ui, |ui| {
                ui.heading("New Task");
                ui.add_space(4.0);

                egui::Grid::new("new_task_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut self.new_name);
                        ui.end_row();

                        ui.label("Cron:");
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut self.new_cron);
                            // Preset dropdown
                            let preset_label = if self.selected_preset == 0 {
                                "Preset..."
                            } else {
                                CRON_PRESETS[self.selected_preset - 1].0
                            };
                            egui::ComboBox::from_id_salt("cron_preset_new")
                                .selected_text(preset_label)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.selected_preset,
                                        0,
                                        "Custom",
                                    );
                                    for (i, (label, expr)) in
                                        CRON_PRESETS.iter().enumerate()
                                    {
                                        if ui
                                            .selectable_value(
                                                &mut self.selected_preset,
                                                i + 1,
                                                format!("{} ({})", label, expr),
                                            )
                                            .clicked()
                                        {
                                            self.new_cron = expr.to_string();
                                        }
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("Command:");
                        ui.text_edit_singleline(&mut self.new_command);
                        ui.end_row();
                    });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let can_save = !self.new_name.is_empty()
                        && !self.new_cron.is_empty()
                        && !self.new_command.is_empty();

                    if ui
                        .add_enabled(can_save, egui::Button::new("Save"))
                        .clicked()
                    {
                        let task = ScheduledTask {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: self.new_name.clone(),
                            cron: self.new_cron.clone(),
                            command: self.new_command.clone(),
                            enabled: true,
                            last_run: None,
                            last_result: None,
                            created_at: chrono::Utc::now().to_rfc3339(),
                            source: None,
                        };
                        self.tasks.push(task);
                        let tasks = self.tasks.clone();
                        runtime.spawn(async move {
                            data::save_tasks(&tasks).await;
                        });
                        self.show_create = false;
                    }

                    if ui.button("Cancel").clicked() {
                        self.show_create = false;
                    }
                });
            });
    }

    // ----- edit dialog -----
    fn show_edit_dialog(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::same(12))
            .corner_radius(6.0)
            .show(ui, |ui| {
                ui.heading("Edit Task");
                ui.add_space(4.0);

                egui::Grid::new("edit_task_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut self.edit_name);
                        ui.end_row();

                        ui.label("Cron:");
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut self.edit_cron);
                            let preset_label = if self.edit_preset == 0 {
                                "Preset..."
                            } else {
                                CRON_PRESETS[self.edit_preset - 1].0
                            };
                            egui::ComboBox::from_id_salt("cron_preset_edit")
                                .selected_text(preset_label)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.edit_preset,
                                        0,
                                        "Custom",
                                    );
                                    for (i, (label, expr)) in
                                        CRON_PRESETS.iter().enumerate()
                                    {
                                        if ui
                                            .selectable_value(
                                                &mut self.edit_preset,
                                                i + 1,
                                                format!("{} ({})", label, expr),
                                            )
                                            .clicked()
                                        {
                                            self.edit_cron = expr.to_string();
                                        }
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("Command:");
                        ui.text_edit_singleline(&mut self.edit_command);
                        ui.end_row();
                    });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let can_save = !self.edit_name.is_empty()
                        && !self.edit_cron.is_empty()
                        && !self.edit_command.is_empty();

                    if ui
                        .add_enabled(can_save, egui::Button::new("Save"))
                        .clicked()
                    {
                        if let Some(ref edit_id) = self.editing_task_id.clone() {
                            if let Some(task) =
                                self.tasks.iter_mut().find(|t| t.id == *edit_id)
                            {
                                task.name = self.edit_name.clone();
                                task.cron = self.edit_cron.clone();
                                task.command = self.edit_command.clone();
                            }
                            let tasks = self.tasks.clone();
                            runtime.spawn(async move {
                                data::save_tasks(&tasks).await;
                            });
                        }
                        self.editing_task_id = None;
                    }

                    if ui.button("Cancel").clicked() {
                        self.editing_task_id = None;
                    }
                });
            });
    }

    // ===================================================================
    // ACTIVE TAB
    // ===================================================================
    fn show_active_tab(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // Auto-refresh every 2 seconds
        ui.ctx()
            .request_repaint_after(Duration::from_secs(2));

        let now = chrono::Utc::now();

        let active_snapshot: Vec<(String, String, String, String, f64, Option<u32>, String)> = {
            let guard = self.active_tasks.lock().unwrap();
            guard
                .iter()
                .map(|t| {
                    let elapsed =
                        (now - t.started_at).num_milliseconds() as f64 / 1000.0;
                    let output = t.output.lock().unwrap().clone();
                    (
                        t.id.clone(),
                        t.name.clone(),
                        t.command.clone(),
                        t.status.clone(),
                        elapsed,
                        t.pid,
                        output,
                    )
                })
                .collect()
        };

        let chat_count = active_chats().lock().unwrap().len();
        let total_active = active_snapshot.len() + chat_count;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} active tasks", total_active))
                    .weak()
                    .small(),
            );
        });

        ui.add_space(4.0);

        // Also show active chat sessions
        let chat_sessions: Vec<ActiveChatSession> = {
            active_chats().lock().unwrap().clone()
        };

        if active_snapshot.is_empty() && chat_sessions.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("No tasks currently running.");
            });
            return;
        }

        // Show active chat sessions first
        if !chat_sessions.is_empty() {
            ui.label(
                egui::RichText::new("Active Chat Sessions")
                    .size(12.0)
                    .strong()
                    .color(egui::Color32::from_rgb(18, 154, 145)),
            );
            ui.add_space(4.0);
            let mut kill_chat_id: Option<String> = None;
            let mut goto_chat_id: Option<String> = None;
            for chat in &chat_sessions {
                let elapsed = (now - chat.started_at).num_seconds();
                let elapsed_str = if elapsed < 60 {
                    format!("{}s", elapsed)
                } else {
                    format!("{}m {}s", elapsed / 60, elapsed % 60)
                };
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_premultiplied(250, 204, 21, 15))
                    .inner_margin(egui::Margin::same(10))
                    .corner_radius(6.0)
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(250, 204, 21)))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Pulsing yellow dot
                            let pulse = (ui.input(|i| i.time) * 3.0).sin() as f32 * 0.3 + 0.7;
                            let yellow = egui::Color32::from_rgba_premultiplied(
                                (250.0 * pulse) as u8,
                                (204.0 * pulse) as u8,
                                (21.0 * pulse) as u8,
                                255,
                            );
                            let (dot_rect, _) = ui.allocate_exact_size(
                                egui::vec2(10.0, 10.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().circle_filled(dot_rect.center(), 5.0, yellow);

                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(&chat.title);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.add(
                                            egui::Button::new(
                                                egui::RichText::new("Kill")
                                                    .color(egui::Color32::WHITE)
                                                    .small(),
                                            )
                                            .fill(egui::Color32::from_rgb(239, 68, 68))
                                            .corner_radius(4.0),
                                        ).clicked() {
                                            kill_chat_id = Some(chat.session_id.clone());
                                        }
                                        if ui.add(
                                            egui::Button::new(
                                                egui::RichText::new("Go to Chat")
                                                    .color(egui::Color32::WHITE)
                                                    .small(),
                                            )
                                            .fill(egui::Color32::from_rgb(18, 154, 145))
                                            .corner_radius(4.0),
                                        ).clicked() {
                                            goto_chat_id = Some(chat.session_id.clone());
                                        }
                                    });
                                });
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("AI Streaming")
                                            .small()
                                            .color(egui::Color32::from_rgb(250, 204, 21)),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!("Elapsed: {}", elapsed_str))
                                            .small()
                                            .weak(),
                                    );
                                    if chat.agent_count > 0 {
                                        ui.label(
                                            egui::RichText::new(format!("Agents: {}", chat.agent_count))
                                                .small()
                                                .color(egui::Color32::from_rgb(34, 197, 94)),
                                        );
                                    }
                                    if chat.tool_calls > 0 {
                                        ui.label(
                                            egui::RichText::new(format!("Tools: {}", chat.tool_calls))
                                                .small()
                                                .color(egui::Color32::from_rgb(18, 154, 145)),
                                        );
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(format!("Session: {}", &chat.session_id[..8.min(chat.session_id.len())]))
                                        .small()
                                        .weak(),
                                );
                            });
                        });
                    });
                ui.add_space(4.0);
            }
            // Handle go to chat
            if let Some(sid) = goto_chat_id {
                self.navigate_to_chat = Some(sid);
            }
            // Handle kill chat session
            if let Some(ref kid) = kill_chat_id {
                // Signal ChatView to cancel the stream
                push_killed_chat_id(kid);
                // Move to finished before removing
                {
                    let chats = active_chats().lock().unwrap();
                    if let Some(chat) = chats.iter().find(|c| c.session_id == *kid) {
                        mark_chat_finished(chat);
                    }
                }
                active_chats().lock().unwrap().retain(|c| c.session_id != *kid);
                // Shutdown any realtime session for this chat and SIGKILL every
                // OS process tree it spawned (so killed tasks leave nothing
                // running with filesystem/network access).
                let kid_clone = kid.clone();
                runtime.spawn(async move {
                    crate::server::services::toolbox::shutdown_realtime_session(&kid_clone).await;
                    crate::server::services::proc_registry::kill_session(&kid_clone);
                });
            }
            ui.add_space(8.0);
        }

        if active_snapshot.is_empty() {
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut kill_id: Option<String> = None;

                for (id, name, command, status, elapsed, pid, _output) in
                    &active_snapshot
                {
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .inner_margin(egui::Margin::same(10))
                        .corner_radius(4.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // Pulsing green dot for running
                                let pulse = (ui.input(|i| i.time) * 3.0).sin() as f32
                                    * 0.3
                                    + 0.7;
                                let green = egui::Color32::from_rgba_premultiplied(
                                    (34.0 * pulse) as u8,
                                    (197.0 * pulse) as u8,
                                    (94.0 * pulse) as u8,
                                    255,
                                );
                                let (dot_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(10.0, 10.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter()
                                    .circle_filled(dot_rect.center(), 5.0, green);

                                ui.vertical(|ui| {
                                    ui.strong(name);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Status: {}",
                                                status
                                            ))
                                            .small()
                                            .color(egui::Color32::from_rgb(
                                                34, 197, 94,
                                            )),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Elapsed: {:.1}s",
                                                elapsed
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                        if let Some(p) = pid {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "PID: {}",
                                                    p
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                        }
                                    });
                                    ui.label(
                                        egui::RichText::new(format!("$ {}", command))
                                            .monospace()
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    );
                                });

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .button(
                                                egui::RichText::new("Kill").color(
                                                    egui::Color32::from_rgb(
                                                        239, 68, 68,
                                                    ),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            kill_id = Some(id.clone());
                                        }
                                    },
                                );
                            });
                        });

                    ui.add_space(4.0);
                }

                // Handle kill
                if let Some(ref kid) = kill_id {
                    let mut guard = self.active_tasks.lock().unwrap();
                    // Move killed task to finished as "cancelled"
                    if let Some(pos) = guard.iter().position(|t| t.id == *kid) {
                        let task = guard.remove(pos);
                        let finish_time = chrono::Utc::now();
                        let duration = (finish_time - task.started_at).num_milliseconds()
                            as f64
                            / 1000.0;
                        let output = task.output.lock().unwrap().clone();
                        let finished = FinishedTask {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: task.name,
                            command: task.command,
                            started_at: task.started_at.to_rfc3339(),
                            finished_at: finish_time.to_rfc3339(),
                            duration_secs: duration,
                            status: "cancelled".to_string(),
                            exit_code: None,
                            output,
                        };
                        self.finished_tasks.push(finished.clone());
                        // Persist
                        let history = self.finished_tasks.clone();
                        runtime.spawn(async move {
                            data::write_json("finished_tasks.json", &history).await;
                        });
                    }
                }
            });
    }

    // ===================================================================
    // REMOTE TAB — shows tasks submitted via web UI / HTTP API
    // ===================================================================
    fn show_remote_tab(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // Auto-refresh every frame when tab is active
        if self.remote_needs_refresh {
            self.remote_needs_refresh = false;
            self.remote_tasks = runtime.block_on(
                crate::server::routes::remote::get_all_remote_tasks()
            );
        }

        // Auto-refresh periodically
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));
        // Silently refresh in background
        self.remote_tasks = runtime.block_on(
            crate::server::routes::remote::get_all_remote_tasks()
        );

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} remote task(s)", self.remote_tasks.len()))
                    .size(13.0)
                    .color(egui::Color32::GRAY),
            );
            if ui.button("Refresh").clicked() {
                self.remote_needs_refresh = true;
            }
        });

        ui.add_space(8.0);

        if self.remote_tasks.is_empty() {
            ui.label(
                egui::RichText::new("No remote tasks. Submit tasks via the web UI at /web")
                    .color(egui::Color32::GRAY),
            );
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut kill_id: Option<String> = None;
            let mut toggle_id: Option<String> = None;

            for task in &self.remote_tasks {
                let id = task["id"].as_str().unwrap_or("?");
                let status = task["status"].as_str().unwrap_or("?");
                let task_text = task["task"].as_str().unwrap_or("");
                let created = task["createdAt"].as_str().unwrap_or("");
                let progress_count = task["progressCount"].as_u64().unwrap_or(0);
                let is_expanded = self.expanded_remote_id.as_deref() == Some(id);

                let status_color = match status {
                    "running" => egui::Color32::from_rgb(18, 154, 145),
                    "completed" => egui::Color32::from_rgb(74, 222, 128),
                    "failed" => egui::Color32::from_rgb(239, 68, 68),
                    "killed" => egui::Color32::from_rgb(107, 114, 128),
                    "pending" => egui::Color32::from_rgb(245, 158, 11),
                    _ => egui::Color32::GRAY,
                };

                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        // Status badge
                        let badge = egui::RichText::new(format!(" {} ", status.to_uppercase()))
                            .size(10.0)
                            .color(egui::Color32::WHITE)
                            .background_color(status_color);
                        ui.label(badge);

                        // Task text (truncated)
                        let display: String = task_text.chars().take(80).collect();
                        if ui.link(egui::RichText::new(&display).size(13.0)).clicked() {
                            toggle_id = Some(id.to_string());
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if status == "running" || status == "pending" {
                                if ui.button(
                                    egui::RichText::new("Kill")
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(239, 68, 68)),
                                ).clicked() {
                                    kill_id = Some(id.to_string());
                                }
                            }
                        });
                    });

                    // Info line
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("ID: {}  |  {}  |  {} progress entries",
                                &id[..8.min(id.len())], created, progress_count))
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                    });

                    // Expanded: show result + progress
                    if is_expanded {
                        ui.add_space(4.0);

                        // Result
                        if let Some(result) = task["result"].as_str() {
                            ui.separator();
                            ui.label(egui::RichText::new("Result:").size(12.0).strong());
                            egui::ScrollArea::vertical()
                                .max_height(200.0)
                                .id_salt(format!("remote_result_{}", id))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(result)
                                            .size(12.0)
                                            .monospace(),
                                    );
                                });
                        }

                        // Progress
                        if let Some(progress) = task["progress"].as_array() {
                            if !progress.is_empty() {
                                ui.separator();
                                ui.label(egui::RichText::new("Progress:").size(12.0).strong());
                                egui::ScrollArea::vertical()
                                    .max_height(150.0)
                                    .id_salt(format!("remote_prog_{}", id))
                                    .show(ui, |ui| {
                                        for p in progress.iter().rev().take(50) {
                                            let msg = p["message"].as_str().unwrap_or("");
                                            let ts = p["timestamp"].as_str().unwrap_or("");
                                            ui.label(
                                                egui::RichText::new(format!("[{}] {}", ts, msg))
                                                    .size(11.0)
                                                    .monospace()
                                                    .color(egui::Color32::GRAY),
                                            );
                                        }
                                    });
                            }
                        }
                    }
                });
                ui.add_space(4.0);
            }

            // Handle kills
            if let Some(kid) = kill_id {
                runtime.block_on(crate::server::routes::remote::kill_remote_task(&kid));
                self.remote_needs_refresh = true;
            }
            // Handle expand toggle
            if let Some(tid) = toggle_id {
                if self.expanded_remote_id.as_deref() == Some(&tid) {
                    self.expanded_remote_id = None;
                } else {
                    self.expanded_remote_id = Some(tid);
                }
            }
        });
    }

    // ===================================================================
    // FINISHED TAB
    // ===================================================================
    fn show_finished_tab(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let finished_chat_sessions: Vec<FinishedChatSession> = {
            finished_chats().lock().unwrap().clone()
        };
        let total_finished = self.finished_tasks.len() + finished_chat_sessions.len();

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} finished tasks",
                    total_finished
                ))
                .weak()
                .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear History").clicked() {
                    self.finished_tasks.clear();
                    finished_chats().lock().unwrap().clear();
                    runtime.spawn(async move {
                        let empty: Vec<FinishedTask> = Vec::new();
                        data::write_json("finished_tasks.json", &empty).await;
                    });
                }
                if ui.button("Refresh").clicked() {
                    self.finished_needs_load = true;
                }
            });
        });

        ui.add_space(4.0);

        if self.finished_tasks.is_empty() && finished_chat_sessions.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("No finished tasks yet.");
            });
            return;
        }

        // Show finished chat sessions first
        if !finished_chat_sessions.is_empty() {
            ui.label(
                egui::RichText::new("Completed Chat Sessions")
                    .size(12.0)
                    .strong()
                    .color(egui::Color32::from_rgb(34, 197, 94)),
            );
            ui.add_space(4.0);

            for chat in finished_chat_sessions.iter().rev() {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin::same(10))
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("[OK]")
                                    .color(egui::Color32::from_rgb(34, 197, 94))
                                    .monospace()
                                    .small(),
                            );
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(&chat.title);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.add(
                                            egui::Button::new(
                                                egui::RichText::new("Go to Chat")
                                                    .color(egui::Color32::WHITE)
                                                    .small(),
                                            )
                                            .fill(egui::Color32::from_rgb(18, 154, 145))
                                            .corner_radius(4.0),
                                        ).clicked() {
                                            self.navigate_to_chat = Some(chat.session_id.clone());
                                        }
                                    });
                                });
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("Duration: {:.1}s", chat.duration_secs))
                                            .small()
                                            .weak(),
                                    );
                                    if chat.agent_count > 0 {
                                        ui.label(
                                            egui::RichText::new(format!("Agents: {}", chat.agent_count))
                                                .small()
                                                .color(egui::Color32::from_rgb(34, 197, 94)),
                                        );
                                    }
                                    if chat.tool_calls > 0 {
                                        ui.label(
                                            egui::RichText::new(format!("Tools: {}", chat.tool_calls))
                                                .small()
                                                .color(egui::Color32::from_rgb(18, 154, 145)),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Finished: {}",
                                            &chat.finished_at.get(..19).unwrap_or(&chat.finished_at)
                                        ))
                                        .small()
                                        .weak(),
                                    );
                                });
                            });
                        });
                    });
                ui.add_space(4.0);
            }
            ui.add_space(8.0);
        }

        if self.finished_tasks.is_empty() {
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Show in reverse chronological order
                let tasks_reversed: Vec<FinishedTask> =
                    self.finished_tasks.iter().rev().cloned().collect();

                for task in &tasks_reversed {
                    let (status_color, status_icon) = match task.status.as_str() {
                        "success" => (
                            egui::Color32::from_rgb(34, 197, 94),
                            "OK",
                        ),
                        "error" => (
                            egui::Color32::from_rgb(239, 68, 68),
                            "ERR",
                        ),
                        "cancelled" => (
                            egui::Color32::from_rgb(250, 204, 21),
                            "CANCELLED",
                        ),
                        _ => (egui::Color32::GRAY, "?"),
                    };

                    let bg = if task.status == "error" {
                        egui::Color32::from_rgba_unmultiplied(239, 68, 68, 15)
                    } else {
                        ui.visuals().faint_bg_color
                    };

                    egui::Frame::new()
                        .fill(bg)
                        .inner_margin(egui::Margin::same(10))
                        .corner_radius(4.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // Status badge
                                ui.label(
                                    egui::RichText::new(format!("[{}]", status_icon))
                                        .color(status_color)
                                        .monospace()
                                        .small(),
                                );

                                ui.vertical(|ui| {
                                    ui.strong(&task.name);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Duration: {:.1}s",
                                                task.duration_secs
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                        if let Some(code) = task.exit_code {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "Exit: {}",
                                                    code
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                        }
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Finished: {}",
                                                &task.finished_at
                                                    .get(..19)
                                                    .unwrap_or(&task.finished_at)
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                    });
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "$ {}",
                                            task.command
                                        ))
                                        .monospace()
                                        .small()
                                        .color(egui::Color32::GRAY),
                                    );

                                    // Truncated output preview
                                    if !task.output.is_empty() {
                                        let is_expanded =
                                            self.expanded_finished_id.as_deref()
                                                == Some(&task.id);
                                        let preview = if is_expanded {
                                            task.output.clone()
                                        } else if task.output.len() > 200 {
                                            format!(
                                                "{}...",
                                                crate::util::truncate_utf8(&task.output, 200)
                                            )
                                        } else {
                                            task.output.clone()
                                        };

                                        ui.add_space(2.0);
                                        egui::Frame::new()
                                            .fill(egui::Color32::from_rgb(30, 30, 30))
                                            .inner_margin(egui::Margin::same(6))
                                            .corner_radius(3.0)
                                            .show(ui, |ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(&preview)
                                                            .monospace()
                                                            .small()
                                                            .color(
                                                                egui::Color32::from_rgb(
                                                                    180, 180, 180,
                                                                ),
                                                            ),
                                                    )
                                                    .wrap(),
                                                );
                                            });

                                        if task.output.len() > 200 {
                                            let label = if is_expanded {
                                                "Collapse"
                                            } else {
                                                "Show full output"
                                            };
                                            if ui
                                                .small_button(label)
                                                .clicked()
                                            {
                                                if is_expanded {
                                                    self.expanded_finished_id = None;
                                                } else {
                                                    self.expanded_finished_id =
                                                        Some(task.id.clone());
                                                }
                                            }
                                        }
                                    }
                                });
                            });
                        });

                    ui.add_space(4.0);
                }
            });
    }
}
