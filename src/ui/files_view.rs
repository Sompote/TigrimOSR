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
        "" => FileType::Text,
        _ => FileType::Binary,
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

// ── Library section ──

#[derive(Debug, Clone, PartialEq)]
enum LibrarySection {
    AllFiles,
    Recent,
    Place(String), // directory name
}

// ── File entry ──

struct FileEntry {
    name: String,
    path: String,
    is_directory: bool,
    size: u64,
    modified: String,
    item_count: Option<usize>, // for directories
}

// ── Extension badge ──

fn extension_badge(name: &str) -> Option<(&'static str, egui::Color32)> {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    let (label, color) = match ext.as_str() {
        "pdf" => ("PDF", egui::Color32::from_rgb(220, 53, 69)),
        "doc" | "docx" => ("DOCX", egui::Color32::from_rgb(43, 87, 154)),
        "xls" | "xlsx" => ("XLSX", egui::Color32::from_rgb(33, 115, 70)),
        "ppt" | "pptx" => ("PPTX", egui::Color32::from_rgb(197, 90, 17)),
        "png" => ("PNG", egui::Color32::from_rgb(194, 100, 39)),
        "jpg" | "jpeg" => ("JPG", egui::Color32::from_rgb(194, 100, 39)),
        "gif" => ("GIF", egui::Color32::from_rgb(194, 100, 39)),
        "webp" => ("WEBP", egui::Color32::from_rgb(194, 100, 39)),
        "svg" => ("SVG", egui::Color32::from_rgb(194, 100, 39)),
        "bmp" => ("BMP", egui::Color32::from_rgb(194, 100, 39)),
        "mp4" | "mov" | "avi" | "mkv" => ("VID", egui::Color32::from_rgb(128, 0, 128)),
        "mp3" | "wav" | "flac" | "ogg" => ("AUD", egui::Color32::from_rgb(128, 0, 128)),
        "zip" | "tar" | "gz" | "7z" | "rar" => ("ZIP", egui::Color32::from_rgb(108, 117, 125)),
        "py" => ("PY", egui::Color32::from_rgb(55, 118, 171)),
        "rs" => ("RS", egui::Color32::from_rgb(183, 65, 14)),
        "js" | "jsx" => ("JS", egui::Color32::from_rgb(247, 223, 30)),
        "ts" | "tsx" => ("TS", egui::Color32::from_rgb(49, 120, 198)),
        "html" | "htm" => ("HTML", egui::Color32::from_rgb(228, 77, 38)),
        "css" | "scss" | "sass" => ("CSS", egui::Color32::from_rgb(86, 61, 124)),
        "json" => ("JSON", egui::Color32::from_rgb(108, 117, 125)),
        "yaml" | "yml" => ("YAML", egui::Color32::from_rgb(108, 117, 125)),
        "toml" => ("TOML", egui::Color32::from_rgb(108, 117, 125)),
        "xml" => ("XML", egui::Color32::from_rgb(108, 117, 125)),
        "md" | "markdown" => ("MD", egui::Color32::from_rgb(86, 61, 124)),
        "txt" => ("TXT", egui::Color32::from_rgb(108, 117, 125)),
        "log" => ("LOG", egui::Color32::from_rgb(108, 117, 125)),
        "csv" => ("CSV", egui::Color32::from_rgb(33, 115, 70)),
        "sql" => ("SQL", egui::Color32::from_rgb(0, 114, 198)),
        "sh" | "bash" | "zsh" => ("SH", egui::Color32::from_rgb(60, 60, 60)),
        "java" => ("JAVA", egui::Color32::from_rgb(176, 114, 25)),
        "c" => ("C", egui::Color32::from_rgb(85, 85, 85)),
        "cpp" | "cc" | "cxx" => ("CPP", egui::Color32::from_rgb(0, 89, 156)),
        "h" | "hpp" => ("H", egui::Color32::from_rgb(85, 85, 85)),
        "go" => ("GO", egui::Color32::from_rgb(0, 173, 216)),
        "rb" => ("RB", egui::Color32::from_rgb(204, 52, 45)),
        "php" => ("PHP", egui::Color32::from_rgb(119, 123, 180)),
        "swift" => ("SWIFT", egui::Color32::from_rgb(240, 81, 56)),
        "r" => ("R", egui::Color32::from_rgb(39, 104, 177)),
        "bib" => ("BIB", egui::Color32::from_rgb(120, 94, 70)),
        "tex" | "latex" => ("TEX", egui::Color32::from_rgb(0, 128, 128)),
        "ini" | "cfg" | "conf" => ("CFG", egui::Color32::from_rgb(108, 117, 125)),
        _ => return None,
    };
    Some((label, color))
}

