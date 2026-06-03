# TigrimOS Plugin Developer Guide

This document covers everything you need to build, package, and distribute plugins for TigrimOS. Plugins bundle skills, MCP servers, agent configs, and service connectors into a single `.zip` file.

---

## Table of Contents

- [Plugin ZIP Structure](#plugin-zip-structure)
- [Manifest Reference (`plugin.yaml`)](#manifest-reference)
- [Components](#components)
  - [Skills](#skills)
  - [Agents](#agents)
  - [MCP Servers](#mcp-servers)
  - [Connectors](#connectors)
- [Claude Format Compatibility](#claude-format-compatibility)
  - [MCPB (Claude Desktop Extension)](#mcpb-claude-desktop-extension)
  - [Claude Code Plugin](#claude-code-plugin)
  - [Claude Desktop Config](#claude-desktop-config)
  - [npm MCP Package](#npm-mcp-package)
- [Plugin Lifecycle](#plugin-lifecycle)
- [REST API Reference](#rest-api-reference)
- [Security](#security)
- [Examples](#examples)

---

## Plugin ZIP Structure

```
my-plugin.zip
├── plugin.yaml              # REQUIRED — plugin manifest
├── skills/
│   └── email-assistant/
│       └── SKILL.md         # skill definition
├── agents/
│   └── email_swarm.yaml     # agent swarm config
├── mcp/
│   └── gmail-server.json    # MCP server config
├── connectors/
│   └── gmail.py             # connector script
│   └── calendar.py
├── README.md                # optional — shown in plugin detail view
└── icon.png                 # optional — shown in plugin card (PNG)
```

The zip may contain a single top-level directory (e.g. `my-plugin/plugin.yaml`) — the installer strips common prefixes automatically.

---

## Manifest Reference

The `plugin.yaml` (or `plugin.yml`) file is the only required file. It declares the plugin identity, its components, and permissions.

```yaml
# ── Identity ──────────────────────────────────────────────
id: "gmail-connector"              # REQUIRED — unique slug [a-z0-9][a-z0-9-]*
name: "Gmail Connector"            # REQUIRED — display name
version: "1.0.0"                   # REQUIRED — semver
author: "TigrimOS Community"       # REQUIRED
description: "Connect to Gmail, read/send emails, manage calendar"

category: "connector"              # optional — connector | toolkit | swarm | utility
                                   # Shown as a colored badge in the UI

# ── Components ────────────────────────────────────────────
components:
  skills:
    - path: "skills/email-assistant"
      name: "Email Assistant"
      description: "Draft and send emails via Gmail"

  agents:
    - path: "agents/email_swarm.yaml"
      name: "Email Processing Swarm"

  mcp_servers:
    - path: "mcp/gmail-server.json"
      name: "Gmail MCP"
      description: "Gmail API via MCP protocol"

  connectors:
    - path: "connectors/gmail.py"
      name: "Gmail"
      service: "gmail"
      description: "Read/send Gmail emails"
      config_fields:
        - key: "email"
          label: "Gmail Address"
          type: "text"
          required: true
        - key: "app_password"
          label: "App Password"
          type: "password"
          required: true
        - key: "imap_server"
          label: "IMAP Server"
          type: "text"
          default: "imap.gmail.com"

# ── Permissions ───────────────────────────────────────────
permissions:
  - "network"                      # declares network access
  - "python"                       # declares Python execution
```

### Field Reference

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Unique slug. Must match `^[a-z0-9][a-z0-9-]*$`. Used as directory name. |
| `name` | string | Yes | Human-readable display name. |
| `version` | string | Yes | Semantic version (e.g. `1.0.0`). |
| `author` | string | Yes | Author name. |
| `description` | string | Yes | One-line description. |
| `category` | string | No | `connector`, `toolkit`, `swarm`, or `utility`. Shown as a UI badge. |
| `components` | object | No | Skills, agents, MCP servers, and connectors (see below). |
| `permissions` | array | No | Declared permission strings (informational). |

---

## Components

### Skills

Each skill entry points to a directory containing a `SKILL.md` file.

```yaml
components:
  skills:
    - path: "skills/email-assistant"   # directory relative to zip root
      name: "Email Assistant"          # registered name in skills.json
      description: "Draft and send emails via Gmail"
```

**On install:**
- The skill directory is copied to `data/skills/plugin_{id}_{slug}/`
- A `Skill` entry is added to `skills.json` with `source: "plugin:{id}"`
- The skill appears in the Skills view and is available to the AI agent

**SKILL.md format:**
```markdown
---
name: Email Assistant
description: Draft and send emails via Gmail
---

You are an email assistant. When the user asks to send an email...
```

Skills can include subdirectories with scripts, references, and supporting files — the entire directory tree is copied.

### Agents

Agent entries point to YAML files in the standard TigrimOS agent config format.

```yaml
components:
  agents:
    - path: "agents/email_swarm.yaml"
      name: "Email Processing Swarm"
```

**On install:**
- The YAML file is copied to `data/agents/plugin_{id}_{filename}`
- It appears in the Agent Swarm selector dropdown

### MCP Servers

MCP server entries point to JSON config files. The format matches the `McpTool` structure used in TigrimOS settings.

```yaml
components:
  mcp_servers:
    - path: "mcp/gmail-server.json"
      name: "Gmail MCP"
      description: "Gmail API via MCP protocol"
```

**MCP server config format (`mcp/gmail-server.json`):**
```json
{
  "name": "gmail-mcp",
  "enabled": true,
  "type": "stdio",
  "command": "python3",
  "args": ["connectors/gmail.py", "--mcp"],
  "env": {}
}
```

**Path resolution:** Relative paths in `command` and `args` that look like file paths (ending in `.py`, `.js`, `.sh`, or containing `/`) are resolved to absolute paths under `data/plugins/{id}/`. Well-known commands (`python`, `python3`, `node`, `npx`, `uvx`, `uv`, `docker`) are left as-is.

**On install:**
- The config is parsed and added to `settings.mcp_tools[]`
- The MCP server is immediately available for tool discovery/calling

**Also supports `mcpServers` map format** (Claude Desktop style):
```json
{
  "mcpServers": {
    "server-a": {
      "command": "python3",
      "args": ["server_a.py"]
    },
    "server-b": {
      "command": "node",
      "args": ["server_b.js"]
    }
  }
}
```
Each entry in the map becomes a separate registered MCP tool.

### Connectors

Connectors are service-specific scripts with user-configurable credentials. The UI renders input fields for each `config_field` so the user can provide API keys, passwords, and settings.

```yaml
components:
  connectors:
    - path: "connectors/gmail.py"
      name: "Gmail"
      service: "gmail"               # unique service identifier
      description: "Read/send Gmail emails"
      config_fields:
        - key: "email"
          label: "Gmail Address"
          type: "text"
          required: true
        - key: "app_password"
          label: "App Password"
          type: "password"            # masked in UI
          required: true
        - key: "imap_server"
          label: "IMAP Server"
          type: "text"
          default: "imap.gmail.com"   # pre-filled
```

**Config field types:**

| Type | UI Widget | Description |
|------|-----------|-------------|
| `text` | Text input | Plain text |
| `password` | Password input | Masked, for secrets/API keys |
| `number` | Text input | Numeric value |
| `bool` | Text input | `true` / `false` |
| `file` | Text input | File path |

**On install:**
- A `connector_config.json` file is created in `data/plugins/{id}/` with defaults
- User-provided values are saved separately (never in the zip)

**Reading config at runtime (Python example):**
```python
import json, os

plugin_dir = os.path.dirname(os.path.abspath(__file__))
config_path = os.path.join(plugin_dir, "..", "connector_config.json")

with open(config_path) as f:
    all_configs = json.load(f)

gmail_config = all_configs.get("gmail", {})
email = gmail_config.get("email", "")
password = gmail_config.get("app_password", "")
```

---

## Claude Format Compatibility

TigrimOS automatically detects and converts four Claude plugin formats. You can install Claude plugins directly — no repackaging needed.

### MCPB (Claude Desktop Extension)

**Detected by:** `manifest.json` with `manifest_version` and `server` fields.

```json
{
  "manifest_version": "0.3",
  "name": "hello-world-node",
  "display_name": "Hello World MCP Server",
  "version": "0.1.0",
  "description": "A simple MCP server",
  "author": { "name": "Acme Inc" },
  "server": {
    "type": "node",
    "entry_point": "server/index.js",
    "mcp_config": {
      "command": "node",
      "args": ["${__dirname}/server/index.js"],
      "env": {
        "API_KEY": "${user_config.api_key}"
      }
    }
  },
  "user_config": {
    "api_key": {
      "type": "string",
      "title": "API Key",
      "sensitive": true,
      "required": false
    },
    "max_results": {
      "type": "number",
      "title": "Maximum Results",
      "default": 10
    }
  }
}
```

**Conversion:**
- `server.mcp_config` → registered as MCP tool (stdio)
- `user_config` → connector config fields (`sensitive: true` → password type)
- `${__dirname}` → resolved to plugin install directory
- `${HOME}` → resolved to user home directory
- Category set to `connector`

### Claude Code Plugin

**Detected by:** `.claude-plugin/plugin.json` in the zip.

```
my-plugin.zip
├── .claude-plugin/
│   └── plugin.json
├── skills/
│   └── my-skill/
│       └── SKILL.md
├── .mcp.json
└── README.md
```

**`.claude-plugin/plugin.json`:**
```json
{
  "name": "my-plugin",
  "version": "1.0.0",
  "description": "A Claude Code plugin",
  "author": { "name": "Developer" },
  "userConfig": {
    "apiKey": {
      "type": "string",
      "title": "API Key",
      "required": false
    }
  }
}
```

**Conversion:**
- `skills/*/SKILL.md` patterns → auto-detected and registered as skills
- `.mcp.json` → parsed as MCP server config (supports `mcpServers` map format)
- `userConfig` → connector config fields
- Category set to `toolkit`

### Claude Desktop Config

**Detected by:** `claude_desktop_config.json` with `mcpServers` field.

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/user"]
    },
    "brave-search": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-brave-search"],
      "env": {
        "BRAVE_API_KEY": "your-key"
      }
    }
  }
}
```

**Conversion:**
- Each entry in `mcpServers` → separate MCP tool registration
- `env` keys with sensitive names (`KEY`, `SECRET`, `TOKEN`, `PASSWORD`) → password config fields
- Other env keys → text config fields
- Plugin name derived from server names
- Category set to `connector`

### npm MCP Package

**Detected by:** `package.json` with `@modelcontextprotocol/sdk` in dependencies or `mcp` keyword.

```json
{
  "name": "my-mcp-server",
  "version": "1.0.0",
  "description": "Custom MCP server",
  "main": "server/index.js",
  "dependencies": {
    "@modelcontextprotocol/sdk": "^1.12.1"
  }
}
```

**Conversion:**
- Entry point from `main` field → stdio MCP config with `node` command
- Category set to `connector`

> **Note:** For npm packages, you should run `npm install` in the plugin directory after installation to install Node.js dependencies. TigrimOS does not auto-install npm dependencies.

---

## Plugin Lifecycle

### Install

1. Upload `.zip` via UI or API
2. Security validation (no path traversal, max 100 MB, max 500 files)
3. Manifest detection (TigrimOS → MCPB → Claude Code → Desktop Config → npm)
4. Plugin ID validation (`^[a-z0-9][a-z0-9-]*$`)
5. Extract to `data/plugins/{id}/`
6. Generate MCP config files for Claude formats
7. Register skills in `skills.json` (source: `plugin:{id}`)
8. Copy agent YAMLs to `data/agents/` (prefixed `plugin_{id}_`)
9. Add MCP servers to settings
10. Initialize connector config with defaults
11. Save to `data/plugins.json`

### Enable/Disable

Toggling a plugin enables or disables all its components:
- Skills: `enabled` flag in `skills.json`
- MCP tools: `enabled` flag in settings

Agent configs remain on disk but are not auto-removed (toggle only affects runtime).

### Uninstall

1. Remove skills from `skills.json` (by source tag `plugin:{id}`)
2. Delete skill directories from `data/skills/`
3. Delete agent files from `data/agents/`
4. Remove MCP entries from settings
5. Delete plugin directory `data/plugins/{id}/`
6. Remove from `data/plugins.json`

### Reinstall

Installing a plugin with the same `id` as an existing plugin triggers uninstall first, then a fresh install.

---

## REST API Reference

All endpoints are under `/api/plugins` and require authentication when auth is configured.

### List Plugins

```
GET /api/plugins
```

**Response:** `200 OK`
```json
[
  {
    "id": "gmail-connector",
    "name": "Gmail Connector",
    "version": "1.0.0",
    "author": "TigrimOS Community",
    "description": "Connect to Gmail...",
    "category": "connector",
    "enabled": true,
    "permissions": ["network", "python"],
    "components": { ... },
    "installedAt": "2026-06-03T10:00:00Z",
    "skillIds": ["uuid-1"],
    "mcpNames": ["gmail-mcp"],
    "agentFiles": ["plugin_gmail-connector_email_swarm.yaml"],
    "hasReadme": true,
    "hasIcon": true
  }
]
```

### Install Plugin

```
POST /api/plugins/upload
Content-Type: multipart/form-data
```

Upload the `.zip` file as multipart form data.

**Response:** `200 OK` — the installed plugin object.

**Errors:**
- `400` — Invalid ZIP, missing manifest, invalid plugin ID, security violation

### Get Plugin Details

```
GET /api/plugins/{id}
```

**Response:** `200 OK` — full plugin object.

### Toggle Enable/Disable

```
PATCH /api/plugins/{id}
Content-Type: application/json

{ "enabled": false }
```

**Response:** `200 OK` — updated plugin object.

### Uninstall Plugin

```
DELETE /api/plugins/{id}
```

**Response:** `200 OK`
```json
{ "ok": true }
```

### Get README

```
GET /api/plugins/{id}/readme
```

**Response:** `200 OK`
```json
{ "content": "# My Plugin\n\nThis plugin does..." }
```

### Get Icon

```
GET /api/plugins/{id}/icon
```

**Response:** `200 OK` with `Content-Type: image/png` body.

### List Connectors

```
GET /api/plugins/{id}/connectors
```

**Response:** `200 OK`
```json
[
  {
    "name": "Gmail",
    "service": "gmail",
    "description": "Read/send Gmail emails",
    "config_fields": [ ... ],
    "configured": true
  }
]
```

### Get Connector Config

```
GET /api/plugins/{id}/connectors/{service}/config
```

**Response:** `200 OK`
```json
{
  "email": "user@gmail.com",
  "app_password": "xxxx",
  "imap_server": "imap.gmail.com"
}
```

### Save Connector Config

```
PUT /api/plugins/{id}/connectors/{service}/config
Content-Type: application/json

{
  "email": "user@gmail.com",
  "app_password": "my-app-password",
  "imap_server": "imap.gmail.com"
}
```

**Response:** `200 OK`
```json
{ "ok": true }
```

---

## Security

| Check | Limit |
|-------|-------|
| Path traversal | `..` and absolute paths rejected |
| Max uncompressed size | 100 MB |
| Max file count | 500 files |
| Plugin ID format | `^[a-z0-9][a-z0-9-]*$` |
| Connector credentials | Stored in `data/plugins/{id}/connector_config.json`, never in the zip |
| Connector scripts | Run through existing sandbox (3-tier fallback: Apple container, sandbox-exec, direct) |

---

## Examples

### Minimal Plugin (Skill Only)

```
hello-skill.zip
├── plugin.yaml
└── skills/
    └── hello/
        └── SKILL.md
```

**plugin.yaml:**
```yaml
id: "hello-skill"
name: "Hello Skill"
version: "1.0.0"
author: "Developer"
description: "A simple greeting skill"

components:
  skills:
    - path: "skills/hello"
      name: "Hello"
      description: "Greet the user"
```

**skills/hello/SKILL.md:**
```markdown
---
name: Hello
description: Greet the user warmly
---

When the user says hello, respond with a warm greeting and ask how you can help.
```

### MCP Server Plugin

```
weather-mcp.zip
├── plugin.yaml
├── mcp/
│   └── weather.json
└── server.py
```

**plugin.yaml:**
```yaml
id: "weather-mcp"
name: "Weather MCP"
version: "1.0.0"
author: "Developer"
description: "Get weather data via MCP"
category: "connector"

components:
  mcp_servers:
    - path: "mcp/weather.json"
      name: "Weather API"
      description: "OpenWeatherMap MCP server"

  connectors:
    - path: "server.py"
      name: "Weather"
      service: "openweathermap"
      description: "Weather data from OpenWeatherMap"
      config_fields:
        - key: "api_key"
          label: "API Key"
          type: "password"
          required: true
        - key: "units"
          label: "Units"
          type: "text"
          default: "metric"

permissions:
  - "network"
  - "python"
```

**mcp/weather.json:**
```json
{
  "name": "weather-mcp",
  "type": "stdio",
  "command": "python3",
  "args": ["server.py", "--mcp"]
}
```

### Full Plugin (All Components)

```
project-manager.zip
├── plugin.yaml
├── skills/
│   └── task-planner/
│       └── SKILL.md
├── agents/
│   └── pm_team.yaml
├── mcp/
│   └── jira-server.json
├── connectors/
│   └── jira.py
├── README.md
└── icon.png
```

### Importing a Claude Desktop Config

Just zip your `claude_desktop_config.json`:

```
my-mcp-servers.zip
└── claude_desktop_config.json
```

All servers defined in `mcpServers` will be imported as MCP tools with auto-generated connector config fields for any `env` variables.
