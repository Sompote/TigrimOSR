use std::collections::HashMap;
use std::io::Read as IoRead;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::fs;

use crate::server::data::{self, data_dir, get_settings, get_skills, save_settings, save_skills, Skill};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub components: PluginComponents,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginComponents {
    #[serde(default)]
    pub skills: Vec<ComponentRef>,
    #[serde(default)]
    pub agents: Vec<ComponentRef>,
    #[serde(default)]
    pub mcp_servers: Vec<ComponentRef>,
    #[serde(default)]
    pub connectors: Vec<ConnectorRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentRef {
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorRef {
    pub path: String,
    pub name: String,
    pub service: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub config_fields: Vec<ConfigField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type", default = "default_field_type")]
    pub field_type: String,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub default: Option<String>,
}

fn default_field_type() -> String {
    "text".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub components: PluginComponents,
    #[serde(rename = "installedAt")]
    pub installed_at: String,
    #[serde(rename = "skillIds", default)]
    pub skill_ids: Vec<String>,
    #[serde(rename = "mcpNames", default)]
    pub mcp_names: Vec<String>,
    #[serde(rename = "agentFiles", default)]
    pub agent_files: Vec<String>,
    #[serde(rename = "hasReadme", default)]
    pub has_readme: bool,
    #[serde(rename = "hasIcon", default)]
    pub has_icon: bool,
}

// ---------------------------------------------------------------------------
// Data persistence
// ---------------------------------------------------------------------------

pub async fn get_plugins() -> Vec<InstalledPlugin> {
    data::read_json("plugins.json").await
}

pub async fn save_plugins(plugins: &[InstalledPlugin]) {
    data::write_json("plugins.json", &plugins.to_vec()).await;
}

// ---------------------------------------------------------------------------
// Plugin directory
// ---------------------------------------------------------------------------

fn plugins_dir() -> PathBuf {
    data_dir().join("plugins")
}

fn plugin_dir(id: &str) -> PathBuf {
    plugins_dir().join(id)
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

const MAX_UNCOMPRESSED_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
const MAX_FILES: usize = 500;

pub async fn install_plugin(zip_bytes: &[u8]) -> Result<InstalledPlugin, String> {
    // Parse ZIP
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("Invalid ZIP: {e}"))?;

    // Security: check file count and total size
    if archive.len() > MAX_FILES {
        return Err(format!("ZIP contains too many files ({} > {})", archive.len(), MAX_FILES));
    }
    let mut total_size: u64 = 0;
    for i in 0..archive.len() {
        let file = archive.by_index(i).map_err(|e| format!("ZIP read error: {e}"))?;
        let name = file.name().to_string();
        // Security: no path traversal
        if name.contains("..") || name.starts_with('/') {
            return Err(format!("Invalid path in ZIP: {}", name));
        }
        total_size += file.size();
        if total_size > MAX_UNCOMPRESSED_SIZE {
            return Err("ZIP uncompressed size exceeds 100 MB limit".to_string());
        }
    }

    // Find and parse manifest (AndrewOS plugin.yaml or Claude formats)
    let manifest = find_and_parse_manifest(&mut archive)?;

    // Validate plugin ID
    let id_re = regex::Regex::new(r"^[a-z0-9][a-z0-9-]*$").unwrap();
    if !id_re.is_match(&manifest.id) {
        return Err(format!("Invalid plugin ID '{}': must match [a-z0-9][a-z0-9-]*", manifest.id));
    }

    // Check for duplicate
    let mut plugins = get_plugins().await;
    if plugins.iter().any(|p| p.id == manifest.id) {
        // Remove existing before reinstall
        uninstall_plugin_inner(&manifest.id, &mut plugins).await?;
    }

    // Determine root prefix (if the zip has a top-level folder)
    let zip_root_prefix = detect_zip_root(&mut archive);

    // Extract to data/plugins/{id}/
    let dest = plugin_dir(&manifest.id);
    let _ = fs::create_dir_all(&dest).await;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| format!("ZIP read error: {e}"))?;
        let raw_name = file.name().to_string();
        if file.is_dir() {
            continue;
        }
        let relative = raw_name.strip_prefix(&zip_root_prefix).unwrap_or(&raw_name);
        if relative.is_empty() {
            continue;
        }
        let out_path = dest.join(relative);
        if let Some(parent) = out_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut buf = Vec::new();
        let _ = file.read_to_end(&mut buf);
        let _ = std::fs::write(&out_path, &buf);
    }

    // Generate MCP config files for Claude format plugins (post-extraction)
    generate_claude_mcp_configs(&mut archive, &dest);

    // Register components
    let mut skill_ids = Vec::new();
    let mut mcp_names = Vec::new();
    let mut agent_files = Vec::new();

    // 1. Skills — register in skills.json
    let mut skills = get_skills().await;
    for comp in &manifest.components.skills {
        let skill_source_dir = dest.join(&comp.path);
        let skill_md = skill_source_dir.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        let skill_id = uuid::Uuid::new_v4().to_string();
        // Copy skill dir to data/skills/plugin_{id}_{name}
        let skill_slug = format!("plugin_{}_{}", manifest.id, slugify(&comp.name));
        let skill_dest = data_dir().join("skills").join(&skill_slug);
        let _ = copy_dir_recursive(&skill_source_dir, &skill_dest).await;

        skills.push(Skill {
            id: skill_id.clone(),
            name: comp.name.clone(),
            description: comp.description.clone().unwrap_or_default(),
            source: format!("plugin:{}", manifest.id),
            script: skill_slug,
            enabled: true,
            installed_at: chrono::Utc::now().to_rfc3339(),
            review_status: Some("approved".to_string()),
            auto_meta: None,
        });
        skill_ids.push(skill_id);
    }
    save_skills(&skills).await;

    // 2. Agent configs — copy to data/agents/ with prefix
    let agents_dir = data_dir().join("agents");
    let _ = fs::create_dir_all(&agents_dir).await;
    for comp in &manifest.components.agents {
        let src = dest.join(&comp.path);
        if !src.exists() {
            continue;
        }
        let filename = format!("plugin_{}_{}", manifest.id,
            Path::new(&comp.path).file_name().unwrap_or_default().to_string_lossy());
        let dst = agents_dir.join(&filename);
        let _ = fs::copy(&src, &dst).await;
        agent_files.push(filename);
    }

    // 3. MCP servers — add to settings
    let mut settings = get_settings().await;
    for comp in &manifest.components.mcp_servers {
        let config_path = dest.join(&comp.path);
        if !config_path.exists() {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(mut mcp_config) = serde_json::from_str::<Value>(&content) {
                // Handle mcpServers map (Claude Desktop / .mcp.json format)
                if let Some(servers) = mcp_config.get("mcpServers").and_then(|v| v.as_object()).cloned() {
                    for (srv_name, srv_config) in servers {
                        let mut cfg = srv_config.clone();
                        resolve_mcp_paths(&mut cfg, &dest);
                        let mcp_tool = value_to_mcp_tool(&srv_name, &cfg);
                        settings.mcp_tools.retain(|m| m.name != srv_name);
                        settings.mcp_tools.push(mcp_tool);
                        mcp_names.push(srv_name);
                    }
                } else {
                    // Single MCP server config
                    resolve_mcp_paths(&mut mcp_config, &dest);
                    let mcp_name = mcp_config.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&comp.name)
                        .to_string();

                    let mcp_tool = value_to_mcp_tool(&mcp_name, &mcp_config);
                    settings.mcp_tools.retain(|m| m.name != mcp_name);
                    settings.mcp_tools.push(mcp_tool);
                    mcp_names.push(mcp_name);
                }
            }
        }
    }
    save_settings(&settings).await;

    // 4. Connectors — initialize config file with defaults
    if !manifest.components.connectors.is_empty() {
        let mut connector_configs: HashMap<String, Value> = HashMap::new();
        for conn in &manifest.components.connectors {
            let mut defaults = serde_json::Map::new();
            for field in &conn.config_fields {
                if let Some(ref default_val) = field.default {
                    defaults.insert(field.key.clone(), Value::String(default_val.clone()));
                }
            }
            connector_configs.insert(conn.service.clone(), Value::Object(defaults));
        }
        let config_path = dest.join("connector_config.json");
        if !config_path.exists() {
            let _ = std::fs::write(&config_path, serde_json::to_string_pretty(&connector_configs).unwrap_or_default());
        }
    }

    // Check for README and icon
    let has_readme = dest.join("README.md").exists();
    let has_icon = dest.join("icon.png").exists();

    let installed = InstalledPlugin {
        id: manifest.id.clone(),
        name: manifest.name,
        version: manifest.version,
        author: manifest.author,
        description: manifest.description,
        category: manifest.category,
        enabled: true,
        permissions: manifest.permissions,
        components: manifest.components,
        installed_at: chrono::Utc::now().to_rfc3339(),
        skill_ids,
        mcp_names,
        agent_files,
        has_readme,
        has_icon,
    };

    plugins.push(installed.clone());
    save_plugins(&plugins).await;

    Ok(installed)
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