/// Native egui file browser for the TigrimOS sandbox directory.
pub struct FilesView {
    sandbox_dir: String,
    current_path: String,
    files: Vec<FileEntry>,
    selected_file: Option<String>,
    file_content: String,
    editing: bool,
    edit_backup: String,
    new_dir_name: String,
    show_new_dir: bool,
    new_file_name: String,
    show_new_file: bool,
    needs_refresh: bool,
    status_message: Option<(String, bool)>,

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

    // Sidebar
    active_section: LibrarySection,
    places: Vec<(String, usize)>, // (dir_name, item_count)

    // File viewer overlay
    show_viewer: bool,
}

impl Default for FilesView {
    fn default() -> Self {
        let sandbox_dir = std::env::var("TIGRIMOS_SANDBOX_DIR")
            .unwrap_or_else(|_| crate::server::data::get_sandbox_dir_sync());
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
            active_section: LibrarySection::AllFiles,
            places: Vec::new(),
            show_viewer: false,
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
                            item_count: None,
                        })
                    })
                    .collect();

                // Count items inside directories
                for entry in &mut self.files {
                    if entry.is_directory {
                        let dir_path = std::path::Path::new(&sandbox).join(&entry.path);
                        if let Ok(rd) = std::fs::read_dir(&dir_path) {
                            entry.item_count = Some(rd.count());
                        }
                    }
                }

                self.sort_files();
                self.status_message = None;
            }
            Err(e) => {
                self.status_message = Some((format!("Failed to list files: {}", e), true));
                self.files.clear();
            }
        }

        // Refresh places (top-level directories) if we're at root
        if self.current_path.is_empty() {
            self.places = self
                .files
                .iter()
                .filter(|f| f.is_directory)
                .map(|f| {
                    let count = f.item_count.unwrap_or(0);
                    (f.name.clone(), count)
                })
                .collect();
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

        if file_type == FileType::Image {
            self.file_content.clear();
            self.editing = false;
            self.status_message = None;
            return;
        }

        if !is_text_like(&file_type) {
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
                            self.file_content =
                                "[Binary file - unable to read metadata]".to_string();
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
                    self.show_viewer = false;
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
                    if self.selected_file.as_deref() == Some(p.as_str()) {
                        self.selected_file = None;
                        self.file_content.clear();
                        self.editing = false;
                        self.show_viewer = false;
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
                    // Binary file
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
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
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
        self.show_viewer = false;
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
        self.show_viewer = false;
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

    fn format_relative_date(modified: &str) -> String {
        if modified.is_empty() {
            return String::new();
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(modified) {
            let now = chrono::Utc::now();
            let file_date = dt.with_timezone(&chrono::Utc).date_naive();
            let today = now.date_naive();
            let diff_days = (today - file_date).num_days();

            if diff_days == 0 {
                // Today — show time
                format!("Today, {}", dt.format("%H:%M"))
            } else if diff_days == 1 {
                "Yesterday".to_string()
            } else if diff_days < 7 {
                format!("{} days ago", diff_days)
            } else if diff_days < 14 {
                "1 week ago".to_string()
            } else if diff_days < 30 {
                format!("{} weeks ago", diff_days / 7)
            } else {
                // Show month + day
                dt.format("%b %d").to_string()
            }
        } else {
            modified.to_string()
        }
    }

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

    fn total_file_count(&self) -> usize {
        self.files.len()
    }

    /// Calculate storage used in sandbox
    fn calculate_storage(&self) -> u64 {
        fn dir_size(path: &std::path::Path) -> u64 {
            let mut total = 0u64;
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        total += dir_size(&p);
                    } else if let Ok(meta) = p.metadata() {
                        total += meta.len();
                    }
                }
            }
            total
        }
        dir_size(std::path::Path::new(&self.sandbox_dir))
    }

    // ── Rendering helpers ──

    fn render_extension_badge(ui: &mut egui::Ui, name: &str) {
        if let Some((label, color)) = extension_badge(name) {
            let badge_rect = ui.allocate_space(egui::vec2(36.0, 20.0));
            let rect = badge_rect.1;
            ui.painter().rect_filled(rect, 3.0, color);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(9.0),
                egui::Color32::WHITE,
            );
        } else {
            // Generic file badge
            let badge_rect = ui.allocate_space(egui::vec2(36.0, 20.0));
            let rect = badge_rect.1;
            ui.painter()
                .rect_filled(rect, 3.0, egui::Color32::from_rgb(108, 117, 125));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "FILE",
                egui::FontId::proportional(9.0),
                egui::Color32::WHITE,
            );
        }
    }

    fn render_folder_icon(ui: &mut egui::Ui) {
        let (_, rect) = ui.allocate_space(egui::vec2(24.0, 20.0));
        // Simple folder shape using painter
        let c = rect.center();
        let folder_color = egui::Color32::from_rgb(164, 156, 144);
        // Folder body
        let body = egui::Rect::from_min_size(
            egui::pos2(c.x - 10.0, c.y - 5.0),
            egui::vec2(20.0, 14.0),
        );
        ui.painter().rect_filled(body, 2.0, folder_color);
        // Folder tab
        let tab = egui::Rect::from_min_size(
            egui::pos2(c.x - 10.0, c.y - 8.0),
            egui::vec2(10.0, 4.0),
        );
        ui.painter().rect_filled(tab, 1.5, folder_color);
    }

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
            } else if trimmed.starts_with("- ") {
                ui.horizontal(|ui| {
                    ui.label("  \u{2022} ");
                    Self::render_inline_markdown(ui, &trimmed[2..]);
                });
            } else if trimmed.starts_with("> ") {
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(8, 2))
                    .stroke(egui::Stroke::new(
                        2.0,
                        egui::Color32::from_rgb(100, 100, 100),
                    ))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&trimmed[2..])
                                .italics()
                                .color(egui::Color32::from_rgb(180, 180, 180)),
                        );
                    });
            } else if trimmed.starts_with("```") {
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
        if text.contains("**") {
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
                        for cell in &header_cells {
                            ui.label(egui::RichText::new(cell.trim()).strong());
                        }
                        ui.end_row();

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
                        egui::RichText::new(format!("... and {} more rows", lines.len() - 501))
                            .color(egui::Color32::GRAY),
                    );
                }
            });
    }

    // ── Sidebar rendering ──

    fn render_sidebar(&mut self, ui: &mut egui::Ui) {
        let sidebar_bg = egui::Color32::from_rgb(247, 247, 248);
        let text_dim = egui::Color32::from_rgb(100, 100, 105);
        let text_normal = egui::Color32::from_rgb(30, 30, 32);
        let accent = egui::Color32::from_rgb(59, 130, 246);

        ui.painter().rect_filled(
            ui.available_rect_before_wrap(),
            0.0,
            sidebar_bg,
        );

        ui.add_space(12.0);

        // Files header
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("Files")
                    .size(16.0)
                    .strong()
                    .color(text_normal),
            );
        });

        ui.add_space(8.0);

        // + New file button
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let btn = egui::Button::new(
                egui::RichText::new("+ New file")
                    .size(13.0)
                    .color(text_normal),
            )
            .frame(false);
            if ui.add(btn).clicked() {
                self.show_new_file = !self.show_new_file;
                self.show_new_dir = false;
                self.new_file_name.clear();
            }
        });

        ui.add_space(12.0);

        // LIBRARY section
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("LIBRARY")
                    .size(10.0)
                    .color(text_dim),
            );
        });
        ui.add_space(4.0);

        // Library items
        let all_count = self.total_file_count();
        let lib_items: Vec<(&str, LibrarySection, Option<usize>)> = vec![
            ("All files", LibrarySection::AllFiles, Some(all_count)),
            ("Recent", LibrarySection::Recent, None),
        ];

        let mut nav_action: Option<LibrarySection> = None;

        for (label, section, count) in &lib_items {
            let is_active = self.active_section == *section;
            let bg = if is_active {
                egui::Color32::from_rgb(232, 232, 235)
            } else {
                egui::Color32::TRANSPARENT
            };
            let text_color = if is_active { accent } else { text_normal };

            let resp = ui.horizontal(|ui| {
                ui.add_space(8.0);
                egui::Frame::NONE
                    .fill(bg)
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width() - 16.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(*label)
                                    .size(13.0)
                                    .color(text_color),
                            );
                            if let Some(c) = count {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{}", c))
                                                .size(11.0)
                                                .color(text_dim),
                                        );
                                    },
                                );
                            }
                        });
                    });
            });
            if resp.response.interact(egui::Sense::click()).clicked() {
                nav_action = Some(section.clone());
            }
        }

        ui.add_space(16.0);

        // PLACES section
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("PLACES")
                    .size(10.0)
                    .color(text_dim),
            );
        });
        ui.add_space(4.0);

        let places_clone = self.places.clone();
        for (dir_name, count) in &places_clone {
            let is_active = self.active_section == LibrarySection::Place(dir_name.clone());
            let bg = if is_active {
                egui::Color32::from_rgb(232, 232, 235)
            } else {
                egui::Color32::TRANSPARENT
            };
            let text_color = if is_active { accent } else { text_normal };

            let resp = ui.horizontal(|ui| {
                ui.add_space(8.0);
                egui::Frame::NONE
                    .fill(bg)
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width() - 16.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(dir_name)
                                    .size(13.0)
                                    .color(text_color),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!("{}", count))
                                            .size(11.0)
                                            .color(text_dim),
                                    );
                                },
                            );
                        });
                    });
            });
            if resp.response.interact(egui::Sense::click()).clicked() {
                nav_action = Some(LibrarySection::Place(dir_name.clone()));
            }
        }

        // Apply navigation
        if let Some(section) = nav_action {
            self.active_section = section.clone();
            match section {
                LibrarySection::AllFiles => {
                    self.current_path.clear();
                    self.needs_refresh = true;
                }
                LibrarySection::Recent => {
                    // Stay in current view but sort by modified descending
                    self.current_path.clear();
                    self.sort_field = SortField::Modified;
                    self.sort_direction = SortDirection::Descending;
                    self.needs_refresh = true;
                }
                LibrarySection::Place(ref dir) => {
                    self.current_path = dir.clone();
                    self.needs_refresh = true;
                }
            }
        }

        // Push storage to bottom
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let used = self.calculate_storage();
                let used_str = Self::format_size(used);
                ui.label(
                    egui::RichText::new(format!("Storage  {}", used_str))
                        .size(11.0)
                        .color(text_dim),
                );
            });
            // Storage bar
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let available_w = (ui.available_width() - 24.0).max(40.0);
                let used = self.calculate_storage() as f64;
                let max_storage = 10.0 * 1024.0 * 1024.0 * 1024.0; // 10 GB
                let ratio = (used / max_storage).min(1.0) as f32;
                let (_, bar_rect) = ui.allocate_space(egui::vec2(available_w, 4.0));
                ui.painter().rect_filled(
                    bar_rect,
                    2.0,
                    egui::Color32::from_rgb(210, 210, 215),
                );
                let filled = egui::Rect::from_min_size(
                    bar_rect.min,
                    egui::vec2(bar_rect.width() * ratio, 4.0),
                );
                ui.painter().rect_filled(filled, 2.0, accent);
            });
            ui.add_space(4.0);
        });
    }

    // ── File viewer overlay ──

    fn render_viewer_window(
        &mut self,
        ctx: &egui::Context,
        runtime: &tokio::runtime::Handle,
    ) {
        if !self.show_viewer {
            return;
        }
        let Some(ref selected) = self.selected_file.clone() else {
            return;
        };

        let file_name = selected
            .rsplit('/')
            .next()
            .unwrap_or(selected)
            .to_string();
        let file_type = classify_file(&file_name);

        let mut open = self.show_viewer;
        egui::Window::new(&file_name)
            .open(&mut open)
            .resizable(true)
            .default_width(700.0)
            .default_height(500.0)
            .show(ctx, |ui| {
                // Toolbar
                ui.horizontal(|ui| {
                    // Edit toggle (only for text-like files)
                    if is_text_like(&file_type) {
                        let edit_label = if self.editing { "View" } else { "Edit" };
                        if ui.button(edit_label).clicked() {
                            if !self.editing {
                                self.edit_backup = self.file_content.clone();
                            }
                            self.editing = !self.editing;
                        }
                    }

                    if self.editing {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Save")
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(34, 197, 94)),
                            )
                            .clicked()
                        {
                            self.save_file_content(runtime);
                        }
                        if ui.button("Cancel").clicked() {
                            self.file_content = self.edit_backup.clone();
                            self.editing = false;
                        }
                    }

                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("Delete")
                                        .color(egui::Color32::from_rgb(239, 68, 68)),
                                ))
                                .clicked()
                            {
                                self.delete_selected(runtime);
                            }
                            if ui.button("Download").clicked() {
                                self.download_file();
                            }
                        },
                    );
                });

                ui.separator();

                // Content
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.editing && is_text_like(&file_type) {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.file_content)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(25)
                                    .code_editor(),
                            );
                        } else {
                            match file_type {
                                FileType::Image => {
                                    let sandbox_c = self.sandbox_dir.clone();
                                    let selected_c = selected.clone();
                                    if let Ok(full_path) =
                                        data::validate_path(&sandbox_c, &selected_c)
                                    {
                                        let uri =
                                            format!("file://{}", full_path.display());
                                        let image = egui::Image::new(&uri)
                                            .max_width(ui.available_width() - 20.0)
                                            .max_height(ui.available_height() - 20.0)
                                            .corner_radius(4.0);
                                        ui.add(image);
                                    }
                                }
                                FileType::Markdown => {
                                    let content = self.file_content.clone();
                                    Self::render_markdown(ui, &content);
                                }
                                FileType::Csv => {
                                    let content = self.file_content.clone();
                                    Self::render_csv_table(ui, &content);
                                }
                                FileType::Code => {
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
                                                .desired_rows(25)
                                                .code_editor(),
                                            );
                                        });
                                }
                                FileType::Text => {
                                    ui.add(
                                        egui::TextEdit::multiline(
                                            &mut self.file_content.as_str(),
                                        )
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(25),
                                    );
                                }
                                FileType::Binary => {
                                    ui.label(
                                        egui::RichText::new(&self.file_content)
                                            .monospace()
                                            .size(13.0),
                                    );
                                }
                            }
                        }
                    });
            });
        self.show_viewer = open;
        if !open {
            self.editing = false;
        }
    }

    // ── Main entry point ──

    pub fn show(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let ctx = ui.ctx().clone();

        self.handle_dropped_files(&ctx, runtime);

        if self.needs_refresh {
            self.refresh(runtime);
        }

        // Bulk delete confirmation dialog
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

        // File viewer overlay window
        self.render_viewer_window(&ctx, runtime);

        let full_rect = ui.available_rect_before_wrap();
        let sidebar_w = 180.0_f32;

        // ── Left Sidebar ──
        let sidebar_rect =
            egui::Rect::from_min_size(full_rect.min, egui::vec2(sidebar_w, full_rect.height()));

        let mut sidebar_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(sidebar_rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );
        self.render_sidebar(&mut sidebar_ui);

        // ── Main content area ──
        let main_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.min.x + sidebar_w + 1.0, full_rect.min.y),
            full_rect.max,
        );

        // Separator line
        ui.painter().line_segment(
            [
                egui::pos2(full_rect.min.x + sidebar_w, full_rect.min.y),
                egui::pos2(full_rect.min.x + sidebar_w, full_rect.max.y),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(230, 230, 232)),
        );

        let mut main_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(main_rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );

        self.render_main_area(&mut main_ui, runtime);
    }

    fn render_main_area(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let text_dim = egui::Color32::from_rgb(134, 134, 139);
        let text_normal = egui::Color32::from_rgb(29, 29, 31);
        let accent = egui::Color32::from_rgb(59, 130, 246);

        // White background
        ui.painter().rect_filled(
            ui.available_rect_before_wrap(),
            0.0,
            egui::Color32::WHITE,
        );

        ui.add_space(8.0);

        // ── Header: breadcrumb + search + actions ──
        ui.horizontal(|ui| {
            ui.add_space(12.0);

            // Breadcrumb
            let root_label = std::path::Path::new(&self.sandbox_dir)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("sandbox")
                .to_string();

            // "Library" root
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Library")
                            .size(14.0)
                            .color(text_dim),
                    )
                    .frame(false),
                )
                .clicked()
            {
                self.navigate_to("");
                self.active_section = LibrarySection::AllFiles;
            }

            if !self.current_path.is_empty() || true {
                ui.label(
                    egui::RichText::new("/")
                        .size(14.0)
                        .color(text_dim),
                );

                if self.current_path.is_empty() {
                    ui.label(
                        egui::RichText::new(&root_label)
                            .size(14.0)
                            .strong()
                            .color(text_normal),
                    );
                } else {
                    // Show sandbox root as clickable
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(&root_label)
                                    .size(14.0)
                                    .color(text_dim),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        self.navigate_to("");
                    }

                    let path_clone = self.current_path.clone();
                    let segments: Vec<&str> = path_clone.split('/').collect();
                    let mut nav_target: Option<String> = None;
                    for (i, seg) in segments.iter().enumerate() {
                        ui.label(
                            egui::RichText::new("/")
                                .size(14.0)
                                .color(text_dim),
                        );
                        let partial: String = segments[..=i].join("/");
                        let is_last = i == segments.len() - 1;
                        if is_last {
                            ui.label(
                                egui::RichText::new(*seg)
                                    .size(14.0)
                                    .strong()
                                    .color(text_normal),
                            );
                        } else if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(*seg)
                                        .size(14.0)
                                        .color(text_dim),
                                )
                                .frame(false),
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

                // File count
                ui.label(
                    egui::RichText::new(format!("  {}", self.files.len()))
                        .size(12.0)
                        .color(text_dim),
                );
            }

            // Right-aligned: search + actions
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                // + New button
                let new_menu_id = ui.id().with("new_menu");
                let new_btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new("+ New")
                            .size(12.0)
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(230, 230, 232))
                    .corner_radius(6.0),
                );
                if new_btn.clicked() {
                    ui.memory_mut(|mem| mem.toggle_popup(new_menu_id));
                }
                egui::popup_below_widget(ui, new_menu_id, &new_btn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                    ui.set_min_width(120.0);
                    if ui.button("New File").clicked() {
                        self.show_new_file = true;
                        self.show_new_dir = false;
                        self.new_file_name.clear();
                        ui.memory_mut(|mem| mem.toggle_popup(new_menu_id));
                    }
                    if ui.button("New Folder").clicked() {
                        self.show_new_dir = true;
                        self.show_new_file = false;
                        self.new_dir_name.clear();
                        ui.memory_mut(|mem| mem.toggle_popup(new_menu_id));
                    }
                    if ui.button("Mount Folder").clicked() {
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
                        ui.memory_mut(|mem| mem.toggle_popup(new_menu_id));
                    }
                });

                // Upload button
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Upload")
                                .size(12.0)
                                .color(text_normal),
                        )
                        .corner_radius(6.0),
                    )
                    .clicked()
                {
                    self.upload_file(runtime);
                }

                ui.add_space(8.0);
            });
        });

        // Search bar (functional)
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let search_w = 200.0_f32.min(ui.available_width() * 0.3);
            egui::Frame::NONE
                .fill(egui::Color32::from_rgb(240, 240, 242))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.add_sized(
                        [search_w, 18.0],
                        egui::TextEdit::singleline(&mut self.search_query)
                            .hint_text("Search files...")
                            .frame(false)
                            .text_color(text_normal)
                            .font(egui::FontId::proportional(12.0)),
                    );
                });
        });

        // ── New file/folder input rows ──
        if self.show_new_file {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("File name:")
                        .size(12.0)
                        .color(text_normal),
                );
                let response = ui.add_sized(
                    [200.0, 20.0],
                    egui::TextEdit::singleline(&mut self.new_file_name),
                );
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Create")
                                .size(12.0)
                                .color(egui::Color32::WHITE),
                        )
                        .fill(accent),
                    )
                    .clicked()
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

        if self.show_new_dir {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("Folder name:")
                        .size(12.0)
                        .color(text_normal),
                );
                let response = ui.add_sized(
                    [200.0, 20.0],
                    egui::TextEdit::singleline(&mut self.new_dir_name),
                );
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Create")
                                .size(12.0)
                                .color(egui::Color32::WHITE),
                        )
                        .fill(accent),
                    )
                    .clicked()
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

        ui.add_space(4.0);

        // ── Sort bar ──
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("SORT")
                    .size(10.0)
                    .color(text_dim),
            );
            ui.add_space(8.0);

            let sort_labels = [
                (SortField::Name, "Name"),
                (SortField::Size, "Size"),
                (SortField::Modified, "Modified"),
            ];
            for (field, label) in &sort_labels {
                let is_active = self.sort_field == *field;
                let arrow = if is_active {
                    match self.sort_direction {
                        SortDirection::Ascending => " ^",
                        SortDirection::Descending => " v",
                    }
                } else {
                    ""
                };
                let text_color = if is_active { accent } else { text_dim };
                let btn = egui::Button::new(
                    egui::RichText::new(format!("{}{}", label, arrow))
                        .size(12.0)
                        .color(text_color),
                )
                .frame(false);
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

            ui.add_space(12.0);

        });

        // ── Selection action bar ──
        if !self.selected_set.is_empty() {
            ui.add_space(4.0);
            egui::Frame::NONE
                .fill(egui::Color32::from_rgb(240, 245, 255))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(12, 6))
                .outer_margin(egui::Margin::symmetric(12, 0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} selected", self.selected_set.len()))
                                .size(13.0)
                                .strong()
                                .color(accent),
                        );

                        ui.add_space(16.0);

                        // Download button
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Download")
                                        .size(12.0)
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(107, 114, 128))
                                .corner_radius(6.0),
                            )
                            .clicked()
                        {
                            // Download first selected file
                            if let Some(first) = self.selected_set.iter().next().cloned() {
                                self.selected_file = Some(first);
                                self.download_file();
                            }
                        }

                        ui.add_space(4.0);

                        // Delete button
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Delete")
                                        .size(12.0)
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(239, 68, 68))
                                .corner_radius(6.0),
                            )
                            .clicked()
                        {
                            self.show_delete_confirm = true;
                        }

                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("Deselect all")
                                                .size(11.0)
                                                .color(text_dim),
                                        )
                                        .frame(false),
                                    )
                                    .clicked()
                                {
                                    self.selected_set.clear();
                                }
                            },
                        );
                    });
                });
        }

        // Status message
        if let Some((ref msg, is_err)) = self.status_message {
            let color = if is_err {
                egui::Color32::from_rgb(239, 68, 68)
            } else {
                egui::Color32::from_rgb(34, 197, 94)
            };
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(msg).size(11.0).color(color));
            });
        }

        if let Some(ref msg) = self.drop_status.clone() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(msg)
                        .size(11.0)
                        .color(accent),
                );
            });
        }

        ui.add_space(2.0);

        // ── Column headers ──
        let avail_w = ui.available_width();
        let name_w = avail_w - 180.0; // Reserve space for SIZE and MODIFIED columns

        ui.horizontal(|ui| {
            ui.add_space(12.0);
            // Checkbox spacer
            ui.add_space(24.0);
            // Icon/badge spacer
            ui.add_space(40.0);
            ui.add_sized([name_w - 80.0, 16.0], egui::Label::new(
                egui::RichText::new("NAME")
                    .size(10.0)
                    .color(text_dim),
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(16.0);
                ui.add_sized([80.0, 16.0], egui::Label::new(
                    egui::RichText::new("MODIFIED")
                        .size(10.0)
                        .color(text_dim),
                ));
                ui.add_sized([60.0, 16.0], egui::Label::new(
                    egui::RichText::new("SIZE")
                        .size(10.0)
                        .color(text_dim),
                ));
            });
        });

        // Thin separator
        ui.painter().line_segment(
            [
                egui::pos2(ui.min_rect().min.x + 12.0, ui.cursor().min.y),
                egui::pos2(ui.min_rect().max.x - 8.0, ui.cursor().min.y),
            ],
            egui::Stroke::new(0.5, egui::Color32::from_rgb(230, 230, 232)),
        );
        ui.add_space(2.0);

        // ── File list ──
        let filtered_indices = self.filtered_files();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // ".." go-back entry
                if !self.current_path.is_empty() {
                    let resp = ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.add_space(24.0); // checkbox space
                        Self::render_folder_icon(ui);
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("..")
                                .size(13.0)
                                .color(text_dim),
                        );
                    });
                    if resp.response.interact(egui::Sense::click()).clicked() {
                        self.navigate_up();
                    }
                }

                let mut nav_target: Option<String> = None;
                let mut select_target: Option<String> = None;
                let mut toggle_checks: Vec<(String, bool)> = Vec::new();

                let row_height = 32.0;

                for &idx in &filtered_indices {
                    let entry = &self.files[idx];
                    let entry_path = entry.path.clone();
                    let entry_name = entry.name.clone();
                    let entry_is_dir = entry.is_directory;
                    let entry_size = entry.size;
                    let entry_modified = entry.modified.clone();
                    let entry_item_count = entry.item_count;

                    let is_selected = self
                        .selected_file
                        .as_ref()
                        .map(|s| s == &entry_path)
                        .unwrap_or(false);
                    let is_checked = self.selected_set.contains(&entry_path);

                    // Row background
                    let row_rect = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(ui.available_width(), row_height),
                    );

                    // Hover highlight
                    let resp = ui.allocate_rect(row_rect, egui::Sense::click());
                    let hovered = resp.hovered();

                    if is_selected {
                        ui.painter().rect_filled(
                            row_rect,
                            0.0,
                            egui::Color32::from_rgb(219, 234, 254),
                        );
                    } else if hovered {
                        ui.painter().rect_filled(
                            row_rect,
                            0.0,
                            egui::Color32::from_rgb(245, 245, 247),
                        );
                    }

                    // Render row content
                    let mut row_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(row_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );

                    row_ui.add_space(12.0);

                    // Checkbox
                    let mut checked = is_checked;
                    if row_ui.checkbox(&mut checked, "").changed() {
                        toggle_checks.push((entry_path.clone(), checked));
                    }

                    // Icon / Badge
                    if entry_is_dir {
                        Self::render_folder_icon(&mut row_ui);
                    } else {
                        Self::render_extension_badge(&mut row_ui, &entry_name);
                    }

                    row_ui.add_space(8.0);

                    // Name
                    row_ui.label(
                        egui::RichText::new(&entry_name)
                            .size(13.0)
                            .color(text_normal),
                    );

                    // Right-aligned: size + modified
                    let right_area = egui::Rect::from_min_max(
                        egui::pos2(row_rect.max.x - 170.0, row_rect.min.y),
                        row_rect.max,
                    );
                    let mut right_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(right_area)
                            .layout(egui::Layout::right_to_left(egui::Align::Center)),
                    );

                    right_ui.add_space(16.0);

                    // Modified date
                    let date_str = Self::format_relative_date(&entry_modified);
                    right_ui.label(
                        egui::RichText::new(&date_str)
                            .size(12.0)
                            .color(text_dim),
                    );

                    // Size
                    let size_str = if entry_is_dir {
                        format!(
                            "{} items",
                            entry_item_count.unwrap_or(0)
                        )
                    } else {
                        Self::format_size(entry_size)
                    };
                    right_ui.add_space(8.0);
                    right_ui.label(
                        egui::RichText::new(&size_str)
                            .size(12.0)
                            .color(text_dim),
                    );

                    // Handle clicks
                    if resp.double_clicked() {
                        if entry_is_dir {
                            nav_target = Some(entry_path.clone());
                        } else {
                            // Open viewer
                            select_target = Some(entry_path.clone());
                            self.show_viewer = true;
                        }
                    } else if resp.clicked() {
                        if entry_is_dir {
                            nav_target = Some(entry_path.clone());
                        } else {
                            select_target = Some(entry_path.clone());
                        }
                    }

                    // Context menu
                    resp.context_menu(|ui| {
                        if !entry_is_dir {
                            if ui.button("Open").clicked() {
                                select_target = Some(entry_path.clone());
                                self.show_viewer = true;
                                ui.close_menu();
                            }
                            if ui.button("Download").clicked() {
                                self.selected_file = Some(entry_path.clone());
                                self.download_file();
                                ui.close_menu();
                            }
                        }
                        if ui.button("Delete").clicked() {
                            self.selected_file = Some(entry_path.clone());
                            // Will be deleted after menu closes
                            ui.close_menu();
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

                // Empty state
                if self.files.is_empty() && self.current_path.is_empty() {
                    ui.add_space(60.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("Empty directory")
                                .size(16.0)
                                .color(text_dim),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(
                                "Upload files or create a folder to get started.\nDrag & drop files here.",
                            )
                            .size(12.0)
                            .color(text_dim),
                        );
                    });
                } else if filtered_indices.is_empty() && !self.search_query.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("No files match the search.")
                                .size(13.0)
                                .color(text_dim),
                        );
                    });
                }
            });
    }
}
