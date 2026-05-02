use eframe::egui;
use std::collections::HashSet;

use crate::server::data;

// ── File type classification ──

#[derive(Debug, Clone, PartialEq)]
enum FileType {
    Text,
    Code,
    Markdown,
    Csv,
    Image,
    Binary,
}

fn classify_file(name: &str) -> FileType {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "bmp" | "webp" | "ico" => FileType::Image,
        "md" | "markdown" => FileType::Markdown,
        "csv" => FileType::Csv,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "java" | "c" | "cpp" | "h" | "hpp"
        | "go" | "rb" | "php" | "swift" | "kt" | "scala" | "lua" | "sh" | "bash" | "zsh"
        | "fish" | "ps1" | "bat" | "cmd" | "asm" | "s" | "r" | "m" | "sql" | "html"
        | "css" | "scss" | "sass" | "less" | "xml" | "json" | "yaml" | "yml" | "toml"
        | "ini" | "cfg" | "conf" | "dockerfile" | "makefile" | "cmake" | "proto"
        | "graphql" | "wasm" | "zig" | "nim" | "v" | "d" | "ex" | "exs" | "erl" | "hrl"
        | "clj" | "cljs" | "hs" | "ml" | "mli" | "fs" | "fsx" | "el" | "vim" => FileType::Code,
        "txt" | "log" | "env" | "gitignore" | "editorconfig" | "lock" => FileType::Text,
        "" => FileType::Text, // no extension, assume text
        _ => {
            // Try to detect text vs binary by name heuristics
            FileType::Binary
        }
    }
}

fn is_text_like(ft: &FileType) -> bool {
    matches!(ft, FileType::Text | FileType::Code | FileType::Markdown | FileType::Csv)
}

// ── Sort mode ──

#[derive(Debug, Clone, Copy, PartialEq)]
enum SortField {
    Name,
    Size,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SortDirection {
    Ascending,
    Descending,
}

// ── File entry ──

struct FileEntry {
    name: String,
    path: String,
    is_directory: bool,
    size: u64,
    modified: String,
}

/// Native egui file browser for the TigrimOS sandbox directory.
/// Replaces the React FilesPage with a direct data-layer integration.
pub struct FilesView {
    sandbox_dir: String,
    current_path: String,
    files: Vec<FileEntry>,
    selected_file: Option<String>,
    file_content: String,
    editing: bool,
    edit_backup: String, // backup content for cancel
    new_dir_name: String,
    show_new_dir: bool,
    new_file_name: String,
    show_new_file: bool,
    needs_refresh: bool,
    status_message: Option<(String, bool)>, // (message, is_error)

    // Search / filter
    search_query: String,

    // Sort
    sort_field: SortField,
    sort_direction: SortDirection,

    // Multi-select
    selected_set: HashSet<String>,
    show_delete_confirm: bool,

    // Drag-drop status
    drop_status: Option<String>,
}

impl Default for FilesView {
    fn default() -> Self {
        let sandbox_dir =
            std::env::var("TIGRIMOS_SANDBOX_DIR").unwrap_or_else(|_| "sandbox".to_string());
        // Ensure the sandbox directory exists
        let _ = std::fs::create_dir_all(&sandbox_dir);
        Self {
            sandbox_dir,
            current_path: String::new(),
            files: Vec::new(),
            selected_file: None,
            file_content: String::new(),
            editing: false,
            edit_backup: String::new(),
            new_dir_name: String::new(),
            show_new_dir: false,
            new_file_name: String::new(),
            show_new_file: false,
            needs_refresh: true,
            status_message: None,
            search_query: String::new(),
            sort_field: SortField::Name,
            sort_direction: SortDirection::Ascending,
            selected_set: HashSet::new(),
            show_delete_confirm: false,
            drop_status: None,
        }
    }
}

impl FilesView {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Data layer helpers ──

    fn refresh(&mut self, runtime: &tokio::runtime::Handle) {
        let sandbox = self.sandbox_dir.clone();
        let path = self.current_path.clone();
        match runtime.block_on(data::list_files(&sandbox, &path)) {
            Ok(entries) => {
                self.files = entries
                    .into_iter()
                    .filter_map(|v| {
                        Some(FileEntry {
                            name: v.get("name")?.as_str()?.to_string(),
                            path: v.get("path")?.as_str()?.to_string(),
                            is_directory: v.get("isDirectory")?.as_bool()?,
                            size: v.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                            modified: v
                                .get("modified")
                                .and_then(|m| m.as_str())
                                .unwrap_or("")
                                .to_string(),
                        })
                    })
                    .collect();
                self.sort_files();
                self.status_message = None;
            }
            Err(e) => {
                self.status_message = Some((format!("Failed to list files: {}", e), true));
                self.files.clear();
            }
        }
        self.needs_refresh = false;
    }

    fn sort_files(&mut self) {
        let field = self.sort_field;
        let dir = self.sort_direction;
        self.files.sort_by(|a, b| {
            // Directories always first
            let dir_cmp = b.is_directory.cmp(&a.is_directory);
            if dir_cmp != std::cmp::Ordering::Equal {
                return dir_cmp;
            }
            let ord = match field {
                SortField::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortField::Size => a.size.cmp(&b.size),
                SortField::Modified => a.modified.cmp(&b.modified),
            };
            match dir {
                SortDirection::Ascending => ord,
                SortDirection::Descending => ord.reverse(),
            }
        });
    }

