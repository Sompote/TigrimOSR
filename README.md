# TigrimOS

**TigrimOS in Rust** — a high-performance native desktop rewrite of [TigerCowork](https://github.com/Sompote/TigerCowork), the original Python/Node.js AI assistant. This version is built entirely in Rust using egui for the UI, delivering faster startup, lower memory usage, and a single self-contained binary with no Node.js or Python runtime required to run the app itself.

TigrimOS is a native desktop AI assistant with multi-agent collaboration, tool calling, and file output. It connects to any OpenAI-compatible API (OpenAI, Anthropic via proxy, DeepSeek, local Ollama, etc.) and lets you orchestrate teams of specialist AI agents defined in simple YAML files.

## Features

- **Multi-agent system** — hierarchical, mesh, and hybrid orchestration modes via YAML config
- **Tool calling** — web search, Python execution, file read/write, shell commands, skill loading
- **Output panel** — inline preview for images (PNG/JPG), markdown reports, CSV tables, JSON, PDF, HTML
- **Agent history log** — JSONL logs per session in `data/agent_history/`
- **Skills system** — loadable skill modules from `skills/` directory
- **Sandboxed Python** — matplotlib plots auto-saved as PNG via Agg backend
- **Resizable layout** — drag handles for chat sidebar and output panel widths
- **Session management** — persistent chat history with project context

## Requirements

- Rust 1.75+ (`rustup` recommended)
- Python 3.8+ with pip (for tool execution)
- macOS 12+ (primary target; Linux supported without sandbox-exec)

### Python packages (optional but recommended)

```bash
pip install duckduckgo-search matplotlib numpy pandas requests
```

## Installation

### 1. Clone the repository

```bash
git clone https://github.com/Sompote/TigrimOSR.git
cd TigrimOSR
```

### 2. Build

```bash
cargo build --release
```

### 3. Run

```bash
cargo run --release
```

Or run the compiled binary directly:

```bash
./target/release/tigrimos
```

## Configuration

On first launch, go to **Settings** to configure:

| Setting | Description |
|---------|-------------|
| API Key | Your OpenAI-compatible API key |
| API URL | Endpoint (e.g. `https://api.openai.com/v1/chat/completions`) |
| Model | Model name (e.g. `gpt-4o`, `claude-3-5-sonnet`) |
| Sub-agent system | Enable multi-agent mode |
| Agent config file | Select a YAML file from `data/agents/` |

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

### YAML format

```yaml
system:
  name: My Agent System
  orchestration_mode: hierarchical  # hierarchical | mesh | hybrid | pipeline

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
│   ├── main.rs              # Entry point
│   ├── ui/
│   │   ├── chat.rs          # Chat UI, streaming, log panel
│   │   ├── output_panel.rs  # File output panel (images, MD, CSV, etc.)
│   │   └── settings.rs      # Settings UI
│   └── server/
│       ├── services/
│       │   └── toolbox.rs   # Tool execution + multi-agent loop
│       └── data.rs          # Data models
├── data/
│   └── agents/              # YAML agent config files
├── skills/                  # Loadable skill modules (SKILL.md)
└── Cargo.toml
```

## Keyboard Shortcuts

- `Enter` — send message
- `Shift+Enter` — new line in input

## License

MIT