pub async fn uninstall_plugin(id: &str) -> Result<(), String> {
    let mut plugins = get_plugins().await;
    uninstall_plugin_inner(id, &mut plugins).await?;
    save_plugins(&plugins).await;
    Ok(())
}

async fn uninstall_plugin_inner(id: &str, plugins: &mut Vec<InstalledPlugin>) -> Result<(), String> {
    let idx = plugins.iter().position(|p| p.id == id)
        .ok_or_else(|| format!("Plugin '{}' not found", id))?;
    let plugin = plugins.remove(idx);

    // Remove skills
    let mut skills = get_skills().await;
    let source_tag = format!("plugin:{}", id);
    skills.retain(|s| s.source != source_tag);
    save_skills(&skills).await;

    // Remove skill directories
    for comp in &plugin.components.skills {
        let skill_slug = format!("plugin_{}_{}", id, slugify(&comp.name));
        let skill_dir = data_dir().join("skills").join(&skill_slug);
        let _ = fs::remove_dir_all(&skill_dir).await;
    }

    // Remove agent files
    let agents_dir = data_dir().join("agents");
    for filename in &plugin.agent_files {
        let _ = fs::remove_file(agents_dir.join(filename)).await;
    }

    // Remove MCP entries
    let mut settings = get_settings().await;
    for mcp_name in &plugin.mcp_names {
        settings.mcp_tools.retain(|m| m.name != *mcp_name);
    }
    save_settings(&settings).await;

    // Remove plugin directory
    let dir = plugin_dir(id);
    let _ = fs::remove_dir_all(&dir).await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Toggle enable/disable
// ---------------------------------------------------------------------------

pub async fn toggle_plugin(id: &str, enabled: bool) -> Result<InstalledPlugin, String> {
    let mut plugins = get_plugins().await;
    let plugin = plugins.iter_mut().find(|p| p.id == id)
        .ok_or_else(|| format!("Plugin '{}' not found", id))?;
    plugin.enabled = enabled;

    // Toggle skills
    let source_tag = format!("plugin:{}", id);
    let mut skills = get_skills().await;
    for skill in &mut skills {
        if skill.source == source_tag {
            skill.enabled = enabled;
        }
    }
    save_skills(&skills).await;

    // Toggle MCP tools
    let mut settings = get_settings().await;
    for mcp_name in &plugin.mcp_names {
        if let Some(mcp) = settings.mcp_tools.iter_mut().find(|m| m.name == *mcp_name) {
            mcp.enabled = enabled;
        }
    }
    save_settings(&settings).await;

    let result = plugin.clone();
    save_plugins(&plugins).await;
    Ok(result)
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

pub async fn list_plugins() -> Vec<InstalledPlugin> {
    get_plugins().await
}

pub async fn get_plugin(id: &str) -> Option<InstalledPlugin> {
    get_plugins().await.into_iter().find(|p| p.id == id)
}

pub async fn get_plugin_readme(id: &str) -> Option<String> {
    let readme_path = plugin_dir(id).join("README.md");
    fs::read_to_string(&readme_path).await.ok()
}

pub async fn get_plugin_icon(id: &str) -> Option<Vec<u8>> {
    let icon_path = plugin_dir(id).join("icon.png");
    fs::read(&icon_path).await.ok()
}

// ---------------------------------------------------------------------------
// Connector config
// ---------------------------------------------------------------------------

pub async fn get_connector_config(plugin_id: &str, service: &str) -> Value {
    let config_path = plugin_dir(plugin_id).join("connector_config.json");
    if let Ok(content) = fs::read_to_string(&config_path).await {
        if let Ok(configs) = serde_json::from_str::<HashMap<String, Value>>(&content) {
            if let Some(cfg) = configs.get(service) {
                return cfg.clone();
            }
        }
    }
    Value::Object(serde_json::Map::new())
}

pub async fn save_connector_config(plugin_id: &str, service: &str, config: Value) -> Result<(), String> {
    let config_path = plugin_dir(plugin_id).join("connector_config.json");
    let mut configs: HashMap<String, Value> = if let Ok(content) = fs::read_to_string(&config_path).await {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        HashMap::new()
    };
    configs.insert(service.to_string(), config);
    let json = serde_json::to_string_pretty(&configs).map_err(|e| e.to_string())?;
    fs::write(&config_path, json).await.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn find_and_parse_manifest(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>) -> Result<PluginManifest, String> {
    // Collect file names for detection
    let mut file_names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            file_names.push(file.name().to_string());
        }
    }

    // 1. AndrewOS native: plugin.yaml
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| format!("ZIP error: {e}"))?;
        let name = file.name().to_string();
        if name.ends_with("plugin.yaml") || name.ends_with("plugin.yml") {
            let mut buf = String::new();
            file.read_to_string(&mut buf).map_err(|e| format!("Read error: {e}"))?;
            let manifest: PluginManifest = serde_yaml::from_str(&buf)
                .map_err(|e| format!("Invalid plugin.yaml: {e}"))?;
            return Ok(manifest);
        }
    }

    // 2. Claude Desktop Extension (MCPB): manifest.json with manifest_version + server
    if let Some(json) = read_zip_json(archive, "manifest.json", &file_names) {
        if json.get("manifest_version").is_some() && json.get("server").is_some() {
            return Ok(manifest_from_mcpb(&json));
        }
    }

    // 3. Claude Code plugin: .claude-plugin/plugin.json
    if let Some(json) = read_zip_json(archive, ".claude-plugin/plugin.json", &file_names) {
        return Ok(manifest_from_claude_code_plugin(&json, &file_names));
    }

    // 4. Claude Desktop config: claude_desktop_config.json with mcpServers
    if let Some(json) = read_zip_json(archive, "claude_desktop_config.json", &file_names) {
        if json.get("mcpServers").is_some() {
            return Ok(manifest_from_desktop_config(&json));
        }
    }

    // 5. package.json with MCP indicators
    if let Some(json) = read_zip_json(archive, "package.json", &file_names) {
        if is_mcp_package(&json) {
            return Ok(manifest_from_package_json(&json));
        }
    }

    Err("ZIP does not contain a recognized manifest (plugin.yaml, manifest.json, .claude-plugin/plugin.json, claude_desktop_config.json, or MCP package.json)".to_string())
}

