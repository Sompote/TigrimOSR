//! Slash-command parsing and dispatch for the CLI REPL.
//!
//! Mirrors the Telegram/LINE bot commands (services/messaging) but calls the
//! underlying services directly and adds the CLI-only commands (/agent,
//! /graph, /skills, /mcp, /settings, /tasks, /clear, /exit).

use crate::cli::project::{self, CliState};
use crate::server::data;
use crate::server::routes::chat;
use crate::server::services::messaging::SUB_AGENT_MODES;
use crate::server::services::{agent_loop, graph, mcp, toolbox};

#[derive(Debug, Clone, PartialEq)]
pub enum CliCommand {
    Agents,
    Agent(Option<String>),
    Model(Option<String>),
    Mode(Option<String>),
    Loop(Option<String>),
    Graph(Option<String>),
    Skills,
    Mcp,
    Settings,
    New,
    Stop,
    Status,
    Tasks,
    Help,
    Clear,
    Exit,
    Chat(String),
    Unknown(String),
}

pub enum Outcome {
    Reply(String),
    RunChat(String),
    ClearScreen,
    Exit,
}

pub fn parse(line: &str) -> CliCommand {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return CliCommand::Chat(String::new());
    }
    if !trimmed.starts_with('/') {
        return CliCommand::Chat(trimmed.to_string());
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
    let arg = parts
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match cmd.as_str() {
        "/agents" => CliCommand::Agents,
        "/agent" => CliCommand::Agent(arg),
        "/model" => CliCommand::Model(arg),
        "/mode" => CliCommand::Mode(arg),
        "/loop" => CliCommand::Loop(arg),
        "/graph" => CliCommand::Graph(arg),
        "/skills" => CliCommand::Skills,
        "/mcp" => CliCommand::Mcp,
        "/settings" => CliCommand::Settings,
        "/new" => CliCommand::New,
        "/stop" => CliCommand::Stop,
        "/status" => CliCommand::Status,
        "/tasks" => CliCommand::Tasks,
        "/help" | "/?" => CliCommand::Help,
        "/clear" => CliCommand::Clear,
        "/exit" | "/quit" => CliCommand::Exit,
        other => CliCommand::Unknown(other.to_string()),
    }
}

pub fn help_text() -> String {
    "\
Commands:
  /agents             List agent team configs (YAML)
  /agent <file|off>   Use an agent team config for this folder's sessions
  /model [id]         Show or set the global model
  /mode [mode]        Sub-agent mode: single|auto|manual|fully_auto|router|graph
  /loop [name|off]    Agent-loop profile for this folder
  /graph [name|off]   Graph (judge panel) profile for this folder
  /skills             List installed skills
  /mcp                List MCP servers and connection status
  /settings           Show effective settings (project overlay marked)
  /new                Start a fresh session
  /stop               Stop the running task
  /status             Show model, mode, profiles, session
  /tasks              Show running chats + scheduled tasks
  /clear              Clear the screen
  /help               This help
  /exit               Quit (also Ctrl-D)

Anything else is sent to the agent as a chat message."
        .to_string()
}

/// Union-list `*.yaml` names across project + global overlay dirs.
/// Returns (filename, is_project_local); project entries shadow global ones.
fn list_yaml_overlay(rel: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let dirs = data::overlay_dirs(rel);
    for dir in dirs.iter() {
        let is_project = data::project_dir().is_some_and(|p| dir.starts_with(p));
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if (name.ends_with(".yaml") || name.ends_with(".yml"))
                    && !out.iter().any(|(n, _)| *n == name)
                {
                    out.push((name, is_project));
                }
            }
        }
    }
    out.sort();
    out
}

fn mark(is_project: bool) -> &'static str {
    if is_project {
        " (project)"
    } else {
        ""
    }
}

fn mask_key(key: &str) -> String {
    if key.is_empty() {
        "(not set)".to_string()
    } else {
        format!("{}…", crate::util::truncate_utf8(key, 6))
    }
}