    fn load_file_content(&mut self, runtime: &tokio::runtime::Handle, file_path: &str) {
        let file_type = classify_file(file_path);

        // Images are rendered directly by egui::Image from file URI — no content to load
        if file_type == FileType::Image {
            self.file_content.clear();
            self.editing = false;
            self.status_message = None;
            return;
        }

        if !is_text_like(&file_type) {
            // Binary file - read raw bytes for size info, show placeholder
            let sandbox = self.sandbox_dir.clone();
            match data::validate_path(&sandbox, file_path) {
                Ok(full_path) => {
                    match std::fs::metadata(&full_path) {
                        Ok(meta) => {
                            self.file_content = format!(
                                "[Binary file]\nSize: {}\nPath: {}",
                                Self::format_size(meta.len()),
                                full_path.display()
                            );
                        }
                        Err(_) => {
                            self.file_content = "[Binary file - unable to read metadata]".to_string();
                        }
                    }
                }
                Err(e) => {
                    self.file_content = format!("[Error: {}]", e);
                }
            }
            self.editing = false;
            self.status_message = None;
            return;
        }
        let sandbox = self.sandbox_dir.clone();
        match runtime.block_on(data::read_file_content(&sandbox, file_path)) {
            Ok(content) => {
                self.file_content = content;
                self.editing = false;
                self.status_message = None;
            }
            Err(e) => {
                self.file_content = String::new();
                self.status_message = Some((format!("Failed to read file: {}", e), true));
            }
        }
    }

    fn save_file_content(&mut self, runtime: &tokio::runtime::Handle) {
        if let Some(ref path) = self.selected_file.clone() {
            let sandbox = self.sandbox_dir.clone();
            let content = self.file_content.clone();
            match runtime.block_on(data::write_file_content(&sandbox, path, &content)) {
                Ok(()) => {
                    self.editing = false;
                    self.status_message = Some(("File saved.".to_string(), false));
                    self.needs_refresh = true;
                }
                Err(e) => {
                    self.status_message = Some((format!("Failed to save: {}", e), true));
                }
            }
        }
    }

    fn delete_selected(&mut self, runtime: &tokio::runtime::Handle) {
        if let Some(ref path) = self.selected_file.clone() {
            let sandbox = self.sandbox_dir.clone();
            match runtime.block_on(data::delete_file_or_dir(&sandbox, path)) {
                Ok(()) => {
                    self.selected_file = None;
                    self.file_content.clear();
                    self.editing = false;
                    self.needs_refresh = true;
                    self.status_message = Some(("Deleted.".to_string(), false));
                }
                Err(e) => {
                    self.status_message = Some((format!("Failed to delete: {}", e), true));
                }
            }
        }
    }

    fn delete_selected_bulk(&mut self, runtime: &tokio::runtime::Handle) {
        let sandbox = self.sandbox_dir.clone();
        let paths: Vec<String> = self.selected_set.drain().collect();
        let mut ok_count = 0usize;
        let mut fail_count = 0usize;
        for p in &paths {
            match runtime.block_on(data::delete_file_or_dir(&sandbox, p)) {
                Ok(()) => {
                    ok_count += 1;
                    // If the currently viewed file was deleted, clear it
                    if self.selected_file.as_deref() == Some(p.as_str()) {
                        self.selected_file = None;
                        self.file_content.clear();
                        self.editing = false;
                    }
                }
                Err(_) => {
                    fail_count += 1;
                }
            }
        }
        self.needs_refresh = true;
        self.show_delete_confirm = false;
        if fail_count == 0 {
            self.status_message = Some((format!("Deleted {} items.", ok_count), false));
        } else {
            self.status_message = Some((
                format!("Deleted {} items, {} failed.", ok_count, fail_count),
                true,
            ));
        }
    }

    fn create_directory(&mut self, runtime: &tokio::runtime::Handle) {
        let dir_name = self.new_dir_name.trim().to_string();
        if dir_name.is_empty() {
            return;
        }
        let rel = if self.current_path.is_empty() {
            dir_name.clone()
        } else {
            format!("{}/{}", self.current_path, dir_name)
        };
        let sandbox = self.sandbox_dir.clone();
        let full = std::path::Path::new(&sandbox).join(&rel);
        match runtime.block_on(tokio::fs::create_dir_all(&full)) {
            Ok(()) => {
                self.new_dir_name.clear();
                self.show_new_dir = false;
                self.needs_refresh = true;
                self.status_message = Some((format!("Created folder: {}", dir_name), false));
            }
            Err(e) => {
                self.status_message = Some((format!("Failed to create folder: {}", e), true));
            }
        }
    }

