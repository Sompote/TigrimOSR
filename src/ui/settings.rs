use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::server::data::{
    generate_token, get_file_tokens, get_settings, save_file_tokens, save_settings, FileToken,
    LocalFileMount, McpTool, RemoteInstance,
};
use crate::vm::{VmConfig, VmManager, VmState};

// ---------------------------------------------------------------------------
// Section enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsSection {
    General,
    AI,
    SubAgent,
    McpTools,
    Plugins,
    Remote,
    FileTokens,
    FileMounts,
    SkillUpdate,
    Security,
    About,
}

impl SettingsSection {
    const ALL: &'static [SettingsSection] = &[
        Self::General,
        Self::AI,
        Self::SubAgent,
        Self::McpTools,
        Self::Plugins,
        Self::Remote,
        Self::FileTokens,
        Self::FileMounts,
        Self::SkillUpdate,
        Self::Security,
        Self::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::AI => "AI / API",
            Self::SubAgent => "Sub-Agent",
            Self::McpTools => "MCP Tools",
            Self::Plugins => "Plugins",
            Self::Remote => "Remote",
            Self::FileTokens => "File Tokens",
            Self::FileMounts => "File Mounts",
            Self::SkillUpdate => "Skill Update",
            Self::Security => "Security",
            Self::About => "About",
        }
    }
}

// ---------------------------------------------------------------------------
// Connection test status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum ConnectionStatus {
    Idle,
    #[allow(dead_code)]
    Testing,
    Success(String),
    Error(String),
}

// ---------------------------------------------------------------------------
// AI Provider definitions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AiProvider {
    name: String,
    api_url: String,
    default_model: String,
}

impl AiProvider {
    fn new(name: &str, api_url: &str, default_model: &str) -> Self {
        Self {
            name: name.to_string(),
            api_url: api_url.to_string(),
            default_model: default_model.to_string(),
        }
    }
}

fn builtin_providers() -> Vec<AiProvider> {
    vec![
        AiProvider::new("Claude Code (Local)", "claude-code", "claude-sonnet-4-20250514"),
        AiProvider::new("Gemini CLI (Local)", "gemini-cli", "gemini-2.5-pro"),
        AiProvider::new("Codex (Local)", "codex-cli", "gpt-5.4"),
        AiProvider::new("OpenRouter", "https://openrouter.ai/api/v1", "openrouter/auto"),
        AiProvider::new("xAI (Grok)", "https://api.x.ai/v1", "grok-3"),
        AiProvider::new("Anthropic (Claude)", "https://api.anthropic.com/v1", "claude-sonnet-4-20250514"),
        AiProvider::new("MiniMax", "https://api.minimax.io/v1", "MiniMax-M2.7"),
        AiProvider::new("Google AI Studio", "https://generativelanguage.googleapis.com/v1beta/openai", "gemini-2.5-flash"),
        AiProvider::new("Kimi (Moonshot)", "https://api.kimi.com/coding/v1", "kimi-k2-0905-preview"),
        AiProvider::new("DeepSeek", "https://api.deepseek.com/v1", "deepseek-chat"),
    ]
}

// ---------------------------------------------------------------------------
// SettingsView
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SettingsView {
    pub open: bool,
    pub cpu_cores: usize,
    pub memory_gb: u64,
    pub auto_start: bool,
    show_reset_alert: bool,
    selected_section: SettingsSection,

    // --- AI / API ---
    api_key: String,
    api_model: String,
    api_url: String,
    selected_provider: String,
    custom_providers: Vec<AiProvider>,
    show_add_provider: bool,
    new_provider_name: String,
    new_provider_url: String,
    new_provider_model: String,
    web_search_enabled: bool,
    web_search_api_key: String,
    api_needs_refresh: bool,
    api_status_msg: Option<String>,
    connection_status: ConnectionStatus,

    // --- Sub-Agent / Swarm ---
    sub_agent_enabled: bool,
    sub_agent_mode: String,
    sub_agent_model: String,
    agent_config_files: Vec<String>,
    selected_agent_config: String,

    // --- MCP Tools ---
    mcp_tools: Vec<McpTool>,
    new_mcp_name: String,
    new_mcp_url: String,
    new_mcp_headers: String,
    mcp_json_mode: bool,
    mcp_json_text: String,
    mcp_json_error: Option<String>,
    mcp_connection_status: Arc<Mutex<Vec<(String, bool, usize, Option<String>)>>>,

    // --- Remote Instances ---
    remote_enabled: bool,
    remote_token: String,
    remote_instances: Vec<RemoteInstance>,
    new_remote_name: String,
    new_remote_url: String,
    new_remote_token: String,

    // --- File Access Tokens ---
    file_tokens: Vec<FileToken>,
    file_tokens_loaded: bool,
    new_token_label: String,
    token_status_msg: Option<String>,

    // --- Local File Mounts ---
    local_file_mounts: Vec<LocalFileMount>,
    new_mount_label: String,
    new_mount_path: String,
    new_mount_permissions: String,

    // --- Security / Tool Approval ---
    approval_shell: bool,
    approval_python: bool,
    approval_file_write: bool,
    approval_file_delete: bool,
    approval_agent_spawn: bool,

    // --- Agent Harness ---
    agent_max_turns: u64,
    agent_max_tool_calls: u64,
    agent_max_tokens: u64,
    agent_temperature: f64,
    agent_max_context_tokens: u64,
    agent_max_consecutive_errors: u64,
    agent_compression_interval: u64,
    agent_reflection_enabled: bool,
    agent_reflection_threshold: f64,
    agent_max_reflection_retries: u64,
    agent_step_verify_enabled: bool,
    agent_step_verify_threshold: f64,
    agent_step_verify_max_retries: u64,
    agent_tool_result_max_len: u64,
    agent_wait_result_timeout: u64,
    agent_wait_result_hard_timeout: u64,
    agent_allow_unsandboxed_exec: bool,

    // --- Soul & Identity ---
    orchestrator_soul: String,
    orchestrator_identity: String,
    soul_section_open: bool,

    // --- Skill Auto-Update ---
    skill_auto_update_enabled: bool,
    skill_auto_update_interval_minutes: u64,
    skill_auto_update_max_candidates: u64,
    skill_auto_update_require_approval: bool,
    skill_auto_update_human_feedback_enabled: bool,

    // --- Plugins ---
    plugins: Vec<crate::server::services::plugin::InstalledPlugin>,
    plugins_loaded: bool,
    selected_plugin_id: Option<String>,
    plugin_readme_cache: HashMap<String, String>,
    plugin_status_msg: Option<String>,
    plugin_connector_configs: HashMap<String, HashMap<String, String>>,
    show_uninstall_confirm: Option<String>,
}

impl Default for SettingsView {
    fn default() -> Self {
        Self {
            open: false,
            cpu_cores: 4,
            memory_gb: 4,
            auto_start: false,
            show_reset_alert: false,
            selected_section: SettingsSection::General,

            api_key: String::new(),
            api_model: String::new(),
            api_url: String::new(),
            selected_provider: String::new(),
            custom_providers: Vec::new(),
            show_add_provider: false,
            new_provider_name: String::new(),
            new_provider_url: String::new(),
            new_provider_model: String::new(),
            web_search_enabled: false,
            web_search_api_key: String::new(),
            api_needs_refresh: true,
            api_status_msg: None,
            connection_status: ConnectionStatus::Idle,

            sub_agent_enabled: false,
            sub_agent_mode: "auto".to_string(),
            sub_agent_model: String::new(),
            agent_config_files: Vec::new(),
            selected_agent_config: String::new(),

            mcp_tools: Vec::new(),
            new_mcp_name: String::new(),
            new_mcp_url: String::new(),
            new_mcp_headers: String::new(),
            mcp_json_mode: false,
            mcp_json_text: String::new(),
            mcp_json_error: None,
            mcp_connection_status: Arc::new(Mutex::new(Vec::new())),

            remote_enabled: false,
            remote_token: String::new(),
            remote_instances: Vec::new(),
            new_remote_name: String::new(),
            new_remote_url: String::new(),
            new_remote_token: String::new(),

            file_tokens: Vec::new(),
            file_tokens_loaded: false,
            new_token_label: String::new(),
            token_status_msg: None,

            local_file_mounts: Vec::new(),
            new_mount_label: String::new(),
            new_mount_path: String::new(),
            new_mount_permissions: "read-only".to_string(),

            approval_shell: true,
            approval_python: true,
            approval_file_write: false,
            approval_file_delete: true,
            approval_agent_spawn: false,

            agent_max_turns: 15,
            agent_max_tool_calls: 25,
            agent_max_tokens: 81920,
            agent_temperature: 0.7,
            agent_max_context_tokens: 100_000,
            agent_max_consecutive_errors: 3,
            agent_compression_interval: 5,
            agent_reflection_enabled: false,
            agent_reflection_threshold: 0.7,
            agent_max_reflection_retries: 2,
            agent_step_verify_enabled: true,
            agent_step_verify_threshold: 0.7,
            agent_step_verify_max_retries: 1,
            agent_tool_result_max_len: 6000,
            agent_wait_result_timeout: 120,
            agent_wait_result_hard_timeout: 1800,
            agent_allow_unsandboxed_exec: false,

            orchestrator_soul: String::new(),
            orchestrator_identity: String::new(),
            soul_section_open: false,

            skill_auto_update_enabled: true,
            skill_auto_update_interval_minutes: 5,
            skill_auto_update_max_candidates: 10,
            skill_auto_update_require_approval: true,
            skill_auto_update_human_feedback_enabled: true,

            plugins: Vec::new(),
            plugins_loaded: false,
            selected_plugin_id: None,
            plugin_readme_cache: HashMap::new(),
            plugin_status_msg: None,
            plugin_connector_configs: HashMap::new(),
            show_uninstall_confirm: None,
        }
    }
}

