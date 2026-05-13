use eframe::egui;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::server::data::{
    get_chat_history, get_projects, get_settings, save_chat_history, ChatMessage,
    ChatMessageFeedback, ChatSession, Project,
};
use crate::server::services::toolbox::{
    call_with_tools, call_with_tools_realtime, force_create_architecture,
    get_session_architecture, load_agent_yaml,
    start_realtime_session, SubAgentConfig, ToolUpdate,
};
use crate::ui::output_panel::OutputPanel;

/// Return the largest byte index <= `max_bytes` that is on a char boundary.
fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() { return s.len(); }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    i
}

// -------------------------------------------------------------------------
// Lightweight sidebar summary (avoids cloning all messages for the list)
// -------------------------------------------------------------------------

#[allow(dead_code)]
struct ChatSessionSummary {
    id: String,
    title: String,
    message_count: usize,
    updated_at: String,
    project_id: Option<String>,
    last_message_preview: String,   // last AI or user message snippet
    last_message_role: String,      // "user" or "assistant"
}

// -------------------------------------------------------------------------
// Streaming state shared between UI and background task
// -------------------------------------------------------------------------

#[derive(Clone)]
#[allow(dead_code)]
struct ToolCallDisplay {
    name: String,
    status: String, // "calling...", "done", "error"
    args_preview: String,
    result_preview: String,
}

#[derive(Clone)]
#[allow(dead_code)]
struct StreamingState {
    /// The text accumulated so far from the streaming response.
    text: Arc<Mutex<String>>,
    /// Whether the streaming task has finished.
    done: Arc<Mutex<bool>>,
    /// Any error that occurred during streaming.
    error: Arc<Mutex<Option<String>>>,
    /// Tool calls made during the response.
    tool_calls: Arc<Mutex<Vec<ToolCallDisplay>>>,
    /// Output files produced during the response.
    files: Arc<Mutex<Vec<String>>>,
    /// Log lines for agent activity
    log_lines: Arc<Mutex<Vec<String>>>,
    /// Pending tool approval: (tool_name, args_preview)
    pending_approval: Arc<Mutex<Option<(String, String)>>>,
    /// Cancellation flag — set to true to abort the task
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl StreamingState {
    fn new() -> Self {
        Self {
            text: Arc::new(Mutex::new(String::new())),
            done: Arc::new(Mutex::new(false)),
            error: Arc::new(Mutex::new(None)),
            tool_calls: Arc::new(Mutex::new(Vec::new())),
            files: Arc::new(Mutex::new(Vec::new())),
            log_lines: Arc::new(Mutex::new(Vec::new())),
            pending_approval: Arc::new(Mutex::new(None)),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        *self.done.lock().unwrap() = true;
        let mut err = self.error.lock().unwrap();
        if err.is_none() {
            *err = Some("Stopped by user".to_string());
        }
    }

    fn get_text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    fn is_done(&self) -> bool {
        *self.done.lock().unwrap()
    }

    fn get_error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    fn get_tool_calls(&self) -> Vec<ToolCallDisplay> {
        self.tool_calls.lock().unwrap().clone()
    }

    fn get_files(&self) -> Vec<String> {
        self.files.lock().unwrap().clone()
    }

    fn get_log(&self) -> String {
        self.log_lines.lock().unwrap().join("\n")
    }

    #[allow(dead_code)]
    fn push_log(&self, line: String) {
        self.log_lines.lock().unwrap().push(line);
    }
}

// -------------------------------------------------------------------------
// Parsed markdown segment for rich rendering
// -------------------------------------------------------------------------

#[allow(dead_code)]
enum MdSegment {
    Text(String),
    Bold(String),
    Italic(String),
    InlineCode(String),
    CodeBlock { lang: String, code: String },
    ListItem(String),
    Heading(u8, String),
    Blockquote(String),
    HorizontalRule,
    NumberedListItem(String, String), // (number, text)
}

#[allow(dead_code)]
fn parse_markdown(input: &str) -> Vec<MdSegment> {
    let mut segments: Vec<MdSegment> = Vec::new();
    let mut lines = input.lines().peekable();

    while let Some(line) = lines.next() {
        // Code block fence
        if line.trim_start().starts_with("```") {
            let lang = line.trim_start().trim_start_matches('`').trim().to_string();
            let mut code_lines: Vec<String> = Vec::new();
            let mut closed = false;
            while let Some(inner) = lines.next() {
                if inner.trim_start().starts_with("```") {
                    closed = true;
                    break;
                }
                code_lines.push(inner.to_string());
            }
            segments.push(MdSegment::CodeBlock {
                lang,
                code: code_lines.join("\n"),
            });
            if !closed {
                // unclosed fence, that's fine
            }
            continue;
        }

        // Horizontal rule
        {
            let trimmed = line.trim();
            if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                segments.push(MdSegment::HorizontalRule);
                continue;
            }
        }

        // Heading
        if line.starts_with("### ") {
            segments.push(MdSegment::Heading(3, line[4..].to_string()));
            continue;
        }
        if line.starts_with("## ") {
            segments.push(MdSegment::Heading(2, line[3..].to_string()));
            continue;
        }
        if line.starts_with("# ") {
            segments.push(MdSegment::Heading(1, line[2..].to_string()));
            continue;
        }

        // Blockquote
        if line.starts_with("> ") {
            segments.push(MdSegment::Blockquote(line[2..].to_string()));
            continue;
        }

        // List item
        if line.starts_with("- ") || line.starts_with("* ") {
            segments.push(MdSegment::ListItem(line[2..].to_string()));
            continue;
        }
        // Numbered list
        if let Some((num, rest)) = try_strip_numbered_list_with_num(line) {
            segments.push(MdSegment::NumberedListItem(num, rest));
            continue;
        }

        // Inline parsing for the line
        parse_inline_segments(line, &mut segments);
        segments.push(MdSegment::Text("\n".to_string()));
    }

    segments
}

#[allow(dead_code)]
fn try_strip_numbered_list_with_num(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let dot_pos = trimmed.find(". ")?;
    let num_part = &trimmed[..dot_pos];
    if num_part.chars().all(|c| c.is_ascii_digit()) && !num_part.is_empty() {
        Some((num_part.to_string(), trimmed[dot_pos + 2..].to_string()))
    } else {
        None
    }
}

#[allow(dead_code)]
fn parse_inline_segments(text: &str, segments: &mut Vec<MdSegment>) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut buf = String::new();

    while i < len {
        // Inline code: `...`
        if chars[i] == '`' {
            if !buf.is_empty() {
                segments.push(MdSegment::Text(std::mem::take(&mut buf)));
            }
            i += 1;
            let mut code = String::new();
            while i < len && chars[i] != '`' {
                code.push(chars[i]);
                i += 1;
            }
            if i < len {
                i += 1; // skip closing `
            }
            segments.push(MdSegment::InlineCode(code));
            continue;
        }

        // Bold: **...**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if !buf.is_empty() {
                segments.push(MdSegment::Text(std::mem::take(&mut buf)));
            }
            i += 2;
            let mut bold = String::new();
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '*') {
                bold.push(chars[i]);
                i += 1;
            }
            if i + 1 < len {
                i += 2; // skip closing **
            }
            segments.push(MdSegment::Bold(bold));
            continue;
        }

        // Italic: *...*  (single star, not followed by another star)
        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            if !buf.is_empty() {
                segments.push(MdSegment::Text(std::mem::take(&mut buf)));
            }
            i += 1;
            let mut italic = String::new();
            while i < len && chars[i] != '*' {
                italic.push(chars[i]);
                i += 1;
            }
            if i < len {
                i += 1; // skip closing *
            }
            segments.push(MdSegment::Italic(italic));
            continue;
        }

        buf.push(chars[i]);
        i += 1;
    }

    if !buf.is_empty() {
        segments.push(MdSegment::Text(buf));
    }
}