    fn create_file(&mut self, runtime: &tokio::runtime::Handle) {
        let file_name = self.new_file_name.trim().to_string();
        if file_name.is_empty() {
            return;
        }
        let rel = if self.current_path.is_empty() {
            file_name.clone()
        } else {
            format!("{}/{}", self.current_path, file_name)
        };
        let sandbox = self.sandbox_dir.clone();
        match runtime.block_on(data::write_file_content(&sandbox, &rel, "")) {
            Ok(()) => {
                self.new_file_name.clear();
                self.show_new_file = false;
                self.needs_refresh = true;
                self.status_message = Some((format!("Created file: {}", file_name), false));
            }
            Err(e) => {
                self.status_message = Some((format!("Failed to create file: {}", e), true));
            }
        }
    }

    fn upload_file(&mut self, runtime: &tokio::runtime::Handle) {
        if let Some(src) = rfd::FileDialog::new()
            .set_title("Select file to upload")
            .pick_file()
        {
            let file_name = src
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "uploaded_file".to_string());
            let dest_rel = if self.current_path.is_empty() {
                file_name.clone()
            } else {
                format!("{}/{}", self.current_path, file_name)
            };
            match std::fs::read_to_string(&src) {
                Ok(content) => {
                    let sandbox = self.sandbox_dir.clone();
                    match runtime
                        .block_on(data::write_file_content(&sandbox, &dest_rel, &content))
                    {
                        Ok(()) => {
                            self.needs_refresh = true;
                            self.status_message =
                                Some((format!("Uploaded: {}", file_name), false));
                        }
                        Err(e) => {
                            self.status_message =
                                Some((format!("Upload write failed: {}", e), true));
                        }
                    }
                }
                Err(_) => {
                    // Binary file: read as bytes and write directly
                    match std::fs::read(&src) {
                        Ok(bytes) => {
                            let sandbox = self.sandbox_dir.clone();
                            let resolved = data::validate_path(&sandbox, &dest_rel);
                            match resolved {
                                Ok(full_path) => {
                                    if let Some(parent) = full_path.parent() {
                                        let _ = std::fs::create_dir_all(parent);
                                    }
                                    match std::fs::write(&full_path, &bytes) {
                                        Ok(()) => {
                                            self.needs_refresh = true;
                                            self.status_message = Some((
                                                format!("Uploaded (binary): {}", file_name),
                                                false,
                                            ));
                                        }
                                        Err(e) => {
                                            self.status_message = Some((
                                                format!("Upload write failed: {}", e),
                                                true,
                                            ));
                                        }
                                    }
                                }
                                Err(e) => {
                                    self.status_message =
                                        Some((format!("Path error: {}", e), true));
                                }
                            }
                        }
                        Err(e) => {
                            self.status_message =
                                Some((format!("Failed to read source file: {}", e), true));
                        }
                    }
                }
            }
        }
    }

    fn download_file(&mut self) {
        if let Some(ref selected) = self.selected_file.clone() {
            let sandbox = self.sandbox_dir.clone();
            let file_name = selected
                .rsplit('/')
                .next()
                .unwrap_or(selected)
                .to_string();
            match data::validate_path(&sandbox, selected) {
                Ok(src_path) => {
                    if let Some(dest) = rfd::FileDialog::new()
                        .set_title("Save file as")
                        .set_file_name(&file_name)
                        .save_file()
                    {
                        match std::fs::copy(&src_path, &dest) {
                            Ok(_) => {
                                self.status_message = Some((
                                    format!("Downloaded to: {}", dest.display()),
                                    false,
                                ));
                            }
                            Err(e) => {
                                self.status_message =
                                    Some((format!("Download failed: {}", e), true));
                            }
                        }
                    }
                }
                Err(e) => {
                    self.status_message = Some((format!("Path error: {}", e), true));
                }
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context, runtime: &tokio::runtime::Handle) {
        let dropped: Vec<egui::DroppedFile> =
            ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let mut count = 0usize;
        for file in &dropped {
            if let Some(ref path) = file.path {
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "dropped_file".to_string());
                let dest_rel = if self.current_path.is_empty() {
                    file_name.clone()
                } else {
                    format!("{}/{}", self.current_path, file_name)
                };
                // Try text first, fallback to binary
                let sandbox = self.sandbox_dir.clone();
                if let Ok(content) = std::fs::read_to_string(path) {
                    if runtime
                        .block_on(data::write_file_content(&sandbox, &dest_rel, &content))
                        .is_ok()
                    {
                        count += 1;
                    }
                } else if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(full_path) = data::validate_path(&sandbox, &dest_rel) {
                        if let Some(parent) = full_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if std::fs::write(&full_path, &bytes).is_ok() {
                            count += 1;
                        }
                    }
                }
            }
        }
        if count > 0 {
            self.needs_refresh = true;
            self.drop_status = Some(format!("Dropped {} file(s).", count));
            self.status_message = Some((format!("Uploaded {} dropped file(s).", count), false));
        }
    }

    fn navigate_to(&mut self, path: &str) {
        self.current_path = path.to_string();
        self.selected_file = None;
        self.file_content.clear();
        self.editing = false;
        self.needs_refresh = true;
    }

    fn navigate_up(&mut self) {
        if let Some(pos) = self.current_path.rfind('/') {
            self.current_path = self.current_path[..pos].to_string();
        } else {
            self.current_path.clear();
        }
        self.selected_file = None;
        self.file_content.clear();
        self.editing = false;
        self.needs_refresh = true;
    }