// ---------------------------------------------------------------------------
// Claude format converters
// ---------------------------------------------------------------------------

/// Read a JSON file from the ZIP by name (handles root prefix).
fn read_zip_json(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    target: &str,
    file_names: &[String],
) -> Option<Value> {
    // Find the file — might be at root or inside a top-level directory
    let idx = file_names.iter().position(|n| {
        let trimmed = n.trim_end_matches('/');
        trimmed == target || trimmed.ends_with(&format!("/{}", target))
    })?;
    let mut file = archive.by_index(idx).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    serde_json::from_str(&buf).ok()
}

/// MCPB manifest.json → PluginManifest
fn manifest_from_mcpb(json: &Value) -> PluginManifest {
    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let display_name = json.get("display_name").and_then(|v| v.as_str())
        .unwrap_or_else(|| json.get("name").and_then(|v| v.as_str()).unwrap_or("Unknown"))
        .to_string();
    let id = slugify(&name);

    let author = json.get("author")
        .and_then(|a| a.get("name").and_then(|v| v.as_str()))
        .or_else(|| json.get("author").and_then(|v| v.as_str()))
        .unwrap_or("Unknown")
        .to_string();

    // Build MCP server component from server.mcp_config
    let mut mcp_servers = Vec::new();
    if let Some(server) = json.get("server") {
        if let Some(mcp_config) = server.get("mcp_config") {
            // Write the mcp_config as a JSON file reference — we'll generate it on disk after extraction
            mcp_servers.push(ComponentRef {
                path: "_generated_mcp.json".to_string(),
                name: format!("{}-mcp", id),
                description: json.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()),
            });
            // Store the config so install_plugin can write it
            // We embed it in a synthetic way — the actual writing happens in install_plugin
            // via the _mcpb_server_config field we attach
        }
    }

    // Convert user_config to connector config_fields
    let mut connectors = Vec::new();
    if let Some(user_config) = json.get("user_config").and_then(|v| v.as_object()) {
        if !user_config.is_empty() {
            let mut config_fields = Vec::new();
            for (key, schema) in user_config {
                let field_type = match schema.get("type").and_then(|v| v.as_str()).unwrap_or("string") {
                    "string" if schema.get("sensitive") == Some(&Value::Bool(true)) => "password",
                    "boolean" => "bool",
                    "number" => "number",
                    "directory" | "file" => "text",
                    _ => "text",
                };
                config_fields.push(ConfigField {
                    key: key.clone(),
                    label: schema.get("title").and_then(|v| v.as_str()).unwrap_or(key).to_string(),
                    field_type: field_type.to_string(),
                    required: schema.get("required").and_then(|v| v.as_bool()),
                    default: schema.get("default").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Bool(b) => Some(b.to_string()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    }),
                });
            }
            connectors.push(ConnectorRef {
                path: String::new(),
                name: display_name.clone(),
                service: id.clone(),
                description: json.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()),
                config_fields,
            });
        }
    }

    PluginManifest {
        id,
        name: display_name,
        version: json.get("version").and_then(|v| v.as_str()).unwrap_or("0.0.0").to_string(),
        author,
        description: json.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        category: Some("connector".to_string()),
        components: PluginComponents {
            skills: Vec::new(),
            agents: Vec::new(),
            mcp_servers,
            connectors,
        },
        permissions: vec!["network".to_string()],
    }
}

