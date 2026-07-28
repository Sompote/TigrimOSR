use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::server::data::{
    generate_token, get_file_tokens, get_settings, save_file_tokens, save_settings, FileToken,
    LocalFileMount, McpTool, ModelPoolEntry, RemoteInstance,
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
    AgentLoop,
    Graph,
    McpTools,
    Tools,
    Plugins,
    Remote,
    Messaging,
    FileTokens,
    FileMounts,
    SkillUpdate,
    Security,
    Theme,
    About,
}

impl SettingsSection {
    const ALL: &'static [SettingsSection] = &[
        Self::General,
        Self::AI,
        Self::SubAgent,
        Self::AgentLoop,
        Self::Graph,
        Self::McpTools,
        Self::Tools,
        Self::Plugins,
        Self::Remote,
        Self::Messaging,
        Self::FileTokens,
        Self::FileMounts,
        Self::SkillUpdate,
        Self::Security,
        Self::Theme,
        Self::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::AI => "AI / API",
            Self::SubAgent => "Sub-Agent",
            Self::AgentLoop => "Agent Loop",
            Self::Graph => "Graph",
            Self::McpTools => "MCP Tools",
            Self::Tools => "Tools",
            Self::Plugins => "Plugins",
            Self::Remote => "Remote",
            Self::Messaging => "Messaging",
            Self::FileTokens => "File Tokens",
            Self::FileMounts => "File Mounts",
            Self::SkillUpdate => "Skill Update",
            Self::Security => "Security",
            Self::Theme => "Theme",
            Self::About => "About",
        }
    }
}

// ---------------------------------------------------------------------------
// Connection test status
// ---------------------------------------------------------------------------

/// Editable form model for one graph judge (mirrors graph::JudgeNode; a
/// negative threshold means "use the aggregation threshold").
#[derive(Debug, Clone)]
struct GraphJudgeForm {
    name: String,
    model: String,
    api_url: String,
    api_key: String,
    rules_file: String,
    rules: String,
    weight: f64,
    threshold: f64, // < 0.0 = inherit aggregation threshold
    use_tools: bool,
    allow_execute: bool,
    max_judge_rounds: u64,
}

