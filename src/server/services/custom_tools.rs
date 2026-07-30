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
    /// "http" | "shell" | "builtin".
    /// "builtin" = this file customizes an EXISTING built-in tool (description,
    /// parameters shown to the model, config) without replacing its code.
    #[serde(default)]
    pub kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Required (`override: true`) for an http/shell tool whose name matches a
    /// built-in: the YAML implementation then REPLACES the built-in at dispatch.
    #[serde(default, rename = "override", skip_serializing_if = "std::ops::Not::not")]
    pub override_builtin: bool,
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
    /// Global runtime config (same fields as a profile's tools.config entry):
    /// approval, param defaults/pins, timeout, result cap. Profile per-tool
    /// config overrides these field-by-field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::server::services::agent_loop::ToolConfig>,
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

/// Look up one enabled EXECUTABLE (http/shell) tool by name — includes
/// override files that shadow a built-in implementation.
fn find(name: &str) -> Option<CustomTool> {
    load_all().into_iter().find(|t| {
        t.name == name && t.enabled && matches!(t.kind.as_str(), "http" | "shell")
    })
}

/// Any tool file by name, including `kind: builtin` customization records.
pub fn find_any(name: &str) -> Option<CustomTool> {
    load_all().into_iter().find(|t| t.name == name)
}

/// True if `name` dispatches through the YAML executor (http/shell, including
/// a built-in shadowed via `override: true`).
pub fn is_custom_tool(name: &str) -> bool {
    find(name).is_some()
}

/// Global runtime config for a tool from its YAML file (any kind). Applied
/// beneath the per-profile tools.config (profile fields win).
pub fn global_config(name: &str) -> Option<crate::server::services::agent_loop::ToolConfig> {
    find_any(name).and_then(|t| t.config)
}

// ---------------------------------------------------------------------------
// Tool schema rendering (OpenAI function format)
// ---------------------------------------------------------------------------

/// Render enabled EXECUTABLE tools into the same JSON tool-spec shape produced
/// by toolbox::tool_definitions(). `kind: builtin` records are not new tools —
/// they customize built-in specs via apply_builtin_overrides instead.
pub fn definitions() -> Vec<Value> {
    load_all()
        .into_iter()
        .filter(|t| t.enabled && matches!(t.kind.as_str(), "http" | "shell"))
        .map(|t| tool_to_definition(&t))
        .collect()
}

/// Apply YAML tool files onto the built-in spec list, in place:
/// - an http/shell file with `override: true` REMOVES the built-in spec
///   (its own spec arrives via definitions(), replacing the implementation);
/// - a `kind: builtin` file rewrites the built-in's description and/or the
///   parameter schema the model sees, and `enabled: false` hides it.
pub fn apply_builtin_overrides(tools: &mut Vec<Value>) {
    let files = load_all();
    if files.is_empty() {
        return;
    }
    for f in &files {
        let shadows = matches!(f.kind.as_str(), "http" | "shell") && f.override_builtin && f.enabled;
        let hides = f.kind == "builtin" && !f.enabled;
        if shadows || hides {
            tools.retain(|t| t["function"]["name"].as_str() != Some(f.name.as_str()));
        }
    }
    for t in tools.iter_mut() {
        let Some(name) = t["function"]["name"].as_str().map(|s| s.to_string()) else { continue };
        let Some(f) = files.iter().find(|f| f.name == name && f.kind == "builtin" && f.enabled) else { continue };
        if !f.description.is_empty() {
            t["function"]["description"] = json!(f.description);
        }
        if !f.parameters.is_empty() {
            t["function"]["parameters"] = params_to_schema(&f.parameters);
        }
    }
}