/// Claude Code .claude-plugin/plugin.json → PluginManifest
fn manifest_from_claude_code_plugin(json: &Value, file_names: &[String]) -> PluginManifest {
    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let id = slugify(&name);

    let author = json.get("author")
        .and_then(|a| a.get("name").and_then(|v| v.as_str()))
        .or_else(|| json.get("author").and_then(|v| v.as_str()))
        .unwrap_or("Unknown")
        .to_string();

    // Detect skills: look for */SKILL.md patterns
    let mut skills = Vec::new();
    for fname in file_names {
        if fname.ends_with("SKILL.md") || fname.ends_with("skill.md") {
            let parts: Vec<&str> = fname.rsplitn(2, '/').collect();
            if parts.len() == 2 {
                let skill_dir = parts[1];
                // Get the last directory component as skill name
                let skill_name = skill_dir.rsplit('/').next().unwrap_or(skill_dir);
                if skill_name != "." && !skill_name.is_empty() && skill_name != ".claude-plugin" {
                    // Path relative from zip root (strip common prefix if any)
                    let path = if let Some(pos) = skill_dir.find("skills/") {
                        &skill_dir[pos..]
                    } else {
                        skill_dir
                    };
                    skills.push(ComponentRef {
                        path: path.to_string(),
                        name: skill_name.to_string(),
                        description: None,
                    });
                }
            }
        }
    }

    // Detect MCP config: .mcp.json
    let mut mcp_servers = Vec::new();
    if file_names.iter().any(|n| n.ends_with(".mcp.json") || n.ends_with("mcp.json")) {
        mcp_servers.push(ComponentRef {
            path: ".mcp.json".to_string(),
            name: format!("{}-mcp", id),
            description: None,
        });
    }

    // Convert userConfig to connector fields
    let mut connectors = Vec::new();
    if let Some(user_config) = json.get("userConfig").and_then(|v| v.as_object()) {
        if !user_config.is_empty() {
            let config_fields: Vec<ConfigField> = user_config.iter().map(|(key, schema)| {
                let field_type = match schema.get("type").and_then(|v| v.as_str()).unwrap_or("string") {
                    "string" => "text",
                    "boolean" => "bool",
                    "number" => "number",
                    _ => "text",
                };
                ConfigField {
                    key: key.clone(),
                    label: schema.get("title").and_then(|v| v.as_str()).unwrap_or(key).to_string(),
                    field_type: field_type.to_string(),
                    required: schema.get("required").and_then(|v| v.as_bool()),
                    default: schema.get("default").and_then(|v| v.as_str()).map(|s| s.to_string()),
                }
            }).collect();
            connectors.push(ConnectorRef {
                path: String::new(),
                name: name.clone(),
                service: id.clone(),
                description: json.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()),
                config_fields,
            });
        }
    }

    PluginManifest {
        id,
        name,
        version: json.get("version").and_then(|v| v.as_str()).unwrap_or("0.0.0").to_string(),
        author,
        description: json.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        category: Some("toolkit".to_string()),
        components: PluginComponents { skills, agents: Vec::new(), mcp_servers, connectors },
        permissions: Vec::new(),
    }
}