// -------------------------------------------------------------------------
// Graphic monitor types (agent network diagram)
// -------------------------------------------------------------------------

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct GraphicAgent {
    id: String,
    name: String,
    role: String,     // orchestrator, worker, peer, human
    status: String,   // idle, working, done
    x: f32,
    y: f32,
    color: egui::Color32,
    last_tool: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct GraphicEdge {
    from: String,
    to: String,
    label: String,
    protocol: String, // tcp, bus, delegate, spawn
    state: String,    // idle, active, done
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct GraphicSignal {
    from: String,
    to: String,
    kind: String,     // delegate, direct, bus, spawn
    tool: String,
    started_at: f64,  // seconds since epoch
}

// Color palette for agent nodes
fn agent_node_color(index: usize) -> egui::Color32 {
    const PALETTE: &[(u8, u8, u8)] = &[
        (99, 102, 241),   // indigo
        (236, 72, 153),   // pink
        (34, 197, 94),    // green
        (245, 158, 11),   // amber
        (6, 182, 212),    // cyan
        (168, 85, 247),   // purple
        (239, 68, 68),    // red
        (59, 130, 246),   // blue
        (16, 185, 129),   // emerald
        (249, 115, 22),   // orange
    ];
    let (r, g, b) = PALETTE[index % PALETTE.len()];
    egui::Color32::from_rgb(r, g, b)
}

fn link_kind_color(kind: &str) -> egui::Color32 {
    match kind {
        "delegate" => egui::Color32::from_rgb(245, 158, 11),  // amber
        "direct"   => egui::Color32::from_rgb(219, 39, 119),  // pink
        "bus"      => egui::Color32::from_rgb(8, 145, 178),   // cyan
        "spawn"    => egui::Color32::from_rgb(124, 58, 237),  // purple
        _          => egui::Color32::from_rgb(156, 163, 175), // gray
    }
}

fn tool_to_link_kind(tool: &str) -> &'static str {
    match tool {
        "send_task" | "wait_result" | "bb_propose" | "bb_bid" | "bb_award" => "delegate",
        "spawn_subagent" => "spawn",
        "proto_tcp_send" | "proto_tcp_read" => "direct",
        "proto_bus_publish" | "proto_bus_history" => "bus",
        _ => "delegate",
    }
}

// -------------------------------------------------------------------------
// ChatView
// -------------------------------------------------------------------------

#[allow(dead_code)]
pub struct ChatView {
    sessions: Vec<ChatSessionSummary>,
    pub selected_session_id: Option<String>,
    selected_session: Option<ChatSession>,
    input_text: String,
    rename_text: String,
    renaming_session_id: Option<String>,
    pub needs_refresh: bool,
    scroll_to_bottom: bool,
    confirm_delete_id: Option<String>,

    // --- Streaming AI responses (multiple sessions can stream in parallel) ---
    active_streams: std::collections::HashMap<String, StreamingState>,

    // --- File attachments ---
    attached_files: Vec<AttachedFile>,

    // --- Project selector ---
    projects: Vec<Project>,
    pub selected_project_id: Option<String>,
    projects_loaded: bool,

    // --- Output panel ---
    output_panel: OutputPanel,

    // --- Resizable sidebar ---
    sidebar_width: f32,

    // --- Log panel ---
    show_log_panel: bool,
    log_session_id: Option<String>,
    log_content: String,
    log_agent_history: String,
    log_tab: u8, // 0=chat log, 1=agent history, 2=graphic monitor

    // --- Tool approval ---
    pending_approval: Option<(String, String)>, // (tool_name, args_preview)

    // --- Graphic monitor ---
    graphic_agents: Vec<GraphicAgent>,
    graphic_edges: Vec<GraphicEdge>,
    graphic_signals: Vec<GraphicSignal>,
    graphic_loaded_config: String, // track which config was loaded
    graphic_pan: egui::Vec2,
    graphic_zoom: f32,
    graphic_drag_start: Option<egui::Pos2>,
    graphic_last_reload: f64, // last auto-reload time (seconds)
}

#[derive(Clone)]
struct AttachedFile {
    name: String,
    content: String,
}

impl Default for ChatView {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatView {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected_session_id: None,
            selected_session: None,
            input_text: String::new(),
            rename_text: String::new(),
            renaming_session_id: None,
            needs_refresh: true,
            scroll_to_bottom: false,
            confirm_delete_id: None,
            active_streams: std::collections::HashMap::new(),
            attached_files: Vec::new(),
            projects: Vec::new(),
            selected_project_id: None,
            projects_loaded: false,
            output_panel: OutputPanel::default(),
            sidebar_width: 262.0,
            show_log_panel: false,
            log_session_id: None,
            log_content: String::new(),
            log_agent_history: String::new(),
            log_tab: 0,
            pending_approval: None,
            graphic_agents: Vec::new(),
            graphic_edges: Vec::new(),
            graphic_signals: Vec::new(),
            graphic_loaded_config: String::new(),
            graphic_pan: egui::Vec2::ZERO,
            graphic_zoom: 1.0,
            graphic_drag_start: None,
            graphic_last_reload: 0.0,
        }
    }

    // ---------------------------------------------------------------------
    // Data access helpers (blocking on the tokio runtime)
    // ---------------------------------------------------------------------

    fn refresh(&mut self, runtime: &tokio::runtime::Handle) {
        let all_sessions = runtime.block_on(get_chat_history());

        // Load projects if not yet loaded
        if !self.projects_loaded {
            self.projects = runtime.block_on(get_projects());
            self.projects_loaded = true;
        }

        // Build sidebar summaries
        self.sessions = all_sessions
            .iter()
            .map(|s| {
                // Find last message for preview
                let last = s.messages.last();
                let last_message_preview = last
                    .map(|m| {
                        // Strip markdown/think tags for clean preview
                        let raw = m.content
                            .split("</think>").last().unwrap_or(&m.content)
                            .replace('\n', " ");
                        let stripped = raw.trim_start_matches(|c: char| c.is_whitespace() || c == '#' || c == '*');
                        let clean: String = stripped.chars().take(80).collect();
                        if stripped.len() > 80 { format!("{}…", clean) } else { clean.to_string() }
                    })
                    .unwrap_or_default();
                let last_message_role = last.map(|m| m.role.clone()).unwrap_or_default();

                ChatSessionSummary {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    message_count: s.messages.len(),
                    updated_at: s.updated_at.clone(),
                    project_id: s.project_id.clone(),
                    last_message_preview,
                    last_message_role,
                }
            })
            .collect();

        // Sort newest first
        self.sessions
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        // Refresh selected session data (skip while streaming to preserve in-memory user message)
        if let Some(ref sel_id) = self.selected_session_id {
            if !self.active_streams.contains_key(sel_id) {
                self.selected_session = all_sessions.into_iter().find(|s| &s.id == sel_id);
            }
        } else {
            self.selected_session = None;
        }

        self.needs_refresh = false;
    }

    fn create_session(&mut self, runtime: &tokio::runtime::Handle) {
        if crate::server::data::get_remote_backend().is_some() {
            // Remote mode: create session via API
            let pid = self.selected_project_id.as_deref();
            let new_id = runtime.block_on(async {
                crate::server::data::remote_create_chat_session("New Chat", pid).await
                    .map(|s| s.id)
            });
            if let Some(id) = new_id {
                self.selected_session_id = Some(id);
                self.needs_refresh = true;
            }
            return;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let session = ChatSession {
            id: Uuid::new_v4().to_string(),
            title: "New Chat".to_string(),
            messages: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            skill_candidate: None,
            skill_feedback: None,
            project_id: self.selected_project_id.clone(),
        };
        let new_id = session.id.clone();

        runtime.block_on(async {
            let mut sessions = get_chat_history().await;
            sessions.push(session);
            save_chat_history(&sessions).await;
        });

        self.selected_session_id = Some(new_id);
        self.needs_refresh = true;
    }

    fn delete_session(&mut self, runtime: &tokio::runtime::Handle, session_id: &str) {
        let sid = session_id.to_string();
        runtime.block_on(async {
            let sessions = get_chat_history().await;
            let filtered: Vec<ChatSession> =
                sessions.into_iter().filter(|s| s.id != sid).collect();
            save_chat_history(&filtered).await;
            crate::server::data::delete_agent_history(&sid).await;
        });

        if self.selected_session_id.as_deref() == Some(session_id) {
            self.selected_session_id = None;
            self.selected_session = None;
        }
        self.needs_refresh = true;
    }

    fn rename_session(
        &mut self,
        runtime: &tokio::runtime::Handle,
        session_id: &str,
        new_title: &str,
    ) {
        let sid = session_id.to_string();
        let title = new_title.to_string();
        runtime.block_on(async {
            let mut sessions = get_chat_history().await;
            if let Some(s) = sessions.iter_mut().find(|s| s.id == sid) {
                s.title = title;
                s.updated_at = chrono::Utc::now().to_rfc3339();
            }
            save_chat_history(&sessions).await;
        });
        self.needs_refresh = true;
    }

    fn send_message(&mut self, runtime: &tokio::runtime::Handle, ctx: &egui::Context) {
        let text = self.input_text.trim().to_string();
        if text.is_empty() && self.attached_files.is_empty() {
            return;
        }

        // Build content with file attachments
        let mut full_content = text.clone();
        let mut file_names: Vec<String> = Vec::new();
        for file in &self.attached_files {
            file_names.push(file.name.clone());
            full_content.push_str(&format!(
                "\n\n--- Attached file: {} ---\n{}\n--- End of {} ---",
                file.name, file.content, file.name
            ));
        }
        let file_names_opt = if file_names.is_empty() { None } else { Some(file_names) };

        // Auto-create remote session if needed (separate API call)
        if self.selected_session_id.is_none() && crate::server::data::get_remote_backend().is_some() {
            let pid = self.selected_project_id.as_deref();
            let new_id = runtime.block_on(async {
                crate::server::data::remote_create_chat_session("New Chat", pid).await
                    .map(|s| s.id)
            });
            if let Some(id) = new_id {
                self.selected_session_id = Some(id);
                self.needs_refresh = true;
            } else {
                return;
            }
        }

        // Create local session in memory if needed
        let need_new = self.selected_session_id.is_none();
        let sid = if need_new {
            let new_id = Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            self.selected_session = Some(ChatSession {
                id: new_id.clone(),
                title: "New Chat".to_string(),
                messages: Vec::new(),
                created_at: now.clone(),
                updated_at: now,
                skill_candidate: None,
                skill_feedback: None,
                project_id: self.selected_project_id.clone(),
            });
            self.selected_session_id = Some(new_id.clone());
            new_id
        } else {
            self.selected_session_id.clone().unwrap()
        };

        // Update in-memory session with user message (no disk write — saved when stream finishes)
        if let Some(ref mut session) = self.selected_session {
            session.messages.push(ChatMessage {
                role: "user".to_string(),
                content: full_content.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                files: file_names_opt,
                feedback: None,
            });
            session.updated_at = chrono::Utc::now().to_rfc3339();
        }

        self.input_text.clear();
        self.attached_files.clear();
        self.scroll_to_bottom = true;
        // Don't set needs_refresh here — avoid disk read that would overwrite in-memory session.
        // Sidebar + disk persistence happen when stream finishes in poll_streaming.

        // Start streaming API call
        self.start_streaming(runtime, ctx, &sid, &full_content);
    }

    fn start_streaming(
        &mut self,
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
        session_id: &str,
        user_message: &str,
    ) {
        // Batch-load settings + projects in one block_on (small files, fast)
        let (settings, cached_projects) = runtime.block_on(async {
            let s = get_settings().await;
            let p = get_projects().await;
            (s, p)
        });
        let api_key = settings.tiger_bot_api_key.clone();
        let model = if settings.tiger_bot_model.is_empty() {
            "deepseek-chat".to_string()
        } else {
            settings.tiger_bot_model.clone()
        };
        let raw_url = settings
            .tiger_bot_api_url
            .clone()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "https://api.deepseek.com/v1".to_string());
        // Ensure URL ends with /chat/completions (OpenAI-compatible format)
        // Skip for claude-code provider which uses CLI, not HTTP
        let api_url = if raw_url == "claude-code" {
            raw_url
        } else if raw_url.ends_with("/chat/completions") {
            raw_url
        } else {
            format!("{}/chat/completions", raw_url.trim_end_matches('/'))
        };

        // Use project's working folder if available, otherwise global sandbox
        let sandbox_dir = {
            // The project filter dropdown is the primary source of truth.
            // Also backfill the session's project_id if missing.
            let active_project_id = self.selected_project_id.clone()
                .or_else(|| self.selected_session.as_ref().and_then(|s| s.project_id.clone()));

            // Backfill: if we have a project_id but the session doesn't (non-blocking)
            if let Some(ref pid) = active_project_id {
                let sid = session_id.to_string();
                let pid2 = pid.clone();
                runtime.spawn(async move {
                    let mut sessions = get_chat_history().await;
                    if let Some(s) = sessions.iter_mut().find(|s| s.id == sid) {
                        if s.project_id.is_none() {
                            s.project_id = Some(pid2);
                            save_chat_history(&sessions).await;
                        }
                    }
                });
            }

            let project_folder = active_project_id.as_ref().and_then(|pid| {
                cached_projects.iter()
                    .find(|p| &p.id == pid)
                    .map(|p| p.working_folder.clone())
            }).filter(|f| !f.is_empty());

            if let Some(folder) = project_folder {
                // Ensure project folder exists
                let _ = std::fs::create_dir_all(&folder);
                folder
            } else {
                // Resolve sandbox dir — convert relative paths to absolute under app data
                let raw = settings.sandbox_dir.clone();
                let sandbox_path = if raw.is_empty() || !std::path::Path::new(&raw).is_absolute() {
                    crate::server::data::data_dir()
                        .parent()
                        .unwrap_or(&std::path::PathBuf::from("."))
                        .join(if raw.is_empty() { "sandbox" } else { &raw })
                } else {
                    std::path::PathBuf::from(&raw)
                };
                let _ = std::fs::create_dir_all(&sandbox_path);
                sandbox_path.to_string_lossy().to_string()
            }
        };

        if api_key.is_empty() {
            // No API key - save a placeholder message
            let sid = session_id.to_string();
            runtime.block_on(async {
                let mut sessions = get_chat_history().await;
                if let Some(s) = sessions.iter_mut().find(|s| s.id == sid) {
                    s.messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: "[No API key configured. Go to Settings to add your API key.]"
                            .to_string(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        files: None,
                        feedback: None,
                    });
                    s.updated_at = chrono::Utc::now().to_rfc3339();
                }
                save_chat_history(&sessions).await;
            });
            self.needs_refresh = true;
            return;
        }

        // Build messages array from in-memory session (no disk read needed)
        let sid = session_id.to_string();
        let messages: Vec<serde_json::Value> = self.selected_session
            .as_ref()
            .filter(|s| s.id == sid)
            .map(|s| {
                s.messages
                    .iter()
                    .filter(|m| m.role == "user" || m.role == "assistant")
                    .map(|m| {
                        serde_json::json!({
                            "role": m.role,
                            "content": m.content,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Build sub-agent config from settings + active project
        let sub_agent_mode = settings.sub_agent_mode.clone().unwrap_or_else(|| "auto".to_string());
        let is_realtime = sub_agent_mode == "manual";

        let sub_agent_config = {
            let enabled = settings.sub_agent_enabled.unwrap_or(false);

            // Check for project-level agent override, fall back to global settings
            let config_file = self
                .selected_project_id
                .as_ref()
                .and_then(|pid| {
                    cached_projects.iter().find(|p| p.id == *pid).and_then(|p| {
                        p.agent_override.as_ref().and_then(|ov| {
                            if ov.enabled.unwrap_or(false) {
                                ov.sub_agent_config_file.clone()
                            } else {
                                None
                            }
                        })
                    })
                })
                .or_else(|| settings.sub_agent_config_file.clone())
                .unwrap_or_default();

            // fully_auto and auto_swarm don't need a config file upfront
            let needs_config = !matches!(sub_agent_mode.as_str(), "fully_auto" | "auto_swarm");

            if enabled && (!config_file.is_empty() || !needs_config) {
                let agent_ids = if !config_file.is_empty() {
                    load_agent_yaml(&config_file)
                        .map(|(_, ids)| ids)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                // Boot realtime session if mode is "realtime"
                if is_realtime && !config_file.is_empty() {
                    let sid2 = sid.clone();
                    let cf2 = config_file.clone();
                    let ak2 = api_key.clone();
                    let au2 = api_url.clone();
                    let m2 = settings.sub_agent_model.clone().unwrap_or_else(|| model.clone());
                    let sd2 = sandbox_dir.clone();
                    runtime.block_on(async move {
                        start_realtime_session(&sid2, &cf2, &ak2, &au2, &m2, &sd2).await;
                    });
                }

                SubAgentConfig {
                    enabled: true,
                    config_file,
                    agent_ids,
                    api_key: api_key.clone(),
                    api_url: api_url.clone(),
                    model: settings.sub_agent_model.clone().unwrap_or_else(|| model.clone()),
                    depth: 0,
                    session_id: sid.clone(),
                    agent_id: "main".to_string(),
                    mode: sub_agent_mode.clone(),
                    agent_role: "orchestrator".to_string(),
                    cancel_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                }
            } else {
                SubAgentConfig::default()
            }
        };

        // Build system prompt: base + project context + sub-agent info
        let (sub_agent_prompt, research_instruction) = if sub_agent_config.enabled
            && (!sub_agent_config.agent_ids.is_empty()
                || matches!(sub_agent_mode.as_str(), "fully_auto" | "auto_swarm"))
        {
            let agents = sub_agent_config.agent_ids.join(", ");
            match sub_agent_mode.as_str() {
                "fully_auto" => {
                    // Architecture is created proactively before LLM is called.
                    // By the time the LLM sees this prompt, agents should already be LIVE.
                    // (This prompt is fallback — normally fully_auto dispatches directly via UI)
                    let orch = sub_agent_config.config_file.as_str();
                    let yaml_orch_mode = if !orch.is_empty() {
                        crate::server::services::toolbox::load_agent_yaml(orch)
                            .and_then(|(y, _)| y.get("system")?.get("orchestration_mode")?.as_str().map(|s| s.to_string()))
                            .unwrap_or_default()
                    } else { String::new() };

                    let prompt = if agents.is_empty() {
                        "\n\nFULLY AUTO MODE: An agent team is being created for this task. \
Use send_task/wait_result to delegate work once agents are ready. \
If no agents are available yet, call create_architecture to design and boot a team. \
Do NOT attempt to do work yourself — delegate everything to agents.".to_string()
                    } else if yaml_orch_mode == "pipeline" {
                        format!(
                            "\n\nFULLY AUTO MODE (PIPELINE): An agent pipeline has been created and all agents are LIVE. \
Pipeline agents: [{}]. \
This is a SEQUENTIAL PIPELINE — send the task to the FIRST agent only. \
The first agent will process and forward to the next stage automatically via send_task. \
Workflow: send_task({{to: \"<first_agent>\", task: \"...\"}}) → wait_result({{from: \"<last_agent>\"}}) to get the final output. \
Do NOT send tasks to intermediate or final agents directly — the pipeline flows automatically.",
                            agents
                        )
                    } else {
                        format!(
                            "\n\nFULLY AUTO MODE: An agent team has been created and all agents are LIVE. \
Available agents: [{}]. \
You MUST delegate ALL work to agents via send_task/wait_result. \
Workflow: send_task({{to: \"<agentId>\", task: \"...\"}}) → wait_result({{from: \"<agentId>\"}}) → synthesize response. \
Only use run_python/write_file for formatting the final output. \
Do NOT do research or analysis yourself — agents handle that. \
If an orchestrator exists, send tasks ONLY to the orchestrator.",
                            agents
                        )
                    };
                    (prompt, "Delegate ALL tasks to agents via send_task/wait_result.")
                }
                "auto_swarm" => {
                    // List available YAML files for the LLM to pick from
                    let mut swarm_list = String::new();
                    if let Ok(entries) = std::fs::read_dir(crate::server::data::data_dir().join("agents")) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if name.ends_with(".yaml") || name.ends_with(".yml") {
                                if let Some((config, _)) = load_agent_yaml(&name) {
                                    let sys_name = config["system"]["name"].as_str().unwrap_or(&name);
                                    let mode = config["system"]["orchestration_mode"].as_str().unwrap_or("hierarchical");
                                    let agent_count = config["agents"].as_array().map(|a| a.len()).unwrap_or(0);
                                    swarm_list.push_str(&format!("\n  - \"{}\": {} [{}] ({} agents)", name, sys_name, mode, agent_count));
                                }
                            }
                        }
                    }
                    let prompt = format!(
                        "\n\nAUTO-SWARM MODE: You MUST call select_swarm FIRST to choose the best agent team for the user's task. \
Available swarm configurations:{}\n\
After selecting a swarm, all agents will be LIVE. Then use send_task/wait_result to delegate work. \
Do NOT attempt to do work yourself until a swarm is selected.",
                        if swarm_list.is_empty() { "\n  (No swarm configs found in data/agents/)".to_string() } else { swarm_list }
                    );
                    (prompt, "Call select_swarm first, then delegate via send_task/wait_result.")
                }
                "manual" => {
                    let orch = sub_agent_config.config_file.as_str();
                    let yaml_orch_mode = if !orch.is_empty() {
                        crate::server::services::toolbox::load_agent_yaml(orch)
                            .and_then(|(y, _)| y.get("system")?.get("orchestration_mode")?.as_str().map(|s| s.to_string()))
                            .unwrap_or_default()
                    } else { String::new() };

                    let prompt = if yaml_orch_mode == "pipeline" {
                        format!(
                            "\n\nMANUAL AGENT MODE (PIPELINE): All agents are alive in a sequential pipeline. \
Pipeline agents: [{}]. \
Send the task to the FIRST agent only — it will automatically forward through the chain via send_task. \
Workflow: send_task({{to: \"<first_agent>\", task: \"...\"}}) → wait_result({{from: \"<last_agent>\"}}) to get the final output. \
Do NOT send tasks to intermediate or final agents directly.",
                            agents
                        )
                    } else {
                        format!(
                            "\n\nMANUAL AGENT MODE: All agents are already alive. You MUST delegate ALL work to the agent team via send_task/wait_result. \
Available agents: [{}]. \
Workflow: send_task({{to: \"<agentId>\", task: \"...\"}}) → wait_result({{from: \"<agentId>\"}}) → synthesize response. \
Only use run_python/write_file for formatting the final output. \
Always delegate, even for simple tasks. If an orchestrator exists, send tasks ONLY to the orchestrator.",
                            agents
                        )
                    };
                    (prompt, "Use send_task/wait_result to delegate ALL tasks to agents.")
                }
                _ => {
                    // "auto" mode
                    let prompt = format!(
                        "\n\nMULTI-AGENT SYSTEM ACTIVE: You have specialist sub-agents available: [{}]. \
IMPORTANT: For research, analysis, marketing, data gathering, or any complex multi-step task, you MUST call spawn_subagent FIRST to delegate to the appropriate specialist agent. \
Do NOT use web_search or run_python directly for tasks that sub-agents can handle. \
Only use your own tools (web_search, run_python, etc.) for quick lookups or tasks not covered by any sub-agent.",
                        agents
                    );
                    (prompt, "Use spawn_subagent to delegate research and analysis tasks to specialist agents.")
                }
            }
        } else {
            (String::new(), "Always use web_search when the user asks for research, information lookup, or current events.")
        };

        let tool_list = match sub_agent_mode.as_str() {
            "fully_auto" => "create_architecture, send_task, wait_result, check_agents, run_python, write_file",
            "auto_swarm" => "select_swarm, send_task, wait_result, check_agents, run_python, write_file",
            "manual" => "web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill, send_task, wait_result, check_agents",
            _ => "web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill, spawn_subagent",
        };

        let base_system = format!(
            "You are TigrimOS, an AI assistant with tool-calling capabilities. \
You have access to these tools: {}. \
{} \
IMPORTANT: Your working directory is the sandbox folder '{}'. All file operations (read_file, write_file, list_files, run_python, run_shell) use this directory as the root. \
When a user asks about files, ALWAYS use list_files first to see what's available in the sandbox. Files uploaded by the user are placed here. \
Use relative paths (e.g. 'score_midterm.xlsx') — they resolve to the sandbox automatically. \
Use run_python for data analysis, charts, and calculations. \
Use run_shell for system commands. \
Provide helpful, detailed responses based on tool results.{}",
            tool_list, research_instruction, sandbox_dir, sub_agent_prompt
        );
        let system_prompt = match self.build_project_system_prompt(runtime) {
            Some(project_prompt) => Some(format!("{}\n\n{}", base_system, project_prompt)),
            None => Some(base_system),
        };

        let state = StreamingState::new();
        self.active_streams.insert(sid.clone(), state.clone());

        // Register active chat session in the shared tasks list
        {
            let title = self.selected_session.as_ref()
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "Chat".to_string());
            let mut chats = crate::ui::tasks_view::active_chats().lock().unwrap();
            chats.retain(|c| c.session_id != sid);
            chats.push(crate::ui::tasks_view::ActiveChatSession {
                session_id: sid.clone(),
                title,
                started_at: chrono::Utc::now(),
                agent_count: 0,
                tool_calls: 0,
            });
        }

        let ctx_clone = ctx.clone();

        // ── Remote mode: POST message to remote server ──────────────
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let remote_sid = sid.clone();
            let remote_state = state.clone();
            let remote_ctx = ctx_clone.clone();
            let remote_msg = user_message.to_string();
            runtime.spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(300))
                    .build()
                    .unwrap_or_default();

                let body = serde_json::json!({ "message": remote_msg });
                let url = format!("{}/api/chat/sessions/{}/messages", rb.url, remote_sid);

                {
                    let mut text = remote_state.text.lock().unwrap();
                    *text = "Waiting for remote server...".to_string();
                }
                remote_ctx.request_repaint();

                match client
                    .post(&url)
                    .bearer_auth(&rb.token)
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if let Ok(val) = resp.json::<serde_json::Value>().await {
                            let content = val.get("content")
                                .or_else(|| val.get("message"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("(no response from remote)");
                            {
                                let mut text = remote_state.text.lock().unwrap();
                                *text = content.to_string();
                            }
                            // Save assistant message to local history
                            let mut sessions = get_chat_history().await;
                            if let Some(s) = sessions.iter_mut().find(|s| s.id == remote_sid) {
                                s.messages.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: content.to_string(),
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                    files: None,
                                    feedback: None,
                                });
                                s.updated_at = chrono::Utc::now().to_rfc3339();
                            }
                            save_chat_history(&sessions).await;
                        } else {
                            let mut text = remote_state.text.lock().unwrap();
                            *text = "(failed to parse remote response)".to_string();
                        }
                    }
                    Err(e) => {
                        let mut err = remote_state.error.lock().unwrap();
                        *err = Some(format!("Remote error: {}", e));
                    }
                }
                {
                    let mut done = remote_state.done.lock().unwrap();
                    *done = true;
                }
                // Remove from active chats
                {
                    let mut chats = crate::ui::tasks_view::active_chats().lock().unwrap();
                    if let Some(active) = chats.iter().find(|c| c.session_id == remote_sid).cloned() {
                        crate::ui::tasks_view::mark_chat_finished(&active);
                    }
                    chats.retain(|c| c.session_id != remote_sid);
                }
                remote_ctx.request_repaint();
            });
            return;
        }

        runtime.spawn(async move {
            let state_text = state.text.clone();
            let state_tool_calls = state.tool_calls.clone();
            let state_error = state.error.clone();
            let state_approval = state.pending_approval.clone();
            let state_log_lines = state.log_lines.clone();
            let state_log_lines2 = state_log_lines.clone();
            let cancelled = state.cancelled.clone();
            let ctx_cb = ctx_clone.clone();

            // Init log with session + user message
            let ts = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
            { state_log_lines.lock().unwrap().push(format!("[{}] === Session: {} ===", ts, sid)); }
            { state_log_lines.lock().unwrap().push(format!("[{}] USER: {}", ts, messages.last().and_then(|m| m["content"].as_str()).unwrap_or(""))); }
            { state_log_lines.lock().unwrap().push(format!("[{}] MODE: enabled={}, mode={}, agents={:?}", ts, sub_agent_config.enabled, sub_agent_config.mode, sub_agent_config.agent_ids)); }

            // Clone for subagent listener, auto_create, and fully_auto (before on_update_cb moves the originals)
            let subagent_log_lines = state_log_lines.clone();
            let subagent_sid = sid.clone();
            let subagent_ctx = ctx_cb.clone();
            let autocreate_log_lines = state_log_lines.clone();
            let autocreate_ctx = ctx_cb.clone();
            let fa_text_pre = state_text.clone();
            let fa_calls_pre = state.tool_calls.clone();

            // Build the on_update closure (same logic for both realtime and manual modes)
            let on_update_cb = move |update: ToolUpdate| {
                match update {
                    ToolUpdate::ToolCall { name, args } => {
                        let mut calls = state_tool_calls.lock().unwrap();
                        // Build a useful preview of args for display
                        let args_preview = if name == "spawn_subagent" {
                            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
                            let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
                            let task_short = if task.len() > 80 { format!("{}...", &task[..floor_char_boundary(task, 80)]) } else { task.to_string() };
                            format!("\u{2192} {} | {}", agent, task_short)
                        } else if name == "send_task" {
                            let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                            let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
                            let task_short = if task.len() > 80 { format!("{}...", &task[..floor_char_boundary(task, 80)]) } else { task.to_string() };
                            format!("\u{2192} {} | {}", to, task_short)
                        } else if name == "wait_result" {
                            let from = args.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                            format!("\u{23F3} waiting for {}", from)
                        } else if name == "run_python" {
                            let code = args.get("code").and_then(|v| v.as_str()).unwrap_or("");
                            let first_line = code.lines().next().unwrap_or("");
                            format!("\u{2192} {}", first_line)
                        } else if name == "web_search" {
                            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                            format!("\u{2192} \"{}\"", q)
                        } else if name == "fetch_url" {
                            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                            format!("\u{2192} {}", url)
                        } else {
                            String::new()
                        };
                        // Log tool call
                        {
                            let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                            let full_args = serde_json::to_string(&args).unwrap_or_default();
                            let args_log = if full_args.len() > 500 { format!("{}...", &full_args[..floor_char_boundary(&full_args, 500)]) } else { full_args };
                            state_log_lines.lock().unwrap().push(format!("[{}] TOOL CALL: {}", ts, name));
                            state_log_lines.lock().unwrap().push(format!("  args: {}", args_log));
                        }
                        calls.push(ToolCallDisplay {
                            name,
                            status: "calling...".to_string(),
                            args_preview,
                            result_preview: String::new(),
                        });
                    }
                    ToolUpdate::ToolResult { name, result } => {
                        let mut calls = state_tool_calls.lock().unwrap();
                        {
                            let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                            let result_str = serde_json::to_string(&result).unwrap_or_default();
                            let short_r = if result_str.len() > 1000 { format!("{}...", &result_str[..floor_char_boundary(&result_str, 1000)]) } else { result_str };
                            state_log_lines.lock().unwrap().push(format!("[{}] TOOL RESULT: {}", ts, name));
                            state_log_lines.lock().unwrap().push(format!("  {}", short_r));
                        }
                        if let Some(tc) = calls.iter_mut().rev().find(|c| c.name == name && c.status == "calling...") {
                            tc.status = "done".to_string();
                            let preview = result
                                .as_object()
                                .and_then(|o| {
                                    o.get("stdout")
                                        .or_else(|| o.get("content"))
                                        .or_else(|| o.get("result"))
                                        .or_else(|| o.get("body"))
                                        .and_then(|v| v.as_str())
                                })
                                .unwrap_or("");
                            let short = if preview.len() > 200 { format!("{}...", &preview[..floor_char_boundary(preview, 200)]) } else { preview.to_string() };
                            tc.result_preview = short;
                        }
                    }
                    ToolUpdate::TextChunk(chunk) => {
                        state_text.lock().unwrap().push_str(&chunk);
                    }
                    ToolUpdate::Error(err) => {
                        let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                        state_log_lines.lock().unwrap().push(format!("[{}] ERROR: {}", ts, err));
                        *state_error.lock().unwrap() = Some(err);
                    }
                    ToolUpdate::ApprovalRequired { name, args } => {
                        let args_str = serde_json::to_string_pretty(&args).unwrap_or_default();
                        let preview = if args_str.len() > 500 {
                            format!("{}...", &args_str[..floor_char_boundary(&args_str, 500)])
                        } else {
                            args_str
                        };
                        *state_approval.lock().unwrap() = Some((name.clone(), preview));
                        let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                        state_log_lines.lock().unwrap().push(
                            format!("[{}] APPROVAL REQUIRED: {} — waiting for user", ts, name)
                        );
                    }
                }
                ctx_cb.request_repaint();
            };

            // Spawn a listener for subagent activity so it appears in the chat log
            let subagent_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let subagent_stop2 = subagent_stop.clone();
            let subagent_listener = tokio::spawn(async move {
                let mut rx = crate::server::services::toolbox::subscribe_subagent_log();
                loop {
                    if subagent_stop2.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
                        Ok(Ok((session_id, _agent_id, line))) => {
                            if session_id == subagent_sid {
                                subagent_log_lines.lock().unwrap().push(line);
                                subagent_ctx.request_repaint();
                            }
                        }
                        Ok(Err(_)) => break, // channel closed
                        Err(_) => {} // timeout, loop again
                    }
                }
            });

            // FULLY_AUTO: create architecture → boot agents → directly delegate
            let mut sub_agent_config = sub_agent_config;
            let system_prompt = system_prompt;
            let mut fully_auto_handled = false;
            let fa_text = fa_text_pre;
            let fa_calls = fa_calls_pre;

            if sub_agent_config.enabled && sub_agent_config.mode == "fully_auto" {
                // Helper: update streaming text so user sees progress
                let fa_update_text = |msg: &str, fa_text: &Arc<Mutex<String>>| {
                    let mut t = fa_text.lock().unwrap();
                    if !t.is_empty() { t.push('\n'); }
                    t.push_str(msg);
                };
                let fa_add_tool = |name: &str, status: &str, preview: &str, fa_calls: &Arc<Mutex<Vec<ToolCallDisplay>>>| {
                    let mut calls = fa_calls.lock().unwrap();
                    // Update existing or add new
                    if let Some(existing) = calls.iter_mut().find(|c| c.name == name) {
                        existing.status = status.to_string();
                        if !preview.is_empty() { existing.result_preview = preview.to_string(); }
                    } else {
                        calls.push(ToolCallDisplay {
                            name: name.to_string(),
                            status: status.to_string(),
                            args_preview: preview.to_string(),
                            result_preview: String::new(),
                        });
                    }
                };

                // Step 1: Get or create architecture
                let config_file = match get_session_architecture(&sid).await {
                    Some(existing_file) => {
                        autocreate_log_lines.lock().unwrap().push(
                            format!("[{}] FULLY_AUTO: Using existing architecture: {}", chrono::Utc::now().format("%H:%M:%S"), existing_file)
                        );
                        fa_add_tool("create_architecture", "done", &format!("Reusing {}", existing_file), &fa_calls);
                        Some(existing_file)
                    }
                    None => {
                        let user_msg = messages.last()
                            .and_then(|m| m["content"].as_str())
                            .unwrap_or("");
                        let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                        autocreate_log_lines.lock().unwrap().push(format!("[{}] FULLY_AUTO: Step 1 — Creating agent architecture...", ts));
                        fa_add_tool("create_architecture", "calling...", "Designing agent team...", &fa_calls);
                        fa_update_text("**Step 1:** Creating agent architecture...", &fa_text);
                        autocreate_ctx.request_repaint();

                        let (ok, config_file, msg) = force_create_architecture(
                            user_msg, &sub_agent_config, &sandbox_dir,
                        ).await;

                        let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                        if ok {
                            autocreate_log_lines.lock().unwrap().push(format!("[{}] FULLY_AUTO: {}", ts, msg));
                            fa_add_tool("create_architecture", "done", &msg, &fa_calls);
                            config_file
                        } else {
                            autocreate_log_lines.lock().unwrap().push(format!("[{}] FULLY_AUTO FAILED: {}", ts, msg));
                            fa_add_tool("create_architecture", "error", &msg, &fa_calls);
                            fa_update_text(&format!("Architecture creation failed: {}", msg), &fa_text);
                            autocreate_ctx.request_repaint();
                            None
                        }
                    }
                };

                if let Some(ref cf) = config_file {
                    sub_agent_config.config_file = cf.clone();

                    // Load agent IDs from YAML
                    let (yaml_val, agent_ids) = crate::server::services::toolbox::load_agent_yaml(cf)
                        .unwrap_or_else(|| (serde_json::json!({}), vec![]));
                    sub_agent_config.agent_ids = agent_ids.clone();

                    // Determine orchestration mode from YAML
                    let orch_mode = yaml_val.get("system")
                        .and_then(|s| s.get("orchestration_mode"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("hierarchical")
                        .to_string();

                    // Find orchestrator
                    let orchestrator_id = yaml_val.get("agents")
                        .and_then(|a| a.as_array())
                        .and_then(|arr| arr.iter().find(|a| a["role"].as_str() == Some("orchestrator")))
                        .and_then(|a| a["id"].as_str())
                        .map(|s| s.to_string());

                    // For pipeline mode: find first and last agents in the sequence chain
                    let (pipeline_first_id, pipeline_last_id) = if orch_mode == "pipeline" {
                        let sequence = yaml_val.get("workflow")
                            .and_then(|w| w.get("sequence"))
                            .and_then(|s| s.as_array());
                        let first = sequence
                            .and_then(|arr| arr.first())
                            .and_then(|step| step.get("agent"))
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string())
                            // Fallback: first non-human agent
                            .or_else(|| yaml_val.get("agents")
                                .and_then(|a| a.as_array())
                                .and_then(|arr| arr.iter()
                                    .find(|a| a["role"].as_str() != Some("human"))
                                    .and_then(|a| a["id"].as_str())
                                    .map(|s| s.to_string())));
                        let last = sequence
                            .and_then(|arr| arr.last())
                            .and_then(|step| step.get("agent"))
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string())
                            // Fallback: last non-human agent
                            .or_else(|| yaml_val.get("agents")
                                .and_then(|a| a.as_array())
                                .and_then(|arr| arr.iter().rev()
                                    .find(|a| a["role"].as_str() != Some("human"))
                                    .and_then(|a| a["id"].as_str())
                                    .map(|s| s.to_string())));
                        (first, last)
                    } else {
                        (None, None)
                    };

                    // Signal the Agents tab to show this architecture
                    crate::server::services::toolbox::set_pending_arch_file(cf);

                    let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                    autocreate_log_lines.lock().unwrap().push(
                        format!("[{}] FULLY_AUTO: Step 2 — Booting {} agents from {}...", ts, agent_ids.len(), cf)
                    );
                    fa_add_tool("boot_agents", "calling...", &format!("Starting {} agents...", agent_ids.len()), &fa_calls);
                    fa_update_text(&format!("**Step 2:** Booting {} agents...", agent_ids.len()), &fa_text);
                    autocreate_ctx.request_repaint();

                    // Step 2: Boot realtime session
                    let boot_ok = start_realtime_session(
                        &sub_agent_config.session_id,
                        &sub_agent_config.config_file,
                        &sub_agent_config.api_key,
                        &sub_agent_config.api_url,
                        &sub_agent_config.model,
                        &sandbox_dir,
                    ).await;

                    if boot_ok {
                        let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                        autocreate_log_lines.lock().unwrap().push(
                            format!("[{}] FULLY_AUTO: Agents LIVE. Step 3 — Delegating task...", ts)
                        );
                        fa_add_tool("boot_agents", "done", &format!("{} agents online", agent_ids.len()), &fa_calls);
                        autocreate_ctx.request_repaint();
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                        // Step 3: Directly send task to orchestrator (or all agents)
                        let user_msg = messages.last()
                            .and_then(|m| m["content"].as_str())
                            .unwrap_or("")
                            .to_string();

                        // Pipeline: send to first pipeline stage; others: send to orchestrator
                        let target = if orch_mode == "pipeline" {
                            pipeline_first_id.as_deref()
                                .unwrap_or_else(|| agent_ids.first().map(|s| s.as_str()).unwrap_or(""))
                        } else {
                            orchestrator_id.as_deref()
                                .unwrap_or_else(|| agent_ids.first().map(|s| s.as_str()).unwrap_or(""))
                        };

                        if !target.is_empty() {
                            let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                            autocreate_log_lines.lock().unwrap().push(
                                format!("[{}] FULLY_AUTO: send_task → {} : {}", ts, target, &user_msg[..user_msg.len().min(100)])
                            );
                            let task_preview = if user_msg.len() > 80 { format!("{}...", &user_msg[..floor_char_boundary(&user_msg, 80)]) } else { user_msg.clone() };
                            fa_add_tool("send_task", "calling...", &format!("→ {} | {}", target, task_preview), &fa_calls);
                            fa_update_text(&format!("**Step 3:** Delegating to **{}**...", target), &fa_text);
                            autocreate_ctx.request_repaint();

                            // Send task
                            let send_args = serde_json::json!({"to": target, "task": &user_msg});
                            let send_result = crate::server::services::toolbox::exec_send_task(
                                &send_args,
                                &sub_agent_config.session_id,
                            ).await;

                            let send_ok = send_result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                            if send_ok {
                                let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                                autocreate_log_lines.lock().unwrap().push(
                                    format!("[{}] FULLY_AUTO: Task sent to {}. Waiting for result...", ts, target)
                                );
                                fa_add_tool("send_task", "done", &format!("Task sent to {}", target), &fa_calls);
                                fa_add_tool("wait_result", "calling...", &format!("⏳ waiting for {}", target), &fa_calls);
                                fa_update_text(&format!("**Step 4:** Waiting for **{}** to complete...", target), &fa_text);
                                autocreate_ctx.request_repaint();

                                // Wait for result with cancel support + live streaming of agent activity
                                // Pipeline: wait for LAST agent (end of chain), others: wait for target
                                let cancel_flag = cancelled.clone();
                                let wait_from = if orch_mode == "pipeline" {
                                    pipeline_last_id.as_deref().unwrap_or(target)
                                } else {
                                    target
                                };
                                let wait_args = serde_json::json!({"from": wait_from, "timeout": 600});
                                let wait_sid = sub_agent_config.session_id.clone();
                                let wait_future = crate::server::services::toolbox::exec_wait_result(
                                    &wait_args,
                                    &wait_sid,
                                );

                                // Spawn a live activity streamer that updates the chat bubble
                                let stream_text = fa_text.clone();
                                let stream_calls = fa_calls.clone();
                                let stream_ctx = autocreate_ctx.clone();
                                let stream_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                                let stream_stop2 = stream_stop.clone();
                                let stream_sid = sub_agent_config.session_id.clone();
                                let activity_streamer = tokio::spawn(async move {
                                    let mut rx = crate::server::services::toolbox::subscribe_subagent_log();
                                    let mut agent_lines: std::collections::HashMap<String, String> = std::collections::HashMap::new();
                                    loop {
                                        if stream_stop2.load(std::sync::atomic::Ordering::Relaxed) { break; }
                                        match tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await {
                                            Ok(Ok((sid, agent_id, line))) => {
                                                if sid == stream_sid {
                                                    // Track latest activity per agent
                                                    let short_line = if line.len() > 120 { format!("{}...", &line[..floor_char_boundary(&line, 120)]) } else { line.clone() };
                                                    agent_lines.insert(agent_id.clone(), short_line);

                                                    // Build activity summary
                                                    let mut summary = String::from("**Agents working:**\n");
                                                    for (aid, last_line) in &agent_lines {
                                                        summary.push_str(&format!("- **{}**: {}\n", aid, last_line));
                                                    }
                                                    {
                                                        let mut t = stream_text.lock().unwrap();
                                                        // Keep step headers, replace activity section
                                                        let header_end = t.find("\n**Agents working:**").unwrap_or(t.len());
                                                        t.truncate(header_end);
                                                        t.push('\n');
                                                        t.push_str(&summary);
                                                    }
                                                    // Update tool call with agent count
                                                    {
                                                        let mut calls = stream_calls.lock().unwrap();
                                                        if let Some(wc) = calls.iter_mut().find(|c| c.name == "wait_result") {
                                                            wc.args_preview = format!("⏳ {} agents active", agent_lines.len());
                                                        }
                                                    }
                                                    stream_ctx.request_repaint();
                                                }
                                            }
                                            Ok(Err(_)) => break,
                                            Err(_) => {} // timeout
                                        }
                                    }
                                });

                                let cancel_watcher = async {
                                    loop {
                                        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                                        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                                            break;
                                        }
                                    }
                                };

                                let wait_result = tokio::select! {
                                    r = wait_future => r,
                                    _ = cancel_watcher => {
                                        serde_json::json!({"ok": false, "error": "Stopped by user"})
                                    }
                                };

                                // Stop the activity streamer
                                stream_stop.store(true, std::sync::atomic::Ordering::Relaxed);
                                let _ = activity_streamer.await;

                                let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                                let result_ok = wait_result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                                if result_ok {
                                    let agent_result = wait_result["result"].as_str().unwrap_or("").to_string();
                                    autocreate_log_lines.lock().unwrap().push(
                                        format!("[{}] FULLY_AUTO: Got result from {} ({} chars)", ts, target, agent_result.len())
                                    );
                                    fa_add_tool("wait_result", "done", &format!("Result from {} ({} chars)", target, agent_result.len()), &fa_calls);

                                    // Collect output files
                                    if let Some(files) = wait_result["output_files"].as_array() {
                                        let mut state_files = state.files.lock().unwrap();
                                        for f in files {
                                            if let Some(s) = f.as_str() {
                                                if !state_files.contains(&s.to_string()) {
                                                    state_files.push(s.to_string());
                                                }
                                            }
                                        }
                                    }

                                    // Set the response text (replace progress with final result)
                                    {
                                        let mut text = state.text.lock().unwrap();
                                        *text = agent_result;
                                    }
                                    fully_auto_handled = true;
                                } else {
                                    let err = wait_result["error"].as_str().unwrap_or("Unknown error");
                                    autocreate_log_lines.lock().unwrap().push(
                                        format!("[{}] FULLY_AUTO: wait_result failed: {}", ts, err)
                                    );
                                    fa_add_tool("wait_result", "error", err, &fa_calls);

                                    // On timeout/error, still mark as handled — don't fall into broken tool loop
                                    // Instead, show what agents produced so far
                                    let current_text = fa_text.lock().unwrap().clone();
                                    {
                                        let mut text = state.text.lock().unwrap();
                                        *text = format!("**Agents finished with:** {}\n\n{}", err, current_text);
                                    }
                                    fully_auto_handled = true;
                                }
                            } else {
                                let err = send_result["error"].as_str().unwrap_or("Unknown error");
                                let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                                autocreate_log_lines.lock().unwrap().push(
                                    format!("[{}] FULLY_AUTO: send_task failed: {}", ts, err)
                                );
                                fa_add_tool("send_task", "error", err, &fa_calls);
                                fa_update_text(&format!("**Error sending task:** {}", err), &fa_text);
                                {
                                    let mut text = state.text.lock().unwrap();
                                    *text = format!("**Error sending task to {}:** {}", target, err);
                                }
                                fully_auto_handled = true;
                            }
                        }
                    } else {
                        let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                        autocreate_log_lines.lock().unwrap().push(
                            format!("[{}] FULLY_AUTO: WARNING — Failed to boot realtime session from {}", ts, cf)
                        );
                        fa_add_tool("boot_agents", "error", "Failed to start agents", &fa_calls);
                        fa_update_text("**Error:** Failed to boot realtime session", &fa_text);
                        {
                            let mut text = state.text.lock().unwrap();
                            *text = format!("**Error:** Failed to boot realtime session from {}", cf);
                        }
                        fully_auto_handled = true;
                    }
                    autocreate_ctx.request_repaint();
                }
            }

            // If fully_auto handled everything, skip the tool loop
            let result = if fully_auto_handled {
                // Shutdown realtime session after completion
                crate::server::services::toolbox::shutdown_realtime_session(&sid).await;
                crate::server::services::toolbox::ToolLoopResult {
                    content: state.text.lock().unwrap().clone(),
                    tool_results: Vec::new(),
                    files: state.files.lock().unwrap().clone(),
                }
            } else {
                // Normal mode: run tool loop with cancel support
                let use_realtime = is_realtime;

                let cancel_flag = cancelled.clone();
                let cancel_watcher = async {
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                    }
                };

                let tool_future = async {
                    if use_realtime {
                        call_with_tools_realtime(
                            &api_key, &api_url, &model, messages, system_prompt, &sandbox_dir,
                            on_update_cb, sub_agent_config,
                        ).await
                    } else {
                        call_with_tools(
                            &api_key, &api_url, &model, messages, system_prompt, &sandbox_dir,
                            on_update_cb, sub_agent_config,
                        ).await
                    }
                };

                let r = tokio::select! {
                    r = tool_future => r,
                    _ = cancel_watcher => {
                        let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                        state_log_lines2.lock().unwrap().push(format!("[{}] === Stopped by user ===", ts));
                        crate::server::services::toolbox::ToolLoopResult {
                            content: "Stopped by user.".to_string(),
                            tool_results: Vec::new(),
                            files: Vec::new(),
                        }
                    }
                };

                // Shutdown realtime session if cancelled
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    crate::server::services::toolbox::shutdown_realtime_session(&sid).await;
                }

                r
            };

            // Stop the subagent log listener
            subagent_stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = subagent_listener.await;

            // If call_with_tools returned content and text is still empty, set it
            {
                let mut text = state.text.lock().unwrap();
                if text.is_empty() && !result.content.is_empty() {
                    *text = result.content;
                }
            }

            // Store output files from tool results
            if !result.files.is_empty() {
                let mut files = state.files.lock().unwrap();
                for f in result.files {
                    if !files.contains(&f) {
                        files.push(f);
                    }
                }
            }

            // Save log to data/chat_logs/{session_id}.log
            {
                let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
                { state_log_lines2.lock().unwrap().push(format!("[{}] === Response complete ===", ts)); }
                let log_text = state_log_lines2.lock().unwrap().join("
");
                let log_dir = crate::server::data::data_dir().join("chat_logs");
                let _ = tokio::fs::create_dir_all(&log_dir).await;
                let log_path = log_dir.join(format!("{}.log", sid));
                // Append to existing log (don't overwrite previous sessions)
                use tokio::io::AsyncWriteExt;
                if let Ok(mut f) = tokio::fs::OpenOptions::new()
                    .create(true).append(true).open(&log_path).await
                {
                    let _ = f.write_all(log_text.as_bytes()).await;
                    let _ = f.write_all(b"\n\n").await;
                }
            }

            // Late result monitor: if a realtime session was active, keep
            // listening for agent results for up to 5 minutes after the main
            // loop finishes so late-arriving sub-agent output is captured.
            if is_realtime {
                let late_session_id = sid.clone();
                tokio::spawn(async move {
                    let deadline = tokio::time::Instant::now()
                        + tokio::time::Duration::from_secs(300);
                    let mut rx = crate::server::services::toolbox::subscribe_subagent_log();
                    loop {
                        if tokio::time::Instant::now() > deadline {
                            break;
                        }
                        match tokio::time::timeout(
                            tokio::time::Duration::from_secs(30),
                            rx.recv(),
                        )
                        .await
                        {
                            Ok(Ok((sid, aid, line))) if sid == late_session_id => {
                                let snippet = if line.len() > 200 {
                                    &line[..floor_char_boundary(&line, 200)]
                                } else {
                                    &line
                                };
                                crate::server::services::toolbox::append_session_progress(
                                    &sid,
                                    &format!(
                                        "> **Late result from {}**: {}\n",
                                        aid, snippet
                                    ),
                                );
                            }
                            Err(_) => continue, // timeout, keep waiting
                            _ => break,         // channel closed or different session
                        }
                    }
                });
            }

            let mut done = state.done.lock().unwrap();
            *done = true;
            ctx_clone.request_repaint();
        });
    }

    fn build_project_system_prompt(&self, runtime: &tokio::runtime::Handle) -> Option<String> {
        let project_id = self
            .selected_session
            .as_ref()
            .and_then(|s| s.project_id.as_ref())
            .or(self.selected_project_id.as_ref())?;

        let projects = runtime.block_on(get_projects());
        let project = projects.iter().find(|p| &p.id == project_id)?;

        let mut prompt_parts: Vec<String> = Vec::new();
        prompt_parts.push(format!("You are assisting with the project: {}", project.name));

        if !project.description.is_empty() {
            prompt_parts.push(format!("Project description: {}", project.description));
        }
        if !project.working_folder.is_empty() {
            prompt_parts.push(format!("Working folder: {}", project.working_folder));
        }
        if !project.memory.is_empty() {
            prompt_parts.push(format!("Project memory/context:\n{}", project.memory));
        }
        if !project.skills.is_empty() {
            prompt_parts.push(format!(
                "Available skills: {}",
                project.skills.join(", ")
            ));
        }
        if let Some(ref sp) = project.system_prompt {
            if !sp.is_empty() {
                prompt_parts.push(format!("Custom instructions:\n{}", sp));
            }
        }

        Some(prompt_parts.join("\n\n"))
    }

    /// Check streaming state and finalize when done — supports parallel streams.
    fn poll_streaming(&mut self, runtime: &tokio::runtime::Handle) {
        if self.active_streams.is_empty() {
            return;
        }

        // Update tool call counts for still-running streams
        for (sid, state) in &self.active_streams {
            if !state.is_done() {
                let tc = state.get_tool_calls().len();
                let mut chats = crate::ui::tasks_view::active_chats().lock().unwrap();
                if let Some(chat) = chats.iter_mut().find(|c| c.session_id == *sid) {
                    chat.tool_calls = tc;
                }
            }
        }

        // Collect finished streams
        let finished_sids: Vec<String> = self.active_streams.iter()
            .filter(|(_, state)| state.is_done())
            .map(|(sid, _)| sid.clone())
            .collect();

        if finished_sids.is_empty() {
            return;
        }

        for sid in &finished_sids {
            let state = self.active_streams.remove(sid).unwrap();
            let response_text = state.get_text();
            let error = state.get_error();
            let tool_calls = state.get_tool_calls();
            let output_files = state.get_files();

            let base_content = if let Some(err) = error {
                if response_text.is_empty() {
                    format!("[Error: {}]", err)
                } else {
                    format!("{}\n\n[Stream interrupted: {}]", response_text, err)
                }
            } else if response_text.is_empty() {
                "[No response received from API]".to_string()
            } else {
                response_text
            };

            // Prepend tool call summary if any tools were used
            let final_content = if tool_calls.is_empty() {
                base_content
            } else {
                let tool_labels: Vec<String> = tool_calls.iter().map(|tc| Self::tool_label(&tc.name).to_string()).collect();
                let mut seen = std::collections::HashSet::new();
                let unique_labels: Vec<&str> = tool_labels
                    .iter()
                    .filter(|n| seen.insert(n.as_str()))
                    .map(|n| n.as_str())
                    .collect();
                format!("[Used tools: {}]\n\n{}", unique_labels.join(", "), base_content)
            };

            // Save assistant message + merge in-memory session (user msgs may not be on disk yet)
            let sid_clone = sid.clone();
            let mem_session = self.selected_session.clone();
            runtime.block_on(async {
                let mut sessions = get_chat_history().await;

                // If session exists on disk, replace with in-memory version (has user msgs)
                // If not (new session), insert it
                let session_on_disk = sessions.iter_mut().find(|s| s.id == sid_clone);
                if let Some(s) = session_on_disk {
                    if let Some(ref ms) = mem_session {
                        if ms.id == sid_clone {
                            s.messages = ms.messages.clone();
                            s.title = ms.title.clone();
                            s.project_id = ms.project_id.clone();
                        }
                    }
                    s.messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: final_content,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        files: if output_files.is_empty() { None } else { Some(output_files.clone()) },
                        feedback: None,
                    });
                    s.updated_at = chrono::Utc::now().to_rfc3339();

                    // Auto-title
                    if s.title == "New Chat" {
                        if let Some(first_user) = s.messages.iter().find(|m| m.role == "user") {
                            let raw = &first_user.content;
                            let title_source = raw.split("\n\n--- Attached file:").next().unwrap_or(raw);
                            s.title = truncate_str(title_source.lines().next().unwrap_or("Chat"), 50);
                        }
                    }
                } else if let Some(ref ms) = mem_session {
                    if ms.id == sid_clone {
                        let mut new_s = ms.clone();
                        new_s.messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: final_content,
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            files: if output_files.is_empty() { None } else { Some(output_files.clone()) },
                            feedback: None,
                        });
                        new_s.updated_at = chrono::Utc::now().to_rfc3339();
                        if new_s.title == "New Chat" {
                            if let Some(first_user) = new_s.messages.iter().find(|m| m.role == "user") {
                                let raw = &first_user.content;
                                let title_source = raw.split("\n\n--- Attached file:").next().unwrap_or(raw);
                                new_s.title = truncate_str(title_source.lines().next().unwrap_or("Chat"), 50);
                            }
                        }
                        sessions.push(new_s);
                    }
                }
                save_chat_history(&sessions).await;
            });

            // Move from active to finished chat sessions
            {
                let mut chats = crate::ui::tasks_view::active_chats().lock().unwrap();
                if let Some(chat) = chats.iter().find(|c| c.session_id == *sid) {
                    crate::ui::tasks_view::mark_chat_finished(chat);
                }
                chats.retain(|c| c.session_id != *sid);
            }

            // Auto-reload log from file so it persists after StreamingState is removed
            if self.show_log_panel && self.log_session_id.as_deref() == Some(sid.as_str()) {
                let log_path = crate::server::data::data_dir()
                    .join("chat_logs")
                    .join(format!("{}.log", sid));
                self.log_content = std::fs::read_to_string(&log_path)
                    .unwrap_or_else(|_| "(Log file not found)".to_string());
            }
        }

        self.scroll_to_bottom = true;
        self.needs_refresh = true;
    }

    fn set_message_feedback(
        &mut self,
        runtime: &tokio::runtime::Handle,
        msg_index: usize,
        rating: &str,
    ) {
        let Some(ref session_id) = self.selected_session_id else {
            return;
        };
        let sid = session_id.clone();
        let rating = rating.to_string();
        runtime.block_on(async {
            let mut sessions = get_chat_history().await;
            if let Some(s) = sessions.iter_mut().find(|s| s.id == sid) {
                if let Some(msg) = s.messages.get_mut(msg_index) {
                    let existing = msg.feedback.clone().unwrap_or(ChatMessageFeedback {
                        rating: None,
                        comment: None,
                        submitted_at: None,
                    });
                    // Toggle off if same rating clicked again
                    let new_rating = if existing.rating.as_deref() == Some(rating.as_str()) {
                        None
                    } else {
                        Some(rating)
                    };
                    msg.feedback = Some(ChatMessageFeedback {
                        rating: new_rating,
                        comment: existing.comment,
                        submitted_at: Some(chrono::Utc::now().to_rfc3339()),
                    });
                    s.updated_at = chrono::Utc::now().to_rfc3339();
                }
            }
            save_chat_history(&sessions).await;
        });
        self.needs_refresh = true;
    }

    fn pick_files(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .set_title("Attach files")
            .pick_files()
        {
            for path in paths {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let content = std::fs::read_to_string(&path).unwrap_or_else(|_| {
                    format!("[Could not read file: {}]", path.display())
                });
                self.attached_files.push(AttachedFile { name, content });
            }
        }
    }

    // ---------------------------------------------------------------------
    // Main entry-point called by the parent UI
    // ---------------------------------------------------------------------

    pub fn show(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // Poll streaming state
        self.poll_streaming(runtime);

        if self.needs_refresh {
            self.refresh(runtime);
        }

        let sidebar_bg     = egui::Color32::from_rgb(248, 249, 250);
        let border_color   = egui::Color32::from_rgb(225, 228, 232);

        // Collect all output files from current session messages
        // Also include files from streaming state
        let current_sid = self.selected_session_id.clone().unwrap_or_default();
        let streaming_files: Vec<String> = self.active_streams.get(&current_sid)
            .map(|s| s.get_files())
            .unwrap_or_default();

        let output_files: Vec<String> = {
            let mut files: Vec<String> = self.selected_session
                .as_ref()
                .map(|s| {
                    s.messages
                        .iter()
                        .flat_map(|m| m.files.iter().flatten().cloned())
                        .collect()
                })
                .unwrap_or_default();
            // Merge streaming files
            for f in &streaming_files {
                if !files.contains(f) {
                    files.push(f.clone());
                }
            }
            files
        };

        // Auto-open output panel when new files appear
        if !output_files.is_empty() && !self.output_panel.open {
            self.output_panel.open = true;
        }

        let full_rect = ui.available_rect_before_wrap();

        // ── Column rects ────────────────────────────────────────────
        let sidebar_drag_w = 6.0;
        let output_drag_w  = if self.output_panel.open { 6.0 } else { 0.0 };
        let left_w  = self.sidebar_width;
        let right_w = if self.output_panel.open { self.output_panel.width } else { 0.0 };
        let mid_w   = (full_rect.width() - left_w - sidebar_drag_w - right_w - output_drag_w).max(200.0);

        let left_rect = egui::Rect::from_min_size(
            full_rect.min,
            egui::vec2(left_w, full_rect.height()),
        );
        // Drag handle between sidebar and chat
        let sidebar_drag_rect = egui::Rect::from_min_size(
            egui::pos2(full_rect.min.x + left_w, full_rect.min.y),
            egui::vec2(sidebar_drag_w, full_rect.height()),
        );
        let mid_rect = egui::Rect::from_min_size(
            egui::pos2(sidebar_drag_rect.max.x, full_rect.min.y),
            egui::vec2(mid_w, full_rect.height()),
        );
        // Drag handle between chat and output panel
        let drag_rect = egui::Rect::from_min_size(
            egui::pos2(mid_rect.max.x, full_rect.min.y),
            egui::vec2(output_drag_w, full_rect.height()),
        );
        let out_rect = egui::Rect::from_min_size(
            egui::pos2(drag_rect.max.x, full_rect.min.y),
            egui::vec2(right_w, full_rect.height()),
        );

        // ── Sidebar ──────────────────────────────────────────────────
        let mut left_ui = ui.new_child(egui::UiBuilder::new().max_rect(left_rect));
        egui::Frame::new()
            .fill(sidebar_bg)
            .inner_margin(egui::Margin::symmetric(10, 10))
            .stroke(egui::Stroke::new(0.5, border_color))
            .show(&mut left_ui, |ui| {
                ui.set_min_size(egui::vec2(self.sidebar_width, full_rect.height()));
                self.sidebar(ui, runtime);
            });

        // ── Sidebar drag handle ───────────────────────────────────────
        {
            let sdrag_id = ui.id().with("sidebar_drag_handle");
            let sdrag_resp = ui.interact(sidebar_drag_rect, sdrag_id, egui::Sense::drag());
            let handle_color = if sdrag_resp.hovered() || sdrag_resp.dragged() {
                egui::Color32::from_rgb(88, 166, 255)
            } else {
                egui::Color32::from_rgb(225, 228, 232)
            };
            ui.painter().rect_filled(sidebar_drag_rect, 0.0, handle_color);
            if sdrag_resp.dragged() {
                self.sidebar_width = (self.sidebar_width + sdrag_resp.drag_delta().x)
                    .clamp(160.0, full_rect.width() - right_w - output_drag_w - 300.0);
            }
            ui.ctx().set_cursor_icon(if sdrag_resp.hovered() || sdrag_resp.dragged() {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::Default
            });
        }

        // ── Chat panel ───────────────────────────────────────────────
        let mut mid_ui = ui.new_child(egui::UiBuilder::new().max_rect(mid_rect));
        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(&mut mid_ui, |ui| {
                // Output toggle button in chat header area when panel is closed
                if !self.output_panel.open && !output_files.is_empty() {
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            self.output_panel.show_toggle_button(ui, output_files.len());
                        });
                    });
                }
                self.chat_panel(ui, runtime);
            });

        // ── Drag handle ──────────────────────────────────────────────
        if self.output_panel.open {
            let drag_id = ui.id().with("output_drag_handle");
            let drag_response = ui.interact(drag_rect, drag_id, egui::Sense::drag());
            // Highlight on hover/drag
            let handle_color = if drag_response.hovered() || drag_response.dragged() {
                egui::Color32::from_rgb(88, 166, 255)
            } else {
                egui::Color32::from_rgb(225, 228, 232)
            };
            ui.painter().rect_filled(drag_rect, 0.0, handle_color);
            if drag_response.dragged() {
                // Dragging left increases panel width, right decreases
                let delta = -drag_response.drag_delta().x;
                self.output_panel.width = (self.output_panel.width + delta).clamp(260.0, full_rect.width() - self.sidebar_width - 300.0);
            }
            ui.ctx().set_cursor_icon(if drag_response.hovered() || drag_response.dragged() {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::Default
            });
        }

        // ── Output panel ─────────────────────────────────────────────
        if self.output_panel.open {
            let mut out_ui = ui.new_child(egui::UiBuilder::new().max_rect(out_rect));
            self.output_panel.show(&mut out_ui, &output_files);
        }

        // Advance parent layout
        ui.allocate_rect(full_rect, egui::Sense::hover());

        // ── Tool approval dialog ──
        if let Some(ref state) = self.active_streams.get(&current_sid) {
            let approval = state.pending_approval.lock().unwrap().clone();
            if let Some((tool_name, args_preview)) = approval {
                let mut response: Option<bool> = None;
                egui::Window::new("\u{1F6E1} Tool Approval Required")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .default_width(500.0)
                    .show(ui.ctx(), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "The AI wants to execute: {}",
                                tool_name
                            ))
                            .size(14.0)
                            .strong()
                            .color(egui::Color32::from_rgb(234, 179, 8)),
                        );
                        ui.add_space(8.0);
                        egui::Frame::default()
                            .inner_margin(egui::Margin::same(8))
                            .corner_radius(egui::CornerRadius::same(4))
                            .fill(egui::Color32::from_gray(240))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("approval_args_scroll")
                                    .max_height(200.0)
                                    .show(ui, |ui| {
                                        ui.monospace(
                                            egui::RichText::new(&args_preview)
                                                .size(11.0)
                                                .color(egui::Color32::from_gray(40)),
                                        );
                                    });
                            });
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Allow")
                                            .size(14.0)
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(34, 197, 94))
                                    .min_size(egui::vec2(100.0, 32.0)),
                                )
                                .clicked()
                            {
                                response = Some(true);
                            }
                            ui.add_space(12.0);
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Deny")
                                            .size(14.0)
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(239, 68, 68))
                                    .min_size(egui::vec2(100.0, 32.0)),
                                )
                                .clicked()
                            {
                                response = Some(false);
                            }
                        });
                    });
                if let Some(approved) = response {
                    // Clear the pending approval
                    *state.pending_approval.lock().unwrap() = None;
                    // Send response to the toolbox
                    runtime.block_on(async {
                        crate::server::services::toolbox::respond_tool_approval(approved).await;
                    });
                }
            }
        }

        // Keep repainting during streaming
        if !self.active_streams.is_empty() {
            ui.ctx().request_repaint();
        }
    }

    // ---------------------------------------------------------------------
    // Sidebar
    // ---------------------------------------------------------------------

    fn sidebar(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        ui.horizontal(|ui| {
            let btn = egui::Button::new(
                egui::RichText::new("+ New").size(12.0).color(egui::Color32::WHITE),
            )
            .fill(egui::Color32::from_rgb(88, 166, 255))
            .corner_radius(6.0);
            if ui.add(btn).clicked() {
                self.create_session(runtime);
            }
            ui.label(
                egui::RichText::new("Chats")
                    .size(15.0)
                    .strong()
                    .color(egui::Color32::from_rgb(31, 35, 40)),
            );
        });

        // Project filter dropdown
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Project:").size(12.0));
            let current_label = self
                .selected_project_id
                .as_ref()
                .and_then(|pid| self.projects.iter().find(|p| &p.id == pid))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "All".to_string());

            egui::ComboBox::from_id_salt("project_filter")
                .selected_text(&current_label)
                .width(140.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.selected_project_id.is_none(), "All")
                        .clicked()
                    {
                        self.selected_project_id = None;
                    }
                    for proj in &self.projects {
                        if ui
                            .selectable_label(
                                self.selected_project_id.as_deref() == Some(&proj.id),
                                &proj.name,
                            )
                            .clicked()
                        {
                            self.selected_project_id = Some(proj.id.clone());
                        }
                    }
                });
        });

        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // We need to collect actions to apply after iteration to avoid borrow issues.
                let mut select_id: Option<String> = None;
                let mut delete_id: Option<String> = None;
                let mut start_rename_id: Option<String> = None;

                // Filter sessions by selected project
                let filtered_sessions: Vec<&ChatSessionSummary> = self
                    .sessions
                    .iter()
                    .filter(|s| {
                        if let Some(ref pid) = self.selected_project_id {
                            s.project_id.as_deref() == Some(pid.as_str())
                        } else {
                            true
                        }
                    })
                    .collect();

                for summary in &filtered_sessions {
                    let is_selected = self.selected_session_id.as_deref() == Some(&summary.id);

                    let label_text = if summary.title.is_empty() {
                        "Untitled"
                    } else {
                        &summary.title
                    };

                    let card_bg = if is_selected {
                        egui::Color32::from_rgba_premultiplied(88, 166, 255, 20)
                    } else {
                        egui::Color32::WHITE
                    };
                    let card_stroke = if is_selected {
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(88, 166, 255))
                    } else {
                        egui::Stroke::new(0.5, egui::Color32::from_rgb(225, 228, 232))
                    };

                    // Format relative timestamp
                    let time_label = format_relative_time(&summary.updated_at);

                    let is_streaming = self.active_streams.contains_key(&summary.id);

                    let frame_resp = egui::Frame::new()
                        .fill(card_bg)
                        .corner_radius(8.0)
                        .stroke(card_stroke)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_width(220.0);
                                // Title row + timestamp
                                ui.horizontal(|ui| {
                                    // Yellow dot for active/streaming chat
                                    if is_streaming {
                                        let (dot_rect, _) = ui.allocate_exact_size(
                                            egui::vec2(10.0, 10.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().circle_filled(
                                            dot_rect.center(),
                                            4.5,
                                            egui::Color32::from_rgb(250, 204, 21), // yellow
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(truncate_str(label_text, 22))
                                            .size(13.0)
                                            .strong()
                                            .color(if is_selected {
                                                egui::Color32::from_rgb(56, 139, 253)
                                            } else {
                                                egui::Color32::from_rgb(31, 35, 40)
                                            }),
                                    );
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        // Delete X button on the right
                                        let del_btn = ui.add(
                                            egui::Button::new(
                                                egui::RichText::new("x")
                                                    .size(12.0)
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(180, 186, 192)),
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .min_size(egui::vec2(18.0, 18.0)),
                                        );
                                        if del_btn.clicked() {
                                            delete_id = Some(summary.id.clone());
                                        }
                                        del_btn.on_hover_text("Delete chat");
                                        ui.label(
                                            egui::RichText::new(&time_label)
                                                .size(10.0)
                                                .color(egui::Color32::from_rgb(150, 158, 168)),
                                        );
                                    });
                                });

                                // Message preview
                                if !summary.last_message_preview.is_empty() {
                                    let preview_color = if summary.last_message_role == "user" {
                                        egui::Color32::from_rgb(56, 139, 253)
                                    } else {
                                        egui::Color32::from_rgb(101, 109, 118)
                                    };
                                    ui.add(egui::Label::new(
                                        egui::RichText::new(&summary.last_message_preview)
                                            .size(11.0)
                                            .color(preview_color),
                                    ).wrap());
                                }

                                // Message count badge
                                ui.label(
                                    egui::RichText::new(format!("{} messages", summary.message_count))
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(180, 186, 192)),
                                );
                            });
                        });

                    let response = frame_resp.response.interact(egui::Sense::click());
                    if response.clicked() && delete_id.is_none() {
                        select_id = Some(summary.id.clone());
                    }
                    response.context_menu(|ui| {
                        if ui.button("Rename").clicked() {
                            start_rename_id = Some(summary.id.clone());
                            ui.close_menu();
                        }
                        if ui.button("Delete").clicked() {
                            delete_id = Some(summary.id.clone());
                            ui.close_menu();
                        }
                    });

                    ui.add_space(3.0);
                }

                // Apply deferred actions (delete takes priority over select)
                if let Some(id) = delete_id {
                    self.confirm_delete_id = Some(id);
                } else if let Some(id) = select_id {
                    self.selected_session_id = Some(id);
                    self.scroll_to_bottom = true;
                    self.needs_refresh = true;
                }
                if let Some(id) = start_rename_id {
                    // Pre-fill rename text with current title
                    if let Some(s) = self.sessions.iter().find(|s| s.id == id) {
                        self.rename_text = s.title.clone();
                    }
                    self.renaming_session_id = Some(id);
                }
            });
    }

    // ---------------------------------------------------------------------
    // Chat panel (messages + input)
    // ---------------------------------------------------------------------

    fn chat_panel(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // Handle rename dialog
        self.rename_dialog(ui, runtime);

        // Handle delete confirmation
        self.delete_dialog(ui, runtime);

        let Some(session) = self.selected_session.clone() else {
            let mut suggestion_clicked: Option<String> = None;
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.heading(
                    egui::RichText::new("TigrimOS")
                        .size(28.0)
                        .strong()
                        .color(egui::Color32::from_rgb(31, 35, 40)),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("How can I help you today?")
                        .size(16.0)
                        .color(egui::Color32::from_rgb(101, 109, 118)),
                );
                ui.add_space(24.0);

                let suggestions = [
                    "Write a Python script to analyze data",
                    "Help me with a CSV file",
                    "Search the web for information",
                    "Explain this code",
                ];
                ui.horizontal_wrapped(|ui| {
                    for suggestion in &suggestions {
                        let btn = ui.add(
                            egui::Button::new(
                                egui::RichText::new(*suggestion)
                                    .size(13.0)
                                    .color(egui::Color32::from_rgb(31, 35, 40)),
                            )
                            .fill(egui::Color32::from_rgb(240, 242, 245))
                            .corner_radius(8.0),
                        );
                        if btn.clicked() {
                            suggestion_clicked = Some(suggestion.to_string());
                        }
                    }
                });
            });

            // If a suggestion was clicked, set the text and send
            if let Some(text) = suggestion_clicked {
                self.input_text = text;
                let ctx = ui.ctx().clone();
                self.send_message(runtime, &ctx);
            }

            return;
        };

        // Clone session id before entering closures to avoid borrow conflict
        let session_id_for_gfx = session.id.clone();

        // Session header with project info
        ui.horizontal(|ui| {
            ui.heading(&session.title);

            // Show project badge if assigned
            if let Some(ref pid) = session.project_id {
                if let Some(proj) = self.projects.iter().find(|p| &p.id == pid) {
                    ui.add_space(8.0);
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(88, 166, 255, 20))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(6, 2))
                        .stroke(egui::Stroke::new(0.5, egui::Color32::from_rgb(88, 166, 255)))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(&proj.name)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(56, 139, 253)),
                            );
                        });
                }
            }

            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    // Log button
                    let log_btn = egui::Button::new(
                        egui::RichText::new("\u{1F4CB} Log")
                            .size(12.0)
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(71, 85, 105))
                    .corner_radius(6.0);
                    if ui.add(log_btn).on_hover_text("View agent activity log").clicked() {
                        self.show_log_panel = !self.show_log_panel;
                        self.log_tab = 0;
                        self.log_session_id = Some(session.id.clone());
                        let log_path = crate::server::data::data_dir()
                            .join("chat_logs")
                            .join(format!("{}.log", session.id));
                        self.log_content = std::fs::read_to_string(&log_path)
                            .unwrap_or_else(|_| "(No log yet -- send a message first)".to_string());
                    }

                    // Graphic button
                    let gfx_btn = egui::Button::new(
                        egui::RichText::new("\u{1F4CA} Graphic")
                            .size(12.0)
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(99, 102, 241))
                    .corner_radius(6.0);
                    if ui.add(gfx_btn).on_hover_text("View agent network diagram").clicked() {
                        self.show_log_panel = true;
                        self.log_tab = 2;
                        self.log_session_id = Some(session_id_for_gfx.clone());
                        self.load_graphic_data(&session_id_for_gfx);
                    }
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!("{} messages", session.messages.len()))
                            .size(12.0)
                            .color(egui::Color32::GRAY),
                    );
                },
            );
        });

        // Architecture / Swarm info card
        {
            let settings = runtime.block_on(get_settings());
            let sub_enabled = settings.sub_agent_enabled.unwrap_or(false);
            let mode = settings.sub_agent_mode.clone().unwrap_or_else(|| "single".to_string());
            let config_file = settings.sub_agent_config_file.clone().unwrap_or_default();

            // Check project-level override
            let (arch_label, arch_file) = if let Some(ref pid) = session.project_id {
                let projects = runtime.block_on(get_projects());
                if let Some(p) = projects.iter().find(|p| p.id == *pid) {
                    if let Some(ref ov) = p.agent_override {
                        if ov.enabled.unwrap_or(false) {
                            let f = ov.sub_agent_config_file.clone().unwrap_or_default();
                            let name = f.rsplit('/').next().unwrap_or(&f).replace(".yaml", "").replace(".yml", "");
                            (if name.is_empty() { "Default".to_string() } else { name }, f)
                        } else {
                            let name = config_file.rsplit('/').next().unwrap_or(&config_file).replace(".yaml", "").replace(".yml", "");
                            (if name.is_empty() { "None".to_string() } else { name }, config_file.clone())
                        }
                    } else {
                        let name = config_file.rsplit('/').next().unwrap_or(&config_file).replace(".yaml", "").replace(".yml", "");
                        (if name.is_empty() { "None".to_string() } else { name }, config_file.clone())
                    }
                } else {
                    ("None".to_string(), String::new())
                }
            } else {
                let name = config_file.rsplit('/').next().unwrap_or(&config_file).replace(".yaml", "").replace(".yml", "");
                (if name.is_empty() { "None".to_string() } else { name }, config_file.clone())
            };

            let swarm_label = if !sub_enabled {
                "Single Agent"
            } else {
                match mode.as_str() {
                    "fully_auto" => "Fully Auto",
                    "auto" => "Auto",
                    "auto_swarm" => "Auto Swarm",
                    "manual" => "Manual",
                    _ => "Swarm",
                }
            };

            let mode_color = if !sub_enabled {
                egui::Color32::from_rgb(107, 114, 128) // gray
            } else {
                match mode.as_str() {
                    "fully_auto" => egui::Color32::from_rgb(59, 130, 246),  // blue
                    "auto" => egui::Color32::from_rgb(34, 197, 94),         // green
                    "auto_swarm" => egui::Color32::from_rgb(168, 85, 247),  // purple
                    "manual" => egui::Color32::from_rgb(239, 68, 68),       // red
                    _ => egui::Color32::from_rgb(59, 130, 246),             // blue
                }
            };

            egui::Frame::new()
                .fill(egui::Color32::from_rgb(240, 242, 245))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(10, 4))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(210, 215, 220)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Swarm mode badge
                        egui::Frame::new()
                            .fill(mode_color)
                            .corner_radius(4.0)
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(swarm_label)
                                        .size(11.0)
                                        .color(egui::Color32::WHITE)
                                        .strong(),
                                );
                            });

                        ui.add_space(6.0);

                        // Architecture badge
                        if sub_enabled && !arch_file.is_empty() {
                            ui.label(
                                egui::RichText::new("Arch:")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(80, 85, 95)),
                            );
                            egui::Frame::new()
                                .fill(egui::Color32::from_rgb(59, 130, 246))
                                .corner_radius(4.0)
                                .inner_margin(egui::Margin::symmetric(6, 2))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(&arch_label)
                                            .size(11.0)
                                            .color(egui::Color32::WHITE),
                                    );
                                });
                        }

                        // Model info
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!("Model: {}", settings.tiger_bot_model))
                                .size(10.0)
                                .color(egui::Color32::from_rgb(100, 105, 115)),
                        );
                    });
                });

            ui.add_space(2.0);
        }

        // Log panel floating window
        if self.show_log_panel {
            if let Some(ref log_sid) = self.log_session_id.clone() {
                // During streaming, pull live log
                if let Some(ref s) = self.active_streams.get(log_sid) {
                    self.log_content = s.get_log();
                }
                let mut open = self.show_log_panel;
                egui::Window::new("\u{1F4CB} Agent Log")
                    .open(&mut open)
                    .resizable(true)
                    .default_size([760.0, 520.0])
                    .scroll(egui::Vec2b::new(false, false))
                    .show(ui.ctx(), |ui| {
                        // Header row: session info + refresh
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("Session: {}", log_sid))
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("\u{1F504} Refresh").clicked() {
                                    let log_path = crate::server::data::data_dir()
                                        .join("chat_logs")
                                        .join(format!("{}.log", log_sid));
                                    self.log_content = std::fs::read_to_string(&log_path)
                                        .unwrap_or_else(|_| "(No log yet)".to_string());
                                    let hist_path = crate::server::data::data_dir()
                                        .join("agent_history")
                                        .join(log_sid)
                                        .join("spawn.jsonl");
                                    self.log_agent_history = std::fs::read_to_string(&hist_path)
                                        .unwrap_or_else(|_| "(No agent history yet)".to_string());
                                }
                            });
                        });

                        // Tab bar
                        ui.horizontal(|ui| {
                            let tab0_color = if self.log_tab == 0 {
                                egui::Color32::from_rgb(59, 130, 246)
                            } else {
                                egui::Color32::GRAY
                            };
                            let tab1_color = if self.log_tab == 1 {
                                egui::Color32::from_rgb(34, 197, 94)
                            } else {
                                egui::Color32::GRAY
                            };
                            if ui.add(egui::Button::new(
                                egui::RichText::new("Chat Log").size(12.0).color(tab0_color)
                            ).frame(self.log_tab == 0)).clicked() {
                                self.log_tab = 0;
                            }
                            if ui.add(egui::Button::new(
                                egui::RichText::new("\u{1F916} Agent History").size(12.0).color(tab1_color)
                            ).frame(self.log_tab == 1)).clicked() {
                                self.log_tab = 1;
                                // Load agent history when switching to this tab
                                let hist_path = crate::server::data::data_dir()
                                    .join("agent_history")
                                    .join(log_sid)
                                    .join("spawn.jsonl");
                                self.log_agent_history = std::fs::read_to_string(&hist_path)
                                    .unwrap_or_else(|_| "(No agent history yet - sub-agents haven't been used in this session)".to_string());
                            }
                            let tab2_color = if self.log_tab == 2 {
                                egui::Color32::from_rgb(99, 102, 241)
                            } else {
                                egui::Color32::GRAY
                            };
                            if ui.add(egui::Button::new(
                                egui::RichText::new("\u{1F4CA} Graphic").size(12.0).color(tab2_color)
                            ).frame(self.log_tab == 2)).clicked() {
                                self.log_tab = 2;
                                self.load_graphic_data(log_sid);
                            }
                        });
                        ui.separator();

                        if self.log_tab == 2 {
                            // Auto-reload graphic data while streaming (every 2 seconds)
                            if self.active_streams.contains_key(log_sid) {
                                let now = ui.input(|i| i.time);
                                if now - self.graphic_last_reload > 2.0 {
                                    self.graphic_last_reload = now;
                                    self.load_graphic_data(log_sid);
                                }
                                ui.ctx().request_repaint();
                            }
                            // Graphic monitor tab — renders inline, no scroll area
                            self.render_graphic_monitor(ui);
                        } else {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if self.log_tab == 0 {
                                    // Chat log view
                                    for line in self.log_content.lines() {
                                        let color = if line.contains("] TOOL CALL:") {
                                            egui::Color32::from_rgb(37, 99, 195)
                                        } else if line.contains("] TOOL RESULT:") {
                                            egui::Color32::from_rgb(22, 163, 74)
                                        } else if line.contains("] ERROR:") || line.contains("FAILED:") {
                                            egui::Color32::from_rgb(220, 38, 38)
                                        } else if line.contains("] USER:") {
                                            egui::Color32::from_rgb(37, 99, 195)
                                        } else if line.contains("] ===") {
                                            egui::Color32::from_rgb(100, 110, 120)
                                        } else if line.contains("FULLY_AUTO:") || line.contains("AUTO_SWARM:") {
                                            egui::Color32::from_rgb(124, 58, 237)
                                        } else if line.starts_with("  ") {
                                            egui::Color32::from_rgb(71, 85, 105)
                                        } else {
                                            egui::Color32::from_rgb(55, 65, 81)
                                        };
                                        ui.add(egui::Label::new(
                                            egui::RichText::new(line)
                                                .size(11.5)
                                                .monospace()
                                                .color(color),
                                        ).wrap());
                                    }
                                } else {
                                    // Agent history view (JSONL)
                                    if self.log_agent_history.is_empty()
                                        || self.log_agent_history.starts_with("(No agent history")
                                    {
                                        ui.label(
                                            egui::RichText::new("(No activity recorded yet)")
                                                .size(12.0)
                                                .color(egui::Color32::GRAY),
                                        );
                                    } else {
                                        for line in self.log_agent_history.lines() {
                                            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                                                let event = entry.get("event").and_then(|v| v.as_str()).unwrap_or("UNKNOWN");
                                                let ts = entry.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
                                                let ts_short = ts.get(11..19).unwrap_or(ts);
                                                let data = entry.get("data");
                                                let agent_id = data.and_then(|d| d.get("agent_id")).and_then(|v| v.as_str()).unwrap_or("main");

                                                let (color, header) = match event {
                                                    "TOOL_CALL" => {
                                                        let tool = data.and_then(|d| d.get("tool")).and_then(|v| v.as_str()).unwrap_or("?");
                                                        let args = data.and_then(|d| d.get("args_preview")).and_then(|v| v.as_str()).unwrap_or("");
                                                        let args_short = if args.len() > 120 { &args[..floor_char_boundary(args, 120)] } else { args };
                                                        (egui::Color32::from_rgb(59, 130, 246),
                                                         format!("[{}] {} > {} {}", ts_short, agent_id, tool, args_short))
                                                    }
                                                    "TOOL_RESULT" => {
                                                        let tool = data.and_then(|d| d.get("tool")).and_then(|v| v.as_str()).unwrap_or("?");
                                                        let ok = data.and_then(|d| d.get("ok")).and_then(|v| v.as_bool()).unwrap_or(false);
                                                        let preview = data.and_then(|d| d.get("result_preview")).and_then(|v| v.as_str()).unwrap_or("");
                                                        let preview_short = if preview.len() > 100 { &preview[..floor_char_boundary(preview, 100)] } else { preview };
                                                        let status = if ok { "ok" } else { "ERR" };
                                                        (if ok { egui::Color32::from_rgb(34, 197, 94) } else { egui::Color32::from_rgb(239, 68, 68) },
                                                         format!("[{}] {} < {} [{}] {}", ts_short, agent_id, tool, status, preview_short))
                                                    }
                                                    "SUBAGENT_SPAWN" => {
                                                        let agent = data.and_then(|d| d.get("agent_name")).and_then(|v| v.as_str()).unwrap_or("?");
                                                        let role = data.and_then(|d| d.get("role")).and_then(|v| v.as_str()).unwrap_or("?");
                                                        let depth = data.and_then(|d| d.get("depth")).and_then(|v| v.as_u64()).unwrap_or(0);
                                                        let task = data.and_then(|d| d.get("task")).and_then(|v| v.as_str()).unwrap_or("");
                                                        let task_short = if task.len() > 100 { &task[..floor_char_boundary(task, 100)] } else { task };
                                                        (egui::Color32::from_rgb(250, 176, 5),
                                                         format!("[{}] SPAWN {} ({}) depth={} | {}", ts_short, agent, role, depth, task_short))
                                                    }
                                                    "SUBAGENT_DONE" => {
                                                        let agent = data.and_then(|d| d.get("agent_name")).and_then(|v| v.as_str()).unwrap_or("?");
                                                        let tools = data.and_then(|d| d.get("tool_calls")).and_then(|v| v.as_u64()).unwrap_or(0);
                                                        let files = data.and_then(|d| d.get("files_generated")).and_then(|v| v.as_u64()).unwrap_or(0);
                                                        let preview = data.and_then(|d| d.get("result_preview")).and_then(|v| v.as_str()).unwrap_or("");
                                                        let preview_short = if preview.len() > 80 { &preview[..floor_char_boundary(preview, 80)] } else { preview };
                                                        (egui::Color32::from_rgb(34, 197, 94),
                                                         format!("[{}] DONE  {} | tools={} files={} | {}", ts_short, agent, tools, files, preview_short))
                                                    }
                                                    _ => (egui::Color32::GRAY, format!("[{}] {} {:?}", ts_short, event, data))
                                                };
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(header)
                                                        .size(11.5)
                                                        .monospace()
                                                        .color(color),
                                                ).wrap());
                                            } else {
                                                // Fallback: show raw line
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(line)
                                                        .size(11.0)
                                                        .monospace()
                                                        .color(egui::Color32::GRAY),
                                                ).wrap());
                                            }
                                        }
                                    }
                                }
                            });
                        } // close else (non-graphic tabs)
                    });
                self.show_log_panel = open;
            }
        }

        ui.separator();

        // Messages area - reserve space for attachment bar + input bar
        let attachment_height = if self.attached_files.is_empty() {
            0.0
        } else {
            32.0
        };
        // Dynamic input height: count newlines, min 2 rows, max 10 rows
        let line_count = self.input_text.chars().filter(|&c| c == '\n').count() + 1;
        let input_rows = (line_count.max(2)).min(10);
        let line_height = 16.0; // approximate per-line height
        let input_bar_height = (input_rows as f32 * line_height) + 24.0 + attachment_height; // 24 = padding
        let messages_height = ui.available_height() - input_bar_height;

        // Clone data needed for feedback actions
        let _session_messages_len = session.messages.len();

        // Collect feedback actions to apply after borrow ends
        let mut feedback_actions: Vec<(usize, String)> = Vec::new();

        let scroll_id = ui.id().with("chat_scroll");
        egui::ScrollArea::vertical()
            .id_salt(scroll_id)
            .max_height(messages_height.max(200.0))
            .auto_shrink([false, false])
            .stick_to_bottom(self.scroll_to_bottom)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                if session.messages.is_empty() && !self.active_streams.contains_key(&session.id) {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("No messages yet. Type below to start chatting.")
                                .size(14.0)
                                .color(egui::Color32::GRAY),
                        );
                    });
                } else {
                    ui.add_space(4.0);
                    for (idx, msg) in session.messages.iter().enumerate() {
                        if let Some(action) = self.render_message(ui, msg, idx) {
                            feedback_actions.push(action);
                        }
                        ui.add_space(6.0);
                    }

                    // Show streaming response in progress
                    if let Some(ref state) = self.active_streams.get(&session.id) {
                        let streaming_text = state.get_text();
                        let streaming_tool_calls = state.get_tool_calls();
                        self.render_streaming_message(ui, &streaming_text, &streaming_tool_calls);
                        ui.add_space(6.0);
                    }

                    ui.add_space(4.0);
                }
            });

        // Apply feedback actions
        for (idx, rating) in feedback_actions {
            self.set_message_feedback(runtime, idx, &rating);
        }

        // Reset scroll flag after one frame
        self.scroll_to_bottom = false;

        ui.add_space(4.0);

        // Attached files chips
        if !self.attached_files.is_empty() {
            ui.horizontal_wrapped(|ui| {
                let mut remove_idx: Option<usize> = None;
                for (i, file) in self.attached_files.iter().enumerate() {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(240, 242, 245))
                        .corner_radius(12.0)
                        .inner_margin(egui::Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&file.name)
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(31, 35, 40)),
                                );
                                if ui
                                    .small_button("x")
                                    .clicked()
                                {
                                    remove_idx = Some(i);
                                }
                            });
                        });
                }
                if let Some(idx) = remove_idx {
                    self.attached_files.remove(idx);
                }
            });
            ui.add_space(2.0);
        }

        // Input bar — styled card
        let is_streaming = self.active_streams.contains_key(&session.id);
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(248, 249, 250))
            .corner_radius(12.0)
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(225, 228, 232)))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Attach button
                    let attach_btn = egui::Button::new(
                        egui::RichText::new("\u{1F4CE}").size(15.0),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .corner_radius(6.0);
                    if ui
                        .add_enabled(!is_streaming, attach_btn)
                        .on_hover_text("Attach files")
                        .clicked()
                    {
                        self.pick_files();
                    }

                    let edit_width = ui.available_width() - 80.0;
                    let max_input_height = 10.0 * line_height;
                    let response = if line_count > 10 {
                        // Wrap in scroll area when exceeding 10 lines
                        let mut te_response: Option<egui::Response> = None;
                        egui::ScrollArea::vertical()
                            .id_salt("chat_input_scroll")
                            .max_height(max_input_height)
                            .show(ui, |ui| {
                                let text_edit = egui::TextEdit::multiline(&mut self.input_text)
                                    .hint_text("Type a message... (Enter to send, Shift+Enter for newline)")
                                    .desired_rows(input_rows)
                                    .desired_width(edit_width)
                                    .frame(false);
                                te_response = Some(ui.add_enabled(!is_streaming, text_edit));
                            });
                        te_response.unwrap()
                    } else {
                        let text_edit = egui::TextEdit::multiline(&mut self.input_text)
                            .hint_text("Type a message... (Enter to send, Shift+Enter for newline)")
                            .desired_rows(input_rows)
                            .desired_width(edit_width)
                            .frame(false);
                        ui.add_enabled(!is_streaming, text_edit)
                    };

                    // Enter without Shift sends
                    let enter_pressed = response.has_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                    if enter_pressed {
                        if self.input_text.ends_with('\n') {
                            self.input_text.pop();
                        }
                    }

                    let can_send =
                        (!self.input_text.trim().is_empty() || !self.attached_files.is_empty())
                            && !is_streaming;

                    if is_streaming {
                        // Stop button — red, always enabled during streaming
                        let stop_btn = egui::Button::new(
                            egui::RichText::new("\u{25A0} Stop").size(13.0).strong().color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(220, 38, 38))
                        .corner_radius(8.0)
                        .min_size(egui::vec2(60.0, 36.0));

                        if ui.add(stop_btn).clicked() {
                            if let Some(state) = self.active_streams.get(&session.id) {
                                state.cancel();
                            }
                        }
                    } else {
                        // Send button
                        let send_btn = egui::Button::new(
                            egui::RichText::new("\u{25B6} Send").size(13.0).strong().color(egui::Color32::WHITE),
                        )
                        .fill(if can_send {
                            egui::Color32::from_rgb(88, 166, 255)
                        } else {
                            egui::Color32::from_rgb(210, 215, 220)
                        })
                        .corner_radius(8.0)
                        .min_size(egui::vec2(60.0, 36.0));

                        if ui.add_enabled(can_send, send_btn).clicked()
                            || (enter_pressed && can_send)
                        {
                            let ctx = ui.ctx().clone();
                            self.send_message(runtime, &ctx);
                            response.request_focus();
                        }
                    }
                });
            });
    }

    // ---------------------------------------------------------------------
    // Render a single chat message bubble with rich markdown
    // ---------------------------------------------------------------------

    fn render_message(
        &self,
        ui: &mut egui::Ui,
        msg: &ChatMessage,
        index: usize,
    ) -> Option<(usize, String)> {
        let is_user = msg.role == "user";
        let mut feedback_action: Option<(usize, String)> = None;

        let (bg_color, text_color, icon) = if is_user {
            (
                egui::Color32::from_rgb(88, 166, 255), // accent blue
                egui::Color32::WHITE,
                "You",
            )
        } else {
            (
                egui::Color32::WHITE, // white card
                egui::Color32::from_rgb(31, 35, 40),
                "AI",
            )
        };

        let layout = if is_user {
            egui::Layout::right_to_left(egui::Align::TOP)
        } else {
            egui::Layout::left_to_right(egui::Align::TOP)
        };

        ui.with_layout(layout, |ui| {
            let max_bubble_width = (ui.available_width() * 0.75).min(650.0);
            let bubble_stroke = if is_user {
                egui::Stroke::NONE
            } else {
                egui::Stroke::new(1.0, egui::Color32::from_rgb(225, 228, 232))
            };

            egui::Frame::new()
                .fill(bg_color)
                .corner_radius(12.0)
                .stroke(bubble_stroke)
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_max_width(max_bubble_width);
                    // Force vertical layout inside bubble (parent is left_to_right for alignment)
                    ui.vertical(|ui| {
                    // Role label
                    ui.label(
                        egui::RichText::new(icon)
                            .size(11.0)
                            .strong()
                            .color(if is_user {
                                egui::Color32::from_rgb(191, 219, 254)
                            } else {
                                egui::Color32::from_rgb(156, 163, 175)
                            }),
                    );

                    // Show attached files as vertical cards
                    if let Some(ref files) = msg.files {
                        if !files.is_empty() {
                            for fname in files {
                                let short_name = std::path::Path::new(fname.as_str())
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| fname.clone());
                                egui::Frame::new()
                                    .fill(egui::Color32::from_rgb(240, 242, 245))
                                    .corner_radius(4.0)
                                    .inner_margin(egui::Margin::symmetric(8, 4))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!("\u{1F4C4} {}", short_name))
                                                    .size(11.0)
                                                    .color(egui::Color32::from_rgb(59, 130, 246)),
                                            );
                                            if ui.small_button("\u{1F4CB}").on_hover_text("Copy path").clicked() {
                                                ui.ctx().copy_text(fname.clone());
                                            }
                                        });
                                    }).response.on_hover_text(fname);
                                ui.add_space(2.0);
                            }
                        }
                    }

                    // Rich message content
                    if is_user {
                        let display_text = msg
                            .content
                            .split("\n\n--- Attached file:")
                            .next()
                            .unwrap_or(&msg.content);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(display_text)
                                    .size(14.0)
                                    .color(text_color),
                            )
                            .wrap(),
                        );
                    } else {
                        render_markdown_content(ui, &msg.content, text_color);
                    }

                    // Timestamp
                    let ts_display = format_timestamp(&msg.timestamp);
                    ui.label(
                        egui::RichText::new(ts_display)
                            .size(10.0)
                            .color(if is_user {
                                egui::Color32::from_rgb(147, 197, 253)
                            } else {
                                egui::Color32::from_rgb(107, 114, 128)
                            }),
                    );

                    // Feedback buttons for assistant messages
                    if !is_user {
                        ui.horizontal(|ui| {
                            let current_rating = msg
                                .feedback
                                .as_ref()
                                .and_then(|f| f.rating.as_deref());

                            let thumbs_up_color = if current_rating == Some("up") {
                                egui::Color32::from_rgb(34, 197, 94) // green
                            } else {
                                egui::Color32::from_rgb(107, 114, 128) // gray
                            };
                            let thumbs_down_color = if current_rating == Some("down") {
                                egui::Color32::from_rgb(239, 68, 68) // red
                            } else {
                                egui::Color32::from_rgb(107, 114, 128) // gray
                            };

                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("\u{1F44D}")
                                        .size(13.0)
                                        .color(thumbs_up_color),
                                ).frame(false))
                                .on_hover_text("Good response")
                                .clicked()
                            {
                                feedback_action = Some((index, "up".to_string()));
                            }

                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("\u{1F44E}")
                                        .size(13.0)
                                        .color(thumbs_down_color),
                                ).frame(false))
                                .on_hover_text("Bad response")
                                .clicked()
                            {
                                feedback_action = Some((index, "down".to_string()));
                            }
                        });
                    }
                    }); // close ui.vertical
                });
        });

        feedback_action
    }

    // ---------------------------------------------------------------------
    // Render streaming message (assistant typing)
    // ---------------------------------------------------------------------

    fn tool_label(name: &str) -> &str {
        match name {
            "web_search" => "Searching the web...",
            "fetch_url" => "Fetching URL...",
            "run_python" => "Running Python...",
            "run_shell" => "Running command...",
            "read_file" => "Reading file...",
            "write_file" => "Writing file...",
            "list_files" => "Listing files...",
            "list_skills" => "Listing skills...",
            "load_skill" => "Loading skill...",
            _ => name,
        }
    }

    fn render_streaming_message(&self, ui: &mut egui::Ui, text: &str, tool_calls: &[ToolCallDisplay]) {
        let bg_color = egui::Color32::WHITE;
        let text_color = egui::Color32::from_rgb(31, 35, 40);

        ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
            let max_bubble_width = (ui.available_width() * 0.75).min(600.0);

            egui::Frame::new()
                .fill(bg_color)
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_max_width(max_bubble_width);
                    ui.vertical(|ui| {

                    // Role label with typing indicator
                    ui.label(
                        egui::RichText::new("AI")
                            .size(11.0)
                            .strong()
                            .color(egui::Color32::from_rgb(156, 163, 175)),
                    );

                    // Show tool calls if any (vertical list with detail)
                    if !tool_calls.is_empty() {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(248, 249, 250))
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                for tc in tool_calls {
                                    let (icon, status_color) = match tc.status.as_str() {
                                        "calling..." => ("\u{23F3}", egui::Color32::from_rgb(250, 180, 21)),
                                        "done"       => ("\u{2705}", egui::Color32::from_rgb(34, 197, 94)),
                                        _            => ("\u{274C}", egui::Color32::from_rgb(239, 68, 68)),
                                    };
                                    let display_name = Self::tool_label(&tc.name);
                                    ui.add(egui::Label::new(
                                        egui::RichText::new(format!("{} {} [{}]", icon, display_name, tc.status))
                                            .size(12.0).strong().color(status_color),
                                    ).wrap());
                                    if !tc.args_preview.is_empty() {
                                        ui.add(egui::Label::new(
                                            egui::RichText::new(&tc.args_preview)
                                                .size(11.0)
                                                .color(egui::Color32::from_rgb(100, 116, 139)),
                                        ).wrap());
                                    }
                                    if tc.status == "done" && !tc.result_preview.is_empty() {
                                        ui.add(egui::Label::new(
                                            egui::RichText::new(format!("\u{21AA} {}", tc.result_preview))
                                                .size(11.0)
                                                .color(egui::Color32::from_rgb(71, 85, 105)),
                                        ).wrap());
                                    }
                                    ui.add_space(2.0);
                                }
                            });
                        ui.add_space(4.0);
                    }

                    if text.is_empty() && tool_calls.is_empty() {
                        ui.label(
                            egui::RichText::new("thinking...")
                                .size(14.0)
                                .italics()
                                .color(egui::Color32::from_rgb(156, 163, 175)),
                        );
                    } else if !text.is_empty() {
                        render_markdown_content(ui, text, text_color);
                    }

                    // Pulsing indicator
                    ui.label(
                        egui::RichText::new("\u{25CF}")
                            .size(10.0)
                            .color(egui::Color32::from_rgb(59, 130, 246)),
                    );

                    }); // close ui.vertical
                });
        });
    }

    // ---------------------------------------------------------------------
    // Rename dialog
    // ---------------------------------------------------------------------

    fn rename_dialog(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        if self.renaming_session_id.is_none() {
            return;
        }
        let session_id = self.renaming_session_id.clone().unwrap();

        let mut open = true;
        egui::Window::new("Rename Chat")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label("Enter a new title:");
                let response = ui.text_edit_singleline(&mut self.rename_text);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.renaming_session_id = None;
                    }
                    if ui.button("Save").clicked()
                        || (response.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        let title = self.rename_text.trim().to_string();
                        if !title.is_empty() {
                            self.rename_session(runtime, &session_id, &title);
                        }
                        self.renaming_session_id = None;
                    }
                });
            });

        if !open {
            self.renaming_session_id = None;
        }
    }

    // ---------------------------------------------------------------------
    // Delete confirmation dialog
    // ---------------------------------------------------------------------

    fn delete_dialog(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        if self.confirm_delete_id.is_none() {
            return;
        }
        let session_id = self.confirm_delete_id.clone().unwrap();

        let mut open = true;
        egui::Window::new("Delete Chat?")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label("This will permanently delete the chat session and its history.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete_id = None;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Delete")
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            ),
                        )
                        .clicked()
                    {
                        self.delete_session(runtime, &session_id);
                        self.confirm_delete_id = None;
                    }
                });
            });

        if !open {
            self.confirm_delete_id = None;
        }
    }

    // -----------------------------------------------------------------
    // Graphic monitor — load data from agent history
    // -----------------------------------------------------------------

    fn load_graphic_data(&mut self, session_id: &str) {
        self.graphic_agents.clear();
        self.graphic_edges.clear();
        self.graphic_signals.clear();
        self.graphic_loaded_config = session_id.to_string();

        // Read agent history JSONL
        let hist_path = crate::server::data::data_dir()
            .join("agent_history")
            .join(session_id)
            .join("spawn.jsonl");
        let hist = std::fs::read_to_string(&hist_path).unwrap_or_default();

        // Collect agents, tool usage, and connections from history
        let mut agent_map: std::collections::HashMap<String, (String, String, Vec<String>)> =
            std::collections::HashMap::new(); // id -> (role, status, tools)
        let mut edge_set: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        // YAML-defined connections (from SESSION_CONFIG event)
        let mut yaml_edges: Vec<(String, String)> = Vec::new();
        let mut orchestration_mode = String::new();
        // Pipeline order from workflow.sequence
        let mut pipeline_order: Vec<String> = Vec::new();

        // Always add "main" orchestrator
        agent_map
            .entry("main".to_string())
            .or_insert_with(|| ("orchestrator".to_string(), "idle".to_string(), Vec::new()));

        for line in hist.lines() {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let event = entry
                .get("event")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let data = entry.get("data");

            match event {
                "SESSION_CONFIG" => {
                    orchestration_mode = data
                        .and_then(|d| d.get("orchestration_mode"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Extract YAML connections
                    if let Some(conns) = data.and_then(|d| d.get("connections")).and_then(|v| v.as_array()) {
                        for conn in conns {
                            let from = conn.get("from").and_then(|v| v.as_str()).unwrap_or("");
                            let to = conn.get("to").and_then(|v| v.as_str()).unwrap_or("");
                            if !from.is_empty() && !to.is_empty() && from != "human" && to != "human" {
                                yaml_edges.push((from.to_string(), to.to_string()));
                            }
                        }
                    }
                    // Extract pipeline order from workflow.sequence
                    if let Some(seq) = data.and_then(|d| d.get("workflow"))
                        .and_then(|w| w.get("sequence"))
                        .and_then(|s| s.as_array())
                    {
                        for step in seq {
                            if let Some(agent) = step.get("agent").and_then(|v| v.as_str()) {
                                if agent != "human" {
                                    pipeline_order.push(agent.to_string());
                                }
                            }
                        }
                    }
                }
                "SUBAGENT_SPAWN" => {
                    let name = data
                        .and_then(|d| d.get("agent_name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let role = data
                        .and_then(|d| d.get("role"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("worker");
                    agent_map
                        .entry(name.to_string())
                        .or_insert_with(|| (role.to_string(), "working".to_string(), Vec::new()));
                    // Only add parent→child edge if no YAML connections (fallback for non-realtime)
                    // For realtime sessions with SESSION_CONFIG, we use YAML edges instead
                }
                "SUBAGENT_DONE" => {
                    let name = data
                        .and_then(|d| d.get("agent_name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    if let Some(entry) = agent_map.get_mut(name) {
                        entry.1 = "done".to_string();
                    }
                }
                "TOOL_CALL" => {
                    let agent_id = data
                        .and_then(|d| d.get("agent_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("main");
                    let tool = data
                        .and_then(|d| d.get("tool"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let ts = entry
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    agent_map
                        .entry(agent_id.to_string())
                        .or_insert_with(|| {
                            ("worker".to_string(), "working".to_string(), Vec::new())
                        });
                    if let Some(e) = agent_map.get_mut(agent_id) {
                        e.2.push(tool.to_string());
                        e.1 = "working".to_string();
                    }

                    // Extract target from args_preview for send_task / wait_result
                    let kind = tool_to_link_kind(tool);
                    let mut target_agent = String::new();
                    if tool == "send_task" || tool == "wait_result" || tool == "spawn_subagent" {
                        let args_str = data
                            .and_then(|d| d.get("args_preview"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        // Try to parse agent name from args_preview JSON
                        if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(args_str) {
                            // send_task has "to" or target in the args
                            if let Some(t) = args_val.get("to").and_then(|v| v.as_str()) {
                                target_agent = t.to_string();
                            } else if let Some(t) = args_val.get("from").and_then(|v| v.as_str()) {
                                target_agent = t.to_string();
                            } else if let Some(t) = args_val.get("agent").and_then(|v| v.as_str()) {
                                target_agent = t.to_string();
                            }
                        }
                    }

                    if !target_agent.is_empty() {
                        if tool == "wait_result" {
                            edge_set.insert((target_agent.clone(), agent_id.to_string()));
                        } else {
                            edge_set.insert((agent_id.to_string(), target_agent.clone()));
                        }
                    }

                    // Add signal
                    let ts_f64 = chrono::DateTime::parse_from_rfc3339(&ts)
                        .map(|dt| dt.timestamp() as f64)
                        .unwrap_or(0.0);
                    self.graphic_signals.push(GraphicSignal {
                        from: agent_id.to_string(),
                        to: target_agent,
                        kind: kind.to_string(),
                        tool: tool.to_string(),
                        started_at: ts_f64,
                    });
                }
                "TOOL_RESULT" => {
                    let agent_id = data
                        .and_then(|d| d.get("agent_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("main");
                    let tool = data
                        .and_then(|d| d.get("tool"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    // Extract target from send_task results (agentId in result_preview)
                    if tool == "send_task" {
                        let result_str = data
                            .and_then(|d| d.get("result_preview"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if let Ok(res_val) = serde_json::from_str::<serde_json::Value>(result_str) {
                            if let Some(target) = res_val.get("agentId").and_then(|v| v.as_str()) {
                                edge_set.insert((agent_id.to_string(), target.to_string()));
                                // Also register the target agent
                                agent_map
                                    .entry(target.to_string())
                                    .or_insert_with(|| {
                                        let name = res_val.get("agentName")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or(target);
                                        (name.to_string(), "working".to_string(), Vec::new())
                                    });
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Decide edges: prefer YAML connections when available, fall back to history edges
        let final_edges: Vec<(String, String)> = if !yaml_edges.is_empty() {
            // Use YAML-defined topology + only runtime edges between main and first agent
            let mut edges = yaml_edges.clone();
            // Add main → first_agent edge from runtime send_task calls
            for (from, to) in &edge_set {
                if from == "main" {
                    // Only add main's direct delegation edge (to first pipeline agent or orchestrator)
                    if !edges.iter().any(|(f, t)| f == from && t == to) {
                        edges.push((from.clone(), to.clone()));
                    }
                    break; // Only first send_task from main
                }
            }
            edges
        } else {
            // No YAML config — use all runtime edges (legacy / spawn_subagent mode)
            // Also add parent edges from SUBAGENT_SPAWN
            for line in hist.lines() {
                if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                    if entry.get("event").and_then(|v| v.as_str()) == Some("SUBAGENT_SPAWN") {
                        let name = entry.get("data").and_then(|d| d.get("agent_name")).and_then(|v| v.as_str()).unwrap_or("");
                        let parent = entry.get("data").and_then(|d| d.get("parent")).and_then(|v| v.as_str()).unwrap_or("main");
                        if !name.is_empty() {
                            edge_set.insert((parent.to_string(), name.to_string()));
                        }
                    }
                }
            }
            edge_set.into_iter().collect()
        };

        // Determine layout mode
        let is_pipeline = orchestration_mode == "pipeline";

        // Build ordered agent list for layout
        let names: Vec<String> = if is_pipeline && !pipeline_order.is_empty() {
            // Pipeline: main first, then pipeline order
            let mut ordered = vec!["main".to_string()];
            for id in &pipeline_order {
                if !ordered.contains(id) {
                    ordered.push(id.clone());
                }
            }
            // Add any remaining agents not in pipeline order
            for id in agent_map.keys() {
                if !ordered.contains(id) {
                    ordered.push(id.clone());
                }
            }
            ordered
        } else {
            let mut n: Vec<String> = agent_map.keys().cloned().collect();
            // Put orchestrators first
            n.sort_by(|a, b| {
                let ra = &agent_map[a].0;
                let rb = &agent_map[b].0;
                let oa = if ra == "orchestrator" { 0 } else { 1 };
                let ob = if rb == "orchestrator" { 0 } else { 1 };
                oa.cmp(&ob).then(a.cmp(b))
            });
            n
        };

        let canvas_w: f32 = 700.0;

        if is_pipeline && !pipeline_order.is_empty() {
            // Pipeline layout: horizontal chain left → right
            // main at top-left, then pipeline stages flow left → right
            let pipeline_count = pipeline_order.len();
            let spacing_x = canvas_w / (pipeline_count as f32 + 1.0);

            for (i, name) in names.iter().enumerate() {
                let (ref role, ref status, ref _tools) = agent_map.get(name)
                    .cloned()
                    .unwrap_or(("worker".to_string(), "idle".to_string(), Vec::new()));

                let (x, y) = if name == "main" {
                    // Main at top-left, connected to first pipeline stage
                    (spacing_x * 0.5 - 50.0, 30.0)
                } else if let Some(pos) = pipeline_order.iter().position(|id| id == name) {
                    // Pipeline stages in a horizontal row
                    (spacing_x * (pos as f32 + 0.5) - 50.0, 160.0)
                } else {
                    // Extra agents below
                    (spacing_x * (i as f32) - 50.0, 280.0)
                };

                self.graphic_agents.push(GraphicAgent {
                    id: name.clone(),
                    name: name.clone(),
                    role: role.clone(),
                    status: status.clone(),
                    x,
                    y,
                    color: agent_node_color(i),
                    last_tool: _tools.last().cloned().unwrap_or_default(),
                });
            }
        } else {
            // Default layout: orchestrator at top, workers in grid
            let worker_count = names.iter().filter(|n| {
                agent_map.get(*n).map(|e| e.0 != "orchestrator").unwrap_or(true)
            }).count();
            let cols = (worker_count as f32).sqrt().ceil().max(1.0) as usize;
            let spacing_x = canvas_w / (cols as f32 + 1.0);
            let mut row = 0usize;
            let mut col = 0usize;

            for (i, name) in names.iter().enumerate() {
                let (ref role, ref status, ref _tools) = agent_map.get(name)
                    .cloned()
                    .unwrap_or(("worker".to_string(), "idle".to_string(), Vec::new()));
                let is_orch = role == "orchestrator";

                if is_orch {
                    let x = canvas_w / 2.0 - 50.0;
                    let y = 30.0;
                    self.graphic_agents.push(GraphicAgent {
                        id: name.clone(),
                        name: name.clone(),
                        role: role.clone(),
                        status: status.clone(),
                        x,
                        y,
                        color: agent_node_color(i),
                        last_tool: _tools.last().cloned().unwrap_or_default(),
                    });
                } else {
                    let x = spacing_x * (col as f32 + 1.0) - 50.0;
                    let y = 140.0 + row as f32 * 100.0;
                    self.graphic_agents.push(GraphicAgent {
                        id: name.clone(),
                        name: name.clone(),
                        role: role.clone(),
                        status: status.clone(),
                        x,
                        y,
                        color: agent_node_color(i),
                        last_tool: _tools.last().cloned().unwrap_or_default(),
                    });
                    col += 1;
                    if col >= cols {
                        col = 0;
                        row += 1;
                    }
                }
            }
        }

        // Build edges from final_edges
        for (from, to) in &final_edges {
            self.graphic_edges.push(GraphicEdge {
                from: from.clone(),
                to: to.clone(),
                label: String::new(),
                protocol: "delegate".to_string(),
                state: "idle".to_string(),
            });
        }
    }

    // -----------------------------------------------------------------
    // Graphic monitor — render network diagram with egui painter
    // -----------------------------------------------------------------

    fn render_graphic_monitor(&mut self, ui: &mut egui::Ui) {
        if self.graphic_agents.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(
                    egui::RichText::new("No agent activity recorded yet.")
                        .size(14.0)
                        .color(egui::Color32::GRAY),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Send a message with sub-agents to see the network diagram.")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(120, 120, 120)),
                );
            });
            return;
        }

        // Controls
        ui.horizontal(|ui| {
            if ui.small_button("\u{1F504} Reload").clicked() {
                let sid = self.graphic_loaded_config.clone();
                self.load_graphic_data(&sid);
            }
            ui.separator();
            if ui.small_button("Zoom +").clicked() {
                self.graphic_zoom = (self.graphic_zoom + 0.1).min(3.0);
            }
            if ui.small_button("Zoom -").clicked() {
                self.graphic_zoom = (self.graphic_zoom - 0.1).max(0.3);
            }
            if ui.small_button("Reset View").clicked() {
                self.graphic_zoom = 1.0;
                self.graphic_pan = egui::Vec2::ZERO;
            }
            ui.separator();
            // Legend
            let legend = [
                ("Orchestrator", egui::Color32::from_rgb(99, 102, 241)),
                ("Working", egui::Color32::from_rgb(34, 197, 94)),
                ("Done", egui::Color32::from_rgb(156, 163, 175)),
                ("Idle", egui::Color32::from_rgb(75, 85, 99)),
            ];
            for (label, color) in legend {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(8.0, 8.0),
                    egui::Sense::hover(),
                );
                ui.painter().circle_filled(rect.center(), 4.0, color);
                ui.label(egui::RichText::new(label).size(10.0).color(egui::Color32::GRAY));
            }
        });
        ui.separator();

        // Canvas area
        let available = ui.available_size();
        let canvas_size = egui::vec2(available.x.max(200.0), available.y.max(200.0));
        let (response, painter) =
            ui.allocate_painter(canvas_size, egui::Sense::click_and_drag());
        let canvas_rect = response.rect;

        // Handle pan via drag
        if response.dragged() {
            self.graphic_pan += response.drag_delta();
        }

        // Background
        painter.rect_filled(
            canvas_rect,
            4.0,
            egui::Color32::from_rgb(15, 20, 30),
        );

        let zoom = self.graphic_zoom;
        let pan = self.graphic_pan;
        let origin = canvas_rect.min.to_vec2() + pan;

        // Build position lookup
        let positions: std::collections::HashMap<String, egui::Pos2> = self
            .graphic_agents
            .iter()
            .map(|a| {
                let pos = egui::pos2(
                    origin.x + a.x * zoom,
                    origin.y + a.y * zoom,
                );
                (a.id.clone(), pos)
            })
            .collect();

        // Build node rects lookup for edge clipping
        let node_w = 100.0 * zoom;
        let node_h = 60.0 * zoom;
        let node_rects: std::collections::HashMap<String, egui::Rect> = positions
            .iter()
            .map(|(id, &pos)| {
                (id.clone(), egui::Rect::from_min_size(pos, egui::vec2(node_w, node_h)))
            })
            .collect();

        // Helper: compute connection point on node rect border toward a target point
        let edge_point = |rect: &egui::Rect, target: egui::Pos2| -> egui::Pos2 {
            let center = rect.center();
            let dx = target.x - center.x;
            let dy = target.y - center.y;
            if dx.abs() < 0.001 && dy.abs() < 0.001 {
                return center;
            }
            let hw = rect.width() / 2.0;
            let hh = rect.height() / 2.0;
            // Scale to hit the border
            let sx = if dx.abs() > 0.001 { hw / dx.abs() } else { f32::MAX };
            let sy = if dy.abs() > 0.001 { hh / dy.abs() } else { f32::MAX };
            let s = sx.min(sy);
            egui::pos2(center.x + dx * s, center.y + dy * s)
        };

        // Draw edges
        for edge in &self.graphic_edges {
            let Some(from_rect) = node_rects.get(&edge.from) else { continue };
            let Some(to_rect) = node_rects.get(&edge.to) else { continue };

            let from_pt = edge_point(from_rect, to_rect.center());
            let to_pt = edge_point(to_rect, from_rect.center());

            let edge_color = link_kind_color(&edge.protocol);
            let stroke = egui::Stroke::new(1.5 * zoom, edge_color.gamma_multiply(0.7));
            painter.line_segment([from_pt, to_pt], stroke);

            // Arrow head at to_pt
            let dir = (to_pt - from_pt).normalized();
            let perp = egui::vec2(-dir.y, dir.x);
            let arrow_size = 7.0 * zoom;
            let tip = to_pt;
            let left = tip - dir * arrow_size + perp * arrow_size * 0.4;
            let right = tip - dir * arrow_size - perp * arrow_size * 0.4;
            painter.add(egui::Shape::convex_polygon(
                vec![tip, left, right],
                edge_color.gamma_multiply(0.8),
                egui::Stroke::NONE,
            ));
        }

        // Draw animated signal dots along edges
        let time = ui.input(|i| i.time);
        // Convert epoch-based signal timestamps to egui-relative time
        let epoch_offset = if let Some(first_sig) = self.graphic_signals.first() {
            first_sig.started_at - time
        } else {
            0.0
        };
        for signal in &self.graphic_signals {
            if signal.from.is_empty() || signal.to.is_empty() {
                continue;
            }
            let Some(from_rect) = node_rects.get(&signal.from) else { continue };
            let Some(to_rect) = node_rects.get(&signal.to) else { continue };

            let from_pt = edge_point(from_rect, to_rect.center());
            let to_pt = edge_point(to_rect, from_rect.center());

            // Draw a faint line for signals that don't have a visible edge
            let has_edge = self.graphic_edges.iter().any(|e|
                (e.from == signal.from && e.to == signal.to) ||
                (e.from == signal.to && e.to == signal.from)
            );
            if !has_edge {
                let faint_stroke = egui::Stroke::new(1.0 * zoom, link_kind_color(&signal.kind).gamma_multiply(0.25));
                painter.line_segment([from_pt, to_pt], faint_stroke);
            }

            // Animate: cycle every 4 seconds, using corrected relative time
            let relative_t = time - (signal.started_at - epoch_offset);
            let t = ((relative_t % 4.0 + 4.0) % 4.0 / 4.0) as f32; // ensure positive modulo
            let dot_pos = from_pt + (to_pt - from_pt) * t;
            let color = link_kind_color(&signal.kind);
            painter.circle_filled(dot_pos, 3.5 * zoom, color);
        }

        // Draw agent nodes
        for agent in &self.graphic_agents {
            let Some(&pos) = positions.get(&agent.id) else { continue };

            let node_rect = egui::Rect::from_min_size(pos, egui::vec2(node_w, node_h));

            // Clip check
            if !canvas_rect.intersects(node_rect) {
                continue;
            }

            // Node background
            let bg_color = match agent.status.as_str() {
                "working" => agent.color.gamma_multiply(0.3),
                "done" => egui::Color32::from_rgb(30, 40, 30),
                _ => egui::Color32::from_rgb(25, 30, 45),
            };
            painter.rect_filled(node_rect, 6.0 * zoom, bg_color);
            painter.rect_stroke(
                node_rect,
                6.0 * zoom,
                egui::Stroke::new(
                    if agent.status == "working" { 2.0 } else { 1.0 } * zoom,
                    if agent.status == "working" {
                        agent.color
                    } else if agent.status == "done" {
                        egui::Color32::from_rgb(34, 197, 94)
                    } else {
                        egui::Color32::from_rgb(75, 85, 99)
                    },
                ),
                egui::epaint::StrokeKind::Middle,
            );

            // Working glow pulse
            if agent.status == "working" {
                let pulse = (0.15 + 0.1 * (time * 2.0).sin() as f32).max(0.0);
                painter.rect_stroke(
                    node_rect.expand(2.0 * zoom),
                    8.0 * zoom,
                    egui::Stroke::new(
                        1.5 * zoom,
                        agent.color.gamma_multiply(pulse),
                    ),
                    egui::epaint::StrokeKind::Outside,
                );
            }

            // Role icon (use text symbols that egui can render)
            let icon = match agent.role.as_str() {
                "orchestrator" => "\u{2605}", // star
                "worker" => "\u{25A0}",       // filled square
                "peer" => "\u{25C6}",         // diamond
                "human" => "\u{25CF}",        // filled circle
                _ => "\u{25CB}",              // open circle
            };
            let icon_pos = pos + egui::vec2(6.0 * zoom, 6.0 * zoom);
            painter.text(
                icon_pos,
                egui::Align2::LEFT_TOP,
                icon,
                egui::FontId::proportional(12.0 * zoom),
                egui::Color32::WHITE,
            );

            // Agent name
            let name_pos = pos + egui::vec2(22.0 * zoom, 6.0 * zoom);
            let display_name = if agent.name.len() > 14 {
                format!("{}..", &agent.name[..12])
            } else {
                agent.name.clone()
            };
            painter.text(
                name_pos,
                egui::Align2::LEFT_TOP,
                &display_name,
                egui::FontId::monospace(10.0 * zoom),
                egui::Color32::WHITE,
            );

            // Status + last tool
            let status_color = match agent.status.as_str() {
                "working" => egui::Color32::from_rgb(34, 197, 94),
                "done" => egui::Color32::from_rgb(156, 163, 175),
                _ => egui::Color32::from_rgb(75, 85, 99),
            };
            let status_text = if !agent.last_tool.is_empty() {
                format!("{} > {}", agent.status, agent.last_tool)
            } else {
                agent.status.clone()
            };
            let status_display = if status_text.len() > 20 {
                format!("{}..", &status_text[..floor_char_boundary(&status_text, 18)])
            } else {
                status_text
            };
            painter.text(
                pos + egui::vec2(6.0 * zoom, 32.0 * zoom),
                egui::Align2::LEFT_TOP,
                &status_display,
                egui::FontId::monospace(8.5 * zoom),
                status_color,
            );

            // Done checkmark
            if agent.status == "done" {
                painter.text(
                    pos + egui::vec2(node_w - 14.0 * zoom, 6.0 * zoom),
                    egui::Align2::LEFT_TOP,
                    "\u{2713}",
                    egui::FontId::proportional(14.0 * zoom),
                    egui::Color32::from_rgb(34, 197, 94),
                );
            }
        }

        // ── All text painted on canvas (scoreboard + agent summary) ──

        // Collect tool counts and connections for summary
        let mut tool_counts: std::collections::HashMap<String, std::collections::HashMap<String, usize>> =
            std::collections::HashMap::new();
        for sig in &self.graphic_signals {
            *tool_counts
                .entry(sig.from.clone())
                .or_default()
                .entry(sig.tool.clone())
                .or_insert(0) += 1;
        }
        let mut connections: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for edge in &self.graphic_edges {
            let targets = connections.entry(edge.from.clone()).or_default();
            if !targets.contains(&edge.to) {
                targets.push(edge.to.clone());
            }
        }

        // Scoreboard (top-left)
        let total = self.graphic_agents.len();
        let working = self.graphic_agents.iter().filter(|a| a.status == "working").count();
        let done = self.graphic_agents.iter().filter(|a| a.status == "done").count();
        let idle = total - working - done;
        let score_text = format!(
            "Agents: {}  |  Working: {}  |  Done: {}  |  Idle: {}  |  Edges: {}",
            total, working, done, idle, self.graphic_edges.len()
        );
        painter.text(
            canvas_rect.min + egui::vec2(8.0, 8.0),
            egui::Align2::LEFT_TOP,
            &score_text,
            egui::FontId::monospace(10.0),
            egui::Color32::from_rgb(156, 163, 175),
        );

        // Agent summary (bottom of canvas)
        let line_h = 14.0;
        let summary_h = line_h * self.graphic_agents.len() as f32 + 20.0;
        let summary_top = canvas_rect.bottom() - summary_h;

        // Summary background bar
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(canvas_rect.left(), summary_top),
                canvas_rect.max,
            ),
            0.0,
            egui::Color32::from_rgba_premultiplied(10, 14, 25, 220),
        );

        // Divider line
        painter.line_segment(
            [
                egui::pos2(canvas_rect.left(), summary_top),
                egui::pos2(canvas_rect.right(), summary_top),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 65, 80)),
        );

        // "Summary" label
        painter.text(
            egui::pos2(canvas_rect.left() + 8.0, summary_top + 3.0),
            egui::Align2::LEFT_TOP,
            "Agent Summary",
            egui::FontId::monospace(10.0),
            egui::Color32::from_rgb(180, 180, 190),
        );

        // Each agent on one line
        let font = egui::FontId::monospace(9.5);
        for (i, agent) in self.graphic_agents.iter().enumerate() {
            let y = summary_top + 18.0 + i as f32 * line_h;
            let x = canvas_rect.left() + 10.0;

            // Status dot
            let dot_color = match agent.status.as_str() {
                "working" => egui::Color32::from_rgb(34, 197, 94),
                "done" => egui::Color32::from_rgb(156, 163, 175),
                _ => egui::Color32::from_rgb(75, 85, 99),
            };
            painter.circle_filled(egui::pos2(x + 4.0, y + 5.0), 3.0, dot_color);

            // Role icon
            let icon = match agent.role.as_str() {
                "orchestrator" => "\u{2605}",
                "worker" => "\u{25A0}",
                _ => "\u{25CB}",
            };

            // Build summary line: icon name [status] tools → targets
            let mut line = format!("{} {} [{}]", icon, agent.name, agent.status);

            if let Some(tools) = tool_counts.get(&agent.id) {
                let tool_parts: Vec<String> = tools
                    .iter()
                    .map(|(t, c)| format!("{}({})", t, c))
                    .collect();
                let tools_str = tool_parts.join(", ");
                if tools_str.len() > 50 {
                    line.push_str(&format!("  {}..", &tools_str[..floor_char_boundary(&tools_str, 48)]));
                } else {
                    line.push_str(&format!("  {}", tools_str));
                }
            }

            if let Some(conns) = connections.get(&agent.id) {
                line.push_str(&format!("  \u{2192} {}", conns.join(", ")));
            }

            painter.text(
                egui::pos2(x + 12.0, y),
                egui::Align2::LEFT_TOP,
                &line,
                font.clone(),
                agent.color,
            );
        }

        // Request repaint for animation
        if working > 0 || !self.graphic_signals.is_empty() {
            ui.ctx().request_repaint();
        }
    }
}

// -------------------------------------------------------------------------
// Render rich markdown content in a UI region
// -------------------------------------------------------------------------

/// Strip `<think>...</think>` blocks and `[Used tools: ...]` prefixes from displayed content.
fn strip_think_tags(content: &str) -> String {
    let mut result = content.to_string();
    // Remove <think>...</think> blocks (including multiline)
    while let Some(start) = result.find("<think>") {
        if let Some(end) = result.find("</think>") {
            let end_pos = end + "</think>".len();
            result = format!("{}{}", &result[..start], result[end_pos..].trim_start());
        } else {
            // Unclosed <think> tag — remove from <think> to end
            result = result[..start].to_string();
            break;
        }
    }
    // Remove [Used tools: ...] prefix line
    if result.starts_with("[Used tools:") {
        if let Some(pos) = result.find("]\n") {
            result = result[pos + 2..].trim_start().to_string();
        }
    }
    result.trim().to_string()
}

fn render_markdown_content(ui: &mut egui::Ui, content: &str, default_color: egui::Color32) {
    let clean = strip_think_tags(content);
    if clean.is_empty() {
        return;
    }

    let all_lines: Vec<&str> = clean.lines().collect();
    let mut i = 0;

    while i < all_lines.len() {
        let line = all_lines[i];

        // Code block fence
        if line.trim_start().starts_with("```") {
            let lang = line.trim_start().trim_start_matches('`').trim().to_string();
            let mut code_lines: Vec<String> = Vec::new();
            i += 1;
            while i < all_lines.len() {
                if all_lines[i].trim_start().starts_with("```") {
                    i += 1;
                    break;
                }
                code_lines.push(all_lines[i].to_string());
                i += 1;
            }
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(246, 248, 250))
                .corner_radius(6.0)
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(208, 215, 222)))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        if !lang.is_empty() {
                            ui.label(
                                egui::RichText::new(&lang)
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(101, 109, 118)),
                            );
                        }
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(code_lines.join("\n"))
                                    .size(13.0)
                                    .monospace()
                                    .color(egui::Color32::from_rgb(31, 35, 40)),
                            )
                            .selectable(true)
                            .wrap(),
                        );
                    });
                });
            ui.add_space(4.0);
            continue;
        }

        // Markdown table detection: line starts with | and contains at least 2 |
        if line.trim().starts_with('|') && line.trim().matches('|').count() >= 2 {
            // Collect all consecutive table lines
            let mut table_lines: Vec<&str> = Vec::new();
            while i < all_lines.len() {
                let tl = all_lines[i].trim();
                if tl.starts_with('|') && tl.matches('|').count() >= 2 {
                    table_lines.push(tl);
                    i += 1;
                } else {
                    break;
                }
            }
            render_markdown_table(ui, &table_lines, default_color);
            continue;
        }

        let trimmed = line.trim();
        i += 1;

        // Empty line = paragraph break
        if trimmed.is_empty() {
            ui.add_space(6.0);
            continue;
        }

        // Horizontal rule
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            ui.separator();
            continue;
        }

        // Headings
        if trimmed.starts_with("### ") {
            ui.add_space(2.0);
            ui.add(egui::Label::new(
                egui::RichText::new(clean_inline_md(&trimmed[4..])).strong().size(14.0).color(default_color),
            ).wrap());
            ui.add_space(2.0);
            continue;
        }
        if trimmed.starts_with("## ") {
            ui.add_space(4.0);
            ui.add(egui::Label::new(
                egui::RichText::new(clean_inline_md(&trimmed[3..])).strong().size(16.0).color(default_color),
            ).wrap());
            ui.add_space(2.0);
            continue;
        }
        if trimmed.starts_with("# ") {
            ui.add_space(4.0);
            ui.add(egui::Label::new(
                egui::RichText::new(clean_inline_md(&trimmed[2..])).strong().size(18.0).color(default_color),
            ).wrap());
            ui.add_space(2.0);
            continue;
        }

        // Blockquote
        if trimmed.starts_with("> ") {
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(246, 248, 250))
                .inner_margin(egui::Margin::symmetric(12, 4))
                .stroke(egui::Stroke::new(2.0, egui::Color32::from_rgb(88, 166, 255)))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(
                        egui::RichText::new(clean_inline_md(&trimmed[2..]))
                            .size(14.0)
                            .italics()
                            .color(egui::Color32::from_rgb(101, 109, 118)),
                    ).wrap());
                });
            continue;
        }

        // Bullet list
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            ui.add(egui::Label::new(
                egui::RichText::new(format!("  \u{2022} {}", clean_inline_md(&trimmed[2..])))
                    .size(14.0)
                    .color(default_color),
            ).wrap());
            continue;
        }

        // Numbered list
        if let Some(dot_pos) = trimmed.find(". ") {
            let num_part = &trimmed[..dot_pos];
            if !num_part.is_empty() && num_part.chars().all(|c| c.is_ascii_digit()) {
                ui.add(egui::Label::new(
                    egui::RichText::new(format!("  {}", clean_inline_md(trimmed)))
                        .size(14.0)
                        .color(default_color),
                ).wrap());
                continue;
            }
        }

        // Regular text
        ui.add(egui::Label::new(
            egui::RichText::new(clean_inline_md(trimmed)).size(14.0).color(default_color),
        ).wrap());
    }
}