impl SettingsView {
    // ------------------------------------------------------------------
    // Top-level show
    // ------------------------------------------------------------------

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        vm_manager: &Arc<VmManager>,
        runtime: &tokio::runtime::Handle,
    ) {
        if !self.open {
            return;
        }

        let mut still_open = self.open;

        egui::Window::new("Settings")
            .open(&mut still_open)
            .resizable(true)
            .default_size([780.0, 560.0])
            .show(ctx, |ui| {
                // Tab bar — plain horizontal wrap, no scroll area (avoids dark hover band)
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(251, 247, 241))
                    .inner_margin(egui::Margin::symmetric(0, 2))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let accent = egui::Color32::from_rgb(18, 154, 145);
                            for &section in SettingsSection::ALL {
                                let is_selected = self.selected_section == section;
                                let label = section.label();

                                let btn = egui::Button::new(
                                    egui::RichText::new(label)
                                        .size(12.0)
                                        .strong()
                                        .color(if is_selected {
                                            accent
                                        } else {
                                            egui::Color32::from_rgb(124, 115, 104)
                                        }),
                                )
                                .fill(if is_selected {
                                    egui::Color32::from_rgba_premultiplied(18, 154, 145, 20)
                                } else {
                                    egui::Color32::TRANSPARENT
                                })
                                .stroke(if is_selected {
                                    egui::Stroke::new(1.0, accent)
                                } else {
                                    egui::Stroke::NONE
                                })
                                .corner_radius(6.0)
                                .min_size(egui::vec2(0.0, 28.0));

                                if ui.add(btn).clicked() {
                                    self.selected_section = section;
                                }
                            }
                        });
                    });
                ui.separator();

                // Body
                egui::ScrollArea::vertical()
                    .id_salt("settings_body")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        match self.selected_section {
                            SettingsSection::General => self.section_general(ui, vm_manager, runtime),
                            SettingsSection::AI => self.section_ai(ui, ctx, runtime),
                            SettingsSection::SubAgent => self.section_sub_agent(ui, runtime),
                            SettingsSection::McpTools => self.section_mcp_tools(ui, runtime),
                            SettingsSection::Plugins => self.section_plugins(ui, ctx, runtime),
                            SettingsSection::Remote => self.section_remote(ui, runtime),
                            SettingsSection::FileTokens => {
                                self.section_file_tokens(ui, ctx, runtime)
                            }
                            SettingsSection::FileMounts => self.section_file_mounts(ui, runtime),
                            SettingsSection::SkillUpdate => self.section_skill_update(ui, runtime),
                            SettingsSection::Security => self.section_security(ui, runtime),
                            SettingsSection::About => Self::section_about(ui),
                        }
                    });
            });

        self.open = still_open;

        // Reset VM confirmation dialog
        if self.show_reset_alert {
            egui::Window::new("Reset VM?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("This will stop the VM and re-provision on next start.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_reset_alert = false;
                        }
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Reset")
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            ))
                            .clicked()
                        {
                            self.show_reset_alert = false;
                            let vm = vm_manager.clone();
                            runtime.spawn(async move {
                                vm.reset_vm().await;
                            });
                        }
                    });
                });
        }
    }

    // ------------------------------------------------------------------
    // Helper: load settings once
    // ------------------------------------------------------------------

    fn load_settings_if_needed(&mut self, runtime: &tokio::runtime::Handle) {
        if !self.api_needs_refresh {
            return;
        }
        self.api_needs_refresh = false;

        let settings = runtime.block_on(get_settings());

        // AI
        self.api_key = settings.tiger_bot_api_key;
        self.api_model = settings.tiger_bot_model;
        self.api_url = settings.tiger_bot_api_url.unwrap_or_default();
        self.web_search_enabled = settings.web_search_enabled;
        self.web_search_api_key = settings.web_search_api_key.unwrap_or_default();

        // Sub-Agent
        self.sub_agent_enabled = settings.sub_agent_enabled.unwrap_or(false);
        self.sub_agent_mode = settings.sub_agent_mode.unwrap_or_else(|| "auto".into());
        self.sub_agent_model = settings.sub_agent_model.unwrap_or_default();
        self.selected_agent_config = settings.sub_agent_config_file.unwrap_or_default();

        // Scan for agent config YAML files (local or remote)
        if crate::server::data::get_remote_backend().is_some() {
            self.agent_config_files = Self::scan_agent_configs_remote();
        } else {
            self.agent_config_files = Self::scan_agent_configs();
        }

        // MCP Tools
        self.mcp_tools = settings.mcp_tools;

        // Remote
        self.remote_enabled = settings.remote_enabled.unwrap_or(false);
        self.remote_token = settings.remote_token.unwrap_or_default();
        self.remote_instances = settings.remote_instances.unwrap_or_default();

        // File Mounts
        self.local_file_mounts = settings.local_file_mounts.unwrap_or_default();

        // Security / Tool Approval
        self.approval_shell = settings.approval_required_for_shell.unwrap_or(true);
        self.approval_python = settings.approval_required_for_python.unwrap_or(true);
        self.approval_file_write = settings.approval_required_for_file_write.unwrap_or(false);
        self.approval_file_delete = settings.approval_required_for_file_delete.unwrap_or(true);
        self.approval_agent_spawn = settings.approval_required_for_agent_spawn.unwrap_or(false);

        // Agent Harness (stored in extra map)
        self.agent_max_turns = settings.extra.get("agentMaxToolRounds")
            .and_then(|v| v.as_u64()).unwrap_or(15);
        self.agent_max_tool_calls = settings.extra.get("agentMaxToolCalls")
            .and_then(|v| v.as_u64()).unwrap_or(25);
        self.agent_max_tokens = settings.extra.get("agentMaxTokens")
            .and_then(|v| v.as_u64()).unwrap_or(81920);
        self.agent_temperature = settings.extra.get("agentTemperature")
            .and_then(|v| v.as_f64()).unwrap_or(0.7);
        self.agent_max_context_tokens = settings.extra.get("agentMaxContextTokens")
            .and_then(|v| v.as_u64()).unwrap_or(100_000);
        self.agent_max_consecutive_errors = settings.extra.get("agentMaxConsecutiveErrors")
            .and_then(|v| v.as_u64()).unwrap_or(3);
        self.agent_compression_interval = settings.extra.get("agentCompressionInterval")
            .and_then(|v| v.as_u64()).unwrap_or(5);
        self.agent_reflection_enabled = settings.extra.get("agentReflectionEnabled")
            .and_then(|v| v.as_bool()).unwrap_or(false);
        self.agent_reflection_threshold = settings.extra.get("agentReflectionThreshold")
            .and_then(|v| v.as_f64()).unwrap_or(0.7);
        self.agent_max_reflection_retries = settings.extra.get("agentMaxReflectionRetries")
            .and_then(|v| v.as_u64()).unwrap_or(2);
        self.agent_step_verify_enabled = settings.extra.get("agentStepVerifyEnabled")
            .and_then(|v| v.as_bool()).unwrap_or(true);
        self.agent_step_verify_threshold = settings.extra.get("agentStepVerifyThreshold")
            .and_then(|v| v.as_f64()).unwrap_or(0.7);
        self.agent_step_verify_max_retries = settings.extra.get("agentStepVerifyMaxRetries")
            .and_then(|v| v.as_u64()).unwrap_or(1);
        self.agent_tool_result_max_len = settings.extra.get("agentToolResultMaxLen")
            .and_then(|v| v.as_u64()).unwrap_or(6000);
        self.agent_wait_result_timeout = settings.extra.get("agentWaitResultTimeout")
            .and_then(|v| v.as_u64()).unwrap_or(120);
        self.agent_wait_result_hard_timeout = settings.extra.get("agentWaitResultHardTimeout")
            .and_then(|v| v.as_u64()).unwrap_or(1800);
        self.agent_allow_unsandboxed_exec = settings.extra.get("agentAllowUnsandboxedExec")
            .and_then(|v| v.as_bool()).unwrap_or(false);

        // Soul & Identity (stored in SOUL.md / IDENTITY.md files).
        // When a remote backend is connected, edit the REMOTE server's persona
        // files — that's the orchestrator actually answering the chats.
        if crate::server::data::get_remote_backend().is_some() {
            let (soul, identity) = Self::fetch_soul_identity_remote();
            self.orchestrator_soul = soul;
            self.orchestrator_identity = identity;
        } else {
            let data_dir = crate::server::data::data_dir();
            self.orchestrator_soul = std::fs::read_to_string(data_dir.join("SOUL.md")).unwrap_or_default();
            self.orchestrator_identity = std::fs::read_to_string(data_dir.join("IDENTITY.md")).unwrap_or_default();
        }

        // Skill Auto-Update
        self.skill_auto_update_enabled = settings.skill_auto_update_enabled.unwrap_or(true);
        self.skill_auto_update_interval_minutes =
            settings.skill_auto_update_interval_minutes.unwrap_or(5);
        self.skill_auto_update_max_candidates =
            settings.skill_auto_update_max_candidates.unwrap_or(10);
        self.skill_auto_update_require_approval =
            settings.skill_auto_update_require_approval.unwrap_or(true);
        self.skill_auto_update_human_feedback_enabled = settings
            .skill_auto_update_human_feedback_enabled
            .unwrap_or(true);

        // File tokens (separate file)
        self.file_tokens = runtime.block_on(get_file_tokens());
        self.file_tokens_loaded = true;
    }

    /// Collect the current UI state back into a Settings struct and save.
    fn save_all_settings(&mut self, runtime: &tokio::runtime::Handle) {
        let mut settings = runtime.block_on(get_settings());

        // AI
        settings.tiger_bot_api_key = self.api_key.clone();
        settings.tiger_bot_model = self.api_model.clone();
        settings.tiger_bot_api_url = if self.api_url.is_empty() {
            None
        } else {
            Some(self.api_url.clone())
        };
        settings.web_search_enabled = self.web_search_enabled;
        settings.web_search_api_key = if self.web_search_api_key.is_empty() {
            None
        } else {
            Some(self.web_search_api_key.clone())
        };

        // Sub-Agent
        settings.sub_agent_enabled = Some(self.sub_agent_enabled);
        settings.sub_agent_mode = Some(self.sub_agent_mode.clone());
        settings.sub_agent_model = if self.sub_agent_model.is_empty() {
            None
        } else {
            Some(self.sub_agent_model.clone())
        };
        settings.sub_agent_config_file = if self.selected_agent_config.is_empty() {
            None
        } else {
            Some(self.selected_agent_config.clone())
        };

        // MCP Tools
        settings.mcp_tools = self.mcp_tools.clone();

        // Reconnect MCP servers after saving
        let mcp_status = self.mcp_connection_status.clone();
        runtime.spawn(async move {
            use crate::server::services::mcp;
            mcp::disconnect_all().await;
            mcp::init_mcp_servers().await;
            let status = mcp::get_connection_status().await;
            *mcp_status.lock().unwrap() = status;
        });

        // Remote
        settings.remote_enabled = Some(self.remote_enabled);
        settings.remote_token = if self.remote_token.is_empty() {
            None
        } else {
            Some(self.remote_token.clone())
        };
        settings.remote_instances = if self.remote_instances.is_empty() {
            None
        } else {
            Some(self.remote_instances.clone())
        };

        // File Mounts
        settings.local_file_mounts = if self.local_file_mounts.is_empty() {
            None
        } else {
            Some(self.local_file_mounts.clone())
        };

        // Security / Tool Approval
        settings.approval_required_for_shell = Some(self.approval_shell);
        settings.approval_required_for_python = Some(self.approval_python);
        settings.approval_required_for_file_write = Some(self.approval_file_write);
        settings.approval_required_for_file_delete = Some(self.approval_file_delete);
        settings.approval_required_for_agent_spawn = Some(self.approval_agent_spawn);

        // Agent Harness (stored in extra map)
        settings.extra.insert("agentMaxToolRounds".into(), serde_json::json!(self.agent_max_turns));
        settings.extra.insert("agentMaxToolCalls".into(), serde_json::json!(self.agent_max_tool_calls));
        settings.extra.insert("agentMaxTokens".into(), serde_json::json!(self.agent_max_tokens));
        settings.extra.insert("agentTemperature".into(), serde_json::json!(self.agent_temperature));
        settings.extra.insert("agentMaxContextTokens".into(), serde_json::json!(self.agent_max_context_tokens));
        settings.extra.insert("agentMaxConsecutiveErrors".into(), serde_json::json!(self.agent_max_consecutive_errors));
        settings.extra.insert("agentCompressionInterval".into(), serde_json::json!(self.agent_compression_interval));
        settings.extra.insert("agentReflectionEnabled".into(), serde_json::json!(self.agent_reflection_enabled));
        settings.extra.insert("agentReflectionThreshold".into(), serde_json::json!(self.agent_reflection_threshold));
        settings.extra.insert("agentMaxReflectionRetries".into(), serde_json::json!(self.agent_max_reflection_retries));
        settings.extra.insert("agentStepVerifyEnabled".into(), serde_json::json!(self.agent_step_verify_enabled));
        settings.extra.insert("agentStepVerifyThreshold".into(), serde_json::json!(self.agent_step_verify_threshold));
        settings.extra.insert("agentStepVerifyMaxRetries".into(), serde_json::json!(self.agent_step_verify_max_retries));
        settings.extra.insert("agentToolResultMaxLen".into(), serde_json::json!(self.agent_tool_result_max_len));
        settings.extra.insert("agentWaitResultTimeout".into(), serde_json::json!(self.agent_wait_result_timeout));
        settings.extra.insert("agentWaitResultHardTimeout".into(), serde_json::json!(self.agent_wait_result_hard_timeout));
        settings.extra.insert("agentAllowUnsandboxedExec".into(), serde_json::json!(self.agent_allow_unsandboxed_exec));

        // Soul & Identity (saved to SOUL.md / IDENTITY.md files — pushed to
        // the remote server when one is connected, since that orchestrator
        // is the one answering the chats)
        if crate::server::data::get_remote_backend().is_some() {
            Self::push_soul_identity_remote(&self.orchestrator_soul, &self.orchestrator_identity);
        } else {
            let data_dir = crate::server::data::data_dir();
            if self.orchestrator_soul.is_empty() {
                let _ = std::fs::remove_file(data_dir.join("SOUL.md"));
            } else {
                let _ = std::fs::write(data_dir.join("SOUL.md"), &self.orchestrator_soul);
            }
            if self.orchestrator_identity.is_empty() {
                let _ = std::fs::remove_file(data_dir.join("IDENTITY.md"));
            } else {
                let _ = std::fs::write(data_dir.join("IDENTITY.md"), &self.orchestrator_identity);
            }
        }

        // Skill Auto-Update
        settings.skill_auto_update_enabled = Some(self.skill_auto_update_enabled);
        settings.skill_auto_update_interval_minutes =
            Some(self.skill_auto_update_interval_minutes);
        settings.skill_auto_update_max_candidates =
            Some(self.skill_auto_update_max_candidates);
        settings.skill_auto_update_require_approval =
            Some(self.skill_auto_update_require_approval);
        settings.skill_auto_update_human_feedback_enabled =
            Some(self.skill_auto_update_human_feedback_enabled);

        runtime.block_on(save_settings(&settings));
    }

    fn scan_agent_configs() -> Vec<String> {
        let dir = crate::server::data::data_dir().join("agents");
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".yaml") || name.ends_with(".yml") {
                    files.push(name);
                }
            }
        }
        files.sort();
        files
    }

    /// Fetch SOUL.md / IDENTITY.md content from the connected remote server.
    fn fetch_soul_identity_remote() -> (String, String) {
        let Some(rb) = crate::server::data::get_remote_backend() else {
            return (String::new(), String::new());
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        match client
            .get(format!("{}/api/settings/soul-identity", rb.url))
            .bearer_auth(&rb.token)
            .send()
        {
            Ok(resp) => match resp.json::<serde_json::Value>() {
                Ok(val) => (
                    val["soul"].as_str().unwrap_or_default().to_string(),
                    val["identity"].as_str().unwrap_or_default().to_string(),
                ),
                Err(_) => (String::new(), String::new()),
            },
            Err(_) => (String::new(), String::new()),
        }
    }

    /// Push SOUL.md / IDENTITY.md content to the connected remote server.
    fn push_soul_identity_remote(soul: &str, identity: &str) {
        let Some(rb) = crate::server::data::get_remote_backend() else { return };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        let _ = client
            .put(format!("{}/api/settings/soul-identity", rb.url))
            .bearer_auth(&rb.token)
            .json(&serde_json::json!({ "soul": soul, "identity": identity }))
            .send();
    }

    fn scan_agent_configs_remote() -> Vec<String> {
        let Some(rb) = crate::server::data::get_remote_backend() else {
            return Vec::new();
        };
        let url = format!("{}/api/chat/agent-configs", rb.url);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        match client.get(&url).bearer_auth(&rb.token).send() {
            Ok(resp) => {
                if let Ok(val) = resp.json::<serde_json::Value>() {
                    if let Some(arr) = val["files"].as_array() {
                        return arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                    }
                }
                Vec::new()
            }
            Err(_) => Vec::new(),
        }
    }

    // ------------------------------------------------------------------
    // Helper: green save button
    // ------------------------------------------------------------------

    fn save_button(ui: &mut egui::Ui, label: &str) -> bool {
        ui.add(
            egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
                .fill(egui::Color32::from_rgb(34, 197, 94)),
        )
        .clicked()
    }

    fn status_label(ui: &mut egui::Ui, msg: &Option<String>) {
        if let Some(m) = msg {
            let color = if m.contains("Error") || m.contains("error") || m.contains("fail") {
                egui::Color32::from_rgb(239, 68, 68)
            } else {
                egui::Color32::from_rgb(34, 197, 94)
            };
            ui.label(egui::RichText::new(m).size(12.0).color(color));
        }
    }

    // ==================================================================
    //  1. General
    // ==================================================================

    fn section_general(&mut self, ui: &mut egui::Ui, vm_manager: &Arc<VmManager>, runtime: &tokio::runtime::Handle) {
        let storage_path = VmConfig::app_support_dir().to_string_lossy().to_string();
        let disk_usage = VmConfig::disk_usage();
        let disk_size_gb = VmConfig::DISK_SIZE_GB;

        // VM Control
        ui.add_space(8.0);
        ui.heading("VM Control");
        ui.add_space(4.0);
        {
            let vm = vm_manager.clone();
            let state = runtime.block_on(vm.state());
            let service_ready = runtime.block_on(vm.service_ready());
            let vm_color = match state {
                VmState::Running if service_ready => egui::Color32::from_rgb(34, 197, 94),
                VmState::Running => egui::Color32::from_rgb(250, 204, 21),
                VmState::Error => egui::Color32::from_rgb(239, 68, 68),
                VmState::Stopped => egui::Color32::from_rgb(168, 158, 144),
                _ => egui::Color32::from_rgb(18, 154, 145),
            };
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 5.0, vm_color);
                ui.label(egui::RichText::new(format!("Status: {}", state.label())).size(13.0));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if state == VmState::Stopped || state == VmState::Error {
                    let btn = egui::Button::new(egui::RichText::new("\u{25B6} Start VM").color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(34, 197, 94)).corner_radius(6.0);
                    if ui.add(btn).clicked() {
                        let mgr = vm_manager.clone();
                        runtime.spawn(async move { let _ = mgr.start_vm().await; });
                    }
                } else if state == VmState::Running {
                    let btn = egui::Button::new(egui::RichText::new("\u{25A0} Stop VM").color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(239, 68, 68)).corner_radius(6.0);
                    if ui.add(btn).clicked() {
                        let mgr = vm_manager.clone();
                        runtime.spawn(async move { mgr.stop_vm().await; });
                    }
                } else {
                    ui.spinner();
                    ui.label(egui::RichText::new(state.label()).size(12.0).color(egui::Color32::GRAY));
                }
                ui.add_space(8.0);
                let reset_btn = egui::Button::new(egui::RichText::new("Reset VM").size(12.0))
                    .fill(egui::Color32::from_rgb(250, 240, 240)).corner_radius(6.0);
                if ui.add(reset_btn).on_hover_text("Stop and delete VM disk. Will re-download on next start.").clicked() {
                    let mgr = vm_manager.clone();
                    runtime.spawn(async move { mgr.reset_vm().await; });
                }
            });
            if state == VmState::Downloading {
                ui.add_space(4.0);
                let progress = runtime.block_on(vm_manager.progress());
                ui.add(egui::ProgressBar::new(progress as f32).show_percentage().animate(true));
            }
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("VM Resources");
        ui.add_space(4.0);

        egui::Grid::new("vm_resources_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("CPU Cores:");
                let max_cores = num_cpus::get().max(1);
                ui.add(egui::Slider::new(&mut self.cpu_cores, 1..=max_cores).text("cores"));
                ui.end_row();

                ui.label("Memory:");
                egui::ComboBox::from_id_salt("mem_picker")
                    .selected_text(format!("{} GB", self.memory_gb))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.memory_gb, 2, "2 GB");
                        ui.selectable_value(&mut self.memory_gb, 4, "4 GB");
                        ui.selectable_value(&mut self.memory_gb, 8, "8 GB");
                    });
                ui.end_row();
            });

        ui.label(
            egui::RichText::new("Changes take effect after VM restart")
                .size(11.0)
                .color(egui::Color32::GRAY),
        );

        ui.add_space(16.0);
        ui.separator();
        ui.heading("VM Storage");
        ui.add_space(4.0);

        egui::Grid::new("vm_storage_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Location:");
                ui.label(
                    egui::RichText::new(&storage_path)
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                ui.end_row();
                ui.label("Disk Usage:");
                ui.label(
                    egui::RichText::new(&disk_usage)
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                ui.end_row();
                ui.label("Max Disk Size:");
                ui.label(
                    egui::RichText::new(format!("{} GB", disk_size_gb))
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                ui.end_row();
            });

        ui.add_space(16.0);
        ui.separator();
        ui.heading("Startup");
        ui.checkbox(&mut self.auto_start, "Start VM automatically on launch");

        ui.add_space(16.0);
        ui.separator();
        ui.heading("Maintenance");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("Reset VM").color(egui::Color32::from_rgb(239, 68, 68)),
                ))
                .clicked()
            {
                self.show_reset_alert = true;
            }
            if ui.button("Open Storage Folder").clicked() {
                let _ = open::that(&storage_path);
            }
        });
    }

    // ==================================================================
    //  2. AI / API
    // ==================================================================

    fn section_ai(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        runtime: &tokio::runtime::Handle,
    ) {
        self.load_settings_if_needed(runtime);

        ui.add_space(8.0);
        ui.heading("AI Provider");
        ui.add_space(4.0);

        // Build combined provider list: builtins + custom
        let all_providers: Vec<AiProvider> = builtin_providers()
            .into_iter()
            .chain(self.custom_providers.clone())
            .collect();
        let provider_names: Vec<String> = all_providers.iter().map(|p| p.name.clone()).collect();

        egui::Grid::new("ai_api_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                // Provider selector
                ui.label("Provider:");
                ui.horizontal(|ui| {
                    let prev_provider = self.selected_provider.clone();
                    egui::ComboBox::from_id_salt("ai_provider_picker")
                        .selected_text(if self.selected_provider.is_empty() {
                            "Select provider..."
                        } else {
                            &self.selected_provider
                        })
                        .width(250.0)
                        .show_ui(ui, |ui| {
                            for name in &provider_names {
                                ui.selectable_value(
                                    &mut self.selected_provider,
                                    name.clone(),
                                    name.as_str(),
                                );
                            }
                        });

                    // Auto-fill URL and model when provider changes
                    if self.selected_provider != prev_provider && !self.selected_provider.is_empty() {
                        if let Some(p) = all_providers.iter().find(|p| p.name == self.selected_provider) {
                            self.api_url = p.api_url.clone();
                            self.api_model = p.default_model.clone();
                        }
                    }

                    if ui.small_button("+ Add").clicked() {
                        self.show_add_provider = !self.show_add_provider;
                    }
                });
                ui.end_row();

                let is_local_cli = self.api_url == "claude-code" || self.api_url == "codex-cli" || self.api_url == "gemini-cli";

                if !is_local_cli {
                    // API Key
                    ui.label("API Key:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.api_key)
                            .password(true)
                            .desired_width(300.0),
                    );
                    ui.end_row();

                    // API URL (auto-filled by provider, but editable)
                    ui.label("API URL:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.api_url)
                            .desired_width(300.0)
                            .hint_text("https://api.deepseek.com/v1"),
                    );
                    ui.end_row();
                }

                // Model (auto-filled by provider, but editable)
                ui.label("Model:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.api_model)
                        .desired_width(300.0)
                        .hint_text(if is_local_cli { "e.g. claude-sonnet-4-20250514 or o4-mini" } else { "e.g. deepseek-chat" }),
                );
                ui.end_row();
            });

        // Add custom provider panel
        if self.show_add_provider {
            ui.add_space(8.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(egui::RichText::new("Add Custom Provider").strong());
                ui.add_space(4.0);
                egui::Grid::new("add_provider_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Name:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_provider_name)
                                .desired_width(250.0)
                                .hint_text("e.g. My Provider"),
                        );
                        ui.end_row();

                        ui.label("API URL:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_provider_url)
                                .desired_width(250.0)
                                .hint_text("https://api.example.com/v1"),
                        );
                        ui.end_row();

                        ui.label("Default Model:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_provider_model)
                                .desired_width(250.0)
                                .hint_text("model-name"),
                        );
                        ui.end_row();
                    });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Add Provider").clicked()
                        && !self.new_provider_name.is_empty()
                        && !self.new_provider_url.is_empty()
                    {
                        self.custom_providers.push(AiProvider::new(
                            &self.new_provider_name,
                            &self.new_provider_url,
                            &self.new_provider_model,
                        ));
                        self.selected_provider = self.new_provider_name.clone();
                        self.api_url = self.new_provider_url.clone();
                        self.api_model = self.new_provider_model.clone();
                        self.new_provider_name.clear();
                        self.new_provider_url.clear();
                        self.new_provider_model.clear();
                        self.show_add_provider = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_add_provider = false;
                    }
                });
            });
        }

        // Remove custom provider button (only for custom ones)
        if !self.selected_provider.is_empty() {
            let is_custom = self.custom_providers.iter().any(|p| p.name == self.selected_provider);
            if is_custom {
                ui.add_space(4.0);
                if ui.small_button("Remove custom provider").clicked() {
                    self.custom_providers.retain(|p| p.name != self.selected_provider);
                    self.selected_provider.clear();
                }
            }
        }

        // Test Connection button
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let is_cc = self.api_url == "claude-code";
            let is_codex = self.api_url == "codex-cli";
            let is_gemini = self.api_url == "gemini-cli";
            let is_local_cli = is_cc || is_codex || is_gemini;
            if ui.button("Test Connection").clicked() && (is_local_cli || !self.api_key.is_empty()) {
                let api_key = self.api_key.clone();
                let raw_url = if self.api_url.is_empty() {
                    "https://api.deepseek.com/v1".to_string()
                } else {
                    self.api_url.clone()
                };
                let model = if self.api_model.is_empty() {
                    "deepseek-chat".to_string()
                } else {
                    self.api_model.clone()
                };
                let ctx_clone = ctx.clone();

                let result = if is_local_cli {
                    // Test local CLI
                    let env_path = crate::server::services::toolbox::cli_env_path();
                    let home = crate::server::services::toolbox::resolve_home();
                    if is_gemini {
                        // Gemini CLI: just run `gemini --version`
                        runtime.block_on(async {
                            let output = tokio::process::Command::new("gemini")
                                .arg("--version")
                                .env("PATH", &env_path)
                                .env("HOME", &home)
                                .output()
                                .await;
                            match output {
                                Ok(o) if o.status.success() => {
                                    let ver = String::from_utf8_lossy(&o.stdout).trim().to_string();
                                    Ok(format!("Gemini CLI found: {}", ver))
                                }
                                Ok(o) => Err(format!("CLI error: {}", String::from_utf8_lossy(&o.stderr).trim())),
                                Err(e) => Err(format!("Gemini CLI not found: {}. Install with: npm i -g @anthropic-ai/gemini-cli", e)),
                            }
                        })
                    } else {
                        let (node_bin, script_path) = if is_cc {
                            crate::server::services::toolbox::find_claude_cli()
                        } else {
                            crate::server::services::toolbox::find_codex_cli()
                        };
                        let label = if is_cc { "Claude Code" } else { "Codex" };
                        runtime.block_on(async {
                            let mut args: Vec<String> = Vec::new();
                            if !script_path.is_empty() {
                                args.push(script_path);
                            }
                            args.push("--version".to_string());
                            let output = tokio::process::Command::new(&node_bin)
                                .args(&args)
                                .env("PATH", &env_path)
                                .env("HOME", &home)
                                .output()
                                .await;
                            match output {
                                Ok(o) if o.status.success() => {
                                    let ver = String::from_utf8_lossy(&o.stdout).trim().to_string();
                                    Ok(format!("{} CLI found: {}", label, ver))
                                }
                                Ok(o) => Err(format!("CLI error: {}", String::from_utf8_lossy(&o.stderr).trim())),
                                Err(e) => Err(format!("{} CLI not found: {}", label, e)),
                            }
                        })
                    }
                } else {
                    // Test HTTP API
                    let api_url = if raw_url.ends_with("/chat/completions") {
                        raw_url
                    } else {
                        format!("{}/chat/completions", raw_url.trim_end_matches('/'))
                    };
                    runtime.block_on(async {
                        let builder = reqwest::Client::new()
                            .post(&api_url)
                            .header("Authorization", format!("Bearer {}", api_key))
                            .header("content-type", "application/json")
                            .header("User-Agent", "claude-code/1.0.6")
                            .header("X-Client-Name", "claude-code")
                            .header("X-Client-Version", "1.0.6")
                            .header("HTTP-Referer", "https://claude.ai")
                            .header("X-Traffic-Source", "claude-code");
                        let resp = builder
                            .json(&serde_json::json!({
                                "model": model,
                                "max_tokens": 1,
                                "messages": [{"role": "user", "content": "hi"}]
                            }))
                            .send()
                            .await;
                        match resp {
                            Ok(r) => {
                                let status = r.status();
                                if status.is_success() {
                                    Ok(format!("Connected (HTTP {})", status.as_u16()))
                                } else {
                                    let body = r.text().await.unwrap_or_default();
                                    Err(format!("HTTP {} - {}", status.as_u16(), body.chars().take(200).collect::<String>()))
                                }
                            }
                            Err(e) => Err(format!("Request failed: {}", e)),
                        }
                    })
                };

                self.connection_status = match result {
                    Ok(msg) => ConnectionStatus::Success(msg),
                    Err(msg) => ConnectionStatus::Error(msg),
                };
                ctx_clone.request_repaint();
            }

            match &self.connection_status {
                ConnectionStatus::Idle => {}
                ConnectionStatus::Testing => {
                    ui.spinner();
                    ui.label("Testing...");
                }
                ConnectionStatus::Success(msg) => {
                    ui.label(
                        egui::RichText::new(format!("[OK] {}", msg))
                            .color(egui::Color32::from_rgb(34, 197, 94)),
                    );
                }
                ConnectionStatus::Error(msg) => {
                    ui.label(
                        egui::RichText::new(format!("[Error] {}", msg))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    );
                }
            }
        });

        ui.add_space(16.0);
        ui.separator();
        ui.heading("Web Search");
        ui.add_space(4.0);

        ui.checkbox(&mut self.web_search_enabled, "Enable web search");

        if self.web_search_enabled {
            ui.horizontal(|ui| {
                ui.label("Search API Key:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.web_search_api_key)
                        .password(true)
                        .desired_width(260.0),
                );
            });
        }

        ui.add_space(16.0);
        ui.separator();

        // --- Soul & Identity ---
        let soul_header = if !self.orchestrator_soul.is_empty() || !self.orchestrator_identity.is_empty() {
            "Orchestrator Soul & Identity  (configured)"
        } else {
            "Orchestrator Soul & Identity"
        };
        if ui.add(egui::Label::new(
            egui::RichText::new(format!("{} {}", if self.soul_section_open { "\u{25BC}" } else { "\u{25B6}" }, soul_header))
                .heading()
        ).sense(egui::Sense::click())).clicked() {
            self.soul_section_open = !self.soul_section_open;
        }

        if !self.soul_section_open {
            ui.label(
                egui::RichText::new("Configure the orchestrator's internal cognition (SOUL.md) and external presentation (IDENTITY.md). Injected into the system prompt.")
                    .size(11.0).color(egui::Color32::GRAY),
            );
        }

        if self.soul_section_open {
            ui.add_space(4.0);
            if crate::server::data::get_remote_backend().is_some() {
                ui.label(
                    egui::RichText::new("🌐 Connected to a remote server — these edits are saved to the REMOTE orchestrator's SOUL.md / IDENTITY.md.")
                        .size(11.0).color(egui::Color32::from_rgb(45, 140, 130)),
                );
                ui.add_space(4.0);
            }
            ui.label(egui::RichText::new("SOUL.md — Internal Cognition, Values & Behavior").strong());
            ui.add(
                egui::TextEdit::multiline(&mut self.orchestrator_soul)
                    .desired_width(f32::INFINITY)
                    .desired_rows(8)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("Define the orchestrator's internal cognition:\n- Core values and principles\n- Decision-making heuristics\n- Behavioral priors and communication style\n- Ethical boundaries\n- How to handle ambiguity\n\nThis is injected as a behavioral prior — it directly shapes model outputs."),
            );
            ui.label(
                egui::RichText::new(format!("{} chars — Directly affects model outputs.", self.orchestrator_soul.len()))
                    .size(10.0).color(egui::Color32::GRAY),
            );

            ui.add_space(8.0);
            ui.label(egui::RichText::new("IDENTITY.md — External Presentation").strong());
            ui.add(
                egui::TextEdit::multiline(&mut self.orchestrator_identity)
                    .desired_width(f32::INFINITY)
                    .desired_rows(3)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("Display name, avatar description, external persona.\nTypically static — used for image generation and display."),
            );
            ui.label(
                egui::RichText::new(format!("{} chars — Affects display name, avatar, image generation.", self.orchestrator_identity.len()))
                    .size(10.0).color(egui::Color32::GRAY),
            );

            ui.add_space(4.0);
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(239, 231, 218))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    let tc = egui::Color32::from_rgb(52, 48, 42);
                    ui.label(egui::RichText::new("Key Distinction:").strong().size(11.0).color(tc));
                    ui.add_space(4.0);
                    egui::Grid::new("soul_identity_table")
                        .num_columns(3)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Dimension").strong().size(11.0).color(tc));
                            ui.label(egui::RichText::new("SOUL.md").strong().size(11.0).color(tc));
                            ui.label(egui::RichText::new("IDENTITY.md").strong().size(11.0).color(tc));
                            ui.end_row();
                            ui.label(egui::RichText::new("Concern").size(11.0).color(tc));
                            ui.label(egui::RichText::new("Internal cognition, values").size(11.0).color(tc));
                            ui.label(egui::RichText::new("External name, avatar").size(11.0).color(tc));
                            ui.end_row();
                            ui.label(egui::RichText::new("Affects outputs").size(11.0).color(tc));
                            ui.label(egui::RichText::new("Directly (behavioral prior)").size(11.0).color(tc));
                            ui.label(egui::RichText::new("Indirectly (display)").size(11.0).color(tc));
                            ui.end_row();
                        });
                });
        }

        ui.add_space(16.0);
        ui.separator();
        ui.heading("Agent Harness");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Controls for the autonomous tool loop (fully_auto / auto modes)")
                .size(11.0)
                .color(egui::Color32::GRAY),
        );
        ui.add_space(4.0);

        egui::Grid::new("agent_harness_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Max Turns:");
                ui.add(egui::Slider::new(&mut self.agent_max_turns, 1..=100).text("rounds"));
                ui.end_row();

                ui.label("Max Tool Calls:");
                ui.add(egui::Slider::new(&mut self.agent_max_tool_calls, 1..=200).text("calls"));
                ui.end_row();

                ui.label("Max Output Tokens:");
                ui.add(egui::DragValue::new(&mut self.agent_max_tokens).range(1024..=131072).speed(1024));
                ui.end_row();

                ui.label("Temperature:");
                ui.add(egui::Slider::new(&mut self.agent_temperature, 0.0..=2.0).step_by(0.05));
                ui.end_row();

                ui.label("Max Context Tokens:");
                ui.add(egui::DragValue::new(&mut self.agent_max_context_tokens).range(4096..=1_000_000).speed(1000));
                ui.end_row();

                ui.label("Max Consecutive Errors:");
                ui.add(egui::Slider::new(&mut self.agent_max_consecutive_errors, 1..=10));
                ui.end_row();

                ui.label("Compression Interval:");
                ui.add(egui::Slider::new(&mut self.agent_compression_interval, 1..=20).text("rounds"));
                ui.end_row();

                ui.label("Tool Result Max Length:");
                ui.add(egui::DragValue::new(&mut self.agent_tool_result_max_len).range(1000..=100_000).speed(500));
                ui.end_row();

                ui.label("Wait Result Timeout:");
                ui.add(egui::Slider::new(&mut self.agent_wait_result_timeout, 30..=1800).text("sec"));
                ui.end_row();

                ui.label("Wait Result Hard Limit:")
                    .on_hover_text("Maximum time one wait_result call blocks (with internal auto-retries) \
                        before returning control to the calling agent. After 2 consecutive hard timeouts \
                        the caller is told to stop waiting and assemble partial results.");
                ui.add(egui::Slider::new(&mut self.agent_wait_result_hard_timeout, 300..=7200).text("sec"));
                ui.end_row();
            });

        ui.add_space(4.0);
        ui.checkbox(&mut self.agent_allow_unsandboxed_exec,
            "Allow UNSANDBOXED execution fallback (not recommended)")
            .on_hover_text("When the Apple container CLI and sandbox-exec are both unavailable, \
                run agent shell/python commands directly on the host with no sandbox. \
                Off (default): such commands fail with an error instead of escaping the sandbox.");

        ui.add_space(4.0);
        ui.checkbox(&mut self.agent_reflection_enabled, "Enable self-reflection / evaluation")
            .on_hover_text("After the agent finishes, a judge scores the answer against the user's objective. \
                Below the threshold, the agent re-enters the loop to fix the gaps.");
        if self.agent_reflection_enabled {
            egui::Grid::new("reflection_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Reflection Threshold:");
                    ui.add(egui::Slider::new(&mut self.agent_reflection_threshold, 0.1..=1.0)
                        .text("score"))
                        .on_hover_text("Answers scoring below this (0-1) trigger a gap-fixing retry. Higher = stricter.");
                    ui.end_row();

                    ui.label("Max Reflection Retries:");
                    ui.add(egui::Slider::new(&mut self.agent_max_reflection_retries, 1..=5)
                        .text("rounds"))
                        .on_hover_text("How many judge-and-fix cycles to run before accepting the answer.");
                    ui.end_row();
                });
        }

        ui.add_space(4.0);
        ui.checkbox(&mut self.agent_step_verify_enabled, "Verify each agent step result")
            .on_hover_text("In multi-agent runs, a judge scores every agent's result against its \
                assigned task as soon as it finishes. Failing results are retried with feedback \
                before being delivered to the orchestrator.");
        if self.agent_step_verify_enabled {
            egui::Grid::new("step_verify_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Step Verify Threshold:");
                    ui.add(egui::Slider::new(&mut self.agent_step_verify_threshold, 0.1..=1.0)
                        .text("score"))
                        .on_hover_text("Step results scoring below this (0-1) trigger a retry. Higher = stricter.");
                    ui.end_row();

                    ui.label("Max Step Retries:");
                    ui.add(egui::Slider::new(&mut self.agent_step_verify_max_retries, 0..=3)
                        .text("retries"))
                        .on_hover_text("How many times a failing agent retries with the judge's feedback. \
                            0 = judge only (no retry), verdict still reported to the orchestrator.");
                    ui.end_row();
                });
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if Self::save_button(ui, "Save API Settings") {
                self.save_all_settings(runtime);
                self.api_status_msg = Some("Settings saved!".to_string());
            }
            Self::status_label(ui, &self.api_status_msg);
        });
    }

    // ==================================================================
    //  3. Sub-Agent / Swarm
    // ==================================================================

    fn section_sub_agent(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);

        ui.add_space(8.0);
        ui.heading("Sub-Agent / Swarm Configuration");
        ui.add_space(4.0);

        ui.checkbox(&mut self.sub_agent_enabled, "Enable Sub-Agent system");

        ui.add_space(8.0);

        egui::Grid::new("sub_agent_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("Mode:");
                egui::ComboBox::from_id_salt("sub_agent_mode_picker")
                    .selected_text(&self.sub_agent_mode)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for mode in &[
                            "fully_auto",
                            "auto",
                            "auto_swarm",
                            "manual",
                        ] {
                            ui.selectable_value(
                                &mut self.sub_agent_mode,
                                mode.to_string(),
                                *mode,
                            );
                        }
                    });
                ui.end_row();

                ui.label("Sub-Agent Model:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.sub_agent_model)
                        .desired_width(300.0)
                        .hint_text("Override model for sub-agents (optional)"),
                );
                ui.end_row();

                ui.label("Agent Config File:");
                let config_label = if self.selected_agent_config.is_empty() {
                    "(none)".to_string()
                } else {
                    self.selected_agent_config.clone()
                };
                egui::ComboBox::from_id_salt("agent_config_picker")
                    .selected_text(&config_label)
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.selected_agent_config,
                            String::new(),
                            "(none)",
                        );
                        for f in &self.agent_config_files {
                            ui.selectable_value(
                                &mut self.selected_agent_config,
                                f.clone(),
                                f.as_str(),
                            );
                        }
                    });
                ui.end_row();
            });

        if self.agent_config_files.is_empty() {
            ui.add_space(4.0);
            let msg = if crate::server::data::get_remote_backend().is_some() {
                "No YAML files found on remote server. Place agent configs in data/agents/ on the remote."
            } else {
                "No YAML files found in data/agents/. Place agent configs there."
            };
            ui.label(
                egui::RichText::new(msg)
                    .size(11.0)
                    .color(egui::Color32::GRAY),
            );
        }

        ui.horizontal(|ui| {
            if ui.button("Refresh Configs").clicked() {
                if crate::server::data::get_remote_backend().is_some() {
                    self.agent_config_files = Self::scan_agent_configs_remote();
                } else {
                    self.agent_config_files = Self::scan_agent_configs();
                }
            }
        });

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if Self::save_button(ui, "Save Sub-Agent Settings") {
                self.save_all_settings(runtime);
                self.api_status_msg = Some("Sub-Agent settings saved!".to_string());
            }
            Self::status_label(ui, &self.api_status_msg);
        });
    }

    // ==================================================================
    //  4. MCP Tools
    // ==================================================================

    /// Convert current mcp_tools list to the Claude Desktop JSON format for the editor.
    fn mcp_tools_to_json(&self) -> String {
        let mut servers = serde_json::Map::new();
        for tool in &self.mcp_tools {
            let mut entry = serde_json::Map::new();
            if let Some(t) = &tool.tool_type {
                entry.insert("type".into(), serde_json::Value::String(t.clone()));
            }
            if let Some(cmd) = &tool.command {
                entry.insert("command".into(), serde_json::Value::String(cmd.clone()));
            }
            if let Some(args) = &tool.args {
                entry.insert("args".into(), serde_json::json!(args));
            }
            if !tool.url.is_empty() {
                entry.insert("url".into(), serde_json::Value::String(tool.url.clone()));
            }
            if !tool.enabled {
                entry.insert("enabled".into(), serde_json::Value::Bool(false));
            }
            if let Some(headers) = &tool.headers {
                if !headers.is_empty() {
                    let h: serde_json::Map<String, serde_json::Value> = headers
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect();
                    entry.insert("headers".into(), serde_json::Value::Object(h));
                }
            }
            servers.insert(tool.name.clone(), serde_json::Value::Object(entry));
        }
        let root = serde_json::json!({ "mcpServers": servers });
        serde_json::to_string_pretty(&root).unwrap_or_default()
    }

    /// Parse Claude Desktop JSON format into McpTool list.
    fn parse_mcp_json(text: &str) -> Result<Vec<McpTool>, String> {
        let val: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("Invalid JSON: {e}"))?;
        let servers = val
            .get("mcpServers")
            .and_then(|v| v.as_object())
            .ok_or_else(|| "Missing \"mcpServers\" object".to_string())?;

        let mut tools = Vec::new();
        for (name, cfg) in servers {
            let obj = cfg
                .as_object()
                .ok_or_else(|| format!("\"{name}\" must be an object"))?;
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let tool_type = obj.get("type").and_then(|v| v.as_str()).map(String::from);
            let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            let command = obj.get("command").and_then(|v| v.as_str()).map(String::from);
            let args = obj.get("args").and_then(|v| v.as_array()).map(|arr| {
                arr.iter().filter_map(|v| v.as_str().map(String::from)).collect()
            });
            let headers = obj.get("headers").and_then(|v| v.as_object()).map(|h| {
                h.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect::<HashMap<String, String>>()
            });
            tools.push(McpTool {
                name: name.clone(),
                url,
                enabled,
                tool_type,
                command,
                args,
                headers,
            });
        }
        Ok(tools)
    }

    fn section_mcp_tools(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);

        ui.add_space(8.0);
        ui.heading("MCP Tools");
        ui.add_space(4.0);

        ui.label(
            egui::RichText::new(format!("{} tool(s) configured", self.mcp_tools.len()))
                .size(12.0)
                .color(egui::Color32::GRAY),
        );
        ui.add_space(8.0);

        // Toggle between form mode and JSON mode
        ui.horizontal(|ui| {
            if ui
                .selectable_label(!self.mcp_json_mode, "Form Editor")
                .clicked()
            {
                self.mcp_json_mode = false;
                self.mcp_json_error = None;
            }
            if ui
                .selectable_label(self.mcp_json_mode, "JSON Editor")
                .clicked()
            {
                self.mcp_json_mode = true;
                self.mcp_json_text = self.mcp_tools_to_json();
                self.mcp_json_error = None;
            }
        });
        ui.add_space(8.0);

        if self.mcp_json_mode {
            self.section_mcp_tools_json(ui, runtime);
        } else {
            self.section_mcp_tools_form(ui, runtime);
        }
    }

    fn section_mcp_tools_json(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        ui.label(
            egui::RichText::new("Paste MCP server configuration in JSON format:")
                .size(12.0)
                .color(egui::Color32::GRAY),
        );
        ui.add_space(4.0);

        // Show example hint
        ui.collapsing("Example format", |ui| {
            ui.label(
                egui::RichText::new(
                    r#"{
  "mcpServers": {
    "web-search": {
      "type": "http",
      "url": "https://api.example.com/mcp",
      "headers": {
        "Authorization": "Bearer your_key"
      }
    },
    "local-tool": {
      "type": "stdio",
      "url": "python -m my_tool"
    }
  }
}"#,
                )
                .size(11.0)
                .monospace()
                .color(egui::Color32::GRAY),
            );
        });
        ui.add_space(4.0);

        // JSON textarea
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.mcp_json_text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(15)
                        .font(egui::TextStyle::Monospace),
                );
            });

        // Error message
        if let Some(err) = &self.mcp_json_error {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(err)
                    .size(12.0)
                    .color(egui::Color32::from_rgb(239, 68, 68)),
            );
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Apply JSON").clicked() {
                match Self::parse_mcp_json(&self.mcp_json_text) {
                    Ok(tools) => {
                        self.mcp_tools = tools;
                        self.mcp_json_error = None;
                        self.api_status_msg = Some("JSON applied — click Save to persist.".to_string());
                    }
                    Err(e) => {
                        self.mcp_json_error = Some(e);
                    }
                }
            }

            if Self::save_button(ui, "Save & Connect All") {
                match Self::parse_mcp_json(&self.mcp_json_text) {
                    Ok(tools) => {
                        self.mcp_tools = tools;
                        self.mcp_json_error = None;
                        self.save_all_settings(runtime);
                        self.api_status_msg = Some("MCP Tools saved!".to_string());
                    }
                    Err(e) => {
                        self.mcp_json_error = Some(e);
                    }
                }
            }

            Self::status_label(ui, &self.api_status_msg);

            // Show connection status for each server
            let statuses = self.mcp_connection_status.lock().unwrap().clone();
            if !statuses.is_empty() {
                ui.add_space(8.0);
                for (name, connected, tool_count, error) in &statuses {
                    if *connected {
                        ui.label(
                            egui::RichText::new(format!("✅ {} — {} tool(s) connected", name, tool_count))
                                .size(12.0)
                                .color(egui::Color32::from_rgb(34, 197, 94)),
                        );
                    } else {
                        let err_msg = error.as_deref().unwrap_or("unknown error");
                        ui.label(
                            egui::RichText::new(format!("❌ {} — {}", name, err_msg))
                                .size(12.0)
                                .color(egui::Color32::from_rgb(239, 68, 68)),
                        );
                    }
                }
            }
        });
    }

    fn section_mcp_tools_form(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // List existing tools
        let mut to_delete: Option<usize> = None;
        for (idx, tool) in self.mcp_tools.iter_mut().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut tool.enabled, "");
                    ui.label(egui::RichText::new(&tool.name).strong());
                    if let Some(t) = &tool.tool_type {
                        ui.label(
                            egui::RichText::new(format!("[{t}]"))
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Delete")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            ))
                            .clicked()
                        {
                            to_delete = Some(idx);
                        }
                    });
                });
                // Show command/args for stdio servers or URL for HTTP
                if let Some(cmd) = &tool.command {
                    let args_str = tool.args.as_ref()
                        .map(|a| a.join(" "))
                        .unwrap_or_default();
                    ui.label(
                        egui::RichText::new(format!("stdio: {} {}", cmd, args_str))
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                } else if !tool.url.is_empty() {
                    ui.label(
                        egui::RichText::new(&tool.url)
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                }
                if let Some(headers) = &tool.headers {
                    if !headers.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("Headers: {}", headers.len()))
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                    }
                }
            });
            ui.add_space(4.0);
        }

        if let Some(idx) = to_delete {
            self.mcp_tools.remove(idx);
        }

        // Add new MCP tool form
        ui.add_space(8.0);
        ui.separator();
        ui.heading("Add New MCP Tool");
        ui.add_space(4.0);

        egui::Grid::new("new_mcp_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Name:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_mcp_name)
                        .desired_width(300.0)
                        .hint_text("Tool name"),
                );
                ui.end_row();

                ui.label("URL:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_mcp_url)
                        .desired_width(300.0)
                        .hint_text("https://..."),
                );
                ui.end_row();

                ui.label("Headers (JSON):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_mcp_headers)
                        .desired_width(300.0)
                        .hint_text("{\"Authorization\": \"Bearer xxx\"} (optional)"),
                );
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can_add = !self.new_mcp_name.is_empty() && !self.new_mcp_url.is_empty();
            if ui
                .add_enabled(can_add, egui::Button::new("Add Tool"))
                .clicked()
            {
                let headers: Option<HashMap<String, String>> = if self.new_mcp_headers.is_empty() {
                    None
                } else {
                    serde_json::from_str(&self.new_mcp_headers).ok()
                };

                self.mcp_tools.push(McpTool {
                    name: self.new_mcp_name.clone(),
                    url: self.new_mcp_url.clone(),
                    enabled: true,
                    tool_type: None,
                    command: None,
                    args: None,
                    headers,
                });
                self.new_mcp_name.clear();
                self.new_mcp_url.clear();
                self.new_mcp_headers.clear();
            }
        });

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if Self::save_button(ui, "Save MCP Tools") {
                self.save_all_settings(runtime);
                self.api_status_msg = Some("MCP Tools saved!".to_string());
            }
            Self::status_label(ui, &self.api_status_msg);
        });

        // Show connection status
        let statuses = self.mcp_connection_status.lock().unwrap().clone();
        if !statuses.is_empty() {
            ui.add_space(8.0);
            for (name, connected, tool_count, error) in &statuses {
                if *connected {
                    ui.label(
                        egui::RichText::new(format!("✅ {} — {} tool(s) connected", name, tool_count))
                            .size(12.0)
                            .color(egui::Color32::from_rgb(34, 197, 94)),
                    );
                } else {
                    let err_msg = error.as_deref().unwrap_or("unknown error");
                    ui.label(
                        egui::RichText::new(format!("❌ {} — {}", name, err_msg))
                            .size(12.0)
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    );
                }
            }
        }
    }

    // ==================================================================
    //  5. Remote Instances
    // ==================================================================

    fn section_remote(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);

        ui.add_space(8.0);
        ui.heading("Remote Instances");
        ui.add_space(4.0);

        ui.checkbox(&mut self.remote_enabled, "Enable remote agent access");

        ui.add_space(8.0);
        ui.separator();
        ui.heading("Remote Token");
        ui.add_space(4.0);

        egui::Grid::new("remote_token_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Token:");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.remote_token)
                            .desired_width(300.0)
                            .hint_text("Enter or generate a token")
                            .font(egui::TextStyle::Monospace),
                    );
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(self.remote_token.clone());
                    }
                    if ui.button("Generate").clicked() {
                        self.remote_token = generate_token();
                    }
                });
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.separator();
        ui.heading("Configured Instances");
        ui.add_space(4.0);

        // List existing instances
        let mut to_delete: Option<usize> = None;
        for (idx, inst) in self.remote_instances.iter().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&inst.name).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Remove")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            ))
                            .clicked()
                        {
                            to_delete = Some(idx);
                        }
                    });
                });
                ui.label(
                    egui::RichText::new(&inst.url)
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                let token_masked = if inst.token.len() > 8 {
                    format!("{}...{}", &inst.token[..4], &inst.token[inst.token.len() - 4..])
                } else {
                    "*".repeat(inst.token.len())
                };
                ui.label(
                    egui::RichText::new(format!("Token: {}", token_masked))
                        .size(11.0)
                        .color(egui::Color32::GRAY)
                        .monospace(),
                );
            });
            ui.add_space(4.0);
        }

        if let Some(idx) = to_delete {
            self.remote_instances.remove(idx);
        }

        // Add new instance form
        ui.add_space(8.0);
        ui.separator();
        ui.heading("Add Remote Instance");
        ui.add_space(4.0);

        egui::Grid::new("new_remote_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Name:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_remote_name)
                        .desired_width(300.0)
                        .hint_text("Instance name"),
                );
                ui.end_row();

                ui.label("URL:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_remote_url)
                        .desired_width(300.0)
                        .hint_text("https://remote-host:port"),
                );
                ui.end_row();

                ui.label("Token:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_remote_token)
                        .password(true)
                        .desired_width(300.0)
                        .hint_text("Authentication token"),
                );
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can_add = !self.new_remote_name.is_empty() && !self.new_remote_url.is_empty();
            if ui
                .add_enabled(can_add, egui::Button::new("Add Instance"))
                .clicked()
            {
                let mut url = self.new_remote_url.clone();
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    url = format!("http://{}", url);
                }
                self.remote_instances.push(RemoteInstance {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: self.new_remote_name.clone(),
                    url,
                    token: self.new_remote_token.clone(),
                });
                self.new_remote_name.clear();
                self.new_remote_url.clear();
                self.new_remote_token.clear();
            }
        });

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if Self::save_button(ui, "Save Remote Settings") {
                self.save_all_settings(runtime);
                self.api_status_msg = Some("Remote settings saved!".to_string());
            }
            Self::status_label(ui, &self.api_status_msg);
        });
    }

    // ==================================================================
    //  6. File Access Tokens
    // ==================================================================

    fn section_file_tokens(
        &mut self,
        ui: &mut egui::Ui,
        _ctx: &egui::Context,
        runtime: &tokio::runtime::Handle,
    ) {
        self.load_settings_if_needed(runtime);

        ui.add_space(8.0);
        ui.heading("File Access Tokens");
        ui.add_space(4.0);

        ui.label(
            egui::RichText::new(format!("{} token(s)", self.file_tokens.len()))
                .size(12.0)
                .color(egui::Color32::GRAY),
        );
        ui.add_space(8.0);

        // List tokens
        let mut to_delete: Option<usize> = None;
        let mut to_regenerate: Option<usize> = None;
        let mut to_copy: Option<String> = None;

        for (idx, ft) in self.file_tokens.iter().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&ft.name).strong());
                    ui.label(
                        egui::RichText::new(&ft.created_at)
                            .size(10.0)
                            .color(egui::Color32::GRAY),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Delete")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            ))
                            .clicked()
                        {
                            to_delete = Some(idx);
                        }
                        if ui.small_button("Regenerate").clicked() {
                            to_regenerate = Some(idx);
                        }
                        if ui.small_button("Copy").clicked() {
                            to_copy = Some(ft.token.clone());
                        }
                    });
                });
                // Show masked token
                let masked = if ft.token.len() > 8 {
                    format!("{}...{}", &ft.token[..4], &ft.token[ft.token.len() - 4..])
                } else {
                    "*".repeat(ft.token.len())
                };
                ui.label(
                    egui::RichText::new(masked)
                        .size(11.0)
                        .color(egui::Color32::GRAY)
                        .monospace(),
                );
            });
            ui.add_space(4.0);
        }

        // Handle actions
        if let Some(token_text) = to_copy {
            ui.ctx().copy_text(token_text);
            self.token_status_msg = Some("Token copied to clipboard!".to_string());
        }
        if let Some(idx) = to_regenerate {
            self.file_tokens[idx].token = generate_token();
            runtime.block_on(save_file_tokens(&self.file_tokens));
            self.token_status_msg = Some("Token regenerated!".to_string());
        }
        if let Some(idx) = to_delete {
            self.file_tokens.remove(idx);
            runtime.block_on(save_file_tokens(&self.file_tokens));
            self.token_status_msg = Some("Token deleted.".to_string());
        }

        // Create new token
        ui.add_space(8.0);
        ui.separator();
        ui.heading("Create New Token");
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Label:");
            ui.add(
                egui::TextEdit::singleline(&mut self.new_token_label)
                    .desired_width(250.0)
                    .hint_text("Token label / description"),
            );
            let can_create = !self.new_token_label.is_empty();
            if ui
                .add_enabled(can_create, egui::Button::new("Create Token"))
                .clicked()
            {
                let new_token = FileToken {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: self.new_token_label.clone(),
                    token: generate_token(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                self.file_tokens.push(new_token);
                runtime.block_on(save_file_tokens(&self.file_tokens));
                self.new_token_label.clear();
                self.token_status_msg = Some("Token created!".to_string());
            }
        });

        if let Some(msg) = &self.token_status_msg {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(msg)
                    .size(12.0)
                    .color(egui::Color32::from_rgb(34, 197, 94)),
            );
        }
    }

    // ==================================================================
    //  7. Local File Mounts
    // ==================================================================

    fn section_file_mounts(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);

        ui.add_space(8.0);
        ui.heading("Local File Mounts");
        ui.add_space(4.0);

        ui.label(
            egui::RichText::new(format!("{} mount(s) configured", self.local_file_mounts.len()))
                .size(12.0)
                .color(egui::Color32::GRAY),
        );
        ui.add_space(8.0);

        // List existing mounts
        let mut to_delete: Option<usize> = None;
        for (idx, mount) in self.local_file_mounts.iter_mut().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut mount.enabled, "");
                    ui.label(egui::RichText::new(&mount.label).strong());

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Remove")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            ))
                            .clicked()
                        {
                            to_delete = Some(idx);
                        }
                    });
                });
                ui.label(
                    egui::RichText::new(format!("Path: {}", mount.path))
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                ui.label(
                    egui::RichText::new(format!("Permissions: {}", mount.permissions))
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
            });
            ui.add_space(4.0);
        }

        if let Some(idx) = to_delete {
            self.local_file_mounts.remove(idx);
        }

        // Add new mount
        ui.add_space(8.0);
        ui.separator();
        ui.heading("Add File Mount");
        ui.add_space(4.0);

        egui::Grid::new("new_mount_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Path:");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_mount_path)
                            .desired_width(240.0)
                            .hint_text("/path/to/folder"),
                    );
                    if ui.button("Browse...").clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            self.new_mount_path = folder.to_string_lossy().to_string();
                        }
                    }
                });
                ui.end_row();

                ui.label("Label:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_mount_label)
                        .desired_width(300.0)
                        .hint_text("Display name"),
                );
                ui.end_row();

                ui.label("Permissions:");
                egui::ComboBox::from_id_salt("mount_perms_picker")
                    .selected_text(&self.new_mount_permissions)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for perm in &["read-only", "read-write"] {
                            ui.selectable_value(
                                &mut self.new_mount_permissions,
                                perm.to_string(),
                                *perm,
                            );
                        }
                    });
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can_add = !self.new_mount_path.is_empty() && !self.new_mount_label.is_empty();
            if ui
                .add_enabled(can_add, egui::Button::new("Add Mount"))
                .clicked()
            {
                self.local_file_mounts.push(LocalFileMount {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: self.new_mount_path.clone(),
                    label: self.new_mount_label.clone(),
                    permissions: self.new_mount_permissions.clone(),
                    enabled: true,
                });
                self.new_mount_path.clear();
                self.new_mount_label.clear();
                self.new_mount_permissions = "read-only".to_string();
            }
        });

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if Self::save_button(ui, "Save File Mounts") {
                self.save_all_settings(runtime);
                self.api_status_msg = Some("File mounts saved!".to_string());
            }
            Self::status_label(ui, &self.api_status_msg);
        });
    }

    // ==================================================================
    //  8. Skill Auto-Update
    // ==================================================================

    fn section_skill_update(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);

        ui.add_space(8.0);
        ui.heading("Skill Auto-Update");
        ui.label(
            egui::RichText::new("Automatically generates reusable skills from your chat sessions using LLM analysis.")
                .size(12.0)
                .color(egui::Color32::from_rgb(168, 158, 144)),
        );
        ui.add_space(8.0);

        // Settings section
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(8.0)
            .inner_margin(egui::Margin::same(12))
            .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.checkbox(
                        &mut self.skill_auto_update_enabled,
                        egui::RichText::new("Enable automatic skill updates").size(14.0),
                    );
                    ui.add_space(8.0);

                    egui::Grid::new("skill_update_grid")
                        .num_columns(2)
                        .spacing([12.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("Update Interval:");
                            let mut interval_i32 = self.skill_auto_update_interval_minutes as i32;
                            ui.add(
                                egui::Slider::new(&mut interval_i32, 1..=60)
                                    .text("minutes")
                                    .clamping(egui::SliderClamping::Always),
                            );
                            self.skill_auto_update_interval_minutes = interval_i32.max(1) as u64;
                            ui.end_row();

                            ui.label("Max Candidates per Run:");
                            let mut max_cand_i32 = self.skill_auto_update_max_candidates as i32;
                            ui.add(
                                egui::Slider::new(&mut max_cand_i32, 1..=50)
                                    .clamping(egui::SliderClamping::Always),
                            );
                            self.skill_auto_update_max_candidates = max_cand_i32.max(1) as u64;
                            ui.end_row();
                        });

                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.skill_auto_update_require_approval,
                        "Require approval before applying updates",
                    );
                    ui.checkbox(
                        &mut self.skill_auto_update_human_feedback_enabled,
                        "Enable human feedback (thumbs up/down on messages)",
                    );
                });
            });

        ui.add_space(8.0);

        // Action buttons
        ui.horizontal(|ui| {
            if Self::save_button(ui, "Save Settings") {
                self.save_all_settings(runtime);
                self.api_status_msg = Some("Skill update settings saved!".to_string());
            }

            ui.add_space(8.0);

            let run_btn = egui::Button::new(
                egui::RichText::new("\u{25B6} Run Auto-Update Now")
                    .size(13.0)
                    .color(egui::Color32::WHITE),
            )
            .fill(egui::Color32::from_rgb(22, 163, 74))
            .corner_radius(6.0)
            .min_size(egui::vec2(0.0, 30.0));

            if ui.add(run_btn).clicked() {
                runtime.spawn(async {
                    match crate::server::services::skill_synthesizer::run_synthesis_forced().await {
                        Ok(s) => tracing::info!("[SkillSynth] Manual: {}", s),
                        Err(e) => tracing::error!("[SkillSynth] Manual error: {}", e),
                    }
                });
                self.api_status_msg = Some("Synthesis started...".to_string());
            }

            Self::status_label(ui, &self.api_status_msg);
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        // Status section
        ui.heading("Status");
        ui.add_space(4.0);

        // Poll synth status
        let synth_status =
            runtime.block_on(crate::server::services::skill_synthesizer::get_synth_status());

        egui::Grid::new("skill_status_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Status:").strong());
                if synth_status.running {
                    ui.label(
                        egui::RichText::new("\u{23F3} Running...")
                            .color(egui::Color32::from_rgb(250, 204, 21)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("\u{2705} Idle")
                            .color(egui::Color32::from_rgb(34, 197, 94)),
                    );
                }
                ui.end_row();

                ui.label(egui::RichText::new("Last Run:").strong());
                ui.label(
                    synth_status
                        .last_run_at
                        .as_deref()
                        .unwrap_or("Never"),
                );
                ui.end_row();

                ui.label(egui::RichText::new("Summary:").strong());
                ui.label(
                    synth_status
                        .last_run_summary
                        .as_deref()
                        .unwrap_or("No runs yet"),
                );
                ui.end_row();
            });

        // Pending proposals
        let pending: Vec<_> = synth_status
            .proposals
            .iter()
            .filter(|p| p.review_status == "pending")
            .collect();

        if !pending.is_empty() {
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.heading(format!("Pending Proposals ({})", pending.len()));
            ui.add_space(4.0);

            for proposal in &pending {
                let proposal_id = proposal.id.clone();
                let _proposal_name = proposal.name.clone();

                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_premultiplied(18, 154, 145, 12))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::same(10))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(18, 154, 145),
                    ))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                let kind_badge = if proposal.kind == "update" {
                                    egui::RichText::new("UPDATE")
                                        .size(10.0)
                                        .strong()
                                        .color(egui::Color32::from_rgb(250, 204, 21))
                                } else {
                                    egui::RichText::new("NEW")
                                        .size(10.0)
                                        .strong()
                                        .color(egui::Color32::from_rgb(34, 197, 94))
                                };

                                egui::Frame::new()
                                    .fill(egui::Color32::from_rgb(17, 24, 39))
                                    .corner_radius(4.0)
                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                    .show(ui, |ui| {
                                        ui.label(kind_badge);
                                    });

                                ui.label(
                                    egui::RichText::new(&proposal.name)
                                        .size(15.0)
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                );
                            });

                            ui.label(
                                egui::RichText::new(&proposal.description)
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(168, 158, 144)),
                            );

                            if !proposal.rationale.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Rationale: {}",
                                        proposal.rationale
                                    ))
                                    .size(11.0)
                                    .italics()
                                    .color(egui::Color32::from_rgb(168, 158, 144)),
                                );
                            }

                            ui.add_space(4.0);

                            // Show content preview in collapsible
                            let preview = crate::util::truncate_utf8_ellipsis(&proposal.content, 300);
                            egui::CollapsingHeader::new("Preview SKILL.md")
                                .id_salt(format!("preview_{}", proposal.id))
                                .show(ui, |ui| {
                                    egui::Frame::new()
                                        .fill(egui::Color32::from_rgb(17, 24, 39))
                                        .corner_radius(4.0)
                                        .inner_margin(egui::Margin::same(8))
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&preview)
                                                        .size(12.0)
                                                        .monospace()
                                                        .color(egui::Color32::from_rgb(
                                                            167, 243, 208,
                                                        )),
                                                )
                                                .wrap(),
                                            );
                                        });
                                });

                            ui.add_space(4.0);

                            // Approve / Reject buttons
                            ui.horizontal(|ui| {
                                let approve_btn = egui::Button::new(
                                    egui::RichText::new("\u{2705} Approve")
                                        .size(13.0)
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(22, 163, 74))
                                .corner_radius(6.0);

                                if ui.add(approve_btn).clicked() {
                                    let pid = proposal_id.clone();
                                    runtime.spawn(async move {
                                        let _ = crate::server::services::skill_synthesizer::approve_proposal(&pid).await;
                                    });
                                }

                                let reject_btn = egui::Button::new(
                                    egui::RichText::new("\u{274C} Reject")
                                        .size(13.0)
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(220, 38, 38))
                                .corner_radius(6.0);

                                if ui.add(reject_btn).clicked() {
                                    let pid = proposal_id.clone();
                                    runtime.spawn(async move {
                                        let _ = crate::server::services::skill_synthesizer::reject_proposal(&pid).await;
                                    });
                                }
                            });
                        });
                    });

                ui.add_space(6.0);
            }
        }

        // Show recent approved/rejected
        let recent_completed: Vec<_> = synth_status
            .proposals
            .iter()
            .filter(|p| p.review_status != "pending")
            .collect();

        if !recent_completed.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Recent Decisions")
                    .size(14.0)
                    .strong(),
            );

            for proposal in &recent_completed {
                ui.horizontal(|ui| {
                    let status_icon = if proposal.review_status == "approved" {
                        egui::RichText::new("\u{2705}")
                            .color(egui::Color32::from_rgb(34, 197, 94))
                    } else {
                        egui::RichText::new("\u{274C}")
                            .color(egui::Color32::from_rgb(239, 68, 68))
                    };
                    ui.label(status_icon);
                    ui.label(&proposal.name);
                    ui.label(
                        egui::RichText::new(&proposal.review_status)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(168, 158, 144)),
                    );
                });
            }
        }
    }

    // ==================================================================
    //  9. Security (unchanged)
    // ==================================================================

    fn section_security(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        ui.add_space(8.0);
        ui.heading("Tool Approval");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "When enabled, the AI must ask for your approval before executing each tool type.",
            )
            .size(12.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(8.0);

        let mut changed = false;

        egui::Grid::new("tool_approval_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Shell commands (run_shell):");
                if ui.checkbox(&mut self.approval_shell, "Require approval").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label("Python / React (run_python, run_react):");
                if ui.checkbox(&mut self.approval_python, "Require approval").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label("Write files (write_file):");
                if ui.checkbox(&mut self.approval_file_write, "Require approval").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label("Delete files (delete_file):");
                if ui.checkbox(&mut self.approval_file_delete, "Require approval").changed() {
                    changed = true;
                }
                ui.end_row();

                ui.label("Spawn sub-agent (claude_code_agent):");
                if ui.checkbox(&mut self.approval_agent_spawn, "Require approval").changed() {
                    changed = true;
                }
                ui.end_row();
            });

        if changed {
            self.save_all_settings(runtime);
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("Sandbox Security");
        ui.add_space(8.0);

        egui::Grid::new("security_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("Isolation:");
                ui.label(
                    egui::RichText::new("Full VM (QEMU)")
                        .color(egui::Color32::from_rgb(34, 197, 94)),
                );
                ui.end_row();
                ui.label("Network:");
                ui.label("NAT - VM can access internet, host sees only forwarded port");
                ui.end_row();
                ui.label("File System:");
                ui.label(
                    egui::RichText::new(
                        "Completely isolated. Only shared folders are accessible.",
                    )
                    .color(egui::Color32::from_rgb(34, 197, 94)),
                );
                ui.end_row();
                ui.label("Process Isolation:");
                ui.label(
                    egui::RichText::new(
                        "VM processes cannot see or affect host processes",
                    )
                    .color(egui::Color32::from_rgb(34, 197, 94)),
                );
                ui.end_row();
            });

        ui.add_space(16.0);
        ui.separator();
        ui.heading("Shared Folder Policy");
        ui.label(
            egui::RichText::new(
                "Folders shared via VirtioFS/9p, mounted inside the VM.\nDefault: Read-only. Write access requires explicit toggle.",
            )
            .size(12.0)
            .color(egui::Color32::GRAY),
        );
    }

    // ==================================================================
    // 10. Plugins
    // ==================================================================

    fn section_plugins(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        runtime: &tokio::runtime::Handle,
    ) {
        use crate::server::services::plugin;

        // Load plugins list on first view
        if !self.plugins_loaded {
            self.plugins_loaded = true;
            self.plugins = runtime.block_on(plugin::list_plugins());
        }

        ui.add_space(8.0);

        // Header with Install button
        ui.horizontal(|ui| {
            ui.heading("Plugins");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let accent = egui::Color32::from_rgb(18, 154, 145);
                let btn = egui::Button::new(
                    egui::RichText::new("Install Plugin")
                        .size(13.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(accent)
                .corner_radius(14.0)
                .min_size(egui::vec2(120.0, 28.0));

                if ui.add(btn).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Plugin ZIP", &["zip"])
                        .pick_file()
                    {
                        match std::fs::read(&path) {
                            Ok(bytes) => {
                                match runtime.block_on(plugin::install_plugin(&bytes)) {
                                    Ok(installed) => {
                                        self.plugin_status_msg = Some(format!(
                                            "Installed '{}' v{}",
                                            installed.name, installed.version
                                        ));
                                        self.plugins = runtime.block_on(plugin::list_plugins());
                                    }
                                    Err(e) => {
                                        self.plugin_status_msg = Some(format!("Error: {}", e));
                                    }
                                }
                            }
                            Err(e) => {
                                self.plugin_status_msg = Some(format!("Read error: {}", e));
                            }
                        }
                    }
                }
            });
        });

        // Status message
        if let Some(ref msg) = self.plugin_status_msg {
            ui.add_space(4.0);
            let color = if msg.starts_with("Error") {
                egui::Color32::from_rgb(239, 68, 68)
            } else {
                egui::Color32::from_rgb(34, 197, 94)
            };
            ui.label(egui::RichText::new(msg).size(12.0).color(color));
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);

        if self.plugins.is_empty() {
            ui.label(
                egui::RichText::new("No plugins installed. Click 'Install Plugin' to add one.")
                    .size(13.0)
                    .color(egui::Color32::GRAY),
            );
            return;
        }

        // Uninstall confirmation dialog
        if let Some(ref uninstall_id) = self.show_uninstall_confirm.clone() {
            egui::Window::new("Uninstall Plugin?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Remove plugin '{}' and all its components?", uninstall_id));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_uninstall_confirm = None;
                        }
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Uninstall")
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            ))
                            .clicked()
                        {
                            let id = uninstall_id.clone();
                            let _ = runtime.block_on(plugin::uninstall_plugin(&id));
                            self.plugins = runtime.block_on(plugin::list_plugins());
                            if self.selected_plugin_id.as_deref() == Some(&id) {
                                self.selected_plugin_id = None;
                            }
                            self.show_uninstall_confirm = None;
                            self.plugin_status_msg = Some(format!("Uninstalled '{}'", id));
                        }
                    });
                });
        }

        // Two-column: list left, detail right
        let selected_id = self.selected_plugin_id.clone();

        ui.columns(2, |cols| {
            // Left column: plugin list
            egui::ScrollArea::vertical()
                .id_salt("plugin_list")
                .auto_shrink([false, false])
                .max_height(400.0)
                .show(&mut cols[0], |ui| {
                    let plugins_snapshot = self.plugins.clone();
                    let mut toggle_id: Option<(String, bool)> = None;
                    let mut select_id: Option<String> = None;

                    for p in &plugins_snapshot {
                        let is_selected = selected_id.as_deref() == Some(&p.id);
                        let frame_fill = if is_selected {
                            egui::Color32::from_rgba_premultiplied(18, 154, 145, 20)
                        } else {
                            egui::Color32::TRANSPARENT
                        };

                        egui::Frame::new()
                            .fill(frame_fill)
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    // Name + version
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(&p.name)
                                                    .size(14.0)
                                                    .strong(),
                                            );
                                            ui.label(
                                                egui::RichText::new(&format!("v{}", p.version))
                                                    .size(11.0)
                                                    .color(egui::Color32::GRAY),
                                            );
                                            if let Some(ref cat) = p.category {
                                                let cat_color = match cat.as_str() {
                                                    "connector" => egui::Color32::from_rgb(59, 130, 246),
                                                    "toolkit" => egui::Color32::from_rgb(168, 85, 247),
                                                    "swarm" => egui::Color32::from_rgb(249, 115, 22),
                                                    _ => egui::Color32::GRAY,
                                                };
                                                ui.label(
                                                    egui::RichText::new(cat)
                                                        .size(10.0)
                                                        .color(cat_color),
                                                );
                                            }
                                        });
                                        ui.label(
                                            egui::RichText::new(&p.description)
                                                .size(11.0)
                                                .color(egui::Color32::GRAY),
                                        );
                                        ui.label(
                                            egui::RichText::new(&format!("by {}", p.author))
                                                .size(10.0)
                                                .color(egui::Color32::from_rgb(160, 160, 160)),
                                        );
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let mut enabled = p.enabled;
                                            if ui.checkbox(&mut enabled, "").changed() {
                                                toggle_id = Some((p.id.clone(), enabled));
                                            }
                                        },
                                    );
                                });
                            });

                        // Click to select
                        let resp = ui.interact(
                            ui.min_rect(),
                            ui.id().with(&p.id),
                            egui::Sense::click(),
                        );
                        if resp.clicked() {
                            select_id = Some(p.id.clone());
                        }

                        ui.add_space(2.0);
                    }

                    // Apply toggle
                    if let Some((id, enabled)) = toggle_id {
                        let _ = runtime.block_on(plugin::toggle_plugin(&id, enabled));
                        self.plugins = runtime.block_on(plugin::list_plugins());
                    }
                    // Apply selection
                    if let Some(id) = select_id {
                        self.selected_plugin_id = Some(id);
                    }
                });

            // Right column: detail view
            egui::ScrollArea::vertical()
                .id_salt("plugin_detail")
                .auto_shrink([false, false])
                .max_height(400.0)
                .show(&mut cols[1], |ui| {
                    let sel_id = self.selected_plugin_id.clone();
                    if let Some(ref id) = sel_id {
                        if let Some(p) = self.plugins.iter().find(|p| p.id == *id).cloned() {
                            self.render_plugin_detail(ui, &p, runtime);
                        } else {
                            ui.label("Select a plugin from the list.");
                        }
                    } else {
                        ui.label(
                            egui::RichText::new("Select a plugin to view details.")
                                .color(egui::Color32::GRAY),
                        );
                    }
                });
        });
    }

    fn render_plugin_detail(
        &mut self,
        ui: &mut egui::Ui,
        plugin: &crate::server::services::plugin::InstalledPlugin,
        runtime: &tokio::runtime::Handle,
    ) {
        use crate::server::services::plugin;

        ui.label(egui::RichText::new(&plugin.name).size(18.0).strong());
        ui.label(
            egui::RichText::new(&format!("v{} by {}", plugin.version, plugin.author))
                .size(12.0)
                .color(egui::Color32::GRAY),
        );
        ui.add_space(4.0);
        ui.label(&plugin.description);
        ui.add_space(8.0);

        // README
        if plugin.has_readme {
            let readme = self
                .plugin_readme_cache
                .entry(plugin.id.clone())
                .or_insert_with(|| {
                    runtime
                        .block_on(plugin::get_plugin_readme(&plugin.id))
                        .unwrap_or_else(|| "(No README)".to_string())
                })
                .clone();

            egui::CollapsingHeader::new("README")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(&readme)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(80, 80, 80)),
                    );
                });
            ui.add_space(4.0);
        }

        // Components
        ui.separator();
        ui.label(egui::RichText::new("Components").size(13.0).strong());
        ui.add_space(4.0);

        if !plugin.components.skills.is_empty() {
            ui.label(
                egui::RichText::new(format!("Skills ({})", plugin.components.skills.len()))
                    .size(12.0)
                    .strong(),
            );
            for s in &plugin.components.skills {
                ui.horizontal(|ui| {
                    ui.label("  ");
                    ui.label(
                        egui::RichText::new(&s.name)
                            .size(12.0)
                            .color(egui::Color32::from_rgb(18, 154, 145)),
                    );
                    if let Some(ref desc) = s.description {
                        ui.label(
                            egui::RichText::new(format!("- {}", desc))
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                    }
                });
            }
            ui.add_space(4.0);
        }

        if !plugin.components.agents.is_empty() {
            ui.label(
                egui::RichText::new(format!("Agents ({})", plugin.components.agents.len()))
                    .size(12.0)
                    .strong(),
            );
            for a in &plugin.components.agents {
                ui.label(
                    egui::RichText::new(format!("  {}", a.name))
                        .size(12.0)
                        .color(egui::Color32::from_rgb(168, 85, 247)),
                );
            }
            ui.add_space(4.0);
        }

        if !plugin.components.mcp_servers.is_empty() {
            ui.label(
                egui::RichText::new(format!(
                    "MCP Servers ({})",
                    plugin.components.mcp_servers.len()
                ))
                .size(12.0)
                .strong(),
            );
            for m in &plugin.components.mcp_servers {
                ui.label(
                    egui::RichText::new(format!("  {}", m.name))
                        .size(12.0)
                        .color(egui::Color32::from_rgb(59, 130, 246)),
                );
            }
            ui.add_space(4.0);
        }

        // Connectors with config forms
        if !plugin.components.connectors.is_empty() {
            ui.separator();
            ui.label(
                egui::RichText::new(format!(
                    "Connectors ({})",
                    plugin.components.connectors.len()
                ))
                .size(13.0)
                .strong(),
            );
            ui.add_space(4.0);

            for conn in &plugin.components.connectors {
                egui::CollapsingHeader::new(
                    egui::RichText::new(&format!("{} ({})", conn.name, conn.service))
                        .size(12.0),
                )
                .default_open(false)
                .show(ui, |ui| {
                    if let Some(ref desc) = conn.description {
                        ui.label(
                            egui::RichText::new(desc)
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                    }
                    ui.add_space(4.0);

                    // Config fields
                    let config_key = format!("{}:{}", plugin.id, conn.service);
                    if !self.plugin_connector_configs.contains_key(&config_key) {
                        let cfg = runtime
                            .block_on(plugin::get_connector_config(&plugin.id, &conn.service));
                        let mut map = HashMap::new();
                        if let Some(obj) = cfg.as_object() {
                            for (k, v) in obj {
                                map.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
                            }
                        }
                        self.plugin_connector_configs
                            .insert(config_key.clone(), map);
                    }

                    let fields_map = self
                        .plugin_connector_configs
                        .get_mut(&config_key)
                        .unwrap();

                    egui::Grid::new(format!("connector_grid_{}", config_key))
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            for field in &conn.config_fields {
                                let required_marker =
                                    if field.required == Some(true) { " *" } else { "" };
                                ui.label(format!("{}{}", field.label, required_marker));

                                let val = fields_map
                                    .entry(field.key.clone())
                                    .or_insert_with(|| {
                                        field.default.clone().unwrap_or_default()
                                    });

                                if field.field_type == "password" {
                                    ui.add(
                                        egui::TextEdit::singleline(val)
                                            .password(true)
                                            .desired_width(200.0),
                                    );
                                } else {
                                    ui.add(
                                        egui::TextEdit::singleline(val).desired_width(200.0),
                                    );
                                }
                                ui.end_row();
                            }
                        });

                    ui.add_space(6.0);
                    if ui.button("Save Config").clicked() {
                        let mut json_map = serde_json::Map::new();
                        for (k, v) in fields_map.iter() {
                            json_map.insert(
                                k.clone(),
                                serde_json::Value::String(v.clone()),
                            );
                        }
                        let _ = runtime.block_on(plugin::save_connector_config(
                            &plugin.id,
                            &conn.service,
                            serde_json::Value::Object(json_map),
                        ));
                        self.plugin_status_msg =
                            Some(format!("Saved config for {}", conn.service));
                    }
                });
            }
            ui.add_space(4.0);
        }

        // Permissions
        if !plugin.permissions.is_empty() {
            ui.separator();
            ui.label(egui::RichText::new("Permissions").size(12.0).strong());
            for perm in &plugin.permissions {
                ui.label(
                    egui::RichText::new(format!("  {}", perm))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(249, 115, 22)),
                );
            }
            ui.add_space(8.0);
        }

        // Uninstall button
        ui.add_space(8.0);
        let uninstall_btn = egui::Button::new(
            egui::RichText::new("Uninstall")
                .size(13.0)
                .color(egui::Color32::from_rgb(239, 68, 68)),
        )
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgb(239, 68, 68),
        ))
        .corner_radius(6.0);

        if ui.add(uninstall_btn).clicked() {
            self.show_uninstall_confirm = Some(plugin.id.clone());
        }
    }

    // ==================================================================
    // 11. About (unchanged)
    // ==================================================================

    fn section_about(ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            // Logo image
            let logo_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png");
            let logo_uri = format!("file://{}", logo_path.display());
            ui.add(
                egui::Image::new(&logo_uri)
                    .max_width(80.0)
                    .max_height(80.0)
                    .corner_radius(12.0),
            );
            ui.add_space(8.0);
            ui.label(egui::RichText::new("TigrimOS").size(28.0).strong());
            ui.label(
                egui::RichText::new("v0.5.3")
                    .size(14.0)
                    .color(egui::Color32::GRAY),
            );
            ui.label("AI Agent Workspace with Remote Agents");
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(12.0);
            for line in [
                "TigrimOS v0.5.3 (Rust/egui edition)",
                "Ubuntu 22.04 VM via QEMU",
                "Node.js 20 + Python 3 + Fastify",
            ] {
                ui.label(
                    egui::RichText::new(line)
                        .size(12.0)
                        .color(egui::Color32::GRAY),
                );
            }
        });
    }
}