/// Convert a JSON parameter schema into the YAML parameter list (for showing
/// a built-in's arguments in its editable tool file).
pub fn schema_to_params(schema: &Value) -> Vec<CustomParam> {
    let required: Vec<&str> = schema
        .pointer("/required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    schema
        .pointer("/properties")
        .and_then(|p| p.as_object())
        .map(|props| {
            props
                .iter()
                .map(|(name, p)| CustomParam {
                    name: name.clone(),
                    type_: p.get("type").and_then(|t| t.as_str()).unwrap_or("string").to_string(),
                    description: p.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
                    required: required.contains(&name.as_str()),
                    default: None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Convert a YAML parameter list back into the JSON schema the model sees.
pub fn params_to_schema(params: &[CustomParam]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    for p in params {
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
    json!({ "type": "object", "properties": properties, "required": required })
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
// Built-in tool editor documents: a built-in's FULL definition as an editable
// YAML file (data/tools/<name>.yaml) — same system as custom tools.
// ---------------------------------------------------------------------------

/// The editor document for a built-in tool: the existing YAML file verbatim,
/// or a generated full definition (description + parameter schema + config)
/// with instructions for replacing the implementation.
pub fn builtin_editor_yaml(
    name: &str,
    builtin_desc: &str,
    schema: Option<&Value>,
    default_approval: bool,
) -> String {
    if let Ok(existing) = std::fs::read_to_string(tools_dir().join(format!("{name}.yaml"))) {
        if !existing.trim().is_empty() {
            return existing;
        }
    }
    let doc = CustomTool {
        name: name.to_string(),
        description: builtin_desc.to_string(),
        kind: "builtin".to_string(),
        enabled: true,
        override_builtin: false,
        parameters: schema.map(schema_to_params).unwrap_or_default(),
        request: None,
        run: None,
        response: None,
        require_approval: None,
        config: Some(crate::server::services::agent_loop::ToolConfig {
            require_approval: Some(default_approval),
            ..Default::default()
        }),
    };
    let body = serde_yaml::to_string(&doc).unwrap_or_default();
    // Central implementation registry: what the native code does + a
    // ready-to-edit replacement (real command/API code) for this tool.
    let impl_doc = crate::server::services::toolbox::builtin_impl_doc(name);
    format!(
        "# Tool '{name}' — full definition, editable as YAML (saved to data/tools/{name}.yaml).\n\
         # - description / parameters: change what the model sees for this tool\n\
         # - config: approval, default & pinned args, timeout_secs, max_result_len\n\
         # - enabled: false hides the tool entirely\n\
         # - implementation: see the ready-to-edit replacement at the bottom\n\
         # Save stores the file only if something differs from the built-in\n\
         # defaults; matching the defaults removes it.\n{body}{impl_doc}"
    )
}

/// Save a built-in tool document: parse + validate, then diff against the
/// built-in baseline — an unchanged document deletes the file (no override).
/// Returns (saved_as_file, warnings).
pub fn save_builtin_doc(
    name: &str,
    content: &str,
    builtin_desc: &str,
    schema: Option<&Value>,
    default_approval: bool,
) -> Result<(bool, Vec<String>), String> {
    let doc: CustomTool =
        serde_yaml::from_str(content).map_err(|e| format!("Invalid tool YAML: {e}"))?;
    if doc.name != name {
        return Err(format!(
            "The document's name '{}' must stay '{}' (create a differently-named custom tool in the Custom Tools tab instead)",
            doc.name, name
        ));
    }
    let warnings = validate(&doc)?;

    // Baseline comparison: is this document just the built-in defaults?
    // Order-insensitive on parameters so a user reordering entries (or a
    // hand-written doc) still resets cleanly.
    let sorted = |mut v: Vec<CustomParam>| {
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    };
    let baseline_params = sorted(schema.map(schema_to_params).unwrap_or_default());
    let doc_params = sorted(doc.parameters.clone());
    let params_unchanged =
        serde_json::to_value(&doc_params).ok() == serde_json::to_value(&baseline_params).ok();
    let config_is_default = match &doc.config {
        None => true,
        Some(c) => {
            c.enabled.is_none()
                && (c.require_approval.is_none() || c.require_approval == Some(default_approval))
                && c.description.is_none()
                && c.params.is_none()
                && c.pinned_params.is_none()
                && c.timeout_secs.is_none()
                && c.max_result_len.is_none()
        }
    };
    let is_baseline = doc.kind == "builtin"
        && doc.enabled
        && !doc.override_builtin
        && doc.description.trim() == builtin_desc.trim()
        && params_unchanged
        && config_is_default
        && doc.request.is_none()
        && doc.run.is_none()
        && doc.require_approval.is_none();

    let path = tools_dir().join(format!("{name}.yaml"));
    if is_baseline {
        let _ = std::fs::remove_file(&path);
        return Ok((false, warnings));
    }
    let _ = std::fs::create_dir_all(tools_dir());
    std::fs::write(&path, content).map_err(|e| format!("Failed to write: {e}"))?;
    Ok((true, warnings))
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
    // A built-in name is allowed for kind:builtin (customization record) and
    // for http/shell with an explicit `override: true` (implementation shadow).
    if collides && tool.kind != "builtin" && !tool.override_builtin {
        return Err(format!(
            "tool name '{}' matches a built-in tool — add `override: true` to replace its implementation, or use `kind: builtin` to customize it",
            tool.name
        ));
    }
    if tool.kind == "builtin" && !collides {
        return Err(format!(
            "kind: builtin requires the name of an existing built-in tool ('{}' is not one)",
            tool.name
        ));
    }
    if tool.override_builtin && !collides {
        warnings.push(format!(
            "`override: true` set but '{}' is not a built-in tool name (harmless)",
            tool.name
        ));
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
        "builtin" => {
            // Customization record: description/parameters/config only.
            if tool.run.is_some() || tool.request.is_some() {
                warnings.push(
                    "run/request blocks are ignored for kind: builtin — set kind: shell or http (with override: true) to replace the implementation"
                        .into(),
                );
            }
        }
        other => return Err(format!("kind must be 'http', 'shell' or 'builtin', got '{other}'")),
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
    User-Agent: "AndrewOS/1.0"
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