/// claude_desktop_config.json → PluginManifest (one MCP server per entry)
fn manifest_from_desktop_config(json: &Value) -> PluginManifest {
    let servers = json.get("mcpServers").and_then(|v| v.as_object());
    let mut mcp_servers = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut connectors = Vec::new();

    if let Some(servers_map) = servers {
        for (name, config) in servers_map {
            names.push(name.clone());
            // Write each server config as a separate JSON file
            let filename = format!("_mcp_{}.json", slugify(name));
            mcp_servers.push(ComponentRef {
                path: filename,
                name: name.clone(),
                description: None,
            });

            // If the server has env vars, create connector config fields for them
            if let Some(env_obj) = config.get("env").and_then(|v| v.as_object()) {
                if !env_obj.is_empty() {
                    let config_fields: Vec<ConfigField> = env_obj.keys().map(|key| {
                        let is_sensitive = key.to_lowercase().contains("key")
                            || key.to_lowercase().contains("secret")
                            || key.to_lowercase().contains("token")
                            || key.to_lowercase().contains("password");
                        ConfigField {
                            key: key.clone(),
                            label: key.replace('_', " "),
                            field_type: if is_sensitive { "password".to_string() } else { "text".to_string() },
                            required: Some(true),
                            default: None,
                        }
                    }).collect();
                    connectors.push(ConnectorRef {
                        path: String::new(),
                        name: name.clone(),
                        service: slugify(name),
                        description: Some(format!("Environment config for {} MCP server", name)),
                        config_fields,
                    });
                }
            }
        }
    }

    let combined_name = if names.len() == 1 {
        names[0].clone()
    } else {
        format!("{} (+{})", names.first().cloned().unwrap_or_default(), names.len().saturating_sub(1))
    };
    let id = slugify(&combined_name);

    PluginManifest {
        id,
        name: combined_name,
        version: "1.0.0".to_string(),
        author: "Claude Desktop".to_string(),
        description: format!("Imported MCP servers: {}", names.join(", ")),
        category: Some("connector".to_string()),
        components: PluginComponents {
            skills: Vec::new(),
            agents: Vec::new(),
            mcp_servers,
            connectors,
        },
        permissions: vec!["network".to_string()],
    }
}

