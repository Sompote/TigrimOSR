# TigrimOSR v0.5.0

**TigrimOSR** is the Rust version of [TigrimOS](https://github.com/Sompote/TigerCowork) — a high-performance native desktop rewrite of the original Python/Node.js AI assistant. Built entirely in Rust using egui for the UI, TigrimOSR delivers faster startup, lower memory usage, and a single self-contained binary with no Node.js or Python runtime required to run the app itself.

TigrimOSR is a native desktop AI assistant with multi-agent collaboration, tool calling, and file output. It connects to any OpenAI-compatible API (OpenAI, Anthropic via proxy, DeepSeek, Kimi, local Ollama, etc.) and lets you orchestrate teams of specialist AI agents defined in simple YAML files.

### Run Locally or Remotely — Control from Anywhere

TigrimOSR runs as a **local desktop app** on macOS or as a **headless server** on remote Linux machines. When running remotely, you can control it from:

- **Native Mac app** — Toggle Local/Remote in the topbar. All tabs (Chat, Projects, Agents, Files, Tasks, Terminal, Settings) transparently switch to controlling the remote server using the same familiar UI.
- **Web browser** — Open `http://<server-ip>:3001/web/` from any device (desktop, tablet, phone). Full web UI with chat, files, terminal, agents, tasks, and settings — with live progress updates while the AI is thinking.

![TigrimOSR Architecture](assets/architecture.png)

![TigrimOSR Screenshot](assets/screenshot.png)

### AI Provider Settings

10 built-in providers including 3 local CLI agents (Claude Code, Gemini CLI, Codex) — no API keys needed for local providers.

![AI Provider Settings](assets/screenshot_providers.png)

### Agent Swarm Editor

Design multi-agent systems visually — create architectures manually or generate them automatically with AI. Supports hierarchical, hybrid, mesh, pipeline, and P2P orchestration modes.

![Agent Swarm Editor](assets/screenshot_agents.png)

## What's New in v0.5.0

- **Kimi-style Files browser** — Complete redesign of the Files tab with a left sidebar (Library / Places), white background, colored extension badges (DOCX=blue, XLSX=green, PNG=orange, etc.), breadcrumb navigation, relative dates ("Today", "Yesterday", "3 days ago"), and a selection action bar with Download/Delete buttons.
- **Agent Swarm light theme** — Agent Swarm view redesigned with white canvas, light sidebar, floating Node Properties and Connection Properties windows, blue selection borders, and hover glow effects with tooltip cards showing agent name/role/persona.
- **Claude Code identity headers** — All LLM call sites now send full Claude Code identity headers (User-Agent, X-Client-Name, X-Client-Version, HTTP-Referer, X-Traffic-Source) for Kimi API compatibility. Applied across toolbox, skill synthesizer, compact, settings validation, and MCP services.
- **Logo image in About & Chat** — About section and chat welcome screen now display the TigrimOS logo as a rendered image instead of text emoji.
- **Zero-lag chat send** — In-memory messages with atomic save on stream complete for instant chat responsiveness.
- **WebSocket live updates** — Remote tasks now receive live updates via WebSocket, plus UTF-8 crash fix and improved chat input.
- **Fast remote sync** — Sync cache fast-path with background fetch and pre-warming for snappy remote mode.

### Previous Releases

<details>
<summary>v0.4.1 — Transparent remote toggle, remote caching, live web progress</summary>

- **Transparent Local/Remote toggle** — Switch between Local and Remote mode from the topbar. When Remote is active, all tabs (Chat, Projects, Agents, Files, Tasks, Terminal, Settings) transparently work against the remote server — same familiar UI, no separate "Remote" view needed.
- **Remote caching** — In-memory cache with TTL avoids repeated HTTP calls on every UI frame, making remote mode fast and responsive.
- **Live progress in web chat** — Web UI now shows real-time tool call progress while the AI is thinking (tool names, results preview, errors) instead of just a static "Thinking..." spinner.
- **Web UI chat fix** — Fixed chat not working in web UI: removed broken remote task detour, fixed 403 auth interception, added auto-session creation when no session is selected.
- **Bulk sync endpoints** — Added `GET/PUT /api/*/bulk` endpoints for efficient full-array sync between local and remote instances.
- **Remote-aware views** — Chat, Agents, Projects, Terminal, and Files views all route through the data layer proxy when remote mode is active.
- **Zero compiler warnings** — All platform-conditional code properly gated with `#[cfg]`.

</details>

<details>
<summary>v0.4.0 — Headless mode, remote web UI, remote server dashboard, auth security</summary>

- **Headless mode** — Run TigrimOS on a remote Linux server without GUI: `./tigrimos --headless`. Interactive token prompt ensures security — empty tokens are blocked.
- **Remote Web UI** — Full embedded web interface at `/web/` for controlling TigrimOS from any browser or mobile phone. Includes Chat, Files, Terminal, Agents, Tasks, and Settings pages. No Node.js or build tools needed — the SPA is compiled into the binary.
- **Remote Server tab** — Native Mac app can connect to and control remote TigrimOS instances. Browse files, submit tasks, chat, and view settings on the remote server from your local desktop.
- **Remote authentication** — Set a Remote Token in Settings to secure API access. When enabled, all API endpoints require the token. The web UI shows a login page — no data accessible without authentication.
- **LaTeX math rendering** — Web UI renders LaTeX equations via KaTeX (`\[...\]`, `\(...\)`, `$$...$$`, `$...$`). Supports fractions, subscripts, Greek letters, and display math.
- **Markdown rendering** — Web UI renders tables, headings, bold/italic, code blocks, lists, and horizontal rules in chat and task results.
- **MCP tool integration** — MCP tools configured in Settings are now injected into the AI agent's tool loop. The agent can discover and call MCP tools during execution.

</details>

### Remote Access Setup

TigrimOS can run on a remote cloud server and be controlled from your Mac or any browser.

**On the remote server (Linux):**

```bash
# Build
git clone https://github.com/Sompote/TigrimOSR.git
cd TigrimOSR
cargo build --release

# Run headless — prompts for a security token
./target/release/tigrimos --headless

# Or set token via environment variable
ACCESS_TOKEN=my-secret-token PORT=3001 ./tigrimos --headless
```

The server will prompt:
```
===========================================
  TigrimOS Headless Mode — Security Setup
===========================================

Enter access token (min 8 chars): ********

Token set. Use this to connect from your Mac or browser.
  Web UI:  http://<server-ip>:3001/web/
  Token:   your-token-here
```

**Access from a browser:**
1. Open `http://<server-ip>:3001/web/`
2. Enter the access token on the login page
3. Use Chat, Files, Terminal, Agents, Tasks, Settings tabs

**Access from your Mac TigrimOS app:**
1. Go to **Settings → Remote Instances**
2. Check **"Enable remote agent access"**
3. Under **Add Remote Instance**, enter:
   - **Name:** My Cloud Server
   - **URL:** `http://<server-ip>:3001`
   - **Token:** the access token from the server
4. Click **Add Instance**
5. Go to the **Remote** tab — select the server, see connection status
6. Submit tasks, browse files, or chat with the remote AI

**With nginx (recommended for production):**

```nginx
server {
    listen 80;
    server_name your-domain.com;

    location / {
        proxy_pass http://127.0.0.1:3001;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 300s;
    }
}
```

Add HTTPS with Let's Encrypt: `sudo certbot --nginx -d your-domain.com`

### Security

| Method | How to Set | When Auth is Required |
|--------|-----------|----------------------|
| `ACCESS_TOKEN` env var | `ACCESS_TOKEN=xxx ./tigrimos` | Always (headless or GUI) |
| Remote Token (Settings) | Settings → Remote Instances → set token + enable | When "Enable remote agent access" is checked |
| `--headless` prompt | Interactive prompt on startup | Always in headless mode |
| No token set | — | No auth (local desktop use only) |

<details>
<summary>v0.3.0 — Pipeline architecture, checkpoint/resume, 9-step compression</summary>

- **Pipeline architecture mode** — True sequential pipeline orchestration: user task flows from agent1 → agent2 → agent3 automatically via `send_task`. Architecture generation now produces correct linear chain connections with `workflow.sequence` and `outputs_to`.
- **Pipeline-aware dispatch** — Fully Auto and Manual modes auto-route user tasks to the first pipeline agent and wait for the last agent's result, instead of treating all agents as orchestrator targets.
- **Checkpoint/Resume on abort** — Tool loop now saves a full checkpoint (messages, tool history, errors, early content) when cancelled, matching tiger_cowork's abort-save behavior. Resumed sessions restore complete state including `tool_call_history`, `consecutive_errors`, and `early_content`.
- **Kimi API compatibility** — Fixed Agents tab "Auto Architecture" failing with Kimi by adding Claude Code identity headers (`User-Agent`, `X-Client-Name`, `X-Client-Version`) to all Kimi API calls.
- **Improved graph layout** — Agent nodes in the System Editor now fit within the visible canvas with proper padding. Animated signal dots in the Graphic view use correct time synchronization and show faint lines for runtime connections.
- **9-step compression pipeline** — Full context compaction system ported from tiger_cowork: LLM-based summarization, smart tool-result compression by type, post-compact context restoration, checkpoint save/resume, circuit breaker, and cooldown.
- **Cancel flag for tool loops** — `SubAgentConfig.cancel_flag` allows external cancellation of running tool loops with automatic checkpoint save.

</details>

<details>
<summary>v0.2.4 — Gemini CLI, live agent progress, 6 orchestration modes</summary>

- **Gemini CLI (Local)** — Use Google's Gemini CLI as an AI backend, no API key needed (same as Claude Code and Codex)
- **Live agent progress in chat** — Fully Auto mode now shows step-by-step progress (architecture → boot → delegate → wait) with live agent activity updates instead of just "thinking..."
- **Live agent graphic monitor** — Agent Log graphic tab shows real-time agent nodes, delegation edges, and working status during execution
- **6 orchestration modes** — Hierarchical, hybrid, mesh, pipeline, P2P, and P2P orchestrator modes cloned from tiger_cowork with exact behavioral parity
- **Apply to Chat button** — Agents tab now has "Apply to Chat" button to use the selected architecture in Manual mode
- **Smarter loop detection** — Monitoring tools (check_agents, bb_read) exempt from loop detection; realtime agents get higher limits (30 rounds, 60 tool calls)
- **Agent history fix** — spawn.jsonl now writes to the correct data directory so the graphic view works from .app bundles

</details>

<details>
<summary>v0.2.3 — Local CLI providers, agent harness settings, VM terminal</summary>

- **Local CLI providers** — Use Claude Code or OpenAI Codex CLI installed on your machine as AI backends, no API key needed
- **Agent harness settings** — Configurable max turns, max tool calls, temperature, max tokens, context limit, compression interval, and reflection toggle in Settings
- **VM Terminal via SSH** — Terminal tab connects to Ubuntu VM via SSH (`sshpass`) instead of local bash
- **VM tool routing** — `run_python` and `run_shell` execute inside the VM via SSH when VM is running
- **Mode rename** — "Realtime" mode renamed to "Manual"; mode order starts with Fully Auto
- **Robust CLI spawning** — Node.js-based CLIs (claude, codex) launched via `node script.js` directly, bypassing shebang issues in .app bundles
- **Environment fixes** — Proper PATH/HOME injection for .app bundle launches where env vars are minimal

</details>

<details>
<summary>v0.2.1 — Cross-platform, .app fixes, parallel streaming</summary>

- **Cross-platform support** — Windows and Linux compatibility for sandbox execution, Python/shell discovery, and subprocess spawning
- **.app bundle fixes** — Resolved issues with data directories, sandbox paths, Python/shell not found when launched from macOS `.app` bundle
- **Persistent chat logs** — Agent activity logs now persist after chat completes instead of disappearing
- **Parallel chat streaming** — Multiple chat sessions can stream responses simultaneously via HashMap-based state
- **Installer improvements** — Robust `curl | bash` support with proper cwd handling, terminal prompt fallbacks
- **Zero compiler warnings** — All 162 warnings resolved (deprecated egui APIs, unused imports, dead code)

</details>

<details>
<summary>v0.2.0 — Multi-agent core, MCP, Cloudflare tunnel</summary>

- **Agent modes** — Auto, Fully Auto, Auto Swarm, and Manual modes for flexible agent orchestration
- **Connection editor** — Click agent connection lines to change protocol type (TCP, Queue, Bus, Blackboard)
- **Chat info card** — Shows active architecture name, swarm mode, and model in the chat view
- **Security settings** — Per-tool approval toggles for shell, Python, file write, file delete, and agent spawn
- **Sandbox file browser** — Files tab shows only the sandbox folder with image file preview support
- **Task management** — Kill button for active sessions, reordered tabs (Active before Scheduled)
- **Remote task API** — Submit, poll, and kill tasks via HTTP endpoints (`/api/remote/*`)
- **Inter-agent protocols** — TCP, Bus, Queue, and Blackboard communication between agents
- **MCP client** — Model Context Protocol support with stdio, SSE, and HTTP transports
- **Cloudflare tunnel** — Built-in tunnel management for remote access
- **ClawHub marketplace** — Search, install, and manage skills from the ClawHub skill marketplace
- **Custom app icon** — Program icon replaces emoji in the title bar

</details>

## Features

- **Multi-agent system** — hierarchical, mesh, hybrid, pipeline, P2P, and P2P orchestrator modes via YAML config
- **Local CLI agents** — Use Claude Code, Gemini CLI, or OpenAI Codex as agent backends without API keys
- **Remote access** — Headless mode + embedded web UI for controlling from any browser or mobile phone
- **Remote server dashboard** — Connect your Mac app to remote TigrimOS instances
- **Tool calling** — web search, Python execution, file read/write, shell commands, skill loading, MCP tools
- **VM integration** — Built-in Ubuntu VM with SSH terminal and tool routing
- **Output panel** — inline preview for images (PNG/JPG), markdown reports, CSV tables, JSON, PDF, HTML
- **Agent history log** — JSONL logs per session in `data/agent_history/`
- **Skills system** — loadable skill modules from `skills/` directory
- **Sandboxed Python** — matplotlib plots auto-saved as PNG via Agg backend
- **Resizable layout** — drag handles for chat sidebar and output panel widths
- **Session management** — persistent chat history with project context
- **LaTeX math** — KaTeX rendering in web UI for equations and formulas

## Requirements

- Rust 1.75+ (`rustup` recommended)
- Python 3.8+ with pip (for tool execution)
- macOS 12+ (primary target; Linux supported for headless mode)

### Optional local CLI agents

- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) — `npm install -g @anthropic-ai/claude-code`
- [Gemini CLI](https://github.com/google-gemini/gemini-cli) — `npm install -g @anthropic-ai/gemini-cli`
- [OpenAI Codex](https://github.com/openai/codex) — `npm install -g @openai/codex`

### Python packages (optional but recommended)

```bash
pip install duckduckgo-search matplotlib numpy pandas requests
```

## Installation

### Quick Install (recommended)

One-command installer that clones, builds, and sets up the app for you.

**Prerequisites:** Install Rust first (if not already installed):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**macOS:**
```bash
curl -sSL https://raw.githubusercontent.com/Sompote/TigrimOSR/main/install.sh | bash
```

**Linux (Desktop):**
```bash
curl -sSL https://raw.githubusercontent.com/Sompote/TigrimOSR/main/install-linux.sh | bash
```
Select **"Desktop mode"** when prompted.

**Linux (Headless / Remote Server):**
```bash
curl -sSL https://raw.githubusercontent.com/Sompote/TigrimOSR/main/install-linux.sh | bash
```
Select **"Headless mode"** when prompted. The installer will:
- Ask for a port (default 3001) and access token (min 8 chars)
- Create a **systemd service** (auto-start on boot)
- Optionally set up **nginx** reverse proxy (port 80)
- Optionally configure **HTTPS** via Let's Encrypt
- Open **firewall** ports (ufw)

After install, manage the server with:
```bash
sudo systemctl start tigrimos    # start server
sudo systemctl stop tigrimos     # stop server
sudo journalctl -u tigrimos -f   # view logs
```

Access the web UI at `http://<server-ip>/web/` and log in with your token.

**Windows (PowerShell):**
```powershell
irm https://raw.githubusercontent.com/Sompote/TigrimOSR/main/install.ps1 | iex
```

**Windows (Command Prompt):**
Download and run [`install.bat`](https://raw.githubusercontent.com/Sompote/TigrimOSR/main/install.bat).

The installer will:
1. Check prerequisites (git, Rust)
2. Let you choose an install location
3. Clone and build in release mode
4. **Desktop**: Create a native app (macOS `.app` / Linux `.desktop` / Windows shortcut)
5. **Headless (Linux)**: Create systemd service + nginx + firewall + HTTPS
6. Optionally launch the app or start the server

---

### Manual Install

If you prefer to install manually, follow the steps below.

#### Step 1 — Install Rust

Rust is required to build the app. Install it via `rustup` (the official Rust installer):

**macOS / Linux:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**Windows:**
Download and run [rustup-init.exe](https://win.rustup.rs/) from https://rustup.rs

Verify the installation:
```bash
rustc --version
cargo --version
```

---

#### Step 2 — Install Python

Python is used for code execution, web search, and data analysis tools inside the app.

**macOS (recommended via Homebrew):**
```bash
brew install python
```

**Linux (Ubuntu/Debian):**
```bash
sudo apt update && sudo apt install python3 python3-pip
```

**Linux (extra dev libraries for GUI):**
```bash
# Debian/Ubuntu
sudo apt install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libgtk-3-dev

# Fedora
sudo dnf install libxcb-devel libxkbcommon-devel gtk3-devel

# Arch
sudo pacman -S libxcb libxkbcommon gtk3
```

**Windows:**
Download from https://www.python.org/downloads/ — make sure to check **"Add Python to PATH"** during install.

Verify:
```bash
python3 --version
pip3 --version
```

Install required Python packages:
```bash
pip3 install -r requirements.txt
```

---

#### Step 3 — Clone the repository

```bash
git clone https://github.com/Sompote/TigrimOSR.git
cd TigrimOSR
```

#### Step 4 — Build

```bash
cargo build --release
```

> First build downloads all Rust dependencies and may take 2-5 minutes.

#### Step 5 — Run

**Desktop mode (with GUI):**
```bash
cargo run --release
```

**Headless mode (remote server, no GUI):**
```bash
./target/release/tigrimos --headless
```

Or run the compiled binary directly:
```bash
./target/release/tigrimos
```

**Windows:**
```bash
target\release\tigrimos.exe
```

## Configuration

On first launch, go to **Settings** to configure:

| Setting | Description |
|---------|-------------|
| AI Provider | Select from Claude Code (Local), Gemini CLI (Local), Codex (Local), OpenRouter, Anthropic, DeepSeek, Kimi, etc. |
| API Key | Your API key (not needed for local CLI providers) |
| Model | Model name (e.g. `o4-mini`, `claude-sonnet-4-20250514`) |
| Agent Harness | Max turns, temperature, max tokens, context limit, reflection |
| Sub-agent system | Enable multi-agent mode |
| Agent config file | Select a YAML file from `data/agents/` |
| Agent mode | Fully Auto, Auto, Auto Swarm, or Manual |
| Remote access | Enable remote + set token for web UI and remote connections |
| MCP tools | Configure external tool servers (stdio/HTTP) in JSON format |

## Multi-Agent System

Enable sub-agents in Settings and select an agent config file. Included configs in `data/agents/`:

| File | Description |
|------|-------------|
| `agents.yaml` | Civil engineering team (PM, structural, geotechnical, checker, reporter) |
| `marketting.yaml` | Marketing research team (6-agent mesh) |
| `designteam.yaml` | Design team |
| `Researcmodel.yaml` | Research pipeline |
| `BOQ.yaml` | Bill of quantities team |
| `research_agent.yaml` | General research agent |

### Agent Modes

| Mode | Description |
|------|-------------|
| **Fully Auto** | Starts with `create_architecture` tool, then switches to agent team |
| **Auto** | Standard tool-calling loop with optional sub-agent delegation |
| **Auto Swarm** | Starts with `select_swarm` to pick an existing YAML config, then boots agent team |
| **Manual** | No automatic tool calling; agents respond with instructions only |

### Inter-Agent Protocols

| Protocol | Description |
|----------|-------------|
| **TCP** | Point-to-point reliable channel between two agents |
| **Bus** | Publish/subscribe messaging with topic filtering |
| **Queue** | FIFO message queue between agent pairs |
| **Blackboard** | Shared key-value store with task proposals and voting |

### YAML format

```yaml
system:
  name: My Agent System
  orchestration_mode: hierarchical  # hierarchical | mesh | hybrid | pipeline | p2p | p2p_orchestrator

agents:
  - id: orchestrator
    name: Project Manager
    role: orchestrator
    persona: You are an expert project manager...
    responsibilities:
      - Analyze the user request
      - Delegate tasks to specialist agents
    bus:
      enabled: true

  - id: analyst
    name: Data Analyst
    role: worker
    persona: You are a data analysis expert...
    responsibilities:
      - Analyze datasets
      - Generate charts using Python

workflow:
  sequence:
    - step: 1
      agent: orchestrator
      outputs_to: [analyst]
    - step: 2
      agent: analyst

connections:
  - from: orchestrator
    to: analyst
    protocol: tcp
```

## Project Structure

```
TigrimOSR/
├── src/
│   ├── main.rs              # Entry point (GUI + headless mode)
│   ├── ui/
│   │   ├── app.rs           # Main app frame, logo, tab routing
│   │   ├── chat.rs          # Chat UI, streaming, info card, log panel
│   │   ├── agents_view.rs   # Agent architecture canvas, connection editor
│   │   ├── files_view.rs    # Sandbox file browser with image preview
│   │   ├── tasks_view.rs    # Active/Scheduled/Finished/Remote task management
│   │   ├── remote_view.rs   # Remote server dashboard (connect to remote instances)
│   │   ├── settings.rs      # Settings UI with harness parameters
│   │   ├── terminal_view.rs # VM Terminal via SSH
│   │   ├── output_panel.rs  # File output panel (images, MD, CSV, etc.)
│   │   └── skills_view.rs   # Skills browser and ClawHub marketplace
│   ├── server/
│   │   ├── services/
│   │   │   ├── toolbox.rs    # Tool execution + multi-agent loop + CLI providers
│   │   │   ├── compact.rs    # 9-step context compression pipeline
│   │   │   ├── protocols.rs  # TCP, Bus, Queue, Blackboard protocols
│   │   │   ├── clawhub.rs    # ClawHub skill marketplace
│   │   │   ├── mcp.rs        # MCP client (stdio/SSE/HTTP)
│   │   │   └── tunnel.rs     # Cloudflare tunnel management
│   │   ├── routes/
│   │   │   ├── remote.rs     # Remote task API endpoints
│   │   │   └── web_ui.rs     # Embedded web UI serving
│   │   └── data.rs           # Data models and persistence
│   └── vm/
│       ├── manager.rs        # QEMU VM lifecycle management
│       └── config.rs         # VM configuration constants
├── static/
│   └── index.html            # Embedded web UI (SPA with KaTeX)
├── data/
│   └── agents/              # YAML agent config files
├── assets/
│   └── icon.png             # App icon
├── skills/                  # Loadable skill modules (SKILL.md)
└── Cargo.toml
```

## Keyboard Shortcuts

- `Enter` — send message
- `Shift+Enter` — new line in input

## License

MIT