async fn session_running(state: &CliState) -> bool {
    let Some(sid) = state.session_id.as_deref() else {
        return false;
    };
    crate::ui::tasks_view::active_chats()
        .lock()
        .map(|chats| chats.iter().any(|c| c.session_id == sid))
        .unwrap_or(false)
}

pub async fn execute(state: &mut CliState, cmd: CliCommand) -> Outcome {
    use Outcome::*;
    match cmd {
        CliCommand::Chat(text) if text.is_empty() => Reply(String::new()),
        CliCommand::Chat(text) => RunChat(text),
        CliCommand::Help => Reply(help_text()),
        CliCommand::Clear => ClearScreen,
        CliCommand::Exit => Exit,
        CliCommand::Unknown(c) => {
            Reply(format!("Unknown command {} — /help for the list.", c))
        }

        CliCommand::Agents => {
            let names = list_yaml_overlay("agents");
            if names.is_empty() {
                return Reply(
                    "No agent team configs found. Put YAML files in .tigrimos/agents/ or let fully_auto/router mode generate a team."
                        .to_string(),
                );
            }
            let mut lines = vec![format!("Agent team configs ({}):", names.len())];
            for (name, is_project) in names {
                match toolbox::load_agent_yaml(&name) {
                    Some((yaml, ids)) => {
                        let team = yaml
                            .get("system")
                            .and_then(|s| s.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if team.is_empty() {
                            lines.push(format!("• {} — {} agents{}", name, ids.len(), mark(is_project)));
                        } else {
                            lines.push(format!(
                                "• {} — {} ({} agents){}",
                                name,
                                team,
                                ids.len(),
                                mark(is_project)
                            ));
                        }
                    }
                    None => lines.push(format!("• {} — (invalid YAML){}", name, mark(is_project))),
                }
            }
            lines.push("\nUse /agent <file> to select, /agent off to clear.".to_string());
            Reply(lines.join("\n"))
        }

        CliCommand::Agent(None) => Reply(match &state.config_file {
            Some(f) => format!("Active agent config: {}\nUse /agent off to clear.", f),
            None => "No agent config selected. /agents lists them, /agent <file> selects.".to_string(),
        }),
        CliCommand::Agent(Some(arg)) => {
            if arg.eq_ignore_ascii_case("off") {
                state.config_file = None;
                project::save_state(state).await;
                return Reply("Agent config cleared.".to_string());
            }
            let filename = if arg.ends_with(".yaml") || arg.ends_with(".yml") {
                arg.clone()
            } else {
                format!("{}.yaml", arg)
            };
            match toolbox::load_agent_yaml(&filename) {
                Some((_, ids)) => {
                    state.config_file = Some(filename.clone());
                    // A team config only takes effect in a team mode.
                    if state.mode.as_deref().unwrap_or("single") == "single" {
                        state.mode = Some("auto".to_string());
                    }
                    project::save_state(state).await;
                    Reply(format!(
                        "Agent config set to {} ({} agents), mode {}.",
                        filename,
                        ids.len(),
                        state.mode.as_deref().unwrap_or("auto")
                    ))
                }
                None => Reply(format!(
                    "Config '{}' not found or invalid. /agents lists available configs.",
                    filename
                )),
            }
        }

        CliCommand::Model(None) => {
            let s = data::get_settings().await;
            let mut lines = vec![format!("Current model: {}", s.tiger_bot_model)];
            if let Some(pool) = s.model_pool.as_ref().filter(|p| !p.is_empty()) {
                lines.push("Model pool:".to_string());
                for e in pool {
                    lines.push(format!("• {} ({})", e.label, e.model));
                }
            }
            lines.push("\nUse /model <model-id> to switch — applies globally.".to_string());
            Reply(lines.join("\n"))
        }
        CliCommand::Model(Some(m)) => {
            let mut s = data::get_settings().await;
            s.tiger_bot_model = m.clone();
            data::save_settings(&s).await;
            Reply(format!("Model set to {} (global settings).", m))
        }

        CliCommand::Mode(None) => Reply(format!(
            "Sub-agent mode: {}\nValid: {}\nUse /mode <mode> to switch.",
            state.mode.as_deref().unwrap_or("single"),
            SUB_AGENT_MODES.join(", ")
        )),
        CliCommand::Mode(Some(m)) => {
            let m = m.to_ascii_lowercase();
            if !SUB_AGENT_MODES.contains(&m.as_str()) {
                return Reply(format!("Unknown mode '{}'. Valid: {}", m, SUB_AGENT_MODES.join(", ")));
            }
            state.mode = if m == "single" { None } else { Some(m.clone()) };
            project::save_state(state).await;
            Reply(format!("Sub-agent mode set to {}.", m))
        }

        CliCommand::Loop(None) => {
            let names = list_yaml_overlay("agent_loops");
            let current = state.loop_profile.as_deref().unwrap_or("(settings default)");
            let mut lines = vec![format!("Agent-loop profile: {}", current)];
            if !names.is_empty() {
                lines.push("Available:".to_string());
                for (n, p) in names {
                    lines.push(format!("• {}{}", n, mark(p)));
                }
            }
            lines.push("\nUse /loop <profile> to set, /loop off to clear.".to_string());
            Reply(lines.join("\n"))
        }
        CliCommand::Loop(Some(name)) => {
            if name.eq_ignore_ascii_case("off") {
                state.loop_profile = None;
                project::save_state(state).await;
                return Reply("Loop profile cleared — using the settings default.".to_string());
            }
            if agent_loop::load_profile(&name).is_none() {
                return Reply(format!("Profile '{}' not found or invalid. /loop lists them.", name));
            }
            state.loop_profile = Some(name.clone());
            project::save_state(state).await;
            Reply(format!("Agent-loop profile set to {}.", name))
        }

        CliCommand::Graph(None) => {
            let names = list_yaml_overlay("graph");
            let current = state.graph_profile.as_deref().unwrap_or("(settings default)");
            let mut lines = vec![format!("Graph profile: {}", current)];
            if !names.is_empty() {
                lines.push("Available:".to_string());
                for (n, p) in names {
                    lines.push(format!("• {}{}", n, mark(p)));
                }
            }
            lines.push("\nUse /graph <profile> to set (activates graph mode), /graph off to clear.".to_string());
            Reply(lines.join("\n"))
        }
        CliCommand::Graph(Some(name)) => {
            if name.eq_ignore_ascii_case("off") {
                state.graph_profile = None;
                if state.mode.as_deref() == Some("graph") {
                    state.mode = None;
                }
                project::save_state(state).await;
                return Reply("Graph profile cleared.".to_string());
            }
            if graph::load_profile(&name).is_none() {
                return Reply(format!("Graph profile '{}' not found or invalid. /graph lists them.", name));
            }
            state.graph_profile = Some(name.clone());
            state.mode = Some("graph".to_string());
            project::save_state(state).await;
            Reply(format!("Graph profile set to {} (mode graph).", name))
        }

        CliCommand::Skills => {
            let skills = data::get_skills().await;
            if skills.is_empty() {
                return Reply("No skills installed.".to_string());
            }
            let mut lines = vec![format!("Installed skills ({}):", skills.len())];
            for s in &skills {
                lines.push(format!(
                    "• {}{} — {}",
                    s.name,
                    if s.enabled { "" } else { " (disabled)" },
                    crate::util::truncate_utf8(&s.description, 80)
                ));
            }
            Reply(lines.join("\n"))
        }

        CliCommand::Mcp => {
            let s = data::get_settings().await;
            if s.mcp_tools.is_empty() {
                return Reply("No MCP servers configured (Settings > MCP, or settings.json mcpTools).".to_string());
            }
            let status = mcp::get_connection_status().await;
            let mut lines = vec![format!("MCP servers ({}):", s.mcp_tools.len())];
            for t in &s.mcp_tools {
                let conn = status.iter().find(|(n, _, _, _)| n == &t.name);
                let state_str = match conn {
                    Some((_, true, tools, _)) => format!("connected, {} tools", tools),
                    Some((_, false, _, err)) => {
                        format!("disconnected{}", err.as_deref().map(|e| format!(" — {}", e)).unwrap_or_default())
                    }
                    None => "not connected".to_string(),
                };
                let transport = t.tool_type.as_deref().unwrap_or(if t.command.is_some() { "stdio" } else { "http" });
                lines.push(format!(
                    "• {} [{}]{} — {}",
                    t.name,
                    transport,
                    if t.enabled { "" } else { " (disabled)" },
                    state_str
                ));
            }
            Reply(lines.join("\n"))
        }

        CliCommand::Settings => {
            let s = data::get_settings().await;
            let proj = data::project_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".to_string());
            let overlay = data::project_dir()
                .map(|p| p.join("settings.json").exists())
                .unwrap_or(false);
            Reply(format!(
                "Model: {}\nAPI URL: {}\nAPI key: {}\nWorkspace: {}\nGlobal data dir: {}\nProject dir: {}{}\nMode: {}\nLoop profile: {}\nGraph profile: {}\nAgent config: {}",
                s.tiger_bot_model,
                s.tiger_bot_api_url.as_deref().unwrap_or("(default)"),
                mask_key(&s.tiger_bot_api_key),
                data::get_sandbox_dir_sync(),
                data::data_dir().display(),
                proj,
                if overlay { " — settings.json overlay active" } else { "" },
                state.mode.as_deref().unwrap_or("single"),
                state.loop_profile.as_deref().unwrap_or("(settings default)"),
                state.graph_profile.as_deref().unwrap_or("(settings default)"),
                state.config_file.as_deref().unwrap_or("(none)"),
            ))
        }

        CliCommand::New => {
            state.session_id = Some(project::new_session_id());
            project::save_state(state).await;
            Reply("Started a fresh session. Previous conversation stays in this folder's history.".to_string())
        }

        CliCommand::Stop => {
            let Some(sid) = state.session_id.clone() else {
                return Reply("Nothing is running.".to_string());
            };
            if chat::kill_session_by_id(&sid).await {
                Reply("🛑 Stopped the running task (its processes were terminated).".to_string())
            } else {
                Reply("Nothing is running.".to_string())
            }
        }

        CliCommand::Status => {
            let s = data::get_settings().await;
            let running = session_running(state).await;
            let loop_display = state
                .loop_profile
                .clone()
                .or(s.agent_loop_profile.clone().filter(|p| !p.is_empty()))
                .unwrap_or_else(|| "(built-in)".to_string());
            Reply(format!(
                "Model: {} (global)\nMode: {}\nLoop profile: {}\nGraph profile: {}\nAgent config: {}\nSession: {}\nRunning: {}",
                s.tiger_bot_model,
                state.mode.as_deref().unwrap_or("single"),
                loop_display,
                state.graph_profile.as_deref().unwrap_or("(settings default)"),
                state.config_file.as_deref().unwrap_or("(none)"),
                state.session_id.as_deref().unwrap_or("(new on first message)"),
                if running { "yes — /stop to cancel" } else { "no" }
            ))
        }

        CliCommand::Tasks => {
            let mut lines = Vec::new();
            {
                let chats = crate::ui::tasks_view::active_chats()
                    .lock()
                    .map(|c| c.clone())
                    .unwrap_or_default();
                if chats.is_empty() {
                    lines.push("No running chats.".to_string());
                } else {
                    lines.push(format!("Running chats ({}):", chats.len()));
                    for c in &chats {
                        lines.push(format!(
                            "• {} — {} agents, {} tool calls (since {})",
                            crate::util::truncate_utf8(&c.title, 50),
                            c.agent_count,
                            c.tool_calls,
                            c.started_at.format("%H:%M:%S")
                        ));
                    }
                }
            }
            let tasks: Vec<data::ScheduledTask> = data::read_json("tasks.json").await;
            if !tasks.is_empty() {
                lines.push(format!("\nScheduled tasks ({}):", tasks.len()));
                for t in &tasks {
                    lines.push(format!(
                        "• {} [{}]{} — {}",
                        t.name,
                        t.cron,
                        if t.enabled { "" } else { " (disabled)" },
                        crate::util::truncate_utf8(&t.command, 60)
                    ));
                }
            }
            Reply(lines.join("\n"))
        }
    }
}