/// package.json with MCP dependencies → PluginManifest
fn manifest_from_package_json(json: &Value) -> PluginManifest {
    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or("mcp-server").to_string();
    let id = slugify(&name);

    let author = json.get("author")
        .and_then(|v| v.as_str())
        .or_else(|| json.get("author").and_then(|a| a.get("name").and_then(|v| v.as_str())))
        .unwrap_or("Unknown")
        .to_string();

    // Determine entry point
    let main = json.get("main").and_then(|v| v.as_str()).unwrap_or("index.js");
    let mcp_servers = vec![ComponentRef {
        path: "_generated_mcp.json".to_string(),
        name: format!("{}-mcp", id),
        description: json.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()),
    }];

    PluginManifest {
        id: id.clone(),
        name,
        version: json.get("version").and_then(|v| v.as_str()).unwrap_or("0.0.0").to_string(),
        author,
        description: json.get("description").and_then(|v| v.as_str()).unwrap_or("MCP server from npm package").to_string(),
        category: Some("connector".to_string()),
        components: PluginComponents {
            skills: Vec::new(),
            agents: Vec::new(),
            mcp_servers,
            connectors: Vec::new(),
        },
        permissions: vec!["network".to_string()],
    }
}

/// Check if a package.json looks like an MCP server
fn is_mcp_package(json: &Value) -> bool {
    // Check dependencies for @modelcontextprotocol/sdk
    let has_mcp_dep = |deps: Option<&Value>| -> bool {
        deps.and_then(|d| d.as_object())
            .map(|m| m.keys().any(|k| k.contains("modelcontextprotocol") || k.contains("mcp")))
            .unwrap_or(false)
    };
    has_mcp_dep(json.get("dependencies"))
        || has_mcp_dep(json.get("devDependencies"))
        || json.get("keywords").and_then(|v| v.as_array())
            .map(|a| a.iter().any(|v| v.as_str().map(|s| s == "mcp" || s == "model-context-protocol").unwrap_or(false)))
            .unwrap_or(false)
}

