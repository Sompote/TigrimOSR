// ---------------------------------------------------------------------------
// User-defined tools (declarative, YAML-driven)
//
// Each file in data_dir()/tools/*.yaml defines a brand-new agent tool without
// any Rust code or a rebuild. Two kinds are supported:
//   - http:  call a REST API (GET/POST), template the request from the model's
//            args, return the (optionally field-selected, truncated) response.
//   - shell: run a shell command built from the model's args. Execution is
//            delegated to toolbox::exec_run_shell, so it inherits ALL of the
//            existing shell sandboxing (dangerous-command block, sandbox cwd,
//            process-group kill, VM routing) — no new security surface here.
//
// Definitions are rendered into the model's tool list next to MCP tools and
// dispatched from toolbox::execute_tool_with_context, so per-tool profile
// config (tools.config.<name>), approval, timeout, and result truncation all
// apply uniformly. See src/server/routes/custom_tools.rs for the REST API.
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// data_dir()/tools — the folder scanned for *.yaml tool definitions.
pub fn tools_dir() -> PathBuf {
    crate::server::data::data_dir().join("tools")
}

// ---------------------------------------------------------------------------
// Spec structs (serde over YAML)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// "http" | "shell"
    #[serde(default)]
    pub kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub parameters: Vec<CustomParam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<HttpSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<ShellSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponseSpec>,
    /// Force approval on/off regardless of kind default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_approval: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomParam {
    pub name: String,
    /// "string" | "integer" | "number" | "boolean"
    #[serde(default = "default_string_type", rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpSpec {
    #[serde(default = "default_get")]
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default = "default_timeout", skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellSpec {
    pub command: String,
    #[serde(default = "default_timeout", skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSpec {
    /// "auto" | "json" | "text"
    #[serde(default = "default_auto")]
    pub format: String,
    /// serde_json Pointer path into a JSON body, e.g. "/results/0".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u64>,
}