impl Default for GraphJudgeForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            model: String::new(),
            api_url: String::new(),
            api_key: String::new(),
            rules_file: String::new(),
            rules: String::new(),
            weight: 1.0,
            threshold: -1.0,
            use_tools: true,
            allow_execute: false,
            max_judge_rounds: 3,
        }
    }
}

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
        AiProvider::new("MiniMax", "https://api.minimax.io/v1", "MiniMax-M3"),
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
    // --- Router mode: heterogeneous model pool + tier ---
    model_pool: Vec<ModelPoolEntry>,
    router_tier: String,
    router_orchestrator_model: String, // "" = use main model
    agent_config_files: Vec<String>,
    selected_agent_config: String,

    // --- Agent Loop Profiles ---
    loop_profiles: Vec<String>,
    active_loop_profile: String, // "" = built-in behavior (no profile)
    loop_selected_file: String,  // file currently open in the editor
    loop_new_name: String,
    loop_yaml_mode: bool,
    loop_yaml_text: String,
    loop_status_msg: Option<String>,
    loop_needs_refresh: bool,
    loop_tool_catalog: Vec<(String, String)>,
    loop_skill_catalog: Vec<String>,
    // form model
    loop_name: String,
    loop_description: String,
    loop_model_model: String,
    loop_model_api_url: String,
    loop_model_api_key: String,
    loop_sp_text: String,
    loop_sp_replace_base: bool,
    loop_tools_mode: String, // all | allowlist | denylist
    loop_tools_checked: std::collections::HashSet<String>,
    /// Per-tool config (tools.config in the profile YAML), editable in the
    /// form editor and carried through Save.
    loop_tools_config:
        std::collections::BTreeMap<String, crate::server::services::agent_loop::ToolConfig>,
    /// Tool name being typed/picked for a new per-tool config entry.
    loop_tools_config_new: String,
    /// Raw JSON text buffers for params / pinned_params — kept separate from
    /// the parsed config so half-typed JSON doesn't get wiped every frame.
    loop_tools_config_params_text: std::collections::BTreeMap<String, String>,
    loop_tools_config_pins_text: std::collections::BTreeMap<String, String>,
    loop_mcp_mode: String, // all | selected | none
    loop_mcp_checked: std::collections::HashSet<String>,
    loop_skills_mode: String, // all | selected | none
    loop_skills_checked: std::collections::HashSet<String>,
    loop_max_rounds: u64,
    loop_max_tool_calls: u64,
    loop_temperature: f64,
    loop_max_tokens: u64,
    loop_reflection_enabled: bool,
    loop_reflection_threshold: f64,
    loop_max_reflection_retries: u64,
    loop_checkpoint_enabled: bool,
    loop_max_spawn_depth: u64,
    loop_step_verification: bool,
    loop_compact_enabled: bool,
    loop_compact_interval: u64,
    loop_compact_window: u64,
    loop_compact_max_context_tokens: u64,
    loop_compact_tool_result_max_len: u64,
    loop_compact_model: String,
    // graph gate knobs (graph: section — inherit | on | off + profile file)
    loop_graph_gate: String,
    loop_graph_profile: String,
    // outer evaluation loop (tool-using job-level judge)
    loop_eval_enabled: bool,
    loop_eval_threshold: f64,
    loop_eval_max_retries: u64,
    loop_eval_max_fix_rounds: u64,
    loop_eval_max_judge_rounds: u64,
    loop_eval_model: String,
    loop_eval_rubric: String,
    loop_eval_allow_execute: bool,

    // --- Graph mode (judge panel gating the final answer) ---
    graph_gate_enabled: bool, // global toggle (graphEnabled) — default off
    graph_profiles: Vec<String>,
    active_graph_profile: String, // "" = default profile
    graph_selected_file: String,
    graph_new_name: String,
    graph_yaml_mode: bool,
    graph_yaml_text: String,
    graph_status_msg: Option<String>,
    graph_needs_refresh: bool,
    // form model
    graph_name: String,
    graph_description: String,
    graph_worker_mode: String,
    graph_worker_loop_profile: String,
    graph_judges: Vec<GraphJudgeForm>,
    graph_agg_policy: String,
    graph_agg_threshold: f64,
    graph_max_iterations: u64,
    graph_max_fix_rounds: u64,
    graph_judge_plain_answers: bool,
    // judge rules sub-editor
    graph_rules_files: Vec<String>,
    graph_rules_selected: String,
    graph_rules_text: String,
    graph_rules_new_name: String,

    // --- Tools (management: catalog + custom YAML tools) ---
    /// 0 = Catalog (all tools), 1 = Custom Tools editor.
    tools_subtab: u8,
    tools_catalog_filter: String,
    tools_show_disabled: bool,
    /// Custom-tool summaries: {filename,name,kind,enabled,valid,error}.
    custom_tools_list: Vec<serde_json::Value>,
    custom_tools_loaded: bool,
    /// tools.config of the active agent-loop profile, for Catalog status chips.
    tools_active_config:
        std::collections::BTreeMap<String, crate::server::services::agent_loop::ToolConfig>,
    custom_tool_selected: String, // filename open in the editor
    custom_tool_yaml: String,
    custom_tool_new_name: String,
    custom_tool_test_args: String,
    custom_tool_test_result: Option<String>,
    custom_tool_status: Option<String>,
    /// Per-tool config YAML editor (Catalog → Configure): the tool being
    /// edited, the profile file it writes to, its YAML buffer and status.
    tool_cfg_editing: Option<String>,
    tool_cfg_profile: String,
    tool_cfg_yaml: String,
    tool_cfg_status: Option<String>,

    // --- MCP Tools ---
    mcp_tools: Vec<McpTool>,
    new_mcp_name: String,
    // Google quick-connect (Gmail / Calendar / Drive via workspace-mcp)
    google_client_id: String,
    google_client_secret: String,
    google_email: String,
    google_svc_gmail: bool,
    google_svc_calendar: bool,
    google_svc_drive: bool,
    google_form_loaded: bool,
    google_status: Arc<Mutex<Option<String>>>,
    new_mcp_url: String,
    new_mcp_headers: String,
    mcp_json_mode: bool,
    mcp_json_text: String,
    mcp_json_error: Option<String>,
    mcp_connection_status: Arc<Mutex<Vec<(String, bool, usize, Option<String>)>>>,

    // --- Remote Instances ---
    remote_enabled: bool,
    vpn_enabled: bool,
    remote_token: String,
    remote_instances: Vec<RemoteInstance>,
    new_remote_name: String,
    new_remote_url: String,
    new_remote_token: String,

    // --- Messaging Bots (Telegram / LINE) ---
    telegram_enabled: bool,
    telegram_bot_token: String,
    /// Comma-separated numeric Telegram user IDs (parsed to a list on save).
    telegram_allowed_ids: String,
    line_enabled: bool,
    line_channel_secret: String,
    line_channel_access_token: String,
    /// Comma-separated LINE user IDs ("U...").
    line_allowed_ids: String,

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

    // --- Browser control ---
    browser_control_enabled: bool,
    browser_engine: String,
    /// Path to the `obscura` binary (used when browser_engine == "obscura").
    browser_obscura_path: String,
    /// None = Auto (follow the server's --headless flag); Some(true)/Some(false)
    /// force the browser headless / headful independently of the server UI.
    browser_headless: Option<bool>,

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
    agent_evaluation_enabled: bool,
    agent_evaluation_threshold: f64,
    agent_evaluation_max_retries: u64,
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

    // --- Theme ---
    theme: crate::ui::theme::Theme,
    theme_loaded: bool,
    theme_status_msg: Option<String>,
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
            model_pool: Vec::new(),
            router_tier: "fast".to_string(),
            router_orchestrator_model: String::new(),
            agent_config_files: Vec::new(),
            selected_agent_config: String::new(),

            loop_profiles: Vec::new(),
            active_loop_profile: String::new(),
            loop_selected_file: String::new(),
            loop_new_name: String::new(),
            loop_yaml_mode: false,
            loop_yaml_text: String::new(),
            loop_status_msg: None,
            loop_needs_refresh: true,
            loop_tool_catalog: Vec::new(),
            loop_skill_catalog: Vec::new(),
            loop_name: String::new(),
            loop_description: String::new(),
            loop_model_model: String::new(),
            loop_model_api_url: String::new(),
            loop_model_api_key: String::new(),
            loop_sp_text: String::new(),
            loop_sp_replace_base: false,
            loop_tools_mode: "all".to_string(),
            loop_tools_checked: std::collections::HashSet::new(),
            loop_tools_config: std::collections::BTreeMap::new(),
            loop_tools_config_new: String::new(),
            loop_tools_config_params_text: std::collections::BTreeMap::new(),
            loop_tools_config_pins_text: std::collections::BTreeMap::new(),
            loop_mcp_mode: "all".to_string(),
            loop_mcp_checked: std::collections::HashSet::new(),
            loop_skills_mode: "all".to_string(),
            loop_skills_checked: std::collections::HashSet::new(),
            loop_max_rounds: 15,
            loop_max_tool_calls: 25,
            loop_temperature: 0.7,
            loop_max_tokens: 81920,
            loop_reflection_enabled: false,
            loop_reflection_threshold: 0.7,
            loop_max_reflection_retries: 2,
            loop_checkpoint_enabled: true,
            loop_max_spawn_depth: 3,
            loop_step_verification: true,
            loop_compact_enabled: true,
            loop_compact_interval: 5,
            loop_compact_window: 10,
            loop_compact_max_context_tokens: 100_000,
            loop_compact_tool_result_max_len: 6000,
            loop_compact_model: String::new(),
            loop_graph_gate: "inherit".to_string(),
            loop_graph_profile: String::new(),
            loop_eval_enabled: false,
            loop_eval_threshold: 0.75,
            loop_eval_max_retries: 2,
            loop_eval_max_fix_rounds: 5,
            loop_eval_max_judge_rounds: 3,
            loop_eval_model: String::new(),
            loop_eval_rubric: String::new(),
            loop_eval_allow_execute: false,

            graph_gate_enabled: false,
            graph_profiles: Vec::new(),
            active_graph_profile: String::new(),
            graph_selected_file: String::new(),
            graph_new_name: String::new(),
            graph_yaml_mode: false,
            graph_yaml_text: String::new(),
            graph_status_msg: None,
            graph_needs_refresh: true,
            graph_name: String::new(),
            graph_description: String::new(),
            graph_worker_mode: "single".to_string(),
            graph_worker_loop_profile: String::new(),
            graph_judges: Vec::new(),
            graph_agg_policy: "all_pass".to_string(),
            graph_agg_threshold: 0.75,
            graph_max_iterations: 2,
            graph_max_fix_rounds: 5,
            graph_judge_plain_answers: true,
            graph_rules_files: Vec::new(),
            graph_rules_selected: String::new(),
            graph_rules_text: String::new(),
            graph_rules_new_name: String::new(),

            tools_subtab: 0,
            tools_catalog_filter: String::new(),
            tools_show_disabled: true,
            custom_tools_list: Vec::new(),
            custom_tools_loaded: false,
            tools_active_config: std::collections::BTreeMap::new(),
            custom_tool_selected: String::new(),
            custom_tool_yaml: String::new(),
            custom_tool_new_name: String::new(),
            custom_tool_test_args: "{}".to_string(),
            custom_tool_test_result: None,
            custom_tool_status: None,
            tool_cfg_editing: None,
            tool_cfg_profile: String::new(),
            tool_cfg_yaml: String::new(),
            tool_cfg_status: None,

            mcp_tools: Vec::new(),
            new_mcp_name: String::new(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
            google_email: String::new(),
            google_svc_gmail: true,
            google_svc_calendar: true,
            google_svc_drive: true,
            google_form_loaded: false,
            google_status: Arc::new(Mutex::new(None)),
            new_mcp_url: String::new(),
            new_mcp_headers: String::new(),
            mcp_json_mode: false,
            mcp_json_text: String::new(),
            mcp_json_error: None,
            mcp_connection_status: Arc::new(Mutex::new(Vec::new())),

            remote_enabled: false,
            vpn_enabled: false,
            remote_token: String::new(),
            remote_instances: Vec::new(),
            new_remote_name: String::new(),
            new_remote_url: String::new(),
            new_remote_token: String::new(),

            telegram_enabled: false,
            telegram_bot_token: String::new(),
            telegram_allowed_ids: String::new(),
            line_enabled: false,
            line_channel_secret: String::new(),
            line_channel_access_token: String::new(),
            line_allowed_ids: String::new(),

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

            browser_control_enabled: false,
            browser_engine: "chrome".to_string(),
            browser_obscura_path: "obscura".to_string(),
            browser_headless: None,

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
            agent_evaluation_enabled: false,
            agent_evaluation_threshold: 0.75,
            agent_evaluation_max_retries: 2,
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

            theme: crate::ui::theme::Theme::default(),
            theme_loaded: false,
            theme_status_msg: None,
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
                            SettingsSection::AgentLoop => self.section_agent_loop(ui, runtime),
                            SettingsSection::Graph => self.section_graph(ui, runtime),
                            SettingsSection::McpTools => self.section_mcp_tools(ui, runtime),
                            SettingsSection::Tools => self.section_tools(ui, runtime),
                            SettingsSection::Plugins => self.section_plugins(ui, ctx, runtime),
                            SettingsSection::Remote => self.section_remote(ui, runtime),
                            SettingsSection::Messaging => self.section_messaging(ui, runtime),
                            SettingsSection::FileTokens => {
                                self.section_file_tokens(ui, ctx, runtime)
                            }
                            SettingsSection::FileMounts => self.section_file_mounts(ui, runtime),
                            SettingsSection::SkillUpdate => self.section_skill_update(ui, runtime),
                            SettingsSection::Security => self.section_security(ui, runtime),
                            SettingsSection::Theme => self.section_theme(ui, ctx),
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
        self.model_pool = settings.model_pool.unwrap_or_default();
        self.router_tier = settings.router_tier.unwrap_or_else(|| "fast".into());
        self.router_orchestrator_model = settings.router_orchestrator_model.unwrap_or_default();
        self.selected_agent_config = settings.sub_agent_config_file.unwrap_or_default();

        // Scan for agent config YAML files (local or remote)
        if crate::server::data::get_remote_backend().is_some() {
            self.agent_config_files = Self::scan_agent_configs_remote();
        } else {
            self.agent_config_files = Self::scan_agent_configs();
        }

        // Agent Loop profile
        self.active_loop_profile = settings.agent_loop_profile.clone().unwrap_or_default();
        self.loop_needs_refresh = true;

        // Graph gate + profile
        self.graph_gate_enabled = settings.graph_enabled.unwrap_or(false);
        self.active_graph_profile = settings.graph_profile.clone().unwrap_or_default();
        self.graph_needs_refresh = true;

        // MCP Tools
        self.mcp_tools = settings.mcp_tools;

        // Remote
        self.remote_enabled = settings.remote_enabled.unwrap_or(false);
        self.vpn_enabled = settings.vpn_enabled.unwrap_or(false);
        self.remote_token = settings.remote_token.unwrap_or_default();
        self.remote_instances = settings.remote_instances.unwrap_or_default();

        // File Mounts
        self.local_file_mounts = settings.local_file_mounts.unwrap_or_default();

        // Messaging bots
        self.telegram_enabled = settings.telegram_enabled.unwrap_or(false);
        self.telegram_bot_token = settings.telegram_bot_token.clone().unwrap_or_default();
        self.telegram_allowed_ids = settings
            .telegram_allowed_user_ids
            .clone()
            .unwrap_or_default()
            .join(", ");
        self.line_enabled = settings.line_enabled.unwrap_or(false);
        self.line_channel_secret = settings.line_channel_secret.clone().unwrap_or_default();
        self.line_channel_access_token =
            settings.line_channel_access_token.clone().unwrap_or_default();
        self.line_allowed_ids = settings
            .line_allowed_user_ids
            .clone()
            .unwrap_or_default()
            .join(", ");

        // Security / Tool Approval
        self.approval_shell = settings.approval_required_for_shell.unwrap_or(true);
        self.approval_python = settings.approval_required_for_python.unwrap_or(true);
        self.approval_file_write = settings.approval_required_for_file_write.unwrap_or(false);
        self.approval_file_delete = settings.approval_required_for_file_delete.unwrap_or(true);
        self.approval_agent_spawn = settings.approval_required_for_agent_spawn.unwrap_or(false);

        // Browser control
        self.browser_control_enabled = settings.browser_control_enabled.unwrap_or(false);
        self.browser_engine = settings
            .browser_engine
            .clone()
            .unwrap_or_else(|| "chrome".to_string());
        self.browser_obscura_path = settings
            .browser_obscura_path
            .clone()
            .unwrap_or_else(|| "obscura".to_string());
        self.browser_headless = settings.browser_headless;

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
        self.agent_evaluation_enabled = settings.extra.get("agentEvaluationEnabled")
            .and_then(|v| v.as_bool()).unwrap_or(false);
        self.agent_evaluation_threshold = settings.extra.get("agentEvaluationThreshold")
            .and_then(|v| v.as_f64()).unwrap_or(0.75);
        self.agent_evaluation_max_retries = settings.extra.get("agentEvaluationMaxRetries")
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
        settings.model_pool = if self.model_pool.is_empty() {
            None
        } else {
            // Backfill a stable id from the label (slugified) when the user
            // didn't set one — pool routing matches agents by `model`, but a
            // non-empty id keeps entries identifiable.
            let pool: Vec<ModelPoolEntry> = self
                .model_pool
                .iter()
                .map(|e| {
                    let mut e = e.clone();
                    if e.id.trim().is_empty() {
                        let base = if e.label.trim().is_empty() { &e.model } else { &e.label };
                        e.id = base
                            .to_lowercase()
                            .chars()
                            .map(|c| if c.is_alphanumeric() { c } else { '_' })
                            .collect::<String>()
                            .trim_matches('_')
                            .to_string();
                    }
                    e
                })
                .collect();
            Some(pool)
        };
        settings.router_tier = Some(self.router_tier.clone());
        settings.router_orchestrator_model = if self.router_orchestrator_model.is_empty() {
            None
        } else {
            Some(self.router_orchestrator_model.clone())
        };
        settings.sub_agent_config_file = if self.selected_agent_config.is_empty() {
            None
        } else {
            Some(self.selected_agent_config.clone())
        };

        // Agent Loop profile ("" = explicit built-in behavior)
        settings.agent_loop_profile = Some(self.active_loop_profile.clone());

        // Graph gate toggle + profile ("" = default)
        settings.graph_enabled = Some(self.graph_gate_enabled);
        settings.graph_profile = Some(self.active_graph_profile.clone());

        // MCP Tools
        settings.mcp_tools = self.mcp_tools.clone();

        // Reconnect MCP servers after saving — on the box that runs them:
        // the remote server when connected, else the local one.
        let mcp_status = self.mcp_connection_status.clone();
        runtime.spawn(async move {
            if let Some(rb) = crate::server::data::get_remote_backend() {
                // Give the settings PUT (block_on below) time to land first.
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let client = reqwest::Client::new();
                if let Ok(resp) = client
                    .post(format!("{}/api/settings/mcp/reconnect-all", rb.url))
                    .bearer_auth(&rb.token)
                    .timeout(std::time::Duration::from_secs(60))
                    .send()
                    .await
                {
                    if let Ok(v) = resp.json::<serde_json::Value>().await {
                        if let Ok(list) = serde_json::from_value::<Vec<(String, bool, usize, Option<String>)>>(v["status"].clone()) {
                            *mcp_status.lock().unwrap() = list;
                        }
                    }
                }
                return;
            }
            use crate::server::services::mcp;
            mcp::disconnect_all().await;
            mcp::init_mcp_servers().await;
            let status = mcp::get_connection_status().await;
            *mcp_status.lock().unwrap() = status;
        });

        // Remote
        settings.remote_enabled = Some(self.remote_enabled);
        settings.vpn_enabled = Some(self.vpn_enabled);
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

        // Messaging bots
        let parse_ids = |s: &str| -> Option<Vec<String>> {
            let ids: Vec<String> = s
                .split([',', '\n', ';', ' '])
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            if ids.is_empty() { None } else { Some(ids) }
        };
        settings.telegram_enabled = Some(self.telegram_enabled);
        settings.telegram_bot_token = if self.telegram_bot_token.trim().is_empty() {
            None
        } else {
            Some(self.telegram_bot_token.trim().to_string())
        };
        settings.telegram_allowed_user_ids = parse_ids(&self.telegram_allowed_ids);
        settings.line_enabled = Some(self.line_enabled);
        settings.line_channel_secret = if self.line_channel_secret.trim().is_empty() {
            None
        } else {
            Some(self.line_channel_secret.trim().to_string())
        };
        settings.line_channel_access_token = if self.line_channel_access_token.trim().is_empty() {
            None
        } else {
            Some(self.line_channel_access_token.trim().to_string())
        };
        settings.line_allowed_user_ids = parse_ids(&self.line_allowed_ids);

        // Security / Tool Approval
        settings.approval_required_for_shell = Some(self.approval_shell);
        settings.approval_required_for_python = Some(self.approval_python);
        settings.approval_required_for_file_write = Some(self.approval_file_write);
        settings.approval_required_for_file_delete = Some(self.approval_file_delete);
        settings.approval_required_for_agent_spawn = Some(self.approval_agent_spawn);

        // Browser control
        settings.browser_control_enabled = Some(self.browser_control_enabled);
        settings.browser_engine = Some(self.browser_engine.clone());
        settings.browser_obscura_path = Some(self.browser_obscura_path.clone());
        settings.browser_headless = self.browser_headless;

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
        settings.extra.insert("agentEvaluationEnabled".into(), serde_json::json!(self.agent_evaluation_enabled));
        settings.extra.insert("agentEvaluationThreshold".into(), serde_json::json!(self.agent_evaluation_threshold));
        settings.extra.insert("agentEvaluationMaxRetries".into(), serde_json::json!(self.agent_evaluation_max_retries));
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
        ui.checkbox(&mut self.agent_evaluation_enabled, "Enable job evaluation (outer loop, tool-using judge)")
            .on_hover_text("After the WHOLE job finishes (all agents done), a judge verifies the final \
                result — it can read output files to check that claimed artifacts exist. Runs once per \
                job for the main agent only, never for sub-agents. Below the threshold, the gap list is \
                fed back so the orchestrator can delegate targeted fixes. Judge model and rubric are \
                configurable per agent-loop profile.");
        if self.agent_evaluation_enabled {
            egui::Grid::new("evaluation_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Evaluation Threshold:");
                    ui.add(egui::Slider::new(&mut self.agent_evaluation_threshold, 0.1..=1.0)
                        .text("score"))
                        .on_hover_text("Job results scoring below this (0-1) trigger a gap-fixing retry. Higher = stricter.");
                    ui.end_row();

                    ui.label("Max Evaluation Retries:");
                    ui.add(egui::Slider::new(&mut self.agent_evaluation_max_retries, 1..=5)
                        .text("rounds"))
                        .on_hover_text("How many judge-and-fix cycles to run before accepting the job result.");
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
                            "router",
                            "graph",
                        ] {
                            ui.selectable_value(
                                &mut self.sub_agent_mode,
                                mode.to_string(),
                                *mode,
                            );
                        }
                    });
                if self.sub_agent_mode == "graph" {
                    ui.label(
                        egui::RichText::new("Judge panel gates the final answer — configure in the Graph tab")
                            .size(10.0)
                            .color(egui::Color32::GRAY),
                    );
                }
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

        // ── Router mode: heterogeneous model pool + tier ──────────────────
        if self.sub_agent_mode == "router" {
            ui.add_space(12.0);
            ui.separator();
            ui.heading("Router Model Pool");
            ui.label(
                egui::RichText::new(
                    "Define the pool of LLMs the orchestrator can route agents to. Each entry \
                     has its own model id, endpoint and key — mix providers freely. Empty \
                     URL/key inherit the main Tiger Bot settings.",
                )
                .size(11.0)
                .color(egui::Color32::GRAY),
            );
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label("Tier:");
                egui::ComboBox::from_id_salt("router_tier_picker")
                    .selected_text(match self.router_tier.as_str() {
                        "ultra" => "Router Ultra (deep teams)",
                        _ => "Router (fast)",
                    })
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.router_tier, "fast".to_string(), "Router (fast)");
                        ui.selectable_value(
                            &mut self.router_tier,
                            "ultra".to_string(),
                            "Router Ultra (deep teams)",
                        );
                    });
            });
            ui.add_space(4.0);

            // Which model the ORCHESTRATOR itself runs on (triage + dispatch +
            // merge). Empty = use the main Tiger Bot model. Workers still get
            // their own per-agent models from the pool.
            ui.horizontal(|ui| {
                ui.label("Orchestrator model:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.router_orchestrator_model)
                        .desired_width(280.0)
                        .hint_text("(blank = main model) e.g. kimi-k2.7-code"),
                );
            });
            ui.label(
                egui::RichText::new(
                    "The orchestrator triages, builds the team and merges results. \
                     Type any model id — if it matches a pool entry it uses that entry's \
                     endpoint/key, otherwise it runs on the main Tiger Bot endpoint/key. \
                     Workers run on their own per-agent models regardless of this.",
                )
                .size(10.0)
                .color(egui::Color32::GRAY),
            );
            ui.add_space(6.0);

            let mut pool_to_delete: Option<usize> = None;
            for (idx, entry) in self.model_pool.iter_mut().enumerate() {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("#{}", idx + 1)).strong());
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("Remove")
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(239, 68, 68)),
                                    ))
                                    .clicked()
                                {
                                    pool_to_delete = Some(idx);
                                }
                            },
                        );
                    });
                    egui::Grid::new(format!("model_pool_grid_{}", idx))
                        .num_columns(2)
                        .spacing([10.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Label:");
                            ui.add(
                                egui::TextEdit::singleline(&mut entry.label)
                                    .desired_width(280.0)
                                    .hint_text("Display name, e.g. Opus (deep)"),
                            );
                            ui.end_row();

                            ui.label("Model:");
                            ui.add(
                                egui::TextEdit::singleline(&mut entry.model)
                                    .desired_width(280.0)
                                    .hint_text("model id, e.g. claude-opus-4-8"),
                            );
                            ui.end_row();

                            ui.label("API URL:");
                            ui.add(
                                egui::TextEdit::singleline(&mut entry.api_url)
                                    .desired_width(280.0)
                                    .hint_text("(optional) inherit if blank"),
                            );
                            ui.end_row();

                            ui.label("API Key:");
                            ui.add(
                                egui::TextEdit::singleline(&mut entry.api_key)
                                    .password(true)
                                    .desired_width(280.0)
                                    .hint_text("(optional) inherit if blank"),
                            );
                            ui.end_row();

                            ui.label("Tier:");
                            egui::ComboBox::from_id_salt(format!("pool_tier_{}", idx))
                                .selected_text(if entry.tier.is_empty() {
                                    "balanced"
                                } else {
                                    entry.tier.as_str()
                                })
                                .width(160.0)
                                .show_ui(ui, |ui| {
                                    for t in &["fast", "balanced", "deep"] {
                                        ui.selectable_value(
                                            &mut entry.tier,
                                            t.to_string(),
                                            *t,
                                        );
                                    }
                                });
                            ui.end_row();

                            ui.label("Strengths:");
                            ui.add(
                                egui::TextEdit::singleline(&mut entry.strengths)
                                    .desired_width(280.0)
                                    .hint_text("hint for the designer, e.g. hard reasoning"),
                            );
                            ui.end_row();
                        });
                });
                ui.add_space(4.0);
            }

            if let Some(idx) = pool_to_delete {
                self.model_pool.remove(idx);
            }

            if ui.button("+ Add Model").clicked() {
                self.model_pool.push(ModelPoolEntry {
                    tier: "balanced".to_string(),
                    ..Default::default()
                });
            }
        }

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
            if let Some(env) = &tool.env {
                if !env.is_empty() {
                    let e: serde_json::Map<String, serde_json::Value> = env
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect();
                    entry.insert("env".into(), serde_json::Value::Object(e));
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
            let env = obj.get("env").and_then(|v| v.as_object()).map(|h| {
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
                env,
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

        self.google_quick_connect_card(ui, runtime);
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

    /// One-click Google connect: Gmail / Calendar / Drive through the
    /// workspace-mcp server. Installs uvx if needed, writes the MCP entry
    /// (with OAuth client env vars), connects, and opens the browser for the
    /// Google login.
    fn google_quick_connect_card(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        use crate::server::services::google;

        // Prefill from an existing "google" entry once.
        if !self.google_form_loaded {
            self.google_form_loaded = true;
            if let Some(entry) = self.mcp_tools.iter().find(|t| t.name == google::GOOGLE_MCP_NAME) {
                if let Some(env) = &entry.env {
                    self.google_client_id = env.get("GOOGLE_OAUTH_CLIENT_ID").cloned().unwrap_or_default();
                    self.google_client_secret = env.get("GOOGLE_OAUTH_CLIENT_SECRET").cloned().unwrap_or_default();
                    self.google_email = env.get("USER_GOOGLE_EMAIL").cloned().unwrap_or_default();
                }
                if let Some(args) = &entry.args {
                    self.google_svc_gmail = args.iter().any(|a| a == "gmail");
                    self.google_svc_calendar = args.iter().any(|a| a == "calendar");
                    self.google_svc_drive = args.iter().any(|a| a == "drive");
                }
            }
        }

        let connected = self
            .mcp_connection_status
            .lock()
            .unwrap()
            .iter()
            .any(|(name, ok, _, _)| name == google::GOOGLE_MCP_NAME && *ok);

        egui::CollapsingHeader::new(if connected {
            "🇬 Google — Gmail · Calendar · Drive  ✅ connected"
        } else {
            "🇬 Google — Gmail · Calendar · Drive (quick connect)"
        })
        .default_open(!connected && self.mcp_tools.iter().all(|t| t.name != google::GOOGLE_MCP_NAME))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Let the agent read/send Gmail, manage Calendar and browse Drive. \
                     One-time setup: paste a Google OAuth Client ID, then log in with Google in your browser.",
                )
                .size(12.0)
                .color(egui::Color32::GRAY),
            );
            ui.add_space(6.0);

            // Step 1 — credentials
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("1.").strong());
                if ui.button("Get credentials (opens Google Cloud Console)").clicked() {
                    let _ = open::that(google::GOOGLE_CONSOLE_URL);
                }
            });
            ui.label(
                egui::RichText::new(
                    "   In the console: enable the Gmail, Calendar and Drive APIs → Create Credentials → \
                     OAuth client ID → Application type “Desktop app” → copy the Client ID (and Secret).",
                )
                .size(11.0)
                .color(egui::Color32::GRAY),
            );
            ui.add_space(4.0);

            egui::Grid::new("google_qc_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Client ID:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.google_client_id)
                        .desired_width(360.0)
                        .hint_text("xxxxx.apps.googleusercontent.com"),
                );
                ui.end_row();
                ui.label("Client secret:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.google_client_secret)
                        .desired_width(360.0)
                        .password(true)
                        .hint_text("optional for Desktop-app clients"),
                );
                ui.end_row();
                ui.label("Google email:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.google_email)
                        .desired_width(360.0)
                        .hint_text("you@gmail.com (optional, recommended)"),
                );
                ui.end_row();
                ui.label("Services:");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.google_svc_gmail, "Gmail");
                    ui.checkbox(&mut self.google_svc_calendar, "Calendar");
                    ui.checkbox(&mut self.google_svc_drive, "Drive");
                });
                ui.end_row();
            });
            ui.add_space(6.0);

            // Step 2 — runtime (uvx)
            let uvx = google::find_uvx();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("2.").strong());
                if let Some(path) = &uvx {
                    ui.label(
                        egui::RichText::new(format!("Runtime ready: {}", path))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(34, 197, 94)),
                    );
                } else if ui.button("Install uv runtime (automatic)").clicked() {
                    let status = self.google_status.clone();
                    *status.lock().unwrap() = Some("Installing uv…".to_string());
                    runtime.spawn(async move {
                        let msg = match google::install_uv().await {
                            Ok(path) => format!("✅ uv installed ({path}). Now press Connect."),
                            Err(e) => format!("❌ {e}"),
                        };
                        *status.lock().unwrap() = Some(msg);
                    });
                }
            });
            ui.add_space(6.0);

            // Step 3 — connect + login
            let ready = !self.google_client_id.trim().is_empty()
                && (self.google_svc_gmail || self.google_svc_calendar || self.google_svc_drive)
                && uvx.is_some();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("3.").strong());
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(
                            egui::RichText::new("Connect & Login with Google (opens browser)").strong(),
                        ),
                    )
                    .clicked()
                {
                    let entry = google::build_google_mcp_entry(
                        &self.google_client_id,
                        &self.google_client_secret,
                        &self.google_email,
                        self.google_svc_gmail,
                        self.google_svc_calendar,
                        self.google_svc_drive,
                    );
                    if let Some(existing) =
                        self.mcp_tools.iter_mut().find(|t| t.name == google::GOOGLE_MCP_NAME)
                    {
                        *existing = entry;
                    } else {
                        self.mcp_tools.push(entry);
                    }
                    // Persist + reconnect all MCP servers (save_all_settings does both).
                    self.save_all_settings(runtime);

                    let status = self.google_status.clone();
                    let email = self.google_email.clone();
                    *status.lock().unwrap() =
                        Some("Connecting to Google MCP server…".to_string());
                    runtime.spawn(async move {
                        let result = crate::server::services::google::start_login(&email).await;
                        if let Some(url) = result["url"].as_str() {
                            let _ = open::that(url);
                        }
                        let msg = result["message"].as_str().unwrap_or("done").to_string();
                        let ok = result["ok"].as_bool().unwrap_or(false);
                        *status.lock().unwrap() =
                            Some(format!("{} {}", if ok { "✅" } else { "❌" }, msg));
                    });
                }
                if !ready && uvx.is_some() && self.google_client_id.trim().is_empty() {
                    ui.label(
                        egui::RichText::new("paste a Client ID first")
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                }
            });

            if let Some(msg) = self.google_status.lock().unwrap().clone() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(msg).size(12.0));
            }
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "Login opens accounts.google.com in your browser; tokens are stored locally \
                     (~/.google_workspace_mcp/credentials) and refresh automatically. \
                     Ask the agent things like “what's on my calendar tomorrow?” or “find the PDF in my Drive”.",
                )
                .size(11.0)
                .color(egui::Color32::GRAY),
            );
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
                    env: None,
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
        ui.heading("Remote Connection Method");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Reach this host over a private Tailscale VPN instead of a public \
                 Cloudflare tunnel. VPN and tunnel are alternatives — use one.",
            )
            .size(11.0)
            .color(egui::Color32::GRAY),
        );
        ui.checkbox(&mut self.vpn_enabled, "Use VPN (Tailscale) for remote connect");

        // Show the detected tailnet address (cached; populated at boot / Start).
        let vpn = runtime.block_on(crate::server::services::vpn::get_vpn_state());
        let vpn_url = vpn["url"].as_str().unwrap_or("").to_string();
        ui.horizontal(|ui| {
            ui.label("Connect address:");
            ui.label(
                egui::RichText::new(if vpn_url.is_empty() { "(not detected)" } else { &vpn_url })
                    .monospace()
                    .color(egui::Color32::GRAY),
            );
            if !vpn_url.is_empty() && ui.button("Copy").clicked() {
                ui.ctx().copy_text(vpn_url.clone());
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Start VPN").clicked() {
                let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3001u16);
                runtime.block_on(crate::server::services::vpn::start_vpn(port));
            }
            if ui.button("Stop VPN").clicked() {
                runtime.block_on(crate::server::services::vpn::stop_vpn());
            }
            if let Some(auth) = vpn["authUrl"].as_str() {
                ui.hyperlink_to("Authenticate Tailscale", auth);
            } else if vpn["running"].as_bool().unwrap_or(false) {
                ui.label(egui::RichText::new("● connected").color(egui::Color32::from_rgb(74, 222, 128)));
            }
        });

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
    //  Messaging Bots (Telegram / LINE)
    // ==================================================================

    fn section_messaging(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);

        // ---------------- Telegram ----------------
        ui.add_space(8.0);
        ui.heading("Telegram Bot");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Chat with the agent from Telegram and control it with /agents, /model, /mode, \
                 /loop, /new, /stop, /status. Create a bot with @BotFather to get a token. \
                 Uses long-polling — no public URL needed.",
            )
            .size(12.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(8.0);

        ui.checkbox(&mut self.telegram_enabled, "Enable Telegram bot");
        ui.add_space(6.0);

        egui::Grid::new("telegram_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Bot token:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.telegram_bot_token)
                        .desired_width(320.0)
                        .password(true)
                        .hint_text("123456789:AA...")
                        .font(egui::TextStyle::Monospace),
                );
                ui.end_row();

                ui.label("Allowed user IDs:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.telegram_allowed_ids)
                        .desired_width(320.0)
                        .hint_text("12345678, 87654321")
                        .font(egui::TextStyle::Monospace),
                )
                .on_hover_text(
                    "Comma-separated numeric Telegram user IDs. Empty = nobody can use the \
                     bot (fail closed). Message the bot once — the rejection reply shows \
                     your ID to copy here.",
                );
                ui.end_row();
            });

        // Live connection status (from the local Telegram supervisor).
        let tg = runtime.block_on(
            crate::server::services::messaging::telegram::get_status(),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Status:");
            if tg.connected {
                ui.label(
                    egui::RichText::new(format!(
                        "● connected as {}",
                        tg.bot_username.as_deref().unwrap_or("?")
                    ))
                    .color(egui::Color32::from_rgb(74, 222, 128)),
                );
            } else if let Some(err) = &tg.error {
                ui.label(
                    egui::RichText::new(format!("● {}", err))
                        .color(egui::Color32::from_rgb(239, 68, 68)),
                );
            } else {
                ui.label(
                    egui::RichText::new(if self.telegram_enabled {
                        "● connecting… (applies within ~30s of saving)"
                    } else {
                        "● disabled"
                    })
                    .color(egui::Color32::GRAY),
                );
            }
        });

        // ---------------- LINE ----------------
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("LINE Bot");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Chat with the agent from LINE (same commands as Telegram). Create a \
                 Messaging API channel in the LINE Developers console, then paste the \
                 webhook URL below into the console. Requires the Cloudflare tunnel for \
                 a public URL.",
            )
            .size(12.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(8.0);

        ui.checkbox(&mut self.line_enabled, "Enable LINE bot");
        ui.add_space(6.0);

        egui::Grid::new("line_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Channel secret:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.line_channel_secret)
                        .desired_width(320.0)
                        .password(true)
                        .font(egui::TextStyle::Monospace),
                )
                .on_hover_text("LINE Developers Console > channel > Basic settings > Channel secret. Used to verify webhook signatures.");
                ui.end_row();

                ui.label("Channel access token:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.line_channel_access_token)
                        .desired_width(320.0)
                        .password(true)
                        .font(egui::TextStyle::Monospace),
                )
                .on_hover_text("LINE Developers Console > channel > Messaging API > Channel access token (long-lived).");
                ui.end_row();

                ui.label("Allowed user IDs:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.line_allowed_ids)
                        .desired_width(320.0)
                        .hint_text("U4af4980629..., U1234...")
                        .font(egui::TextStyle::Monospace),
                )
                .on_hover_text(
                    "Comma-separated LINE user IDs (start with 'U'). Empty = nobody can \
                     use the bot (fail closed). Message the bot once — the rejection \
                     reply shows your ID to copy here.",
                );
                ui.end_row();
            });

        // Webhook URL via the Cloudflare tunnel.
        ui.add_space(8.0);
        let tunnel = runtime.block_on(crate::server::services::tunnel::get_tunnel_state());
        let tunnel_running = tunnel["running"].as_bool().unwrap_or(false);
        let webhook_url = tunnel["url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .map(|u| format!("{}/line/webhook", u.trim_end_matches('/')));
        ui.horizontal(|ui| {
            ui.label("Webhook URL:");
            match &webhook_url {
                Some(url) => {
                    ui.label(
                        egui::RichText::new(url.as_str())
                            .monospace()
                            .color(egui::Color32::GRAY),
                    );
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(url.clone());
                    }
                }
                None => {
                    ui.label(
                        egui::RichText::new("(start the tunnel to get a public URL)")
                            .color(egui::Color32::GRAY),
                    );
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Start tunnel").clicked() {
                let port = std::env::var("PORT")
                    .ok()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(3001u16);
                runtime.block_on(crate::server::services::tunnel::start_tunnel(port));
            }
            if ui.button("Stop tunnel").clicked() {
                runtime.block_on(crate::server::services::tunnel::stop_tunnel());
            }
            if tunnel_running {
                ui.label(
                    egui::RichText::new("● tunnel running")
                        .color(egui::Color32::from_rgb(74, 222, 128)),
                );
            } else if let Some(err) = tunnel["error"].as_str().filter(|e| !e.is_empty()) {
                ui.label(
                    egui::RichText::new(format!("● {}", err))
                        .color(egui::Color32::from_rgb(239, 68, 68)),
                );
            }
        });
        ui.label(
            egui::RichText::new(
                "Quick tunnels get a NEW URL each time they start — re-paste the webhook \
                 URL into the LINE console after restarting the tunnel, then use the \
                 console's Verify button.",
            )
            .size(11.0)
            .color(egui::Color32::GRAY),
        );

        // ---------------- Save ----------------
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if Self::save_button(ui, "Save Messaging Settings") {
                self.save_all_settings(runtime);
                self.api_status_msg = Some(
                    "Messaging settings saved! Telegram applies within ~30s; LINE applies immediately."
                        .to_string(),
                );
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
        ui.heading("Browser Control");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Let the agent drive a real web browser (navigate, click, type, screenshot). \
                 Off by default for safety: once on, the agent can act in the browser — submit \
                 forms, click buttons, use logged-in sessions — as you. Requires Node.js (npx).",
            )
            .size(12.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(8.0);

        let mut browser_changed = false;
        if ui
            .checkbox(&mut self.browser_control_enabled, "Enable browser control")
            .changed()
        {
            browser_changed = true;
        }

        if self.browser_control_enabled {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Engine:");
                if ui
                    .radio_value(&mut self.browser_engine, "chromium".to_string(), "Chromium")
                    .on_hover_text("Playwright's bundled Chromium — portable, always available.")
                    .changed()
                {
                    browser_changed = true;
                }
                if ui
                    .radio_value(&mut self.browser_engine, "chrome".to_string(), "Chrome")
                    .on_hover_text("Your installed Google Chrome — fewer bot blocks, must be installed.")
                    .changed()
                {
                    browser_changed = true;
                }
                if ui
                    .radio_value(&mut self.browser_engine, "obscura".to_string(), "Obscura")
                    .on_hover_text(
                        "Stealthy Rust headless browser (github.com/h4ckf0r0day/obscura), \
                         driven via its `obscura mcp` mode. Always headless; install the \
                         `obscura` binary separately.",
                    )
                    .changed()
                {
                    browser_changed = true;
                }
            });

            // Obscura: let the user point at the binary if it isn't on PATH.
            if self.browser_engine == "obscura" {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Obscura binary:");
                    if ui
                        .text_edit_singleline(&mut self.browser_obscura_path)
                        .on_hover_text("Path to the `obscura` binary. Default \"obscura\" resolves from PATH.")
                        .changed()
                    {
                        browser_changed = true;
                    }
                });
            }

            // Window (headless) mode only applies to Playwright's Chromium/Chrome.
            // Obscura is always headless, so hide these controls when it's picked.
            if self.browser_engine != "obscura" {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Window:");
                if ui
                    .radio_value(&mut self.browser_headless, None, "Auto")
                    .on_hover_text("Follow the server: headless when started with --headless, otherwise headful.")
                    .changed()
                {
                    browser_changed = true;
                }
                if ui
                    .radio_value(&mut self.browser_headless, Some(false), "Real browser")
                    .on_hover_text(
                        "Force a real, visible (headful) browser even on a UI-less server. \
                         Trips far fewer bot blocks (Google etc.). On a server with no display, \
                         run TigrimOS under a virtual one, e.g. `xvfb-run`.",
                    )
                    .changed()
                {
                    browser_changed = true;
                }
                if ui
                    .radio_value(&mut self.browser_headless, Some(true), "Headless")
                    .on_hover_text("Force headless (no window). More likely to be blocked by Google and others.")
                    .changed()
                {
                    browser_changed = true;
                }
            });
            } // end "Window" controls (Chromium/Chrome only)

            ui.add_space(4.0);
            let help = if self.browser_engine == "obscura" {
                "Obscura is a stealthy Rust headless browser. Install the `obscura` binary \
                 separately (github.com/h4ckf0r0day/obscura — release, `cargo install`, or AUR) \
                 so it's on PATH, or set its path above. It's always headless and runs with \
                 --stealth (anti-detection + tracker blocking)."
            } else {
                "Uses a dedicated browser profile (kept under the app data dir), so your \
                 everyday browsing stays separate. First use downloads the browser on demand. \
                 To beat Google's headless blocking on Ubuntu: pick \"Chrome\" + \"Real browser\" \
                 and launch the server under `xvfb-run -a ./TigrimOS --headless`."
            };
            ui.label(
                egui::RichText::new(help)
                    .size(11.0)
                    .color(egui::Color32::GRAY),
            );
        }

        if browser_changed {
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

    /// One color-picker row inside the theme editor grid. Returns true if the
    /// color changed. `field` is the `#RRGGBB` hex string kept in the theme.
    fn theme_color_row(
        ui: &mut egui::Ui,
        label: &str,
        field: &mut String,
        fallback: egui::Color32,
    ) -> bool {
        ui.label(label);
        let mut c = crate::ui::theme::hex_to_color(field, fallback);
        let mut changed = false;
        if ui.color_edit_button_srgba(&mut c).changed() {
            *field = crate::ui::theme::color_to_hex(c);
            changed = true;
        }
        ui.label(
            egui::RichText::new(field.clone())
                .size(11.0)
                .monospace()
                .color(egui::Color32::GRAY),
        );
        ui.end_row();
        changed
    }

    fn section_theme(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use crate::ui::theme::Theme;

        // Lazy-load the persisted theme the first time this section opens so the
        // editor reflects what is actually saved on disk (not just defaults).
        if !self.theme_loaded {
            self.theme = Theme::load();
            self.theme_loaded = true;
        }

        ui.add_space(8.0);
        ui.heading("Appearance");
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Customize colors and font sizes. Changes preview instantly; click \
                 \"Save to theme.yaml\" to persist them across restarts.",
            )
            .size(12.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(12.0);

        let mut changed = false;

        // ── Presets ─────────────────────────────────────────────────
        ui.label(egui::RichText::new("Preset").strong());
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Pick a ready-made look (you can still fine-tune colors below).")
                .size(12.0)
                .color(egui::Color32::GRAY),
        );
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            for &name in crate::ui::theme::preset_names() {
                if ui.button(name).clicked() {
                    if self.theme.apply_preset(name) {
                        self.theme.apply_fonts(ctx);
                        self.theme.apply(ctx);
                        changed = true;
                        self.theme_status_msg =
                            Some(format!("Applied '{}' preset — click Save to keep it", name));
                    }
                }
            }
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label(egui::RichText::new("Colors").strong());
        ui.add_space(6.0);

        egui::Grid::new("theme_colors_grid")
            .num_columns(3)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                let c = &mut self.theme.colors;
                changed |= Self::theme_color_row(ui, "Surface (panels/windows):", &mut c.surface, egui::Color32::from_rgb(251, 247, 241));
                changed |= Self::theme_color_row(ui, "Canvas (inputs/fields):", &mut c.canvas, egui::Color32::from_rgb(244, 238, 229));
                changed |= Self::theme_color_row(ui, "Card fill:", &mut c.card, egui::Color32::WHITE);
                changed |= Self::theme_color_row(ui, "Hover background:", &mut c.hover, egui::Color32::from_rgb(239, 231, 218));
                changed |= Self::theme_color_row(ui, "Border / separators:", &mut c.border, egui::Color32::from_rgb(230, 220, 204));
                changed |= Self::theme_color_row(ui, "Primary text:", &mut c.text_primary, egui::Color32::from_rgb(52, 48, 42));
                changed |= Self::theme_color_row(ui, "Secondary text:", &mut c.text_secondary, egui::Color32::from_rgb(124, 115, 104));
                changed |= Self::theme_color_row(ui, "Accent:", &mut c.accent, egui::Color32::from_rgb(18, 154, 145));
                changed |= Self::theme_color_row(ui, "Accent (pressed):", &mut c.accent_hover, egui::Color32::from_rgb(12, 129, 122));
                let accent_fb = crate::ui::theme::hex_to_color(&c.accent, egui::Color32::from_rgb(18, 154, 145));
                let card_fb = crate::ui::theme::hex_to_color(&c.card, egui::Color32::WHITE);
                changed |= Self::theme_color_row(ui, "User bubble:", &mut c.user_bubble, accent_fb);
                changed |= Self::theme_color_row(ui, "AI bubble:", &mut c.ai_bubble, card_fb);
            });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Font sizes").strong());
        ui.add_space(6.0);

        egui::Grid::new("theme_fonts_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                let f = &mut self.theme.fonts;
                ui.label("Chat messages:");
                if ui.add(egui::Slider::new(&mut f.chat, 10.0..=28.0).suffix(" pt")).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label("Body:");
                if ui.add(egui::Slider::new(&mut f.body, 10.0..=24.0).suffix(" pt")).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label("Small:");
                if ui.add(egui::Slider::new(&mut f.small, 8.0..=20.0).suffix(" pt")).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label("Button:");
                if ui.add(egui::Slider::new(&mut f.button, 10.0..=22.0).suffix(" pt")).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label("Heading:");
                if ui.add(egui::Slider::new(&mut f.heading, 14.0..=36.0).suffix(" pt")).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label("Monospace:");
                if ui.add(egui::Slider::new(&mut f.monospace, 9.0..=22.0).suffix(" pt")).changed() {
                    changed = true;
                }
                ui.end_row();
            });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Font family").strong());
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Pick a bundled modern web font (Inter is what Vite/VitePress uses), \
                 or load your own .ttf / .otf / .ttc file. Thai / emoji glyphs always \
                 fall back automatically; code blocks use JetBrains Mono.",
            )
            .size(12.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(6.0);

        // True when the font *family* changes — rebuilding the glyph atlas is
        // heavier than a size/color tweak, so we re-apply fonts only then.
        let mut font_changed = false;
        let using_custom = !self.theme.custom_font_path.is_empty();

        ui.horizontal(|ui| {
            ui.label("Font:");
            let selected_label = if using_custom {
                "Custom file…".to_string()
            } else {
                self.theme.font_family.clone()
            };
            egui::ComboBox::from_id_salt("theme_font_family")
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    for &name in crate::ui::theme::bundled_font_names() {
                        let is_sel = !using_custom && self.theme.font_family == name;
                        if ui.selectable_label(is_sel, name).clicked() {
                            self.theme.font_family = name.to_string();
                            self.theme.custom_font_path.clear();
                            font_changed = true;
                        }
                    }
                });
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("📂 Choose custom file…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Select a font file")
                    .add_filter("Fonts", &["ttf", "otf", "ttc", "TTF", "OTF", "TTC"])
                    .pick_file()
                {
                    self.theme.custom_font_path = path.display().to_string();
                    font_changed = true;
                }
            }
            if using_custom && ui.button("↺ Use bundled font").clicked() {
                self.theme.custom_font_path.clear();
                font_changed = true;
            }
        });

        ui.add_space(4.0);
        let current_font = if using_custom {
            self.theme.custom_font_path.clone()
        } else {
            format!("{} (bundled)", self.theme.font_family)
        };
        ui.label(
            egui::RichText::new(format!("Current: {}", current_font))
                .size(11.0)
                .monospace()
                .color(egui::Color32::GRAY),
        );

        if font_changed {
            self.theme.apply_fonts(ctx);
            changed = true;
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Output files (graphs & pictures)").strong());
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Choose how files the AI produces are shown: in a side panel, or \
                 embedded inline in the chat (click an image to view it full size).",
            )
            .size(12.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(6.0);
        let mut is_embed = self.theme.file_display.trim() == "chat";
        let before = is_embed;
        ui.horizontal(|ui| {
            ui.radio_value(&mut is_embed, false, "Side output panel (default)");
            ui.add_space(12.0);
            ui.radio_value(&mut is_embed, true, "Embedded in chat");
        });
        if is_embed != before {
            self.theme.file_display = if is_embed { "chat" } else { "panel" }.to_string();
            changed = true;
        }

        // Live preview: re-apply to the running context as soon as anything changes.
        if changed {
            self.theme.apply(ctx);
            self.theme_status_msg = Some("Previewing — not yet saved".to_string());
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if ui.button("💾 Save to theme.yaml").clicked() {
                match self.theme.save() {
                    Ok(()) => {
                        self.theme.apply(ctx);
                        self.theme_status_msg =
                            Some(format!("Saved to {}", Theme::path().display()));
                    }
                    Err(e) => {
                        self.theme_status_msg = Some(format!("Save failed: {}", e));
                    }
                }
            }
            if ui.button("↻ Reload from file").clicked() {
                self.theme = Theme::load();
                self.theme.apply_fonts(ctx);
                self.theme.apply(ctx);
                self.theme_status_msg = Some("Reloaded from theme.yaml".to_string());
            }
            if ui.button("⟲ Reset to defaults").clicked() {
                self.theme = Theme::default();
                self.theme.apply_fonts(ctx);
                self.theme.apply(ctx);
                self.theme_status_msg =
                    Some("Reset to defaults (click Save to persist)".to_string());
            }
        });

        if let Some(msg) = &self.theme_status_msg {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(msg).size(12.0).color(egui::Color32::GRAY));
        }

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!("File: {}", Theme::path().display()))
                .size(11.0)
                .monospace()
                .color(egui::Color32::GRAY),
        );
    }

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
                egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                    .size(14.0)
                    .color(egui::Color32::GRAY),
            );
            ui.label("AI Agent Workspace with Remote Agents");
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(12.0);
            for line in [
                concat!("TigrimOS v", env!("CARGO_PKG_VERSION"), " (Rust/egui edition)"),
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

// ---------------------------------------------------------------------------
//  Agent Loop profile editor (Settings > Agent Loop)
// ---------------------------------------------------------------------------

impl SettingsView {
    fn scan_loop_profiles() -> Vec<String> {
        let dir = crate::server::services::agent_loop::agent_loops_dir();
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

    fn scan_loop_profiles_remote() -> Vec<String> {
        let Some(rb) = crate::server::data::get_remote_backend() else {
            return Vec::new();
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        client
            .get(format!("{}/api/agent-loops", rb.url))
            .bearer_auth(&rb.token)
            .send()
            .ok()
            .and_then(|r| r.json::<serde_json::Value>().ok())
            .and_then(|v| v.as_array().cloned())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e["filename"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn read_loop_profile_content(filename: &str) -> Option<String> {
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            return client
                .get(format!("{}/api/agent-loops/{}", rb.url, filename))
                .bearer_auth(&rb.token)
                .send()
                .ok()
                .and_then(|r| r.json::<serde_json::Value>().ok())
                .and_then(|v| v["content"].as_str().map(|s| s.to_string()));
        }
        std::fs::read_to_string(
            crate::server::services::agent_loop::agent_loops_dir().join(filename),
        )
        .ok()
    }

    fn write_loop_profile_content(filename: &str, content: &str) -> Result<(), String> {
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            let resp = client
                .post(format!("{}/api/agent-loops", rb.url))
                .bearer_auth(&rb.token)
                .json(&serde_json::json!({"filename": filename, "content": content}))
                .send()
                .map_err(|e| e.to_string())?;
            if resp.status().is_success() {
                return Ok(());
            }
            let err = resp
                .json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["error"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "Save failed".to_string());
            return Err(err);
        }
        let dir = crate::server::services::agent_loop::agent_loops_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(filename), content).map_err(|e| e.to_string())
    }

    fn refresh_loop_profiles(&mut self) {
        if crate::server::data::get_remote_backend().is_some() {
            self.loop_profiles = Self::scan_loop_profiles_remote();
        } else {
            crate::server::services::agent_loop::ensure_default_profile();
            self.loop_profiles = Self::scan_loop_profiles();
        }
        self.loop_tool_catalog = crate::server::services::toolbox::tool_catalog();
        // Installed skills for the skills filter checkboxes (local registry;
        // remote backends still allow typing names in YAML mode).
        self.loop_skill_catalog = std::fs::read_to_string(
            crate::server::data::data_dir().join("skills.json"),
        )
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
        // Open the active profile (or the first available) in the editor.
        if self.loop_selected_file.is_empty()
            || !self.loop_profiles.contains(&self.loop_selected_file)
        {
            let initial = if !self.active_loop_profile.is_empty()
                && self.loop_profiles.contains(&self.active_loop_profile)
            {
                self.active_loop_profile.clone()
            } else {
                self.loop_profiles.first().cloned().unwrap_or_default()
            };
            self.loop_selected_file = initial;
        }
        if !self.loop_selected_file.is_empty() {
            let file = self.loop_selected_file.clone();
            self.open_loop_profile(&file);
        }
    }

    fn open_loop_profile(&mut self, filename: &str) {
        self.loop_selected_file = filename.to_string();
        let content = Self::read_loop_profile_content(filename).unwrap_or_default();
        self.loop_yaml_text = content.clone();
        match serde_yaml::from_str::<crate::server::services::agent_loop::AgentLoopProfile>(&content) {
            Ok(p) => {
                self.populate_loop_form(&p);
                self.loop_status_msg = None;
            }
            Err(e) => {
                self.loop_status_msg = Some(format!("Error parsing {}: {}", filename, e));
                self.loop_yaml_mode = true; // let the user fix the raw YAML
            }
        }
    }

    fn populate_loop_form(&mut self, p: &crate::server::services::agent_loop::AgentLoopProfile) {
        self.loop_name = p.name.clone();
        self.loop_description = p.description.clone();
        let m = p.model.clone().unwrap_or_default();
        self.loop_model_model = m.model;
        self.loop_model_api_url = m.api_url;
        self.loop_model_api_key = m.api_key;
        let sp = p.system_prompt.clone().unwrap_or_default();
        self.loop_sp_text = sp.text;
        self.loop_sp_replace_base = sp.replace_base;
        let tf = p.tools.clone().unwrap_or_default();
        self.loop_tools_mode = if tf.mode.is_empty() { "all".into() } else { tf.mode };
        self.loop_tools_checked = tf.list.into_iter().collect();
        self.loop_tools_config = tf.config;
        self.loop_tools_config_new.clear();
        let json_text = |m: &Option<serde_json::Map<String, serde_json::Value>>| {
            m.as_ref()
                .map(|m| serde_json::Value::Object(m.clone()).to_string())
                .unwrap_or_default()
        };
        self.loop_tools_config_params_text = self
            .loop_tools_config
            .iter()
            .map(|(n, c)| (n.clone(), json_text(&c.params)))
            .collect();
        self.loop_tools_config_pins_text = self
            .loop_tools_config
            .iter()
            .map(|(n, c)| (n.clone(), json_text(&c.pinned_params)))
            .collect();
        let mf = p.mcp.clone().unwrap_or_default();
        self.loop_mcp_mode = if mf.mode.is_empty() { "all".into() } else { mf.mode };
        self.loop_mcp_checked = mf.servers.into_iter().collect();
        let sf = p.skills.clone().unwrap_or_default();
        self.loop_skills_mode = if sf.mode.is_empty() { "all".into() } else { sf.mode };
        self.loop_skills_checked = sf.list.into_iter().collect();
        let k = p.loop_.clone().unwrap_or_default();
        self.loop_max_rounds = k.max_rounds.unwrap_or(self.agent_max_turns);
        self.loop_max_tool_calls = k.max_tool_calls.unwrap_or(self.agent_max_tool_calls);
        self.loop_temperature = k.temperature.unwrap_or(self.agent_temperature);
        self.loop_max_tokens = k.max_tokens.unwrap_or(self.agent_max_tokens);
        self.loop_reflection_enabled = k.reflection_enabled.unwrap_or(self.agent_reflection_enabled);
        self.loop_reflection_threshold = k.reflection_threshold.unwrap_or(self.agent_reflection_threshold);
        self.loop_max_reflection_retries = k.max_reflection_retries.unwrap_or(self.agent_max_reflection_retries);
        self.loop_checkpoint_enabled = k.checkpoint_enabled.unwrap_or(true);
        self.loop_max_spawn_depth = k.max_spawn_depth.unwrap_or(3);
        self.loop_step_verification = k.step_verification.unwrap_or(self.agent_step_verify_enabled);
        let c = p.compaction.clone().unwrap_or_default();
        self.loop_compact_enabled = c.enabled.unwrap_or(true);
        self.loop_compact_interval = c.interval.unwrap_or(self.agent_compression_interval);
        self.loop_compact_window = c.window.unwrap_or(10);
        self.loop_compact_max_context_tokens = c.max_context_tokens.unwrap_or(self.agent_max_context_tokens);
        self.loop_compact_tool_result_max_len = c.tool_result_max_len.unwrap_or(self.agent_tool_result_max_len);
        self.loop_compact_model = c.model.unwrap_or_default();
        let e = p.evaluation.clone().unwrap_or_default();
        self.loop_eval_enabled = e.enabled.unwrap_or(self.agent_evaluation_enabled);
        self.loop_eval_threshold = e.threshold.unwrap_or(self.agent_evaluation_threshold);
        self.loop_eval_max_retries = e.max_retries.unwrap_or(self.agent_evaluation_max_retries);
        self.loop_eval_max_fix_rounds = e.max_fix_rounds.unwrap_or(5);
        self.loop_eval_max_judge_rounds = e.max_judge_rounds.unwrap_or(3);
        self.loop_eval_model = e.model.unwrap_or_default();
        self.loop_eval_rubric = e.rubric.unwrap_or_default();
        self.loop_eval_allow_execute = e.allow_execute.unwrap_or(false);
        let g = p.graph.clone().unwrap_or_default();
        self.loop_graph_gate = match g.enabled {
            Some(true) => "on".to_string(),
            Some(false) => "off".to_string(),
            None => "inherit".to_string(),
        };
        self.loop_graph_profile = g.profile.unwrap_or_default();
    }

    fn loop_form_to_profile(&self) -> crate::server::services::agent_loop::AgentLoopProfile {
        use crate::server::services::agent_loop::*;
        let mut sorted = |set: &std::collections::HashSet<String>| -> Vec<String> {
            let mut v: Vec<String> = set.iter().cloned().collect();
            v.sort();
            v
        };
        AgentLoopProfile {
            name: self.loop_name.clone(),
            description: self.loop_description.clone(),
            model: if self.loop_model_model.trim().is_empty()
                && self.loop_model_api_url.trim().is_empty()
                && self.loop_model_api_key.trim().is_empty()
            {
                None
            } else {
                Some(ModelOverride {
                    model: self.loop_model_model.trim().to_string(),
                    api_url: self.loop_model_api_url.trim().to_string(),
                    api_key: self.loop_model_api_key.trim().to_string(),
                })
            },
            system_prompt: if self.loop_sp_text.trim().is_empty() {
                None
            } else {
                Some(SystemPromptOverride {
                    text: self.loop_sp_text.clone(),
                    replace_base: self.loop_sp_replace_base,
                })
            },
            tools: Some(ToolFilter {
                mode: self.loop_tools_mode.clone(),
                list: if self.loop_tools_mode == "all" { Vec::new() } else { sorted(&self.loop_tools_checked) },
                config: self.loop_tools_config.clone(),
            }),
            mcp: Some(McpFilter {
                mode: self.loop_mcp_mode.clone(),
                servers: if self.loop_mcp_mode == "selected" { sorted(&self.loop_mcp_checked) } else { Vec::new() },
            }),
            skills: Some(SkillFilter {
                mode: self.loop_skills_mode.clone(),
                list: if self.loop_skills_mode == "selected" { sorted(&self.loop_skills_checked) } else { Vec::new() },
            }),
            loop_: Some(LoopKnobs {
                max_rounds: Some(self.loop_max_rounds),
                max_tool_calls: Some(self.loop_max_tool_calls),
                temperature: Some(self.loop_temperature),
                max_tokens: Some(self.loop_max_tokens),
                reflection_enabled: Some(self.loop_reflection_enabled),
                reflection_threshold: Some(self.loop_reflection_threshold),
                max_reflection_retries: Some(self.loop_max_reflection_retries),
                checkpoint_enabled: Some(self.loop_checkpoint_enabled),
                max_consecutive_errors: None,
                max_error_recoveries: None,
                max_spawn_depth: Some(self.loop_max_spawn_depth),
                step_verification: Some(self.loop_step_verification),
            }),
            compaction: Some(CompactionKnobs {
                enabled: Some(self.loop_compact_enabled),
                interval: Some(self.loop_compact_interval),
                window: Some(self.loop_compact_window),
                max_context_tokens: Some(self.loop_compact_max_context_tokens),
                tool_result_max_len: Some(self.loop_compact_tool_result_max_len),
                model: if self.loop_compact_model.trim().is_empty() {
                    None
                } else {
                    Some(self.loop_compact_model.trim().to_string())
                },
            }),
            evaluation: Some(EvaluationKnobs {
                enabled: Some(self.loop_eval_enabled),
                threshold: Some(self.loop_eval_threshold),
                max_retries: Some(self.loop_eval_max_retries),
                max_fix_rounds: Some(self.loop_eval_max_fix_rounds),
                max_judge_rounds: Some(self.loop_eval_max_judge_rounds),
                model: if self.loop_eval_model.trim().is_empty() {
                    None
                } else {
                    Some(self.loop_eval_model.trim().to_string())
                },
                // Judge api_url/api_key stay YAML-mode only: keeps secrets
                // out of the form and off casual screenshots.
                api_url: None,
                api_key: None,
                rubric: if self.loop_eval_rubric.trim().is_empty() {
                    None
                } else {
                    Some(self.loop_eval_rubric.clone())
                },
                allow_execute: Some(self.loop_eval_allow_execute),
            }),
            graph: {
                let enabled = match self.loop_graph_gate.as_str() {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
                let profile = Some(self.loop_graph_profile.trim().to_string())
                    .filter(|s| !s.is_empty());
                if enabled.is_none() && profile.is_none() {
                    None
                } else {
                    Some(GraphKnobs { enabled, profile })
                }
            },
        }
    }

    /// Per-tool config editor (tools.config in the profile YAML): approval
    /// override, enable/disable, description, param defaults/pins, limits.
    fn per_tool_config_editor(&mut self, ui: &mut egui::Ui) {
        let dim = egui::Color32::from_rgb(124, 115, 104);
        let red = egui::Color32::from_rgb(200, 80, 70);

        ui.add_space(6.0);
        ui.separator();
        ui.label(egui::RichText::new("Per-tool config").strong());
        ui.label(
            egui::RichText::new(
                "Approval, parameter defaults/pins and limits per tool. Works for built-in and MCP tools; leave a field on Inherit/empty to keep the default behavior.",
            )
            .size(11.0)
            .color(dim),
        );

        // Add row: pick from the catalog or type a name (MCP tools).
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("loop_tcfg_pick")
                .selected_text(if self.loop_tools_config_new.is_empty() {
                    "pick a tool…"
                } else {
                    self.loop_tools_config_new.as_str()
                })
                .width(180.0)
                .show_ui(ui, |ui| {
                    for (name, _) in &self.loop_tool_catalog {
                        if !self.loop_tools_config.contains_key(name) {
                            ui.selectable_value(&mut self.loop_tools_config_new, name.clone(), name);
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.loop_tools_config_new)
                    .desired_width(180.0)
                    .hint_text("or type an MCP tool name"),
            );
            if ui.button("➕ Add").clicked() {
                let name = self.loop_tools_config_new.trim().to_string();
                if !name.is_empty() {
                    self.loop_tools_config.entry(name.clone()).or_default();
                    self.loop_tools_config_params_text.entry(name.clone()).or_default();
                    self.loop_tools_config_pins_text.entry(name).or_default();
                    self.loop_tools_config_new.clear();
                }
            }
        });

        let params_text = &mut self.loop_tools_config_params_text;
        let pins_text = &mut self.loop_tools_config_pins_text;
        let mut remove: Option<String> = None;

        for (name, cfg) in self.loop_tools_config.iter_mut() {
            let summary = {
                let mut parts: Vec<&str> = Vec::new();
                if cfg.enabled == Some(false) {
                    parts.push("disabled");
                }
                match cfg.require_approval {
                    Some(true) => parts.push("always ask"),
                    Some(false) => parts.push("never ask"),
                    None => {}
                }
                if cfg.timeout_secs.is_some() {
                    parts.push("timeout");
                }
                if cfg.max_result_len.is_some() {
                    parts.push("result cap");
                }
                if cfg.params.is_some() || cfg.pinned_params.is_some() {
                    parts.push("params");
                }
                if cfg.description.is_some() {
                    parts.push("description");
                }
                if parts.is_empty() { String::new() } else { format!("  ({})", parts.join(", ")) }
            };
            egui::CollapsingHeader::new(format!("🔧 {}{}", name, summary))
                .id_salt(format!("loop_tcfg_{name}"))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Tool:");
                        let mut v = cfg.enabled;
                        if ui.selectable_label(v.is_none(), "Inherit").on_hover_text("Follow the allow/deny filter above").clicked() {
                            v = None;
                        }
                        if ui.selectable_label(v == Some(true), "Enabled").clicked() {
                            v = Some(true);
                        }
                        if ui.selectable_label(v == Some(false), "Disabled").on_hover_text("Removed from the model's tool list").clicked() {
                            v = Some(false);
                        }
                        cfg.enabled = v;
                    });
                    ui.horizontal(|ui| {
                        ui.label("Approval:");
                        let mut v = cfg.require_approval;
                        if ui.selectable_label(v.is_none(), "Inherit").on_hover_text("Follow the global Tool Approval settings").clicked() {
                            v = None;
                        }
                        if ui.selectable_label(v == Some(true), "Always ask").clicked() {
                            v = Some(true);
                        }
                        if ui.selectable_label(v == Some(false), "Never ask").clicked() {
                            v = Some(false);
                        }
                        cfg.require_approval = v;
                    });
                    ui.horizontal(|ui| {
                        let mut on = cfg.timeout_secs.is_some();
                        if ui.checkbox(&mut on, "Timeout (s):").changed() {
                            cfg.timeout_secs = if on { Some(60) } else { None };
                        }
                        if let Some(t) = cfg.timeout_secs.as_mut() {
                            ui.add(egui::DragValue::new(t).range(1..=3600));
                        }
                        ui.add_space(12.0);
                        let mut on = cfg.max_result_len.is_some();
                        if ui.checkbox(&mut on, "Max result bytes:").changed() {
                            cfg.max_result_len = if on { Some(4000) } else { None };
                        }
                        if let Some(m) = cfg.max_result_len.as_mut() {
                            ui.add(egui::DragValue::new(m).range(200..=1_000_000).speed(200));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Description override:");
                        let mut s = cfg.description.clone().unwrap_or_default();
                        if ui
                            .add(egui::TextEdit::singleline(&mut s).desired_width(360.0).hint_text("what the model sees; empty = built-in"))
                            .changed()
                        {
                            cfg.description = if s.trim().is_empty() { None } else { Some(s) };
                        }
                    });
                    // params / pinned_params: JSON object text with parse-on-change.
                    let json_field =
                        |ui: &mut egui::Ui,
                         label: &str,
                         hover: &str,
                         buf: &mut String,
                         target: &mut Option<serde_json::Map<String, serde_json::Value>>| {
                            ui.horizontal(|ui| {
                                ui.label(label).on_hover_text(hover);
                                let resp = ui.add(
                                    egui::TextEdit::singleline(buf)
                                        .desired_width(360.0)
                                        .hint_text(r#"{"key": "value"}"#),
                                );
                                if resp.changed() {
                                    if buf.trim().is_empty() {
                                        *target = None;
                                    } else if let Ok(m) = serde_json::from_str::<
                                        serde_json::Map<String, serde_json::Value>,
                                    >(buf)
                                    {
                                        *target = Some(m);
                                    }
                                }
                                if !buf.trim().is_empty()
                                    && serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(buf).is_err()
                                {
                                    ui.label(egui::RichText::new("invalid JSON — not applied").size(11.0).color(red));
                                }
                            });
                        };
                    json_field(
                        ui,
                        "Default params:",
                        "JSON object — used only when the model omits the key",
                        params_text.entry(name.clone()).or_default(),
                        &mut cfg.params,
                    );
                    json_field(
                        ui,
                        "Pinned params:",
                        "JSON object — always overwrites what the model sends",
                        pins_text.entry(name.clone()).or_default(),
                        &mut cfg.pinned_params,
                    );
                    if ui.button("🗑 Remove config").clicked() {
                        remove = Some(name.clone());
                    }
                });
        }

        if let Some(name) = remove {
            self.loop_tools_config.remove(&name);
            self.loop_tools_config_params_text.remove(&name);
            self.loop_tools_config_pins_text.remove(&name);
        }
    }

    fn save_loop_profile(&mut self) {
        if self.loop_selected_file.is_empty() {
            self.loop_status_msg = Some("Error: no profile file selected".to_string());
            return;
        }
        let content = if self.loop_yaml_mode {
            self.loop_yaml_text.clone()
        } else {
            match serde_yaml::to_string(&self.loop_form_to_profile()) {
                Ok(y) => y,
                Err(e) => {
                    self.loop_status_msg = Some(format!("Error serializing profile: {}", e));
                    return;
                }
            }
        };
        // Typed validation — same rules as the REST API (validate-on-save).
        match crate::server::routes::agent_loops::validate_profile_yaml(&content) {
            Ok((profile, warnings)) => {
                let file = self.loop_selected_file.clone();
                match Self::write_loop_profile_content(&file, &content) {
                    Ok(()) => {
                        self.loop_yaml_text = content;
                        self.populate_loop_form(&profile);
                        self.loop_status_msg = Some(if warnings.is_empty() {
                            format!("Saved {}", file)
                        } else {
                            format!("Saved {} — warnings: {}", file, warnings.join("; "))
                        });
                        if !self.loop_profiles.contains(&file) {
                            self.loop_profiles.push(file);
                            self.loop_profiles.sort();
                        }
                    }
                    Err(e) => self.loop_status_msg = Some(format!("Error saving: {}", e)),
                }
            }
            Err(e) => self.loop_status_msg = Some(format!("Error: {}", e)),
        }
    }

    fn save_active_loop_profile(&mut self, runtime: &tokio::runtime::Handle) {
        let active = self.active_loop_profile.clone();
        runtime.block_on(async move {
            let mut settings = get_settings().await;
            settings.agent_loop_profile = Some(active);
            save_settings(&settings).await;
        });
    }

    // -----------------------------------------------------------------------
    //  Graph mode (judge panel) — profile + rules editors. Dual IO like the
    //  agent-loop editor: direct fs locally, /api/graph-profiles when remote.
    // -----------------------------------------------------------------------

    fn scan_graph_profiles() -> Vec<String> {
        let dir = crate::server::services::graph::graph_dir();
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue; // skip rules/
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".yaml") || name.ends_with(".yml") {
                    files.push(name);
                }
            }
        }
        files.sort();
        files
    }

    fn scan_graph_profiles_remote() -> Vec<String> {
        let Some(rb) = crate::server::data::get_remote_backend() else {
            return Vec::new();
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        client
            .get(format!("{}/api/graph-profiles", rb.url))
            .bearer_auth(&rb.token)
            .send()
            .ok()
            .and_then(|r| r.json::<serde_json::Value>().ok())
            .and_then(|v| v.as_array().cloned())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e["filename"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn read_graph_profile_content(filename: &str) -> Option<String> {
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            return client
                .get(format!("{}/api/graph-profiles/{}", rb.url, filename))
                .bearer_auth(&rb.token)
                .send()
                .ok()
                .and_then(|r| r.json::<serde_json::Value>().ok())
                .and_then(|v| v["content"].as_str().map(|s| s.to_string()));
        }
        std::fs::read_to_string(crate::server::services::graph::graph_dir().join(filename)).ok()
    }

    fn write_graph_profile_content(filename: &str, content: &str) -> Result<(), String> {
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            let resp = client
                .post(format!("{}/api/graph-profiles", rb.url))
                .bearer_auth(&rb.token)
                .json(&serde_json::json!({"filename": filename, "content": content}))
                .send()
                .map_err(|e| e.to_string())?;
            if resp.status().is_success() {
                return Ok(());
            }
            return Err(resp
                .json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["error"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "Save failed".to_string()));
        }
        let dir = crate::server::services::graph::graph_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(filename), content).map_err(|e| e.to_string())
    }

    fn scan_graph_rules(&mut self) {
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            self.graph_rules_files = client
                .get(format!("{}/api/graph-profiles/rules", rb.url))
                .bearer_auth(&rb.token)
                .send()
                .ok()
                .and_then(|r| r.json::<Vec<String>>().ok())
                .unwrap_or_default();
            return;
        }
        let dir = crate::server::services::graph::rules_dir();
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
        self.graph_rules_files = files;
    }

    fn open_graph_rules(&mut self, filename: &str) {
        self.graph_rules_selected = filename.to_string();
        self.graph_rules_text = if crate::server::data::get_remote_backend().is_some() {
            let rb = crate::server::data::get_remote_backend().unwrap();
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            client
                .get(format!("{}/api/graph-profiles/rules/{}", rb.url, filename))
                .bearer_auth(&rb.token)
                .send()
                .ok()
                .and_then(|r| r.json::<serde_json::Value>().ok())
                .and_then(|v| v["content"].as_str().map(|s| s.to_string()))
                .unwrap_or_default()
        } else {
            std::fs::read_to_string(crate::server::services::graph::rules_dir().join(filename))
                .unwrap_or_default()
        };
    }

    fn save_graph_rules(&mut self) {
        let filename = self.graph_rules_selected.clone();
        if filename.is_empty() {
            self.graph_status_msg = Some("Error: no rules file selected".to_string());
            return;
        }
        if let Err(e) = serde_yaml::from_str::<serde_yaml::Value>(&self.graph_rules_text) {
            self.graph_status_msg = Some(format!("Error: invalid rules YAML: {}", e));
            return;
        }
        let result = if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            client
                .post(format!("{}/api/graph-profiles/rules/{}", rb.url, filename))
                .bearer_auth(&rb.token)
                .json(&serde_json::json!({"content": self.graph_rules_text}))
                .send()
                .map_err(|e| e.to_string())
                .and_then(|r| {
                    if r.status().is_success() { Ok(()) } else { Err("Save failed".to_string()) }
                })
        } else {
            let dir = crate::server::services::graph::rules_dir();
            std::fs::create_dir_all(&dir)
                .map_err(|e| e.to_string())
                .and_then(|_| {
                    std::fs::write(dir.join(&filename), &self.graph_rules_text)
                        .map_err(|e| e.to_string())
                })
        };
        self.graph_status_msg = Some(match result {
            Ok(()) => {
                if !self.graph_rules_files.contains(&filename) {
                    self.graph_rules_files.push(filename.clone());
                    self.graph_rules_files.sort();
                }
                format!("Saved rules {}", filename)
            }
            Err(e) => format!("Error saving rules: {}", e),
        });
    }

    fn refresh_graph_profiles(&mut self) {
        if crate::server::data::get_remote_backend().is_some() {
            self.graph_profiles = Self::scan_graph_profiles_remote();
        } else {
            crate::server::services::graph::ensure_default_profile();
            self.graph_profiles = Self::scan_graph_profiles();
        }
        self.scan_graph_rules();
        if self.graph_selected_file.is_empty()
            || !self.graph_profiles.contains(&self.graph_selected_file)
        {
            let initial = if !self.active_graph_profile.is_empty()
                && self.graph_profiles.contains(&self.active_graph_profile)
            {
                self.active_graph_profile.clone()
            } else {
                self.graph_profiles.first().cloned().unwrap_or_default()
            };
            self.graph_selected_file = initial;
        }
        if !self.graph_selected_file.is_empty() {
            let file = self.graph_selected_file.clone();
            self.open_graph_profile(&file);
        }
    }

    fn open_graph_profile(&mut self, filename: &str) {
        self.graph_selected_file = filename.to_string();
        let content = Self::read_graph_profile_content(filename).unwrap_or_default();
        self.graph_yaml_text = content.clone();
        match serde_yaml::from_str::<crate::server::services::graph::GraphProfile>(&content) {
            Ok(p) => {
                self.populate_graph_form(&p);
                self.graph_status_msg = None;
            }
            Err(e) => {
                self.graph_status_msg = Some(format!("Error parsing {}: {}", filename, e));
                self.graph_yaml_mode = true; // let the user fix the raw YAML
            }
        }
    }

    fn populate_graph_form(&mut self, p: &crate::server::services::graph::GraphProfile) {
        self.graph_name = p.name.clone();
        self.graph_description = p.description.clone();
        self.graph_worker_mode = p.worker_mode().to_string();
        self.graph_worker_loop_profile = p
            .worker
            .as_ref()
            .and_then(|w| w.agent_loop_profile.clone())
            .unwrap_or_default();
        self.graph_judges = p
            .judges
            .iter()
            .map(|j| GraphJudgeForm {
                name: j.name.clone(),
                model: j.model.clone(),
                api_url: j.api_url.clone(),
                api_key: j.api_key.clone(),
                rules_file: j.rules_file.clone().unwrap_or_default(),
                rules: j.rules.clone().unwrap_or_default(),
                weight: j.weight.unwrap_or(1.0),
                threshold: j.threshold.unwrap_or(-1.0),
                use_tools: j.use_tools.unwrap_or(true),
                allow_execute: j.allow_execute.unwrap_or(false),
                max_judge_rounds: j.max_judge_rounds.unwrap_or(3),
            })
            .collect();
        let agg = p.aggregation.clone().unwrap_or_default();
        self.graph_agg_policy = if agg.policy.trim().is_empty() {
            "all_pass".to_string()
        } else {
            agg.policy.clone()
        };
        self.graph_agg_threshold = agg.threshold.unwrap_or(0.75);
        let knobs = p.loop_.clone().unwrap_or_default();
        self.graph_max_iterations = knobs.max_iterations.unwrap_or(2);
        self.graph_max_fix_rounds = knobs.max_fix_rounds.unwrap_or(5);
        self.graph_judge_plain_answers = knobs.judge_plain_answers.unwrap_or(true);
    }

    fn graph_form_to_profile(&self) -> crate::server::services::graph::GraphProfile {
        use crate::server::services::graph::*;
        GraphProfile {
            name: self.graph_name.clone(),
            description: self.graph_description.clone(),
            worker: Some(WorkerNode {
                mode: self.graph_worker_mode.clone(),
                agent_loop_profile: if self.graph_worker_loop_profile.trim().is_empty() {
                    None
                } else {
                    Some(self.graph_worker_loop_profile.trim().to_string())
                },
            }),
            judges: self
                .graph_judges
                .iter()
                .map(|f| JudgeNode {
                    name: f.name.clone(),
                    model: f.model.clone(),
                    api_url: f.api_url.clone(),
                    api_key: f.api_key.clone(),
                    rules: if f.rules.trim().is_empty() { None } else { Some(f.rules.clone()) },
                    rules_file: if f.rules_file.trim().is_empty() {
                        None
                    } else {
                        Some(f.rules_file.clone())
                    },
                    weight: Some(f.weight),
                    threshold: if f.threshold < 0.0 { None } else { Some(f.threshold) },
                    use_tools: Some(f.use_tools),
                    allow_execute: Some(f.allow_execute),
                    max_judge_rounds: Some(f.max_judge_rounds),
                })
                .collect(),
            aggregation: Some(AggregationPolicy {
                policy: self.graph_agg_policy.clone(),
                threshold: Some(self.graph_agg_threshold),
            }),
            loop_: Some(GraphLoopKnobs {
                max_iterations: Some(self.graph_max_iterations),
                max_fix_rounds: Some(self.graph_max_fix_rounds),
                judge_plain_answers: Some(self.graph_judge_plain_answers),
            }),
        }
    }

    fn save_graph_profile(&mut self) {
        if self.graph_selected_file.is_empty() {
            self.graph_status_msg = Some("Error: no profile file selected".to_string());
            return;
        }
        let content = if self.graph_yaml_mode {
            self.graph_yaml_text.clone()
        } else {
            match serde_yaml::to_string(&self.graph_form_to_profile()) {
                Ok(y) => y,
                Err(e) => {
                    self.graph_status_msg = Some(format!("Error serializing profile: {}", e));
                    return;
                }
            }
        };
        match crate::server::services::graph::validate_graph_yaml(&content) {
            Ok((profile, warnings)) => {
                let file = self.graph_selected_file.clone();
                match Self::write_graph_profile_content(&file, &content) {
                    Ok(()) => {
                        self.graph_yaml_text = content;
                        self.populate_graph_form(&profile);
                        self.graph_status_msg = Some(if warnings.is_empty() {
                            format!("Saved {}", file)
                        } else {
                            format!("Saved {} — warnings: {}", file, warnings.join("; "))
                        });
                        if !self.graph_profiles.contains(&file) {
                            self.graph_profiles.push(file);
                            self.graph_profiles.sort();
                        }
                    }
                    Err(e) => self.graph_status_msg = Some(format!("Error saving: {}", e)),
                }
            }
            Err(e) => self.graph_status_msg = Some(format!("Error: {}", e)),
        }
    }

    fn save_active_graph_profile(&mut self, runtime: &tokio::runtime::Handle) {
        let active = self.active_graph_profile.clone();
        runtime.block_on(async move {
            let mut settings = get_settings().await;
            settings.graph_profile = Some(active);
            save_settings(&settings).await;
        });
    }

    fn section_graph(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);
        if self.graph_needs_refresh {
            self.graph_needs_refresh = false;
            self.refresh_graph_profiles();
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.heading("Graph Mode — Judge Panel");
            let (tag, color) = if self.graph_gate_enabled {
                ("● GATE ON", egui::Color32::from_rgb(34, 197, 94))
            } else {
                ("○ GATE OFF", egui::Color32::from_rgb(124, 115, 104))
            };
            ui.label(egui::RichText::new(tag).size(12.0).strong().color(color));
        });
        ui.label(
            egui::RichText::new(
                "Evaluator-optimizer graph: a panel of judge agents reviews the final answer \
                 against YAML rules BEFORE it reaches you. A rejected answer is sent back to \
                 the main loop with the structured verdict until it passes or max iterations \
                 run out. Edit here or drop YAML files into data/graph/ (rules in \
                 data/graph/rules/).",
            )
            .size(12.0)
            .color(egui::Color32::from_rgb(124, 115, 104)),
        );
        ui.add_space(8.0);
        if ui
            .checkbox(
                &mut self.graph_gate_enabled,
                "Enable graph gate (judge final answers in every mode)",
            )
            .on_hover_text(
                "Default off. On = the judge panel reviews every final answer regardless of the \
                 selected sub-agent mode. Selecting the 'graph' mode enables the gate even when \
                 this is off; an agent-loop profile's graph.enabled overrides this toggle.",
            )
            .changed()
        {
            let on = self.graph_gate_enabled;
            runtime.block_on(async move {
                let mut settings = get_settings().await;
                settings.graph_enabled = Some(on);
                save_settings(&settings).await;
            });
            self.graph_status_msg =
                Some(format!("Graph gate {}", if on { "enabled" } else { "disabled" }));
        }
        ui.label(
            egui::RichText::new(
                "Also available per agent-loop profile YAML:  graph: { enabled: true, profile: strict.yaml }",
            )
            .size(10.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(8.0);

        // --- Active profile selector ---
        ui.horizontal(|ui| {
            ui.label("Active profile:");
            let mut changed = false;
            egui::ComboBox::from_id_salt("active_graph_profile")
                .selected_text(if self.active_graph_profile.is_empty() {
                    "default.yaml".to_string()
                } else {
                    self.active_graph_profile.clone()
                })
                .show_ui(ui, |ui| {
                    for f in self.graph_profiles.clone() {
                        changed |= ui
                            .selectable_value(&mut self.active_graph_profile, f.clone(), f)
                            .changed();
                    }
                });
            if changed {
                self.save_active_graph_profile(runtime);
                self.graph_status_msg =
                    Some(format!("Active graph profile set to {}", self.active_graph_profile));
            }
        });
        ui.add_space(8.0);
        ui.separator();

        // --- Profile file picker + New / Delete / Reset default ---
        ui.horizontal(|ui| {
            ui.label("Edit profile:");
            let prev = self.graph_selected_file.clone();
            let mut pick = self.graph_selected_file.clone();
            egui::ComboBox::from_id_salt("graph_profile_editor_file")
                .selected_text(if pick.is_empty() { "(none)".to_string() } else { pick.clone() })
                .show_ui(ui, |ui| {
                    for f in self.graph_profiles.clone() {
                        ui.selectable_value(&mut pick, f.clone(), f);
                    }
                });
            if pick != prev && !pick.is_empty() {
                self.open_graph_profile(&pick);
            }

            ui.add_space(8.0);
            ui.label("New:");
            ui.add(
                egui::TextEdit::singleline(&mut self.graph_new_name)
                    .hint_text("profile-name")
                    .desired_width(140.0),
            );
            if ui.button("Create").clicked() && !self.graph_new_name.trim().is_empty() {
                let file =
                    crate::server::services::graph::normalize_filename(&self.graph_new_name);
                self.graph_name =
                    self.graph_new_name.trim().trim_end_matches(".yaml").to_string();
                self.graph_new_name.clear();
                self.graph_selected_file = file;
                self.graph_yaml_mode = false;
                if self.graph_judges.is_empty() {
                    self.graph_judges.push(GraphJudgeForm {
                        name: "quality".to_string(),
                        rules_file: crate::server::services::graph::DEFAULT_RULES_FILE.to_string(),
                        ..Default::default()
                    });
                }
                self.save_graph_profile();
            }

            if !self.graph_selected_file.is_empty()
                && self.graph_selected_file != crate::server::services::graph::DEFAULT_PROFILE_FILE
                && ui
                    .add(egui::Button::new(
                        egui::RichText::new("Delete").color(egui::Color32::from_rgb(239, 68, 68)),
                    ))
                    .clicked()
            {
                let file = self.graph_selected_file.clone();
                if let Some(rb) = crate::server::data::get_remote_backend() {
                    let client = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(5))
                        .build()
                        .unwrap_or_default();
                    let _ = client
                        .delete(format!("{}/api/graph-profiles/{}", rb.url, file))
                        .bearer_auth(&rb.token)
                        .send();
                } else {
                    let _ = std::fs::remove_file(
                        crate::server::services::graph::graph_dir().join(&file),
                    );
                }
                if self.active_graph_profile == file {
                    self.active_graph_profile.clear();
                    self.save_active_graph_profile(runtime);
                }
                self.graph_selected_file.clear();
                self.graph_status_msg = Some(format!("Deleted {}", file));
                self.graph_needs_refresh = true;
            }

            if ui
                .button("Reset default")
                .on_hover_text("Regenerate default.yaml (and re-seed default rules if missing)")
                .clicked()
            {
                if let Some(rb) = crate::server::data::get_remote_backend() {
                    let client = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(5))
                        .build()
                        .unwrap_or_default();
                    let _ = client
                        .post(format!("{}/api/graph-profiles/reset-default", rb.url))
                        .bearer_auth(&rb.token)
                        .send();
                } else if let Ok(yaml) =
                    serde_yaml::to_string(&crate::server::services::graph::default_profile())
                {
                    let _ = Self::write_graph_profile_content(
                        crate::server::services::graph::DEFAULT_PROFILE_FILE,
                        &yaml,
                    );
                    crate::server::services::graph::ensure_default_profile();
                }
                self.graph_status_msg = Some("default.yaml regenerated".to_string());
                self.graph_needs_refresh = true;
            }
        });

        Self::status_label(ui, &self.graph_status_msg);
        ui.add_space(8.0);

        if self.graph_selected_file.is_empty() {
            ui.label("No profile selected — create one above.");
            return;
        }

        // --- Form / YAML mode toggle ---
        ui.horizontal(|ui| {
            let was_yaml = self.graph_yaml_mode;
            ui.selectable_value(&mut self.graph_yaml_mode, false, "Form");
            ui.selectable_value(&mut self.graph_yaml_mode, true, "Edit as YAML");
            if was_yaml != self.graph_yaml_mode {
                if self.graph_yaml_mode {
                    if let Ok(y) = serde_yaml::to_string(&self.graph_form_to_profile()) {
                        self.graph_yaml_text = y;
                    }
                } else {
                    match serde_yaml::from_str::<crate::server::services::graph::GraphProfile>(
                        &self.graph_yaml_text,
                    ) {
                        Ok(p) => {
                            self.populate_graph_form(&p);
                            self.graph_status_msg = None;
                        }
                        Err(e) => {
                            self.graph_status_msg =
                                Some(format!("Error: fix YAML before switching to Form: {}", e));
                            self.graph_yaml_mode = true;
                        }
                    }
                }
            }
            ui.add_space(12.0);
            if Self::save_button(ui, &format!("Save {}", self.graph_selected_file)) {
                self.save_graph_profile();
            }
        });
        ui.add_space(8.0);

        if self.graph_yaml_mode {
            egui::ScrollArea::vertical()
                .id_salt("graph_yaml_editor")
                .max_height(420.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.graph_yaml_text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(24),
                    );
                });
        } else {
            self.graph_form_ui(ui);
        }

        // --- Judge rules sub-editor (same files the judges load at run time) ---
        ui.add_space(12.0);
        ui.separator();
        ui.heading("Judge Rules Files");
        ui.label(
            egui::RichText::new(
                "Rule files in data/graph/rules/ — referenced by judges via rules_file and \
                 rendered verbatim into the judge's system prompt. Hand-dropped files appear \
                 here after Refresh.",
            )
            .size(11.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let prev = self.graph_rules_selected.clone();
            let mut pick = self.graph_rules_selected.clone();
            egui::ComboBox::from_id_salt("graph_rules_file_picker")
                .selected_text(if pick.is_empty() { "(none)".to_string() } else { pick.clone() })
                .show_ui(ui, |ui| {
                    for f in self.graph_rules_files.clone() {
                        ui.selectable_value(&mut pick, f.clone(), f);
                    }
                });
            if pick != prev && !pick.is_empty() {
                self.open_graph_rules(&pick);
            }
            ui.add_space(8.0);
            ui.label("New:");
            ui.add(
                egui::TextEdit::singleline(&mut self.graph_rules_new_name)
                    .hint_text("rules-name")
                    .desired_width(140.0),
            );
            if ui.button("Create rules").clicked() && !self.graph_rules_new_name.trim().is_empty() {
                let file =
                    crate::server::services::graph::normalize_filename(&self.graph_rules_new_name);
                self.graph_rules_new_name.clear();
                self.graph_rules_selected = file;
                self.graph_rules_text =
                    "rules:\n  - id: my-rule\n    severity: blocker\n    description: Describe what must hold for the answer to pass.\n".to_string();
                self.save_graph_rules();
            }
            if ui.button("Refresh").clicked() {
                self.scan_graph_rules();
            }
            if !self.graph_rules_selected.is_empty() && ui.button("Save rules").clicked() {
                self.save_graph_rules();
            }
        });
        if !self.graph_rules_selected.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("graph_rules_editor")
                .max_height(220.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.graph_rules_text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(10),
                    );
                });
        }
    }

    fn graph_form_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.add(egui::TextEdit::singleline(&mut self.graph_name).desired_width(180.0));
            ui.label("Description:");
            ui.add(egui::TextEdit::singleline(&mut self.graph_description).desired_width(320.0));
        });
        ui.add_space(8.0);

        // --- Worker node ---
        ui.horizontal(|ui| {
            ui.label("Worker mode:");
            egui::ComboBox::from_id_salt("graph_worker_mode")
                .selected_text(&self.graph_worker_mode)
                .width(160.0)
                .show_ui(ui, |ui| {
                    for m in crate::server::services::graph::WORKER_MODES {
                        ui.selectable_value(&mut self.graph_worker_mode, m.to_string(), *m);
                    }
                });
            ui.label(
                egui::RichText::new("the mode the main loop runs in under the gate")
                    .size(10.0)
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(8.0);
            ui.label("Worker loop profile:");
            ui.add(
                egui::TextEdit::singleline(&mut self.graph_worker_loop_profile)
                    .desired_width(160.0)
                    .hint_text("(optional agent-loop file)"),
            );
        });
        ui.add_space(8.0);

        // --- Judge panel ---
        ui.heading("Judges");
        let mut judge_to_delete: Option<usize> = None;
        let rules_files = self.graph_rules_files.clone();
        for (idx, judge) in self.graph_judges.iter_mut().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("Judge #{}", idx + 1)).strong());
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut judge.name).desired_width(120.0));
                    ui.label("Model:");
                    ui.add(
                        egui::TextEdit::singleline(&mut judge.model)
                            .desired_width(180.0)
                            .hint_text("(blank = session model)"),
                    );
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new("Remove").color(egui::Color32::from_rgb(239, 68, 68)),
                        ))
                        .clicked()
                    {
                        judge_to_delete = Some(idx);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("API URL:");
                    ui.add(
                        egui::TextEdit::singleline(&mut judge.api_url)
                            .desired_width(220.0)
                            .hint_text("(blank = session)"),
                    );
                    ui.label("API key:");
                    ui.add(
                        egui::TextEdit::singleline(&mut judge.api_key)
                            .desired_width(160.0)
                            .password(true)
                            .hint_text("(blank = session)"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Rules file:");
                    egui::ComboBox::from_id_salt(format!("graph_judge_rules_{idx}"))
                        .selected_text(if judge.rules_file.is_empty() {
                            "(none)".to_string()
                        } else {
                            judge.rules_file.clone()
                        })
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut judge.rules_file, String::new(), "(none)");
                            for f in &rules_files {
                                ui.selectable_value(&mut judge.rules_file, f.clone(), f.as_str());
                            }
                        });
                    ui.label("Weight:");
                    ui.add(
                        egui::DragValue::new(&mut judge.weight).speed(0.1).range(0.0..=10.0),
                    );
                    ui.checkbox(&mut judge.use_tools, "verify with tools")
                        .on_hover_text("judge may call read_file/list_files to check claims");
                    ui.checkbox(&mut judge.allow_execute, "allow execute")
                        .on_hover_text("also grants run_python/run_shell to this judge");
                    ui.label("Judge rounds:");
                    ui.add(egui::DragValue::new(&mut judge.max_judge_rounds).range(1..=6));
                });
                ui.horizontal(|ui| {
                    ui.label("Extra rules:");
                    ui.add(
                        egui::TextEdit::multiline(&mut judge.rules)
                            .desired_width(f32::INFINITY)
                            .desired_rows(2)
                            .hint_text("inline rules appended after the rules file (optional)"),
                    );
                });
            });
            ui.add_space(4.0);
        }
        if let Some(idx) = judge_to_delete {
            self.graph_judges.remove(idx);
        }
        if ui.button("+ Add judge").clicked() {
            self.graph_judges.push(GraphJudgeForm {
                name: format!("judge-{}", self.graph_judges.len() + 1),
                ..Default::default()
            });
        }
        ui.add_space(8.0);

        // --- Aggregation + loop knobs ---
        ui.heading("Aggregation & Loop");
        ui.horizontal(|ui| {
            ui.label("Policy:");
            egui::ComboBox::from_id_salt("graph_agg_policy")
                .selected_text(match self.graph_agg_policy.as_str() {
                    "majority" => "Majority vote",
                    "weighted_average" => "Weighted average",
                    _ => "All must pass",
                })
                .width(180.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.graph_agg_policy, "all_pass".to_string(), "All must pass");
                    ui.selectable_value(&mut self.graph_agg_policy, "majority".to_string(), "Majority vote");
                    ui.selectable_value(
                        &mut self.graph_agg_policy,
                        "weighted_average".to_string(),
                        "Weighted average",
                    );
                });
            ui.label("Threshold:");
            ui.add(
                egui::DragValue::new(&mut self.graph_agg_threshold)
                    .speed(0.05)
                    .range(0.0..=1.0),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Max iterations:");
            ui.add(egui::DragValue::new(&mut self.graph_max_iterations).range(1..=5))
                .on_hover_text("judge → revise cycles before the answer is released anyway");
            ui.label("Fix rounds per iteration:");
            ui.add(egui::DragValue::new(&mut self.graph_max_fix_rounds).range(1..=10))
                .on_hover_text("worker tool rounds granted to address a rejection");
            ui.checkbox(&mut self.graph_judge_plain_answers, "judge plain answers")
                .on_hover_text("also judge answers produced without any tool calls");
        });
    }

    // -----------------------------------------------------------------------
    //  Tools management (Settings > Tools): Catalog + Custom Tools editor.
    //  Custom-tool CRUD is dual: direct fs/service calls locally, REST when
    //  the desktop app is pointed at a remote backend.
    // -----------------------------------------------------------------------

    /// List custom tools (filename/name/kind/enabled/valid/error) from the
    /// local data/tools folder, mirroring the /api/custom-tools list route.
    fn load_custom_tools_local() -> Vec<serde_json::Value> {
        use crate::server::services::custom_tools::{tools_dir, CustomTool};
        let dir = tools_dir();
        let _ = std::fs::create_dir_all(&dir);
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !(name.ends_with(".yaml") || name.ends_with(".yml")) {
                    continue;
                }
                let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
                match serde_yaml::from_str::<CustomTool>(&content) {
                    Ok(t) => out.push(serde_json::json!({
                        "filename": name, "name": t.name, "kind": t.kind,
                        "enabled": t.enabled, "valid": true, "error": "",
                    })),
                    Err(e) => out.push(serde_json::json!({
                        "filename": name,
                        "name": name.trim_end_matches(".yaml").trim_end_matches(".yml"),
                        "kind": "", "enabled": false, "valid": false, "error": e.to_string(),
                    })),
                }
            }
        }
        out.sort_by(|a, b| a["filename"].as_str().unwrap_or("").cmp(b["filename"].as_str().unwrap_or("")));
        out
    }

    /// Refresh the custom-tool list and the active profile's tools.config
    /// (used for the Catalog status chips).
    fn refresh_custom_tools(&mut self, runtime: &tokio::runtime::Handle) {
        // Custom tool list — remote via REST, else local fs.
        self.custom_tools_list = if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            client
                .get(format!("{}/api/custom-tools", rb.url))
                .bearer_auth(&rb.token)
                .send()
                .ok()
                .and_then(|r| r.json::<Vec<serde_json::Value>>().ok())
                .unwrap_or_default()
        } else {
            Self::load_custom_tools_local()
        };

        // Active profile tools.config for chips.
        let active = runtime.block_on(get_settings()).agent_loop_profile.unwrap_or_default();
        self.tools_active_config.clear();
        if !active.is_empty() {
            if let Some(p) = crate::server::services::agent_loop::load_profile(&active) {
                if let Some(tf) = p.tools {
                    self.tools_active_config = tf.config;
                }
            }
        }
        self.custom_tools_loaded = true;
    }

    fn open_custom_tool(&mut self, filename: &str) {
        self.custom_tool_selected = filename.to_string();
        self.custom_tool_test_result = None;
        self.custom_tool_status = None;
        self.custom_tool_yaml = if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            client
                .get(format!("{}/api/custom-tools/{}", rb.url, filename))
                .bearer_auth(&rb.token)
                .send()
                .ok()
                .and_then(|r| r.json::<serde_json::Value>().ok())
                .and_then(|v| v["content"].as_str().map(|s| s.to_string()))
                .unwrap_or_default()
        } else {
            std::fs::read_to_string(
                crate::server::services::custom_tools::tools_dir().join(filename),
            )
            .unwrap_or_default()
        };
    }

    fn save_custom_tool(&mut self, runtime: &tokio::runtime::Handle) {
        let filename = self.custom_tool_selected.clone();
        if filename.is_empty() {
            self.custom_tool_status = Some("Error: no tool selected".into());
            return;
        }
        let content = self.custom_tool_yaml.clone();
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            let resp = client
                .post(format!("{}/api/custom-tools", rb.url))
                .bearer_auth(&rb.token)
                .json(&serde_json::json!({"filename": filename, "content": content}))
                .send();
            match resp {
                Ok(r) => {
                    let v = r.json::<serde_json::Value>().unwrap_or_default();
                    self.custom_tool_status = Some(match v.get("error").and_then(|e| e.as_str()) {
                        Some(e) => format!("Error: {e}"),
                        None => {
                            let w = v["warnings"].as_array().map(|a| a.len()).unwrap_or(0);
                            if w > 0 { format!("Saved (warnings: {w})") } else { "Saved".into() }
                        }
                    });
                }
                Err(e) => self.custom_tool_status = Some(format!("Error: {e}")),
            }
        } else {
            // Local: parse + validate + write.
            match serde_yaml::from_str::<crate::server::services::custom_tools::CustomTool>(&content) {
                Ok(tool) => match crate::server::services::custom_tools::validate(&tool) {
                    Ok(warnings) => {
                        let dir = crate::server::services::custom_tools::tools_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        match std::fs::write(dir.join(&filename), &content) {
                            Ok(()) => {
                                self.custom_tool_status = Some(if warnings.is_empty() {
                                    "Saved".into()
                                } else {
                                    format!("Saved — warnings: {}", warnings.join("; "))
                                });
                            }
                            Err(e) => self.custom_tool_status = Some(format!("Error writing: {e}")),
                        }
                    }
                    Err(e) => self.custom_tool_status = Some(format!("Invalid: {e}")),
                },
                Err(e) => self.custom_tool_status = Some(format!("Invalid YAML: {e}")),
            }
        }
        self.custom_tools_loaded = false; // force list refresh
    }

    fn delete_custom_tool(&mut self, filename: &str) {
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            let _ = client
                .delete(format!("{}/api/custom-tools/{}", rb.url, filename))
                .bearer_auth(&rb.token)
                .send();
        } else {
            let fp = crate::server::services::custom_tools::tools_dir().join(filename);
            let _ = std::fs::remove_file(fp);
        }
        if self.custom_tool_selected == filename {
            self.custom_tool_selected.clear();
            self.custom_tool_yaml.clear();
        }
        self.custom_tools_loaded = false;
    }

    fn test_custom_tool(&mut self, runtime: &tokio::runtime::Handle) {
        // Resolve the tool name from the buffer (name: line) or filename.
        let name = serde_yaml::from_str::<serde_json::Value>(&self.custom_tool_yaml)
            .ok()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| {
                self.custom_tool_selected
                    .trim_end_matches(".yaml")
                    .trim_end_matches(".yml")
                    .to_string()
            });
        let args: serde_json::Value =
            serde_json::from_str(&self.custom_tool_test_args).unwrap_or(serde_json::json!({}));

        let result = if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default();
            client
                .post(format!("{}/api/custom-tools/{}/test", rb.url, name))
                .bearer_auth(&rb.token)
                .json(&serde_json::json!({"args": args}))
                .send()
                .ok()
                .and_then(|r| r.json::<serde_json::Value>().ok())
                .unwrap_or(serde_json::json!({"error": "request failed"}))
        } else {
            let sandbox = crate::server::data::data_dir()
                .join("..")
                .join("sandbox")
                .to_string_lossy()
                .to_string();
            runtime.block_on(crate::server::services::custom_tools::execute(&name, &args, &sandbox))
        };
        self.custom_tool_test_result =
            Some(serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()));
    }

    fn section_tools(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);
        if !self.custom_tools_loaded {
            self.refresh_custom_tools(runtime);
        }
        if self.loop_tool_catalog.is_empty() {
            self.loop_tool_catalog = crate::server::services::toolbox::tool_catalog();
        }
        let dim = egui::Color32::from_rgb(124, 115, 104);

        ui.add_space(8.0);
        ui.heading("Tools");
        ui.label(
            egui::RichText::new(
                "Every tool the agent can call. Catalog shows built-in and custom tools with \
                 their per-tool config; Custom Tools lets you add your own (HTTP or shell) in YAML.",
            )
            .size(12.0)
            .color(dim),
        );
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tools_subtab, 0, "Catalog");
            ui.selectable_value(&mut self.tools_subtab, 1, "Custom Tools");
            if ui.button("🔄 Refresh").clicked() {
                self.custom_tools_loaded = false;
                self.loop_tool_catalog = crate::server::services::toolbox::tool_catalog();
            }
        });
        ui.separator();

        if self.tools_subtab == 0 {
            self.tools_catalog_view(ui, runtime);
        } else {
            self.tools_custom_view(ui, runtime);
        }
    }

    /// Sub-tab 1: read-only catalog of every tool with status chips and a
    /// jump-to-config / edit action.
    fn tools_catalog_view(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let dim = egui::Color32::from_rgb(124, 115, 104);
        let green = egui::Color32::from_rgb(34, 197, 94);
        let amber = egui::Color32::from_rgb(214, 158, 46);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Filter:");
            ui.add(
                egui::TextEdit::singleline(&mut self.tools_catalog_filter)
                    .desired_width(200.0)
                    .hint_text("name…"),
            );
            ui.checkbox(&mut self.tools_show_disabled, "Show disabled");
        });
        ui.add_space(4.0);

        // Inline per-tool config YAML editor (opened by "Configure").
        if self.tool_cfg_editing.is_some() {
            self.tool_config_editor(ui, runtime);
            ui.separator();
        }

        // Custom tool names (for the SOURCE column).
        let custom_names: std::collections::HashMap<String, (String, bool)> = self
            .custom_tools_list
            .iter()
            .filter_map(|t| {
                Some((
                    t["name"].as_str()?.to_string(),
                    (t["kind"].as_str().unwrap_or("").to_string(), t["enabled"].as_bool().unwrap_or(true)),
                ))
            })
            .collect();

        let filter = self.tools_catalog_filter.to_lowercase();
        let catalog = self.loop_tool_catalog.clone();
        let mut jump_to: Option<String> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("tools_catalog_grid")
                .num_columns(4)
                .spacing([12.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Tool").strong());
                    ui.label(egui::RichText::new("Source").strong());
                    ui.label(egui::RichText::new("Status").strong());
                    ui.label(egui::RichText::new("").strong());
                    ui.end_row();

                    for (name, _desc) in &catalog {
                        if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        let cfg = self.tools_active_config.get(name);
                        let disabled = cfg.and_then(|c| c.enabled) == Some(false);
                        if disabled && !self.tools_show_disabled {
                            continue;
                        }
                        let is_custom = custom_names.contains_key(name);
                        let source = if is_custom {
                            custom_names.get(name).map(|(k, _)| format!("custom · {k}")).unwrap_or("custom".into())
                        } else {
                            "built-in".to_string()
                        };

                        // Tool name (+ protected marker)
                        let protected = name.starts_with("proto_")
                            || name.starts_with("bb_")
                            || matches!(name.as_str(), "send_task" | "wait_result" | "check_agents" | "spawn_subagent" | "create_architecture" | "select_swarm");
                        ui.horizontal(|ui| {
                            ui.label(name);
                            if protected {
                                ui.label(egui::RichText::new("🔒").size(11.0).color(dim));
                            }
                        });
                        ui.label(egui::RichText::new(source).color(dim));

                        // Status chips
                        ui.horizontal(|ui| {
                            let mut any = false;
                            if disabled {
                                ui.label(egui::RichText::new("disabled").color(amber));
                                any = true;
                            }
                            if let Some(c) = cfg {
                                match c.require_approval {
                                    Some(true) => { ui.label(egui::RichText::new("always-ask").color(amber)); any = true; }
                                    Some(false) => { ui.label(egui::RichText::new("never-ask").color(green)); any = true; }
                                    None => {}
                                }
                                if c.pinned_params.is_some() { ui.label("pinned"); any = true; }
                                if c.params.is_some() { ui.label("defaults"); any = true; }
                                if c.timeout_secs.is_some() { ui.label("timeout"); any = true; }
                                if c.max_result_len.is_some() { ui.label("result-cap"); any = true; }
                                if c.description.is_some() { ui.label("renamed"); any = true; }
                            }
                            if !any {
                                ui.label(egui::RichText::new("on").color(green));
                            }
                        });

                        // Action
                        if is_custom {
                            if ui.button("Edit").clicked() {
                                jump_to = Some(format!("custom:{name}"));
                            }
                        } else if ui.button("Configure").clicked() {
                            jump_to = Some(format!("config:{name}"));
                        }
                        ui.end_row();
                    }
                });
        });

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!(
                "{} tools · {} custom. MCP tools are managed under Settings → MCP Tools.",
                catalog.len(),
                custom_names.len()
            ))
            .size(11.0)
            .color(dim),
        );

        // Handle a queued action after the immutable borrow ends.
        if let Some(action) = jump_to {
            if let Some(name) = action.strip_prefix("custom:") {
                // Open the matching custom file in the Custom Tools sub-tab.
                let file = self
                    .custom_tools_list
                    .iter()
                    .find(|t| t["name"].as_str() == Some(name))
                    .and_then(|t| t["filename"].as_str().map(|s| s.to_string()));
                if let Some(f) = file {
                    self.open_custom_tool(&f);
                    self.tools_subtab = 1;
                }
            } else if let Some(name) = action.strip_prefix("config:") {
                // Open this tool's per-tool config as YAML, right here.
                let name = name.to_string();
                self.open_tool_config_editor(&name, runtime);
            }
        }
    }

    /// Sub-tab 2: create / edit / delete / test custom YAML tools.
    fn tools_custom_view(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let dim = egui::Color32::from_rgb(124, 115, 104);
        let red = egui::Color32::from_rgb(200, 80, 70);

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Add your own tools in data/tools/*.yaml — an HTTP/REST call or a sandboxed shell \
                 command. Templated with {{param}}. No rebuild needed.",
            )
            .size(11.0)
            .color(dim),
        );
        ui.add_space(6.0);

        // New-tool row
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.custom_tool_new_name)
                    .desired_width(160.0)
                    .hint_text("new tool name"),
            );
            if ui.button("➕ New HTTP").clicked() {
                self.create_custom_tool_template("http");
            }
            if ui.button("➕ New Shell").clicked() {
                self.create_custom_tool_template("shell");
            }
        });
        ui.add_space(6.0);

        let list = self.custom_tools_list.clone();
        egui::SidePanel::left("custom_tools_list_panel")
            .resizable(true)
            .default_width(200.0)
            .show_inside(ui, |ui| {
                ui.label(egui::RichText::new("Your tools").strong());
                ui.separator();
                let mut delete: Option<String> = None;
                for t in &list {
                    let filename = t["filename"].as_str().unwrap_or("").to_string();
                    let name = t["name"].as_str().unwrap_or(&filename);
                    let kind = t["kind"].as_str().unwrap_or("");
                    let valid = t["valid"].as_bool().unwrap_or(false);
                    let enabled = t["enabled"].as_bool().unwrap_or(true);
                    ui.horizontal(|ui| {
                        let selected = self.custom_tool_selected == filename;
                        let mut label = format!("{name}  ({kind})");
                        if !valid {
                            label = format!("⚠ {name}");
                        } else if !enabled {
                            label = format!("{name}  (off)");
                        }
                        if ui.selectable_label(selected, label).clicked() {
                            self.open_custom_tool(&filename);
                        }
                        if ui.small_button("🗑").clicked() {
                            delete = Some(filename.clone());
                        }
                    });
                }
                if let Some(f) = delete {
                    self.delete_custom_tool(&f);
                }
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            if self.custom_tool_selected.is_empty() {
                ui.add_space(20.0);
                ui.label(egui::RichText::new("Select a tool on the left, or create a new one above.").color(dim));
                return;
            }
            ui.label(egui::RichText::new(&self.custom_tool_selected).strong());
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("custom_tool_yaml_scroll")
                .max_height(260.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.custom_tool_yaml)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(14),
                    );
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if Self::save_button(ui, "💾 Save") {
                    self.save_custom_tool(runtime);
                }
                if ui.button("🗑 Delete").clicked() {
                    let f = self.custom_tool_selected.clone();
                    self.delete_custom_tool(&f);
                }
            });
            if let Some(msg) = &self.custom_tool_status {
                let c = if msg.starts_with("Error") || msg.starts_with("Invalid") { red } else { dim };
                ui.label(egui::RichText::new(msg).size(11.0).color(c));
            }

            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Test run (no LLM)").strong());
            ui.horizontal(|ui| {
                ui.label("args (JSON):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_tool_test_args)
                        .desired_width(280.0)
                        .hint_text("{\"query\":\"...\"}"),
                );
                if ui.button("▶ Run").clicked() {
                    self.test_custom_tool(runtime);
                }
            });
            if let Some(res) = &self.custom_tool_test_result {
                egui::ScrollArea::vertical()
                    .id_salt("custom_tool_test_scroll")
                    .max_height(160.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut res.clone())
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    });
            }
        });
    }

    fn create_custom_tool_template(&mut self, kind: &str) {
        let raw = self.custom_tool_new_name.trim().to_lowercase();
        let name: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        if name.is_empty() {
            self.custom_tool_status = Some("Enter a tool name first".into());
            return;
        }
        let yaml = if kind == "shell" {
            format!(
                "name: {name}\ndescription: Describe what this tool does.\nkind: shell\nenabled: true\nparameters:\n  - name: arg1\n    type: string\n    required: true\nrun:\n  command: \"echo {{{{arg1}}}}\"\n  timeout_secs: 30\nrequire_approval: true\n"
            )
        } else {
            format!(
                "name: {name}\ndescription: Describe what this tool does.\nkind: http\nenabled: true\nparameters:\n  - name: query\n    type: string\n    required: true\n  - name: limit\n    type: integer\n    default: 10\nrequest:\n  method: GET\n  url: \"https://api.example.com/search?q={{{{query}}}}&n={{{{limit}}}}\"\n  headers:\n    User-Agent: \"TigrimOS/1.0\"\n  timeout_secs: 20\nresponse:\n  format: auto\n  max_len: 4000\n"
            )
        };
        self.custom_tool_selected = format!("{name}.yaml");
        self.custom_tool_yaml = yaml;
        self.custom_tool_new_name.clear();
        self.custom_tool_test_result = None;
        self.custom_tool_status = Some("New tool — edit and Save.".into());
    }

    /// Open a built-in tool's FULL definition as an editable YAML file
    /// (data/tools/<name>.yaml) — the same system as custom tools: change
    /// description/parameters/config, or replace the implementation with
    /// kind: shell/http + override: true.
    fn open_tool_config_editor(&mut self, tool: &str, runtime: &tokio::runtime::Handle) {
        self.tool_cfg_profile = format!("data/tools/{tool}.yaml");
        self.tool_cfg_status = None;
        self.tool_cfg_yaml = if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            client
                .get(format!("{}/api/custom-tools/builtin/{}", rb.url, tool))
                .bearer_auth(&rb.token)
                .send()
                .ok()
                .and_then(|r| r.json::<serde_json::Value>().ok())
                .and_then(|v| v["content"].as_str().map(|s| s.to_string()))
                .unwrap_or_default()
        } else {
            // Baselines must be the RAW built-in values (not override-applied
            // catalog entries), so a saved file can be diffed back to default.
            let desc = crate::server::services::toolbox::builtin_tool_description(tool)
                .unwrap_or_default();
            let default_approval = runtime.block_on(
                crate::server::services::toolbox::tool_default_requires_approval(tool),
            );
            let schema = crate::server::services::toolbox::tool_parameter_schema(tool);
            crate::server::services::custom_tools::builtin_editor_yaml(
                tool,
                &desc,
                schema.as_ref(),
                default_approval,
            )
        };
        self.tool_cfg_editing = Some(tool.to_string());
    }

    fn tool_config_editor(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let dim = egui::Color32::from_rgb(124, 115, 104);
        let red = egui::Color32::from_rgb(200, 80, 70);
        let tool = self.tool_cfg_editing.clone().unwrap_or_default();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("Per-tool config · {tool}")).strong());
                ui.label(egui::RichText::new(format!("→ {}", self.tool_cfg_profile)).size(11.0).color(dim));
                if ui.button("✕ Close").clicked() {
                    self.tool_cfg_editing = None;
                }
            });
            ui.label(
                egui::RichText::new(
                    "The tool's full definition: edit description, parameters, config — or set kind: shell/http + override: true with a run:/request: block to replace the implementation. Saving defaults removes the file.",
                )
                .size(11.0)
                .color(dim),
            );
            egui::ScrollArea::vertical()
                .id_salt("tool_cfg_scroll")
                .max_height(200.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.tool_cfg_yaml)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(9),
                    );
                });
            ui.horizontal(|ui| {
                if Self::save_button(ui, "💾 Save") {
                    self.save_tool_config_editor(runtime);
                }
            });
            if let Some(msg) = &self.tool_cfg_status {
                let c = if msg.starts_with("Error") || msg.starts_with("Invalid") { red } else { dim };
                ui.label(egui::RichText::new(msg).size(11.0).color(c));
            }
        });
    }

    fn save_tool_config_editor(&mut self, runtime: &tokio::runtime::Handle) {
        let tool = match &self.tool_cfg_editing {
            Some(t) => t.clone(),
            None => return,
        };
        let content = self.tool_cfg_yaml.clone();
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            let resp = client
                .post(format!("{}/api/custom-tools/builtin/{}", rb.url, tool))
                .bearer_auth(&rb.token)
                .json(&serde_json::json!({"content": content}))
                .send();
            self.tool_cfg_status = Some(match resp {
                Ok(r) => {
                    let v = r.json::<serde_json::Value>().unwrap_or_default();
                    match v.get("error").and_then(|e| e.as_str()) {
                        Some(e) => format!("Error: {e}"),
                        None => v["note"].as_str().unwrap_or("Saved ✓").to_string(),
                    }
                }
                Err(e) => format!("Error: {e}"),
            });
        } else {
            let desc = crate::server::services::toolbox::builtin_tool_description(&tool)
                .unwrap_or_default();
            let default_approval = runtime.block_on(
                crate::server::services::toolbox::tool_default_requires_approval(&tool),
            );
            let schema = crate::server::services::toolbox::tool_parameter_schema(&tool);
            self.tool_cfg_status = Some(
                match crate::server::services::custom_tools::save_builtin_doc(
                    &tool,
                    &content,
                    &desc,
                    schema.as_ref(),
                    default_approval,
                ) {
                    Ok((true, w)) if w.is_empty() => "Saved ✓ (override file written)".into(),
                    Ok((true, w)) => format!("Saved — warnings: {}", w.join("; ")),
                    Ok((false, _)) => "Matches built-in defaults — override removed".into(),
                    Err(e) => format!("Error: {e}"),
                },
            );
        }
        // Refresh lists so Catalog source/chips and Custom Tools pick it up.
        self.custom_tools_loaded = false;
    }

    fn section_agent_loop(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        self.load_settings_if_needed(runtime);
        if self.loop_needs_refresh {
            self.loop_needs_refresh = false;
            self.refresh_loop_profiles();
        }

        ui.add_space(8.0);
        ui.heading("Agent Loop Profiles");
        ui.label(
            egui::RichText::new(
                "Customize the agent loop as YAML profiles: allowed tools, MCP servers, skills, \
                 model/system-prompt overrides, loop limits and context compaction. \
                 Omitted sections inherit the built-in behavior; approval prompts always apply.",
            )
            .size(12.0)
            .color(egui::Color32::from_rgb(124, 115, 104)),
        );
        ui.add_space(8.0);

        // --- Active profile selector ---
        ui.horizontal(|ui| {
            ui.label("Active profile:");
            let mut changed = false;
            egui::ComboBox::from_id_salt("active_loop_profile")
                .selected_text(if self.active_loop_profile.is_empty() {
                    "(built-in — no profile)".to_string()
                } else {
                    self.active_loop_profile.clone()
                })
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut self.active_loop_profile, String::new(), "(built-in — no profile)")
                        .changed();
                    for f in self.loop_profiles.clone() {
                        changed |= ui
                            .selectable_value(&mut self.active_loop_profile, f.clone(), f)
                            .changed();
                    }
                });
            if changed {
                self.save_active_loop_profile(runtime);
                self.loop_status_msg = Some(format!(
                    "Active profile set to {}",
                    if self.active_loop_profile.is_empty() { "(built-in)" } else { &self.active_loop_profile }
                ));
            }
        });
        ui.add_space(8.0);
        ui.separator();

        // --- Profile file picker + New / Delete / Reset default ---
        ui.horizontal(|ui| {
            ui.label("Edit profile:");
            let prev = self.loop_selected_file.clone();
            let mut pick = self.loop_selected_file.clone();
            egui::ComboBox::from_id_salt("loop_profile_editor_file")
                .selected_text(if pick.is_empty() { "(none)".to_string() } else { pick.clone() })
                .show_ui(ui, |ui| {
                    for f in self.loop_profiles.clone() {
                        ui.selectable_value(&mut pick, f.clone(), f);
                    }
                });
            if pick != prev && !pick.is_empty() {
                self.open_loop_profile(&pick);
            }

            ui.add_space(8.0);
            ui.label("New:");
            ui.add(
                egui::TextEdit::singleline(&mut self.loop_new_name)
                    .hint_text("profile-name")
                    .desired_width(140.0),
            );
            if ui.button("Create").clicked() && !self.loop_new_name.trim().is_empty() {
                let file = crate::server::services::agent_loop::normalize_filename(
                    &self.loop_new_name,
                );
                self.loop_name = self.loop_new_name.trim().trim_end_matches(".yaml").to_string();
                self.loop_new_name.clear();
                self.loop_selected_file = file;
                self.loop_yaml_mode = false;
                self.save_loop_profile();
            }

            if !self.loop_selected_file.is_empty()
                && self.loop_selected_file != crate::server::services::agent_loop::DEFAULT_PROFILE_FILE
                && ui
                    .add(egui::Button::new(
                        egui::RichText::new("Delete").color(egui::Color32::from_rgb(239, 68, 68)),
                    ))
                    .clicked()
            {
                let file = self.loop_selected_file.clone();
                if let Some(rb) = crate::server::data::get_remote_backend() {
                    let client = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(5))
                        .build()
                        .unwrap_or_default();
                    let _ = client
                        .delete(format!("{}/api/agent-loops/{}", rb.url, file))
                        .bearer_auth(&rb.token)
                        .send();
                } else {
                    let _ = std::fs::remove_file(
                        crate::server::services::agent_loop::agent_loops_dir().join(&file),
                    );
                }
                if self.active_loop_profile == file {
                    self.active_loop_profile.clear();
                    self.save_active_loop_profile(runtime);
                }
                self.loop_selected_file.clear();
                self.loop_status_msg = Some(format!("Deleted {}", file));
                self.loop_needs_refresh = true;
            }

            if ui
                .button("Reset default")
                .on_hover_text("Regenerate default.yaml from the current settings")
                .clicked()
            {
                if crate::server::data::get_remote_backend().is_some() {
                    let rb = crate::server::data::get_remote_backend().unwrap();
                    let client = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(5))
                        .build()
                        .unwrap_or_default();
                    let _ = client
                        .post(format!("{}/api/agent-loops/reset-default", rb.url))
                        .bearer_auth(&rb.token)
                        .send();
                } else {
                    let settings_json = std::fs::read_to_string(
                        crate::server::data::data_dir().join("settings.json"),
                    )
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
                    let profile = crate::server::services::agent_loop::default_profile_from_settings(&settings_json);
                    if let Ok(yaml) = serde_yaml::to_string(&profile) {
                        let _ = Self::write_loop_profile_content(
                            crate::server::services::agent_loop::DEFAULT_PROFILE_FILE,
                            &yaml,
                        );
                    }
                }
                self.loop_status_msg = Some("default.yaml regenerated from current settings".to_string());
                self.loop_needs_refresh = true;
            }
        });

        Self::status_label(ui, &self.loop_status_msg);
        ui.add_space(8.0);

        if self.loop_selected_file.is_empty() {
            ui.label("No profile selected — create one above.");
            return;
        }

        // --- Form / YAML mode toggle ---
        ui.horizontal(|ui| {
            let was_yaml = self.loop_yaml_mode;
            ui.selectable_value(&mut self.loop_yaml_mode, false, "Form");
            ui.selectable_value(&mut self.loop_yaml_mode, true, "Edit as YAML");
            if was_yaml != self.loop_yaml_mode {
                if self.loop_yaml_mode {
                    // form -> YAML: regenerate text from the current form
                    if let Ok(y) = serde_yaml::to_string(&self.loop_form_to_profile()) {
                        self.loop_yaml_text = y;
                    }
                } else {
                    // YAML -> form: re-parse; on error stay in YAML mode
                    match serde_yaml::from_str::<crate::server::services::agent_loop::AgentLoopProfile>(&self.loop_yaml_text) {
                        Ok(p) => {
                            self.populate_loop_form(&p);
                            self.loop_status_msg = None;
                        }
                        Err(e) => {
                            self.loop_status_msg = Some(format!("Error: fix YAML before switching to Form: {}", e));
                            self.loop_yaml_mode = true;
                        }
                    }
                }
            }
            ui.add_space(12.0);
            if Self::save_button(ui, &format!("Save {}", self.loop_selected_file)) {
                self.save_loop_profile();
            }
        });
        ui.add_space(8.0);

        if self.loop_yaml_mode {
            egui::ScrollArea::vertical()
                .id_salt("loop_yaml_editor")
                .max_height(420.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.loop_yaml_text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(24),
                    );
                });
            return;
        }

        // ================= Form mode =================
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.add(egui::TextEdit::singleline(&mut self.loop_name).desired_width(180.0));
            ui.label("Description:");
            ui.add(egui::TextEdit::singleline(&mut self.loop_description).desired_width(320.0));
        });
        ui.add_space(8.0);

        // --- Tools ---
        egui::CollapsingHeader::new("Tools")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    ui.selectable_value(&mut self.loop_tools_mode, "all".to_string(), "All tools");
                    ui.selectable_value(&mut self.loop_tools_mode, "allowlist".to_string(), "Allowlist");
                    ui.selectable_value(&mut self.loop_tools_mode, "denylist".to_string(), "Denylist");
                });
                if self.loop_tools_mode != "all" {
                    ui.label(
                        egui::RichText::new(if self.loop_tools_mode == "allowlist" {
                            "Checked tools are ALLOWED. Coordination tools (send_task, spawn_subagent, …) are always kept while sub-agents are enabled."
                        } else {
                            "Checked tools are BLOCKED. Coordination tools are always kept while sub-agents are enabled."
                        })
                        .size(11.0)
                        .color(egui::Color32::from_rgb(124, 115, 104)),
                    );
                    let catalog = self.loop_tool_catalog.clone();
                    egui::ScrollArea::vertical()
                        .id_salt("loop_tools_list")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            egui::Grid::new("loop_tools_grid").num_columns(3).show(ui, |ui| {
                                for (i, (name, desc)) in catalog.iter().enumerate() {
                                    let mut checked = self.loop_tools_checked.contains(name);
                                    let short = crate::util::truncate_utf8_ellipsis(desc, 60);
                                    if ui.checkbox(&mut checked, name).on_hover_text(short).changed() {
                                        if checked {
                                            self.loop_tools_checked.insert(name.clone());
                                        } else {
                                            self.loop_tools_checked.remove(name);
                                        }
                                    }
                                    if i % 3 == 2 {
                                        ui.end_row();
                                    }
                                }
                            });
                        });
                }
                self.per_tool_config_editor(ui);
            });

        // --- MCP servers ---
        egui::CollapsingHeader::new("MCP Servers")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    ui.selectable_value(&mut self.loop_mcp_mode, "all".to_string(), "All servers");
                    ui.selectable_value(&mut self.loop_mcp_mode, "selected".to_string(), "Selected");
                    ui.selectable_value(&mut self.loop_mcp_mode, "none".to_string(), "None");
                });
                if self.loop_mcp_mode == "selected" {
                    let servers: Vec<String> = self.mcp_tools.iter().map(|s| s.name.clone()).collect();
                    if servers.is_empty() {
                        ui.label("No MCP servers configured (Settings > MCP Tools).");
                    }
                    for name in servers {
                        let mut checked = self.loop_mcp_checked.contains(&name);
                        if ui.checkbox(&mut checked, &name).changed() {
                            if checked {
                                self.loop_mcp_checked.insert(name.clone());
                            } else {
                                self.loop_mcp_checked.remove(&name);
                            }
                        }
                    }
                }
            });

        // --- Skills ---
        egui::CollapsingHeader::new("Skills")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    ui.selectable_value(&mut self.loop_skills_mode, "all".to_string(), "All skills");
                    ui.selectable_value(&mut self.loop_skills_mode, "selected".to_string(), "Selected");
                    ui.selectable_value(&mut self.loop_skills_mode, "none".to_string(), "None");
                });
                if self.loop_skills_mode == "selected" {
                    let skills = self.loop_skill_catalog.clone();
                    if skills.is_empty() {
                        ui.label("No installed skills found.");
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("loop_skills_list")
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for name in skills {
                                let mut checked = self.loop_skills_checked.contains(&name);
                                if ui.checkbox(&mut checked, &name).changed() {
                                    if checked {
                                        self.loop_skills_checked.insert(name.clone());
                                    } else {
                                        self.loop_skills_checked.remove(&name);
                                    }
                                }
                            }
                        });
                }
            });

        // --- Model override ---
        egui::CollapsingHeader::new("Model Override")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Empty fields inherit the main AI settings.")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(124, 115, 104)),
                );
                ui.horizontal(|ui| {
                    ui.label("Model:");
                    ui.add(egui::TextEdit::singleline(&mut self.loop_model_model).desired_width(220.0));
                });
                ui.horizontal(|ui| {
                    ui.label("API URL:");
                    ui.add(egui::TextEdit::singleline(&mut self.loop_model_api_url).desired_width(320.0));
                });
                ui.horizontal(|ui| {
                    ui.label("API Key:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.loop_model_api_key)
                            .password(true)
                            .desired_width(320.0),
                    );
                });
            });

        // --- System prompt ---
        egui::CollapsingHeader::new("System Prompt")
            .default_open(false)
            .show(ui, |ui| {
                ui.checkbox(
                    &mut self.loop_sp_replace_base,
                    "Replace built-in base prompt (skills/persona/project blocks still apply)",
                );
                ui.add(
                    egui::TextEdit::multiline(&mut self.loop_sp_text)
                        .hint_text("Extra instructions appended to the system prompt (or replacing the base when checked)")
                        .desired_width(f32::INFINITY)
                        .desired_rows(5),
                );
            });

        // --- Loop limits & verification ---
        egui::CollapsingHeader::new("Loop Limits & Verification")
            .default_open(false)
            .show(ui, |ui| {
                egui::Grid::new("loop_knobs_grid").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Max tool rounds:");
                    ui.add(egui::DragValue::new(&mut self.loop_max_rounds).range(1..=500));
                    ui.label("Max tool calls:");
                    ui.add(egui::DragValue::new(&mut self.loop_max_tool_calls).range(1..=1000));
                    ui.end_row();
                    ui.label("Temperature:");
                    ui.add(egui::DragValue::new(&mut self.loop_temperature).speed(0.05).range(0.0..=2.0));
                    ui.label("Max tokens:");
                    ui.add(egui::DragValue::new(&mut self.loop_max_tokens).range(256..=200_000));
                    ui.end_row();
                    ui.label("Max spawn depth:");
                    ui.add(egui::DragValue::new(&mut self.loop_max_spawn_depth).range(1..=5));
                    ui.label("Checkpoints:");
                    ui.checkbox(&mut self.loop_checkpoint_enabled, "");
                    ui.end_row();
                });
                ui.label(
                    egui::RichText::new("Note: Kimi/thinking/MiniMax models always run at temperature 1.0 (provider requirement).")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(124, 115, 104)),
                );
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.loop_reflection_enabled,
                    "Reflection loop — judge the final answer against the objective and retry gaps",
                );
                if self.loop_reflection_enabled {
                    ui.horizontal(|ui| {
                        ui.label("Threshold:");
                        ui.add(egui::Slider::new(&mut self.loop_reflection_threshold, 0.1..=1.0));
                        ui.label("Max retries:");
                        ui.add(egui::DragValue::new(&mut self.loop_max_reflection_retries).range(1..=5));
                    });
                }
                ui.checkbox(
                    &mut self.loop_step_verification,
                    "Step verification — judge each team agent's finished step and retry failures",
                );
            });

        // --- Outer evaluation loop ---
        egui::CollapsingHeader::new("Job Evaluation (Outer Loop)")
            .default_open(false)
            .show(ui, |ui| {
                ui.checkbox(
                    &mut self.loop_eval_enabled,
                    "Evaluate the finished job — tool-using judge verifies the final result",
                )
                .on_hover_text("Runs ONCE after the whole job finishes — main agent only, never \
                    sub-agents. The judge may read output files to verify claimed artifacts. Below \
                    the threshold, the gap list is fed back so the orchestrator can delegate fixes.");
                if self.loop_eval_enabled {
                    egui::Grid::new("loop_eval_grid").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Threshold:");
                        ui.add(egui::Slider::new(&mut self.loop_eval_threshold, 0.1..=1.0));
                        ui.label("Max retries:");
                        ui.add(egui::DragValue::new(&mut self.loop_eval_max_retries).range(1..=5));
                        ui.end_row();
                        ui.label("Judge tool rounds:");
                        ui.add(egui::DragValue::new(&mut self.loop_eval_max_judge_rounds).range(1..=6));
                        ui.label("Fix rounds per retry:");
                        ui.add(egui::DragValue::new(&mut self.loop_eval_max_fix_rounds).range(1..=10));
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Judge model:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.loop_eval_model)
                                .hint_text("empty = session model")
                                .desired_width(220.0),
                        )
                        .on_hover_text("A different model avoids the agent grading its own work. \
                            Judge api_url/api_key can be set in the YAML editor.");
                    });
                    ui.label("Rubric (success criteria the judge must check):");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.loop_eval_rubric)
                            .hint_text("Optional, e.g. \"A PNG chart file must exist and the answer must reference it.\"")
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    ui.checkbox(
                        &mut self.loop_eval_allow_execute,
                        "Allow the judge to execute code (run_python/run_shell)",
                    );
                    if self.loop_eval_allow_execute {
                        ui.label(
                            egui::RichText::new("⚠ The evaluator judge will run code in the sandbox without approval prompts.")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(200, 120, 40)),
                        );
                    }
                }
            });

        // --- Graph gate (judge panel) ---
        egui::CollapsingHeader::new("Graph Gate (Judge Panel)")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Judge panel reviewing the final answer before delivery (configure judges \
                         in the Graph tab). Inherit = follow the global toggle in the Graph tab \
                         (default off).",
                    )
                    .size(11.0)
                    .color(egui::Color32::from_rgb(124, 115, 104)),
                );
                ui.horizontal(|ui| {
                    ui.label("Gate:");
                    egui::ComboBox::from_id_salt("loop_graph_gate")
                        .selected_text(match self.loop_graph_gate.as_str() {
                            "on" => "On",
                            "off" => "Off",
                            _ => "Inherit (settings)",
                        })
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.loop_graph_gate, "inherit".to_string(), "Inherit (settings)");
                            ui.selectable_value(&mut self.loop_graph_gate, "on".to_string(), "On");
                            ui.selectable_value(&mut self.loop_graph_gate, "off".to_string(), "Off");
                        });
                    ui.label("Graph profile:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.loop_graph_profile)
                            .desired_width(180.0)
                            .hint_text("(blank = active profile)"),
                    )
                    .on_hover_text("Filename in data/graph/, e.g. strict.yaml");
                });
            });

        // --- Compaction ---
        egui::CollapsingHeader::new("Context Compaction")
            .default_open(false)
            .show(ui, |ui| {
                ui.checkbox(
                    &mut self.loop_compact_enabled,
                    "Periodic compression (over-budget and overflow compaction always stay on)",
                );
                egui::Grid::new("loop_compact_grid").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Every N rounds:");
                    ui.add(egui::DragValue::new(&mut self.loop_compact_interval).range(1..=50));
                    ui.label("Keep last N messages:");
                    ui.add(egui::DragValue::new(&mut self.loop_compact_window).range(2..=100));
                    ui.end_row();
                    ui.label("Max context tokens:");
                    ui.add(egui::DragValue::new(&mut self.loop_compact_max_context_tokens).range(4_000..=2_000_000));
                    ui.label("Tool result max chars:");
                    ui.add(egui::DragValue::new(&mut self.loop_compact_tool_result_max_len).range(500..=100_000));
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    ui.label("Summarization model:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.loop_compact_model)
                            .hint_text("empty = session model")
                            .desired_width(220.0),
                    );
                });
            });
    }
}