fn detect_zip_root(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>) -> String {
    // If all entries share a common top-level directory prefix, strip it
    let mut common_prefix: Option<String> = None;
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            let name = file.name().to_string();
            if let Some(slash_pos) = name.find('/') {
                let prefix = &name[..=slash_pos];
                match &common_prefix {
                    None => common_prefix = Some(prefix.to_string()),
                    Some(existing) => {
                        if existing != prefix {
                            return String::new(); // no common prefix
                        }
                    }
                }
            } else {
                return String::new(); // file at root level
            }
        }
    }
    common_prefix.unwrap_or_default()
}

fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Convert a JSON value to an McpTool struct.
fn value_to_mcp_tool(name: &str, config: &Value) -> data::McpTool {
    data::McpTool {
        name: name.to_string(),
        url: config.get("url").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        enabled: config.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        tool_type: config.get("type").or(config.get("tool_type"))
            .and_then(|v| v.as_str()).map(|s| s.to_string()),
        command: config.get("command").and_then(|v| v.as_str()).map(|s| s.to_string()),
        args: config.get("args").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()),
        headers: None,
        env: config.get("env").and_then(|v| v.as_object()).map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        }),
    }
}

/// After extraction, generate MCP JSON config files for Claude-format plugins.
/// These are referenced by the synthetic ComponentRef entries created by the converters.
fn generate_claude_mcp_configs(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    dest: &Path,
) {
    // Collect file names
    let mut file_names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            file_names.push(file.name().to_string());
        }
    }

    // 1. MCPB format: manifest.json → _generated_mcp.json
    if let Some(json) = read_zip_json(archive, "manifest.json", &file_names) {
        if json.get("manifest_version").is_some() {
            if let Some(server) = json.get("server") {
                if let Some(mcp_config) = server.get("mcp_config") {
                    let mut config = mcp_config.clone();
                    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or("mcp-server");
                    config["name"] = Value::String(format!("{}-mcp", slugify(name)));
                    config["enabled"] = Value::Bool(true);
                    if config.get("type").is_none() {
                        config["type"] = Value::String("stdio".to_string());
                    }
                    // Replace ${__dirname} with actual plugin dir
                    substitute_dirname(&mut config, dest);
                    let out = dest.join("_generated_mcp.json");
                    let _ = std::fs::write(&out, serde_json::to_string_pretty(&config).unwrap_or_default());
                }
            }
        }
    }

    // 2. claude_desktop_config.json → _mcp_*.json per server
    if let Some(json) = read_zip_json(archive, "claude_desktop_config.json", &file_names) {
        if let Some(servers) = json.get("mcpServers").and_then(|v| v.as_object()) {
            for (name, server_config) in servers {
                let mut config = server_config.clone();
                config["name"] = Value::String(name.clone());
                config["enabled"] = Value::Bool(true);
                if config.get("type").is_none() {
                    config["type"] = Value::String("stdio".to_string());
                }
                substitute_dirname(&mut config, dest);
                let filename = format!("_mcp_{}.json", slugify(name));
                let out = dest.join(&filename);
                let _ = std::fs::write(&out, serde_json::to_string_pretty(&config).unwrap_or_default());
            }
        }
    }

    // 3. package.json MCP server → _generated_mcp.json
    if let Some(json) = read_zip_json(archive, "package.json", &file_names) {
        if is_mcp_package(&json) && !dest.join("_generated_mcp.json").exists() {
            let name = json.get("name").and_then(|v| v.as_str()).unwrap_or("mcp-server");
            let main = json.get("main").and_then(|v| v.as_str()).unwrap_or("index.js");
            let config = serde_json::json!({
                "name": format!("{}-mcp", slugify(name)),
                "enabled": true,
                "type": "stdio",
                "command": "node",
                "args": [main]
            });
            let out = dest.join("_generated_mcp.json");
            let _ = std::fs::write(&out, serde_json::to_string_pretty(&config).unwrap_or_default());
        }
    }

    // 4. Claude Code .mcp.json — copy as _generated_mcp.json if it contains server defs
    if let Some(json) = read_zip_json(archive, ".mcp.json", &file_names) {
        let out = dest.join(".mcp.json");
        if out.exists() {
            // .mcp.json might contain mcpServers map like claude_desktop_config
            if let Some(servers) = json.get("mcpServers").and_then(|v| v.as_object()) {
                for (name, server_config) in servers {
                    let mut config = server_config.clone();
                    config["name"] = Value::String(name.clone());
                    config["enabled"] = Value::Bool(true);
                    if config.get("type").is_none() {
                        config["type"] = Value::String("stdio".to_string());
                    }
                    substitute_dirname(&mut config, dest);
                    let filename = format!("_mcp_{}.json", slugify(name));
                    let mcp_out = dest.join(&filename);
                    // Also update the ComponentRef path to match
                    let _ = std::fs::write(&mcp_out, serde_json::to_string_pretty(&config).unwrap_or_default());
                }
            }
        }
    }
}