fn default_true() -> bool {
    true
}
fn default_string_type() -> String {
    "string".to_string()
}
fn default_get() -> String {
    "GET".to_string()
}
fn default_auto() -> String {
    "auto".to_string()
}
fn default_timeout() -> Option<u64> {
    Some(20)
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Read every valid *.yaml tool from data/tools/. Invalid files are skipped
/// with a warning (tolerant, like agent_loop::load_profile). Read fresh per
/// call: cheap (few small files) and gives edits effect without a restart.
pub fn load_all() -> Vec<CustomTool> {
    let dir = tools_dir();
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return out, // folder not created yet
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // Only *.yaml / *.yml are live; e.g. example.yaml.disabled is ignored.
        if !(name.ends_with(".yaml") || name.ends_with(".yml")) {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        match serde_yaml::from_str::<CustomTool>(&content) {
            Ok(t) => out.push(t),
            Err(e) => tracing::warn!("[custom_tools] Failed to parse {:?}: {}", path, e),
        }
    }
    out
}

/// Look up one enabled tool by name.
fn find(name: &str) -> Option<CustomTool> {
    load_all().into_iter().find(|t| t.name == name && t.enabled)
}

/// True if `name` is a live, enabled user-defined tool.
pub fn is_custom_tool(name: &str) -> bool {
    find(name).is_some()
}

// ---------------------------------------------------------------------------
// Tool schema rendering (OpenAI function format)
// ---------------------------------------------------------------------------

/// Render enabled tools into the same JSON tool-spec shape produced by
/// toolbox::tool_definitions().
pub fn definitions() -> Vec<Value> {
    load_all()
        .into_iter()
        .filter(|t| t.enabled)
        .map(|t| tool_to_definition(&t))
        .collect()
}

fn tool_to_definition(t: &CustomTool) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    for p in &t.parameters {
        let json_type = match p.type_.as_str() {
            "integer" => "integer",
            "number" => "number",
            "boolean" => "boolean",
            _ => "string",
        };
        properties.insert(
            p.name.clone(),
            json!({ "type": json_type, "description": p.description }),
        );
        if p.required {
            required.push(p.name.clone());
        }
    }
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Approval classification (consulted by toolbox::tool_requires_approval)
// ---------------------------------------------------------------------------

pub enum CustomApproval {
    /// Explicit require_approval on the tool.
    Force(bool),
    /// shell-kind default → follow the global shell-approval toggle.
    ShellDefault,
    /// http-kind default → no approval (like web_search / fetch_url).
    HttpDefault,
}

pub fn approval_kind(name: &str) -> Option<CustomApproval> {
    let t = find(name)?;
    if let Some(v) = t.require_approval {
        return Some(CustomApproval::Force(v));
    }
    match t.kind.as_str() {
        "shell" => Some(CustomApproval::ShellDefault),
        _ => Some(CustomApproval::HttpDefault),
    }
}

// ---------------------------------------------------------------------------
// Templating: substitute {{param}} from args (falling back to defaults)
// ---------------------------------------------------------------------------

/// Resolve each declared parameter to a string form for substitution, applying
/// declared defaults for omitted args. Returns an error naming the first
/// missing required parameter.
fn resolve_params(tool: &CustomTool, args: &Value) -> Result<HashMap<String, Value>, String> {
    let mut resolved = HashMap::new();
    for p in &tool.parameters {
        let v = args.get(&p.name).cloned().or_else(|| p.default.clone());
        match v {
            Some(val) => {
                resolved.insert(p.name.clone(), val);
            }
            None if p.required => {
                return Err(format!("missing required parameter '{}'", p.name));
            }
            None => {}
        }
    }
    Ok(resolved)
}

/// Value → plain string (unquoted) for URL / header / shell contexts.
fn value_to_plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Substitute {{name}} placeholders. `url_encode` percent-encodes string
/// values (for URLs); otherwise substitutes the plain string form.
fn substitute(template: &str, params: &HashMap<String, Value>, url_encode: bool) -> String {
    let mut out = template.to_string();
    for (k, v) in params {
        let needle = format!("{{{{{}}}}}", k);
        let plain = value_to_plain(v);
        let replacement = if url_encode {
            urlencoding::encode(&plain).into_owned()
        } else {
            plain
        };
        out = out.replace(&needle, &replacement);
    }
    out
}

/// Substitute into a JSON body string: string values are JSON-escaped so a
/// quote in a param can't break the surrounding JSON; non-strings inline raw.
fn substitute_body(template: &str, params: &HashMap<String, Value>) -> String {
    let mut out = template.to_string();
    for (k, v) in params {
        let needle = format!("{{{{{}}}}}", k);
        let replacement = match v {
            Value::String(s) => {
                let escaped = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
                // strip the outer quotes serde adds, keeping inner escaping
                escaped[1..escaped.len().saturating_sub(1)].to_string()
            }
            other => other.to_string(),
        };
        out = out.replace(&needle, &replacement);
    }
    out
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Dispatch entry point (called from toolbox::execute_tool_with_context).
pub async fn execute(name: &str, args: &Value, sandbox_dir: &str) -> Value {
    let tool = match find(name) {
        Some(t) => t,
        None => return json!({ "ok": false, "error": format!("Unknown custom tool: {name}") }),
    };
    let params = match resolve_params(&tool, args) {
        Ok(p) => p,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    match tool.kind.as_str() {
        "http" => execute_http(&tool, &params).await,
        "shell" => execute_shell(&tool, &params, sandbox_dir).await,
        other => json!({ "ok": false, "error": format!("Unknown tool kind '{other}' (use http|shell)") }),
    }
}

async fn execute_http(tool: &CustomTool, params: &HashMap<String, Value>) -> Value {
    let spec = match &tool.request {
        Some(s) => s,
        None => return json!({ "ok": false, "error": "http tool missing `request` block" }),
    };

    let url = substitute(&spec.url, params, true);
    // Reuse the shared SSRF guard: block loopback/link-local/metadata targets.
    if let Err(reason) = crate::server::services::toolbox::validate_url_no_ssrf(&url) {
        return json!({ "ok": false, "error": format!("Security: {reason}") });
    }

    let method = spec.method.to_uppercase();
    let timeout_s = spec.timeout_secs.unwrap_or(20).clamp(1, 120);
    let client = reqwest::Client::new();
    let mut req = match method.as_str() {
        "POST" => client.post(&url),
        _ => client.get(&url),
    };
    for (k, v) in &spec.headers {
        req = req.header(k.as_str(), substitute(v, params, false));
    }
    if let Some(body) = &spec.body {
        req = req.body(substitute_body(body, params));
    }

    let resp = match tokio::time::timeout(Duration::from_secs(timeout_s), req.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return json!({ "ok": false, "error": format!("Request failed: {e}") }),
        Err(_) => return json!({ "ok": false, "error": format!("Request timed out ({timeout_s}s)") }),
    };
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return json!({ "ok": false, "error": format!("Failed to read body: {e}") }),
    };

    let rspec = tool.response.clone().unwrap_or(ResponseSpec {
        format: "auto".into(),
        select: None,
        max_len: None,
    });
    let want_json = match rspec.format.as_str() {
        "json" => true,
        "text" => false,
        _ => content_type.contains("json"),
    };
    let max_len = rspec.max_len.unwrap_or(4000) as usize;

    if want_json {
        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
            let selected = match &rspec.select {
                Some(ptr) => parsed.pointer(ptr).cloned().unwrap_or(Value::Null),
                None => parsed,
            };
            // Truncate the stringified result to keep tool output bounded.
            let pretty = serde_json::to_string(&selected).unwrap_or_default();
            let trimmed = crate::util::truncate_utf8_ellipsis(&pretty, max_len);
            let result: Value = serde_json::from_str(&trimmed).unwrap_or(Value::String(trimmed));
            return json!({ "ok": status < 400, "status": status, "result": result });
        }
    }
    json!({
        "ok": status < 400,
        "status": status,
        "result": crate::util::truncate_utf8_ellipsis(&text, max_len),
    })
}

async fn execute_shell(
    tool: &CustomTool,
    params: &HashMap<String, Value>,
    sandbox_dir: &str,
) -> Value {
    let spec = match &tool.run {
        Some(s) => s,
        None => return json!({ "ok": false, "error": "shell tool missing `run` block" }),
    };
    let command = substitute(&spec.command, params, false);
    // Delegate to the built-in shell tool so all sandboxing/security applies.
    crate::server::services::toolbox::exec_run_shell(&json!({ "command": command }), sandbox_dir)
        .await
}

// ---------------------------------------------------------------------------
// Validation (used by the save endpoint)
// ---------------------------------------------------------------------------

/// Validate a parsed tool. Hard problems are Err; soft issues are warnings.
pub fn validate(tool: &CustomTool) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();

    // Name: charset + no collision with built-ins or the mcp_ namespace.
    if tool.name.is_empty() {
        return Err("tool `name` is required".into());
    }
    if !tool.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(format!(
            "tool name '{}' must be lowercase letters, digits, and underscores only",
            tool.name
        ));
    }
    if tool.name.starts_with("mcp_") {
        return Err("tool name must not start with 'mcp_' (reserved for MCP tools)".into());
    }
    let builtins = crate::server::services::toolbox::tool_definitions();
    let collides = builtins.iter().any(|t| {
        t.pointer("/function/name").and_then(|n| n.as_str()) == Some(tool.name.as_str())
    });
    if collides {
        return Err(format!("tool name '{}' collides with a built-in tool", tool.name));
    }

    // Kind ⇔ spec block consistency.
    match tool.kind.as_str() {
        "http" => {
            let req = tool
                .request
                .as_ref()
                .ok_or("http tool requires a `request` block")?;
            if !matches!(req.method.to_uppercase().as_str(), "GET" | "POST") {
                return Err(format!("request.method must be GET or POST, got '{}'", req.method));
            }
            if req.url.is_empty() {
                return Err("request.url is required".into());
            }
            check_placeholders(&req.url, tool, &mut warnings);
            if let Some(body) = &req.body {
                check_placeholders(body, tool, &mut warnings);
            }
            for v in req.headers.values() {
                check_placeholders(v, tool, &mut warnings);
            }
        }
        "shell" => {
            let run = tool.run.as_ref().ok_or("shell tool requires a `run` block")?;
            if run.command.is_empty() {
                return Err("run.command is required".into());
            }
            check_placeholders(&run.command, tool, &mut warnings);
        }
        other => return Err(format!("kind must be 'http' or 'shell', got '{other}'")),
    }

    // Parameter types.
    for p in &tool.parameters {
        if !matches!(p.type_.as_str(), "string" | "integer" | "number" | "boolean") {
            warnings.push(format!(
                "parameter '{}' has unknown type '{}' (treated as string)",
                p.name, p.type_
            ));
        }
    }
    Ok(warnings)
}