/// Clean inline markdown markers (**bold**, *italic*, `code`, etc.) for plain display
fn clean_inline_md(text: &str) -> String {
    text.replace("**", "")
        .replace("__", "")
        .replace("~~", "")
}

/// Parse a markdown table line into cells
fn parse_table_row(line: &str) -> Vec<String> {
    let trimmed = line.trim().trim_start_matches('|').trim_end_matches('|');
    trimmed
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// Check if a table line is a separator (|---|---|)
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim().trim_start_matches('|').trim_end_matches('|');
    trimmed
        .split('|')
        .all(|cell| {
            let c = cell.trim();
            !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
        })
}

/// Render a markdown table as a formatted grid
fn render_markdown_table(ui: &mut egui::Ui, lines: &[&str], default_color: egui::Color32) {
    if lines.is_empty() {
        return;
    }

    // Parse header and data rows (skip separator lines)
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut header_count = 0;
    for (idx, line) in lines.iter().enumerate() {
        if is_table_separator(line) {
            continue;
        }
        let cells = parse_table_row(line);
        if !cells.is_empty() && !cells.iter().all(|c| c.is_empty()) {
            rows.push(cells);
            if idx == 0 {
                header_count = 1;
            }
        }
    }

    if rows.is_empty() {
        return;
    }

    // Find number of columns
    let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if num_cols == 0 {
        return;
    }

    ui.add_space(4.0);

    let header_bg = egui::Color32::from_rgba_premultiplied(88, 166, 255, 25);
    let even_bg = egui::Color32::WHITE;
    let odd_bg = egui::Color32::from_rgb(246, 248, 250);
    let border_color = egui::Color32::from_rgb(208, 215, 222);
    let header_text_color = egui::Color32::from_rgb(56, 139, 253);

    egui::Frame::new()
        .fill(even_bg)
        .corner_radius(6.0)
        .stroke(egui::Stroke::new(1.0, border_color))
        .inner_margin(egui::Margin::same(1))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                let available_width = ui.available_width();
                let col_width = (available_width / num_cols as f32).max(60.0);

                for (row_idx, row) in rows.iter().enumerate() {
                    let is_header = row_idx < header_count;
                    let bg = if is_header {
                        header_bg
                    } else if (row_idx - header_count) % 2 == 0 {
                        even_bg
                    } else {
                        odd_bg
                    };

                    egui::Frame::new()
                        .fill(bg)
                        .inner_margin(egui::Margin::symmetric(4, 4))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for col_idx in 0..num_cols {
                                    let cell_text = row.get(col_idx).cloned().unwrap_or_default();
                                    let text = clean_inline_md(&cell_text);

                                    ui.allocate_ui(egui::vec2(col_width, 20.0), |ui| {
                                        let rt = if is_header {
                                            egui::RichText::new(&text)
                                                .size(13.0)
                                                .strong()
                                                .color(header_text_color)
                                        } else {
                                            egui::RichText::new(&text)
                                                .size(13.0)
                                                .color(default_color)
                                        };
                                        ui.add(egui::Label::new(rt).wrap());
                                    });
                                }
                            });
                        });

                    // Draw separator line after header
                    if is_header {
                        let rect = ui.min_rect();
                        let y = rect.max.y;
                        ui.painter().hline(
                            rect.left()..=rect.right(),
                            y,
                            egui::Stroke::new(1.5, egui::Color32::from_rgb(59, 130, 246)),
                        );
                    }
                }
            });
        });

    ui.add_space(4.0);
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..floor_char_boundary(s, max_len)])
    }
}

fn format_timestamp(ts: &str) -> String {
    // Try to parse RFC3339 and show a short form; fall back to raw string
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.format("%b %d %H:%M").to_string())
        .unwrap_or_else(|_| ts.to_string())
}

fn format_relative_time(ts: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return String::new();
    };
    let now = chrono::Utc::now();
    let diff = now.signed_duration_since(dt.with_timezone(&chrono::Utc));
    let secs = diff.num_seconds();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 86400 * 7 {
        format!("{}d ago", secs / 86400)
    } else {
        dt.format("%b %d").to_string()
    }
}