/// Replace ${__dirname} and ${HOME} template variables in MCP config values.
fn substitute_dirname(config: &mut Value, plugin_dir: &Path) {
    let dir_str = plugin_dir.to_string_lossy().to_string();
    let home = dirs::home_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    substitute_value(config, &dir_str, &home);
}

fn substitute_value(val: &mut Value, dirname: &str, home: &str) {
    match val {
        Value::String(s) => {
            *s = s.replace("${__dirname}", dirname)
                .replace("${HOME}", home);
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                substitute_value(item, dirname, home);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                substitute_value(v, dirname, home);
            }
        }
        _ => {}
    }
}

fn resolve_mcp_paths(config: &mut Value, plugin_dir: &Path) {
    // Resolve command path
    if let Some(cmd) = config.get("command").and_then(|v| v.as_str()) {
        let cmd_path = Path::new(cmd);
        if cmd_path.is_relative()
            && !cmd.starts_with("python") && !cmd.starts_with("node")
            && !cmd.starts_with("npx") && !cmd.starts_with("uvx")
            && !cmd.starts_with("uv") && !cmd.starts_with("docker")
        {
            config["command"] = Value::String(plugin_dir.join(cmd).to_string_lossy().to_string());
        }
    }
    // Resolve relative args that look like file paths
    if let Some(args) = config.get("args").and_then(|v| v.as_array()).cloned() {
        let resolved: Vec<Value> = args.into_iter().map(|v| {
            if let Some(s) = v.as_str() {
                let p = Path::new(s);
                if p.is_relative() && (s.ends_with(".py") || s.ends_with(".js") || s.ends_with(".sh") || s.contains('/')) {
                    return Value::String(plugin_dir.join(s).to_string_lossy().to_string());
                }
            }
            v
        }).collect();
        config["args"] = Value::Array(resolved);
    }
}

async fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst).await?;
    let mut entries = fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let ft = entry.file_type().await?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ft.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else {
            fs::copy(&src_path, &dst_path).await?;
        }
    }
    Ok(())
}