/// Warn about {{placeholders}} that aren't declared parameters.
fn check_placeholders(template: &str, tool: &CustomTool, warnings: &mut Vec<String>) {
    let re = regex::Regex::new(r"\{\{\s*([a-zA-Z0-9_]+)\s*\}\}").unwrap();
    for caps in re.captures_iter(template) {
        let name = &caps[1];
        if !tool.parameters.iter().any(|p| p.name == name) {
            warnings.push(format!("template uses '{{{{{}}}}}'  but no such parameter is declared", name));
        }
    }
}

// ---------------------------------------------------------------------------
// Example seeding
// ---------------------------------------------------------------------------

const EXAMPLE_TOOL: &str = r#"# Example custom tool (HTTP). Rename to `arxiv.yaml` to enable it,
# or copy this as a starting point. Files ending in `.disabled` are ignored.
#
# Drop any *.yaml file in this folder to add a tool — no rebuild needed.
name: arxiv_search
description: Search arXiv for academic papers by keyword. Returns Atom XML.
kind: http
enabled: true
parameters:
  - name: query
    type: string
    description: Search keywords
    required: true
  - name: limit
    type: integer
    description: Max number of results (default 10)
    default: 10
request:
  method: GET
  url: "http://export.arxiv.org/api/query?search_query=all:{{query}}&max_results={{limit}}"
  headers:
    User-Agent: "TigrimOS/1.0"
  timeout_secs: 20
response:
  format: text
  max_len: 4000

# --- Shell tool example (delegates to the sandboxed run_shell) ---
# name: pdf_pages
# description: Report the page count of a PDF in the sandbox.
# kind: shell
# enabled: true
# parameters:
#   - name: file
#     type: string
#     required: true
# run:
#   command: "pdfinfo {{file}} | grep Pages"
#   timeout_secs: 30
# require_approval: true
"#;

/// Seed a commented example (as *.disabled so it isn't loaded) on first run.
pub fn ensure_examples() {
    let dir = tools_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("[custom_tools] Failed to create {:?}: {}", dir, e);
        return;
    }
    let example = dir.join("example.yaml.disabled");
    if !example.exists() {
        if let Err(e) = std::fs::write(&example, EXAMPLE_TOOL) {
            tracing::warn!("[custom_tools] Failed to seed example: {}", e);
        } else {
            tracing::info!("[custom_tools] Seeded example tool at {:?}", example);
        }
    }
}