    fn format_size(bytes: u64) -> String {
        if bytes < 1024 {
            format!("{} B", bytes)
        } else if bytes < 1024 * 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else if bytes < 1024 * 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    }

    fn format_modified(modified: &str) -> String {
        if modified.is_empty() {
            return String::new();
        }
        // Try to parse RFC3339 and display a friendlier format
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(modified) {
            dt.format("%Y-%m-%d %H:%M").to_string()
        } else {
            modified.to_string()
        }
    }

    /// Get filtered files based on search query
    fn filtered_files(&self) -> Vec<usize> {
        let query = self.search_query.to_lowercase();
        self.files
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                if query.is_empty() {
                    true
                } else {
                    e.name.to_lowercase().contains(&query)
                }
            })
            .map(|(i, _)| i)
            .collect()
    }

    // ── Rendering helpers ──

    fn render_markdown(ui: &mut egui::Ui, text: &str) {
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("### ") {
                ui.label(
                    egui::RichText::new(&trimmed[4..])
                        .size(15.0)
                        .strong(),
                );
            } else if trimmed.starts_with("## ") {
                ui.label(
                    egui::RichText::new(&trimmed[3..])
                        .size(17.0)
                        .strong(),
                );
            } else if trimmed.starts_with("# ") {
                ui.label(
                    egui::RichText::new(&trimmed[2..])
                        .size(20.0)
                        .strong(),
                );
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                ui.horizontal(|ui| {
                    ui.label("  \u{2022} ");
                    Self::render_inline_markdown(ui, &trimmed[2..]);
                });
            } else if trimmed.starts_with("> ") {
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(8, 2))
                    .stroke(egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 100, 100)))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&trimmed[2..])
                                .italics()
                                .color(egui::Color32::from_rgb(180, 180, 180)),
                        );
                    });
            } else if trimmed.starts_with("```") {
                // Simple code fence indicator (content handled line-by-line)
                ui.label(
                    egui::RichText::new(trimmed)
                        .monospace()
                        .color(egui::Color32::from_rgb(150, 150, 150)),
                );
            } else if trimmed.is_empty() {
                ui.add_space(4.0);
            } else {
                Self::render_inline_markdown(ui, trimmed);
            }
        }
    }

    fn render_inline_markdown(ui: &mut egui::Ui, text: &str) {
        // Simple bold/italic detection
        if text.contains("**") {
            // Split around bold markers
            let parts: Vec<&str> = text.split("**").collect();
            ui.horizontal_wrapped(|ui| {
                for (i, part) in parts.iter().enumerate() {
                    if part.is_empty() {
                        continue;
                    }
                    if i % 2 == 1 {
                        ui.label(egui::RichText::new(*part).strong());
                    } else {
                        ui.label(*part);
                    }
                }
            });
        } else if text.contains('`') {
            let parts: Vec<&str> = text.split('`').collect();
            ui.horizontal_wrapped(|ui| {
                for (i, part) in parts.iter().enumerate() {
                    if part.is_empty() {
                        continue;
                    }
                    if i % 2 == 1 {
                        ui.label(
                            egui::RichText::new(*part)
                                .monospace()
                                .background_color(egui::Color32::from_rgb(50, 50, 50)),
                        );
                    } else {
                        ui.label(*part);
                    }
                }
            });
        } else {
            ui.label(text);
        }
    }

    fn render_csv_table(ui: &mut egui::Ui, text: &str) {
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            ui.label("Empty CSV file");
            return;
        }

        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let header_cells: Vec<&str> = lines[0].split(',').collect();
                let col_count = header_cells.len();

                egui::Grid::new("csv_grid")
                    .striped(true)
                    .min_col_width(60.0)
                    .show(ui, |ui| {
                        // Header row
                        for cell in &header_cells {
                            ui.label(egui::RichText::new(cell.trim()).strong());
                        }
                        ui.end_row();

                        // Data rows (limit to 500 for performance)
                        for line in lines.iter().skip(1).take(500) {
                            let cells: Vec<&str> = line.split(',').collect();
                            for i in 0..col_count {
                                let val = cells.get(i).unwrap_or(&"");
                                ui.label(val.trim());
                            }
                            ui.end_row();
                        }
                    });

                if lines.len() > 501 {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "... and {} more rows",
                            lines.len() - 501
                        ))
                        .color(egui::Color32::GRAY),
                    );
                }
            });
    }

    // ── Main entry point ──

    pub fn show(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let ctx = ui.ctx().clone();

        // Handle drag-drop
        self.handle_dropped_files(&ctx, runtime);

        if self.needs_refresh {
            self.refresh(runtime);
        }

        // ── Top bar: breadcrumbs + actions ──
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Files").size(18.0).strong());
            ui.separator();

            // Root breadcrumb — show mounted folder name
            let root_label = std::path::Path::new(&self.sandbox_dir)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("sandbox");
            if ui
                .add(
                    egui::Button::new(egui::RichText::new(root_label).size(13.0)).frame(false),
                )
                .on_hover_text(&self.sandbox_dir)
                .clicked()
            {
                self.navigate_to("");
            }

            if !self.current_path.is_empty() {
                let path_clone = self.current_path.clone();
                let segments: Vec<&str> = path_clone.split('/').collect();
                let mut nav_target: Option<String> = None;
                for (i, seg) in segments.iter().enumerate() {
                    ui.label(
                        egui::RichText::new("/")
                            .size(13.0)
                            .color(egui::Color32::GRAY),
                    );
                    let partial: String = segments[..=i].join("/");
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new(*seg).size(13.0)).frame(false),
                        )
                        .clicked()
                    {
                        nav_target = Some(partial);
                    }
                }
                if let Some(target) = nav_target {
                    self.navigate_to(&target);
                }
            }

            // Right-aligned action buttons
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Upload button
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("\u{2B06} Upload").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(59, 130, 246)),
                    )
                    .clicked()
                {
                    self.upload_file(runtime);
                }

                // New Folder button
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("\u{1F4C1} New Folder")
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(34, 197, 94)),
                    )
                    .clicked()
                {
                    self.show_new_dir = !self.show_new_dir;
                    self.show_new_file = false;
                    self.new_dir_name.clear();
                }

                // Mount Folder button
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("\u{1F4C2} Mount Folder")
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(234, 179, 8)),
                    )
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Select folder to mount")
                        .pick_folder()
                    {
                        self.sandbox_dir = path.to_string_lossy().to_string();
                        self.current_path.clear();
                        self.selected_file = None;
                        self.file_content.clear();
                        self.needs_refresh = true;
                    }
                }

                // New File button
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("\u{1F4C4} New File")
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(139, 92, 246)),
                    )
                    .clicked()
                {
                    self.show_new_file = !self.show_new_file;
                    self.show_new_dir = false;
                    self.new_file_name.clear();
                }
            });
        });

        // ── New folder input row ──
        if self.show_new_dir {
            ui.horizontal(|ui| {
                ui.label("Folder name:");
                let response = ui.text_edit_singleline(&mut self.new_dir_name);
                if ui.button("Create").clicked()
                    || (response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    self.create_directory(runtime);
                }
                if ui.button("Cancel").clicked() {
                    self.show_new_dir = false;
                    self.new_dir_name.clear();
                }
            });
        }

        // ── New file input row ──
        if self.show_new_file {
            ui.horizontal(|ui| {
                ui.label("File name:");
                let response = ui.text_edit_singleline(&mut self.new_file_name);
                if ui.button("Create").clicked()
                    || (response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    self.create_file(runtime);
                }
                if ui.button("Cancel").clicked() {
                    self.show_new_file = false;
                    self.new_file_name.clear();
                }
            });
        }

        // ── Search bar + Sort options + Multi-select actions ──
        ui.horizontal(|ui| {
            // Search
            ui.label("\u{1F50D}");
            let search_width = 200.0_f32.min(ui.available_width() * 0.3);
            ui.add_sized(
                [search_width, 20.0],
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("Filter files..."),
            );

            ui.separator();

            // Sort controls
            ui.label("Sort:");
            let sort_labels = [
                (SortField::Name, "Name"),
                (SortField::Size, "Size"),
                (SortField::Modified, "Date"),
            ];
            for (field, label) in &sort_labels {
                let is_active = self.sort_field == *field;
                let text = if is_active {
                    let arrow = match self.sort_direction {
                        SortDirection::Ascending => "\u{25B2}",
                        SortDirection::Descending => "\u{25BC}",
                    };
                    format!("{} {}", label, arrow)
                } else {
                    label.to_string()
                };
                let btn = if is_active {
                    egui::Button::new(egui::RichText::new(&text).strong())
                } else {
                    egui::Button::new(&text)
                };
                if ui.add(btn).clicked() {
                    if self.sort_field == *field {
                        self.sort_direction = match self.sort_direction {
                            SortDirection::Ascending => SortDirection::Descending,
                            SortDirection::Descending => SortDirection::Ascending,
                        };
                    } else {
                        self.sort_field = *field;
                        self.sort_direction = SortDirection::Ascending;
                    }
                    self.sort_files();
                }
            }

            ui.separator();

            // Multi-select actions
            if !self.selected_set.is_empty() {
                ui.label(
                    egui::RichText::new(format!("{} selected", self.selected_set.len()))
                        .color(egui::Color32::from_rgb(59, 130, 246)),
                );
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Delete Selected")
                                .color(egui::Color32::from_rgb(239, 68, 68)),
                        ),
                    )
                    .clicked()
                {
                    self.show_delete_confirm = true;
                }
                if ui.button("Deselect All").clicked() {
                    self.selected_set.clear();
                }
            }

            // Select All button
            let filtered = self.filtered_files();
            if ui.button("Select All").clicked() {
                for &idx in &filtered {
                    self.selected_set.insert(self.files[idx].path.clone());
                }
            }
        });

        // ── Bulk delete confirmation dialog ──
        if self.show_delete_confirm {
            egui::Window::new("Confirm Delete")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
                    ui.label(format!(
                        "Are you sure you want to delete {} item(s)?",
                        self.selected_set.len()
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Delete")
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(239, 68, 68)),
                            )
                            .clicked()
                        {
                            self.delete_selected_bulk(runtime);
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_delete_confirm = false;
                        }
                    });
                });
        }

        // ── Status message ──
        if let Some((ref msg, is_err)) = self.status_message {
            let color = if is_err {
                egui::Color32::from_rgb(239, 68, 68)
            } else {
                egui::Color32::from_rgb(34, 197, 94)
            };
            ui.label(egui::RichText::new(msg).size(12.0).color(color));
        }

        // Drop status
        if let Some(ref msg) = self.drop_status.clone() {
            ui.label(
                egui::RichText::new(msg)
                    .size(11.0)
                    .color(egui::Color32::from_rgb(59, 130, 246)),
            );
        }

        ui.separator();

        // ── Main content: left file list + right file viewer ──
        let available = ui.available_size();
        let left_width = (available.x * 0.35).max(200.0);
        let filtered_indices = self.filtered_files();

        ui.horizontal(|ui| {
            // ── Left panel: file list ──
            ui.vertical(|ui| {
                ui.set_width(left_width);
                ui.set_min_height(available.y - 8.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // ".." go-back entry
                        if !self.current_path.is_empty() {
                            let resp = ui.add(
                                egui::Button::new(
                                    egui::RichText::new("\u{1F4C2} ..")
                                        .size(14.0)
                                        .color(egui::Color32::from_rgb(156, 163, 175)),
                                )
                                .frame(false),
                            );
                            if resp.clicked() || resp.double_clicked() {
                                self.navigate_up();
                            }
                            ui.separator();
                        }

                        let mut nav_target: Option<String> = None;
                        let mut select_target: Option<String> = None;
                        let mut toggle_checks: Vec<(String, bool)> = Vec::new();

                        for &idx in &filtered_indices {
                            let entry = &self.files[idx];
                            let entry_path = entry.path.clone();
                            let entry_name = entry.name.clone();
                            let entry_is_dir = entry.is_directory;
                            let entry_size = entry.size;
                            let entry_modified = entry.modified.clone();

                            let is_selected = self
                                .selected_file
                                .as_ref()
                                .map(|s| s == &entry_path)
                                .unwrap_or(false);

                            let is_checked = self.selected_set.contains(&entry_path);

                            ui.horizontal(|ui| {
                                // Checkbox for multi-select
                                let mut checked = is_checked;
                                if ui.checkbox(&mut checked, "").changed() {
                                    toggle_checks.push((entry_path.clone(), checked));
                                }

                                let icon = if entry_is_dir {
                                    "\u{1F4C1}"
                                } else {
                                    "\u{1F4C4}"
                                };

                                let label_text = if entry_is_dir {
                                    format!("{} {}", icon, entry_name)
                                } else {
                                    format!(
                                        "{} {}  ({})",
                                        icon,
                                        entry_name,
                                        Self::format_size(entry_size)
                                    )
                                };

                                let text = if is_selected {
                                    egui::RichText::new(&label_text)
                                        .size(13.0)
                                        .color(egui::Color32::WHITE)
                                } else {
                                    egui::RichText::new(&label_text).size(13.0)
                                };

                                let btn = if is_selected {
                                    egui::Button::new(text)
                                        .fill(egui::Color32::from_rgb(59, 130, 246))
                                } else {
                                    egui::Button::new(text).frame(false)
                                };

                                let resp =
                                    ui.add_sized([left_width - 40.0, 24.0], btn);

                                // Tooltip with modified date
                                let resp = if !entry_modified.is_empty() {
                                    resp.on_hover_text(format!(
                                        "Modified: {}",
                                        Self::format_modified(&entry_modified)
                                    ))
                                } else {
                                    resp
                                };

                                if resp.double_clicked() && entry_is_dir {
                                    nav_target = Some(entry_path.clone());
                                } else if resp.clicked() {
                                    if entry_is_dir {
                                        nav_target = Some(entry_path.clone());
                                    } else {
                                        select_target = Some(entry_path.clone());
                                    }
                                }
                            });
                        }

                        // Apply checkbox toggles
                        for (path, checked) in toggle_checks {
                            if checked {
                                self.selected_set.insert(path);
                            } else {
                                self.selected_set.remove(&path);
                            }
                        }

                        // Apply deferred actions
                        if let Some(path) = nav_target {
                            self.navigate_to(&path);
                        } else if let Some(path) = select_target {
                            let p = path.clone();
                            self.selected_file = Some(path);
                            self.load_file_content(runtime, &p);
                        }

                        if self.files.is_empty() && self.current_path.is_empty() {
                            ui.add_space(40.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("\u{1F4C2}")
                                        .size(48.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new("Empty directory")
                                        .size(14.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Upload files or create a folder to get started.\nYou can also drag & drop files here.",
                                    )
                                    .size(12.0)
                                    .color(egui::Color32::GRAY),
                                );
                            });
                        } else if filtered_indices.is_empty() && !self.search_query.is_empty() {
                            ui.add_space(20.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No files match the filter.")
                                        .size(13.0)
                                        .color(egui::Color32::GRAY),
                                );
                            });
                        }
                    });
            });

            ui.separator();

            // ── Right panel: file viewer / editor / info ──
            ui.vertical(|ui| {
                ui.set_min_width(available.x - left_width - 20.0);

                if let Some(ref selected) = self.selected_file.clone() {
                    let file_name = selected
                        .rsplit('/')
                        .next()
                        .unwrap_or(selected)
                        .to_string();
                    let file_type = classify_file(&file_name);

                    // Toolbar
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&file_name).size(15.0).strong(),
                        );

                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                // Delete button
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("\u{1F5D1} Delete")
                                            .color(egui::Color32::from_rgb(239, 68, 68)),
                                    ))
                                    .clicked()
                                {
                                    self.delete_selected(runtime);
                                }

                                // Download button
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("\u{2B07} Download")
                                                .color(egui::Color32::WHITE),
                                        )
                                        .fill(egui::Color32::from_rgb(107, 114, 128)),
                                    )
                                    .clicked()
                                {
                                    self.download_file();
                                }

                                // Save / Cancel buttons (only when editing)
                                if self.editing {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("Cancel")
                                                    .color(egui::Color32::WHITE),
                                            )
                                            .fill(egui::Color32::from_rgb(107, 114, 128)),
                                        )
                                        .clicked()
                                    {
                                        self.file_content = self.edit_backup.clone();
                                        self.editing = false;
                                    }

                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("\u{1F4BE} Save")
                                                    .color(egui::Color32::WHITE),
                                            )
                                            .fill(egui::Color32::from_rgb(34, 197, 94)),
                                        )
                                        .clicked()
                                    {
                                        self.save_file_content(runtime);
                                    }
                                }

                                // Edit toggle (only for text-like files)
                                if is_text_like(&file_type) {
                                    let edit_label = if self.editing {
                                        "\u{1F512} View"
                                    } else {
                                        "\u{270F} Edit"
                                    };
                                    if ui.button(edit_label).clicked() {
                                        if !self.editing {
                                            self.edit_backup = self.file_content.clone();
                                        }
                                        self.editing = !self.editing;
                                    }
                                }
                            },
                        );
                    });

                    ui.separator();

                    // ── File Info Panel ──
                    let sandbox = self.sandbox_dir.clone();
                    let selected_clone = selected.clone();
                    egui::CollapsingHeader::new(
                        egui::RichText::new("File Info").size(12.0),
                    )
                    .default_open(false)
                    .show(ui, |ui| {
                        if let Ok(full_path) = data::validate_path(&sandbox, &selected_clone) {
                            let full_path_str = full_path.display().to_string();
                            if let Ok(meta) = std::fs::metadata(&full_path) {
                                egui::Grid::new("file_info_grid")
                                    .num_columns(2)
                                    .spacing([8.0, 4.0])
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new("Size:")
                                                .strong()
                                                .size(12.0),
                                        );
                                        ui.label(
                                            egui::RichText::new(Self::format_size(meta.len()))
                                                .size(12.0),
                                        );
                                        ui.end_row();

                                        ui.label(
                                            egui::RichText::new("Modified:")
                                                .strong()
                                                .size(12.0),
                                        );
                                        let mod_str = meta
                                            .modified()
                                            .ok()
                                            .map(|t| {
                                                let dt: chrono::DateTime<chrono::Utc> = t.into();
                                                dt.format("%Y-%m-%d %H:%M:%S").to_string()
                                            })
                                            .unwrap_or_else(|| "N/A".to_string());
                                        ui.label(egui::RichText::new(&mod_str).size(12.0));
                                        ui.end_row();

                                        ui.label(
                                            egui::RichText::new("Permissions:")
                                                .strong()
                                                .size(12.0),
                                        );
                                        let perm_str = if meta.permissions().readonly() {
                                            "Read-only"
                                        } else {
                                            "Read-write"
                                        };
                                        ui.label(egui::RichText::new(perm_str).size(12.0));
                                        ui.end_row();

                                        ui.label(
                                            egui::RichText::new("Path:")
                                                .strong()
                                                .size(12.0),
                                        );
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(&full_path_str)
                                                    .size(11.0)
                                                    .monospace(),
                                            );
                                            if ui
                                                .small_button("\u{1F4CB} Copy")
                                                .clicked()
                                            {
                                                ui.ctx().copy_text(full_path_str.clone());
                                                self.status_message = Some((
                                                    "Path copied to clipboard.".to_string(),
                                                    false,
                                                ));
                                            }
                                        });
                                        ui.end_row();

                                        ui.label(
                                            egui::RichText::new("Type:")
                                                .strong()
                                                .size(12.0),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!("{:?}", file_type))
                                                .size(12.0),
                                        );
                                        ui.end_row();
                                    });
                            }
                        }
                    });

                    ui.separator();

                    // ── File content area with rich preview ──
                    egui::ScrollArea::both()
                        .id_salt("file_content_preview")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if self.editing && is_text_like(&file_type) {
                                // Line count
                                let line_count = self.file_content.lines().count();
                                ui.label(
                                    egui::RichText::new(format!("{} lines", line_count))
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.file_content)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(30)
                                        .code_editor(),
                                );
                            } else {
                                // Read-only preview based on file type
                                match file_type {
                                    FileType::Image => {
                                        // Show image path info - egui can load images
                                        // via URI if egui_extras image loaders are set up.
                                        // Attempt to display using egui::Image with file URI.
                                        let sandbox_c = self.sandbox_dir.clone();
                                        let selected_c = selected.clone();
                                        if let Ok(full_path) =
                                            data::validate_path(&sandbox_c, &selected_c)
                                        {
                                            let uri = format!("file://{}", full_path.display());
                                            // Try to show via egui::Image (requires image loaders)
                                            let image = egui::Image::new(&uri)
                                                .max_width(ui.available_width() - 20.0)
                                                .max_height(ui.available_height() - 20.0)
                                                .corner_radius(4.0);
                                            let resp = ui.add(image);
                                            if resp.hovered() {
                                                resp.on_hover_text(&file_name);
                                            }
                                        } else {
                                            ui.label("Unable to resolve image path.");
                                        }
                                    }
                                    FileType::Markdown => {
                                        // Render markdown with basic formatting
                                        let line_count = self.file_content.lines().count();
                                        ui.label(
                                            egui::RichText::new(format!("{} lines", line_count))
                                                .size(11.0)
                                                .color(egui::Color32::GRAY),
                                        );
                                        ui.add_space(4.0);
                                        let content = self.file_content.clone();
                                        Self::render_markdown(ui, &content);
                                    }
                                    FileType::Csv => {
                                        let row_count = self.file_content.lines().count().saturating_sub(1);
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} data rows",
                                                row_count
                                            ))
                                            .size(11.0)
                                            .color(egui::Color32::GRAY),
                                        );
                                        ui.add_space(4.0);
                                        let content = self.file_content.clone();
                                        Self::render_csv_table(ui, &content);
                                    }
                                    FileType::Code => {
                                        let line_count = self.file_content.lines().count();
                                        ui.label(
                                            egui::RichText::new(format!("{} lines", line_count))
                                                .size(11.0)
                                                .color(egui::Color32::GRAY),
                                        );
                                        // Dark background code view
                                        egui::Frame::NONE
                                            .fill(egui::Color32::from_rgb(30, 30, 30))
                                            .inner_margin(egui::Margin::same(8))
                                            .corner_radius(4.0)
                                            .show(ui, |ui| {
                                                ui.add(
                                                    egui::TextEdit::multiline(
                                                        &mut self.file_content.as_str(),
                                                    )
                                                    .font(egui::TextStyle::Monospace)
                                                    .desired_width(f32::INFINITY)
                                                    .desired_rows(30)
                                                    .code_editor(),
                                                );
                                            });
                                    }
                                    FileType::Text => {
                                        let line_count = self.file_content.lines().count();
                                        ui.label(
                                            egui::RichText::new(format!("{} lines", line_count))
                                                .size(11.0)
                                                .color(egui::Color32::GRAY),
                                        );
                                        ui.add(
                                            egui::TextEdit::multiline(
                                                &mut self.file_content.as_str(),
                                            )
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(30),
                                        );
                                    }
                                    FileType::Binary => {
                                        ui.label(
                                            egui::RichText::new("Binary File")
                                                .size(16.0)
                                                .strong(),
                                        );
                                        ui.add_space(8.0);
                                        // Show the info we loaded
                                        ui.label(
                                            egui::RichText::new(&self.file_content)
                                                .monospace()
                                                .size(13.0),
                                        );
                                        ui.add_space(8.0);

                                        // Show hex dump of first bytes
                                        let sandbox_c = self.sandbox_dir.clone();
                                        let selected_c = selected.clone();
                                        if let Ok(full_path) =
                                            data::validate_path(&sandbox_c, &selected_c)
                                        {
                                            if let Ok(bytes) = std::fs::read(&full_path) {
                                                let preview_len = bytes.len().min(256);
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "Hex dump (first {} bytes):",
                                                        preview_len
                                                    ))
                                                    .size(12.0)
                                                    .color(egui::Color32::GRAY),
                                                );
                                                let mut hex = String::new();
                                                for (i, byte) in
                                                    bytes.iter().take(preview_len).enumerate()
                                                {
                                                    if i > 0 && i % 16 == 0 {
                                                        hex.push('\n');
                                                    } else if i > 0 && i % 2 == 0 {
                                                        hex.push(' ');
                                                    }
                                                    hex.push_str(&format!("{:02X}", byte));
                                                }
                                                egui::Frame::NONE
                                                    .fill(egui::Color32::from_rgb(30, 30, 30))
                                                    .inner_margin(egui::Margin::same(8))
                                                    .corner_radius(4.0)
                                                    .show(ui, |ui| {
                                                        ui.label(
                                                            egui::RichText::new(&hex)
                                                                .monospace()
                                                                .size(12.0)
                                                                .color(egui::Color32::from_rgb(
                                                                    180, 180, 180,
                                                                )),
                                                        );
                                                    });
                                            }
                                        }
                                    }
                                }
                            }
                        });
                } else {
                    // No file selected placeholder
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.label(
                            egui::RichText::new("\u{1F4C4}")
                                .size(48.0)
                                .color(egui::Color32::GRAY),
                        );
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new("Select a file to view its contents")
                                .size(14.0)
                                .color(egui::Color32::GRAY),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Drag & drop files here to upload")
                                .size(12.0)
                                .color(egui::Color32::from_rgb(120, 120, 120)),
                        );
                    });
                }
            });
        });
    }
}
