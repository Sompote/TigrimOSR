# TigrimOS CLI (`tigrim`) — Complete Reference

Everything the `tigrim` CLI reads and writes: command-line flags, environment variables, slash commands, the `.tigrimos/` project folder, and **every setting in every YAML/JSON file** — `settings.json`, agent-loop profiles, agent team YAMLs, graph (judge-panel) profiles, and judge rules files.

> Quick start lives in the [README CLI section](README.md#cli-mode-tigrim). This document is the full reference.

---

## Table of contents

1. [Command-line flags & exit codes](#1-command-line-flags--exit-codes)
2. [Environment variables & `.env`](#2-environment-variables--env)
3. [Slash commands](#3-slash-commands)
4. [The `.tigrimos/` folder & resolution order](#4-the-tigrimos-folder--resolution-order)
5. [`settings.json` — every key](#5-settingsjson--every-key)
6. [`cli_state.json`](#6-cli_statejson)
7. [Agent-loop profile YAML (`agent_loops/*.yaml`)](#7-agent-loop-profile-yaml-agent_loopsyaml)
8. [Agent team YAML (`agents/*.yaml`)](#8-agent-team-yaml-agentsyaml)
9. [Graph profile YAML (`graph/*.yaml`) & rules files](#9-graph-profile-yaml-graphyaml--rules-files)
10. [Sub-agent modes](#10-sub-agent-modes)

---

## 1. Command-line flags & exit codes

Run `tigrim` with no arguments for the interactive REPL; `tigrim -p "<prompt>"` for a one-shot run (final answer → stdout, progress/tool lines → stderr, safe to pipe).

| Flag | Argument | Default | What it does |
|---|---|---|---|
| `-p`, `--print` | `"<prompt>"` | — | One-shot mode: run a single agent turn and exit. |
| `--mode` | `<m>` | `single` | Sub-agent mode for this run: `single`, `auto`, `manual`, `fully_auto`, `router`, `graph`. |
| `--loop` | `<name>` | from settings | Agent-loop profile (project `.tigrimos/agent_loops/` first, then global). |
| `--graph` | `<name>` | from settings | Graph (judge-panel) profile; also activates graph mode. |
| `--agent` | `<file>` | none | Agent team YAML (auto-appends `.yaml`); switches mode to `auto` if currently `single`. |
| `--model` | `<id>` | from settings | Model override for this run only (not persisted). |
| `--session` | `<id>` | new session | Continue an existing session id (persisted to `cli_state.json`). |
| `--new` | — | — | Start a fresh session (persisted; overrides `--session`). |
| `--yes`, `-y` | — | off | Auto-approve all tool executions (no y/n prompts). |
| `--cwd` | `<dir>` | `.` | Change working directory before initializing the project. |
| `-h`, `--help` | — | — | Print usage and exit. |
| `-V`, `--version` | — | — | Print version and exit. |

`--mode`, `--loop`, `--graph`, `--agent`, `--model` are **transient** (apply to that run only). `--session` / `--new` **are** persisted to `.tigrimos/cli_state.json`.

**Exit codes:** `0` success · `1` run failed · `2` usage/config error (invalid flag, missing profile, no TTY for setup wizard) · `130` interrupted (Ctrl-C / ESC).

---

## 2. Environment variables & `.env`

The CLI is folder-local: each folder configures its own provider via `.env`. Precedence (highest wins, already-set values are never overwritten):

1. Shell environment
2. `.tigrimos/.env` (created by the first-run wizard; gitignored)
3. `.env` in the project root
4. Global `data/.env` (lowest)

| Variable | Default | Purpose |
|---|---|---|
| `TIGRIMOS_API_KEY` | *(required)* | API key for your LLM provider. When set, `/model` refuses to change the model (edit `.env` instead) and `/settings` shows the source as `.env`. |
| `TIGRIMOS_API_URL` | `https://api.deepseek.com/v1` | API base URL. URLs containing `anthropic.com` automatically use the native Claude Messages API; everything else uses the OpenAI-compatible protocol. |
| `TIGRIMOS_MODEL` | `deepseek-chat` | Model id for agent runs; overrides `settings.json`. |

### Provider quick reference

| Provider | `TIGRIMOS_API_URL` | Example `TIGRIMOS_MODEL` |
|---|---|---|
| **Anthropic (Claude)** | `https://api.anthropic.com/v1` | `claude-opus-5` |
| **OpenAI** | `https://api.openai.com/v1` | `gpt-5.2`, `o4-mini` |
| **Kimi (Moonshot)** | `https://api.kimi.com/coding/v1` | `kimi-k3` |
| **MiniMax** | `https://api.minimax.io/v1` | `MiniMax-M3` |
| **DeepSeek** | `https://api.deepseek.com/v1` | `deepseek-chat` |

Kimi and MiniMax reasoning models always run at temperature 1.0 (provider requirement) — the CLI pins this automatically.

**First-run wizard:** if no API key is found and you're on a TTY, the CLI prompts for URL/key/model and writes `.tigrimos/.env` for you. Without a TTY it exits with code 2.

---

## 3. Slash commands

Commands are case-insensitive; anything not starting with `/` is sent as a chat message.

| Command | What it does |
|---|---|
| `/agents` | List agent team YAMLs, project entries marked `(project)`, with agent counts. |
| `/agent [file\|off]` | Show / set / clear the active team config (auto-appends `.yaml`; selecting one switches `single` → `auto`). |
| `/model [id]` | Show the model + model pool, or set the model (saved to the folder's `settings.json`; refused when `TIGRIMOS_MODEL` is set — edit `.env` instead). |
| `/mode [m]` | Show or set the sub-agent mode (`single`, `auto`, `manual`, `fully_auto`, `router`, `graph`). |
| `/loop [name\|off]` | Show / set / clear the agent-loop profile (project-first listing). |
| `/graph [name\|off]` | Show / set / clear the graph profile; setting one activates graph mode, `off` exits it. |
| `/skills` | List installed skills with enabled status and descriptions. |
| `/mcp` | List MCP servers with transport (stdio/http), enabled status, connection state and tool count. |
| `/settings` | Effective settings: model (+ source), API URL, masked key, workspace, global data dir, overlay status, mode/profiles. |
| `/new` | Start a fresh session (old conversation stays in `chat_history.json`). |
| `/stop` | Kill the currently running task. |
| `/status` | Model, mode, loop/graph/agent profiles, session id, running state. |
| `/tasks` | List running chats and scheduled tasks. |
| `/clear` | Clear the screen. |
| `/help`, `/?` | Command list and examples. |
| `/exit`, `/quit` | Quit (also Ctrl-D). ESC or Ctrl-C cancels a running turn. |

---

## 4. The `.tigrimos/` folder & resolution order

Created automatically where you run `tigrim` (like Claude Code's `.claude/`):

```
my-project/
├── .tigrimos/
│   ├── .env                  # your credentials (wizard-created, gitignored)
│   ├── .env.example          # setup template — copy to .env
│   ├── .gitignore            # auto-maintained: excludes .env, settings.json, state files
│   ├── README.md             # explains the folder (seeded once)
│   ├── settings.json         # PARTIAL or full settings override (gitignored — no keys!)
│   ├── cli_state.json        # session/mode/profile state (gitignored)
│   ├── chat_history.json     # this folder's conversations (gitignored)
│   ├── repl_history          # readline history (gitignored)
│   ├── agents/               # team YAMLs         (example_team.yaml seeded)
│   ├── agent_loops/          # loop profiles      (default.yaml seeded from your settings)
│   └── graph/                # graph profiles     (default.yaml + rules/ seeded)
└── ... your files — this folder is the agent's workspace
```

**Resolution is project-first, global fallback**, per file type:

- **Credentials:** shell env → `.tigrimos/.env` → `./.env` → global `data/.env`.
- **YAML profiles** (`agents/`, `agent_loops/`, `graph/`): a project file shadows a global file with the same name; listings union both and mark project entries `(project)`.
- **`settings.json`:** `.tigrimos/settings.json` keys override the seeded defaults for this folder only — a file containing just `{"tigerBotModel": "gpt-5.2"}` overrides only that key.
- Seeding never overwrites: existing files are left untouched on every run.

The seeded `.gitignore` excludes `.env`, `settings.json`, `cli_state.json`, `chat_history.json`, and `repl_history`, so `git add .` can never commit a key.

---

## 5. `settings.json` — every key

The CLI seeds a complete `settings.json` with every desktop-UI setting so you can see and edit all of them per folder. Keys are camelCase. Credentials are intentionally blank — use `.env` (env vars override `settings.json` at load time).

### Provider & authentication

| Key | Type | Default | Purpose |
|---|---|---|---|
| `tigerBotApiKey` | string | `""` | LLM API key — leave blank, use `.env` / `TIGRIMOS_API_KEY`. |
| `tigerBotApiUrl` | string | null | API endpoint; null falls back to DeepSeek v1. |
| `tigerBotModel` | string | `""` | Model id; `TIGRIMOS_MODEL` overrides. |
| `modelPool` | array | `[]` | Quick-switch pool: `[{"label": "Fast", "model": "gpt-4o-mini"}, ...]` — listed by `/model`. |

### Sub-agents & orchestration

| Key | Type | Default | Purpose |
|---|---|---|---|
| `subAgentMode` | string | `"single"` | Default mode: `single`, `auto`, `manual`, `fully_auto`, `router`, `graph`. |
| `subAgentConfigFile` | string | `""` | Default agent team YAML filename. |
| `agentLoopProfile` | string | unset | Active agent-loop profile filename; unset/empty = built-in loop. |
| `graphEnabled` | boolean | `false` | Global toggle: judge panel gates the answer in **all** modes (a profile's `graph.enabled` can override). |
| `routerTier` | string | null | Router tier (`router` vs `router ultra` behavior). |
| `routerOrchestratorModel` | string | null | Model the router's triage/merge orchestrator runs on; empty = main model. |

### Tools & integrations

| Key | Type | Default | Purpose |
|---|---|---|---|
| `mcpTools` | array | `[]` | MCP servers: `{name, enabled, tool_type, command, ...}` objects. |
| `webSearchEnabled` | boolean | `false` | Enable the web search tool. |
| `sandboxDir` | string | `""` | Execution sandbox directory (defaults to a per-install sandbox). |
| `agentAllowUnsandboxedExec` | boolean | `false` | Allow execution outside the sandbox when no sandbox backend is available (opt-in). |

### Agent-loop knobs (overridable per profile — see §7)

| Key | Type | Default | Purpose |
|---|---|---|---|
| `agentMaxToolRounds` | number | 15 | Max reasoning rounds per turn. |
| `agentMaxToolCalls` | number | 25 | Hard ceiling on total tool calls. |
| `agentTemperature` | number | 0.7 | Model temperature. |
| `agentMaxTokens` | number | 81920 | Max output tokens per response. |
| `agentReflectionEnabled` | boolean | `false` | Self-reflection step after rounds. |
| `agentReflectionThreshold` | number | 0.7 | Confidence below which reflection triggers. |
| `agentMaxReflectionRetries` | number | 2 | Reflection attempts per round. |
| `agentCheckpointEnabled` | boolean | `true` | Periodic checkpointing for recovery. |
| `agentMaxConsecutiveErrors` | number | 3 | Consecutive tool errors before recovery flow. |
| `agentMaxErrorRecoveries` | number | 5 | Max recovery attempts per session. |
| `agentCompressionInterval` | number | 5 | Compact context every N rounds. |
| `agentCompressionWindow` | number | 10 | Messages kept when compacting. |
| `agentMaxContextTokens` | number | 100000 | Target max context size. |
| `agentToolResultMaxLen` | number | 6000 | Truncate tool results to N chars during compaction. |
| `agentEvaluationEnabled` | boolean | `false` | Outer job-level judge (see §7 `evaluation`). |
| `agentEvaluationThreshold` | number | 0.75 | Judge pass threshold. |
| `agentEvaluationMaxJudgeRounds` | number | 3 | Judge tool-call rounds per evaluation. |

### Tool approvals

| Key | Type | Default | Purpose |
|---|---|---|---|
| `approvalRequiredForShell` | boolean | unset | Prompt before shell commands. |
| `approvalRequiredForPython` | boolean | unset | Prompt before Python execution. |
| `approvalRequiredForFileWrite` | boolean | unset | Prompt before file writes. |
| `approvalRequiredForFileDelete` | boolean | unset | Prompt before file deletions. |
| `approvalRequiredForAgentSpawn` | boolean | unset | Prompt before spawning sub-agents. |
| `autoApproveSubagentTools` | boolean | `true` | Auto-approve gated tools inside background sub-agents (they have no prompt UI); `false` makes them refuse instead. |

### Browser control

| Key | Type | Default | Purpose |
|---|---|---|---|
| `browserControlEnabled` | boolean | `false` | Let the agent drive a real browser (navigate/click/type/screenshot). |
| `browserEngine` | string | `"chrome"` | `"chromium"` (Playwright bundle), `"chrome"` (your Chrome), or `"obscura"` (stealthy Rust engine). |
| `browserObscuraPath` | string | `"obscura"` | Path to the `obscura` binary (when engine is `obscura`). |
| `browserHeadless` | boolean | null | Force headless (`true`) or visible (`false`); null follows the run mode. |

### Messaging bots

| Key | Type | Default | Purpose |
|---|---|---|---|
| `telegramEnabled` | boolean | `false` | Telegram long-poll bot. |
| `telegramBotToken` | string | null | Bot token. |
| `telegramAllowedUserIds` | array | null | Allowed numeric user IDs (fail-closed: empty = nobody). |
| `lineEnabled` | boolean | `false` | LINE webhook bot. |
| `lineChannelSecret` | string | null | LINE channel secret (HMAC verification). |
| `lineChannelAccessToken` | string | null | LINE access token. |
| `lineAllowedUserIds` | array | null | Allowed LINE user IDs (fail-closed). |

### Skill auto-update

| Key | Type | Default | Purpose |
|---|---|---|---|
| `skillAutoUpdateEnabled` | boolean | unset | Auto-generate skill candidates from session logs. |
| `skillAutoUpdateIntervalMinutes` | number | unset | Generation interval. |
| `skillAutoUpdateRequireApproval` | boolean | unset | Require approval before applying updates. |
| `skillAutoUpdateHumanFeedbackEnabled` | boolean | unset | Collect human feedback on candidates. |
| `skillAutoUpdateMaxCandidates` | number | unset | Cap on kept candidates. |

### Remote & VPN

| Key | Type | Default | Purpose |
|---|---|---|---|
| `remoteEnabled` | boolean | unset | Proxy agent runs to a remote TigrimOS host. |
| `remoteToken` | string | null | Auth token for the remote host. |
| `remoteTaskMaxRetries` | number | unset | Retries for remote task execution. |
| `remoteInstances` | array | null | Remote instance configs. |
| `vpnEnabled` | boolean | `false` | Use Tailscale instead of a Cloudflare tunnel. |
| `localFileMounts` | array | null | Local file mounts for the sandbox. |

---

## 6. `cli_state.json`

Per-folder REPL state, written on every `/new`, `/mode`, `/loop`, `/graph`, `/agent` (and `--session`/`--new`):

| Key | Type | Purpose |
|---|---|---|
| `sessionId` | string | Current session id (`cli_<timestamp>`); `/new` generates a fresh one. |
| `mode` | string | Sub-agent mode; absent = `single`. |
| `loopProfile` | string | Active agent-loop profile filename. |
| `graphProfile` | string | Active graph profile filename. |
| `configFile` | string | Active agent team YAML filename. |

`chat_history.json` stores this folder's conversations keyed by session id; `repl_history` is plain readline history.

---

## 7. Agent-loop profile YAML (`agent_loops/*.yaml`)

An agent-loop profile shapes the loop itself: which tools/MCP/skills the agent sees, model & prompt overrides, loop knobs, context compaction, and the outer job judge. Every section is **optional** — anything omitted inherits the settings in §5. Select with `/loop <name>` or `--loop <name>`.

```yaml
name: my-profile              # required — shown in /loop listings
description: What this profile is for

model:                        # optional — empty fields inherit main AI settings
  model: claude-opus-5
  api_url: https://api.anthropic.com/v1
  api_key: ""                 # ⚠ never commit a key — leave "" and use .env

system_prompt:
  text: Extra instructions for this profile.
  replace_base: false         # true = replace the built-in base prompt entirely

tools:
  mode: allowlist             # all (default) | allowlist | denylist
  list: [read_file, write_file, run_python]
  config:                     # per-tool overrides, keyed by tool name (built-in or MCP)
    run_shell:
      enabled: true           # false = remove the tool entirely
      require_approval: true  # true = always gate, false = never, absent = global default
      description: ""         # override the description the model sees
      params: {}              # default args injected when the model omits them
      pinned_params: {}       # hard overrides — always win over model-sent values
      max_result_len: 4000    # truncate results to N bytes (UTF-8 safe)
      timeout_secs: 60        # wall-clock cap on execution

mcp:
  mode: selected              # all (default) | selected | none
  servers: [playwright]

skills:
  mode: selected              # all (default) | selected | none
  list: [my-skill]

loop:
  max_rounds: 15              # reasoning rounds per turn
  max_tool_calls: 25          # hard ceiling on tool calls
  temperature: 0.7
  max_tokens: 81920
  reflection_enabled: false
  reflection_threshold: 0.7   # confidence below which reflection triggers
  max_reflection_retries: 2
  checkpoint_enabled: true
  max_consecutive_errors: 3
  max_error_recoveries: 5
  max_spawn_depth: 3          # sub-agent recursion depth (clamped 1–5)
  step_verification: true     # realtime judge verifies each team agent's step

compaction:
  enabled: true               # periodic compaction only; emergency compaction is always on
  interval: 5                 # compact every N rounds
  window: 10                  # messages kept
  max_context_tokens: 100000
  tool_result_max_len: 6000
  model: deepseek-chat        # optional cheaper summarizer (empty = session model)

evaluation:                   # outer loop — tool-using judge runs ONCE after the job
  enabled: true               # top-level main agent only, never sub-agents
  threshold: 0.75             # pass score 0.0–1.0
  max_retries: 2              # judge→fix cycles (clamped 1–5)
  max_fix_rounds: 5           # worker tool rounds per fix cycle (clamped 1–10)
  max_judge_rounds: 3         # judge's own tool rounds (clamped 1–6)
  model: ""                   # dedicated judge model — avoid self-grading
  api_url: ""                 # judge endpoint (empty = session)
  api_key: ""
  rubric: |                   # freeform success criteria for the judge
    The report must contain a chart saved to output/chart.png.
  allow_execute: false        # true also grants run_python/run_shell to the judge

graph:
  enabled: true               # overrides the global graphEnabled for this profile
  profile: strict.yaml        # graph profile in graph/ to use when gated
```

Field-by-field notes:

| Section.key | Type | Default | Notes |
|---|---|---|---|
| `name` | string | — | Required; identifies the profile. |
| `description` | string | `""` | Shown in listings. |
| `model.model` / `.api_url` / `.api_key` | string | `""` | Empty inherits the session's AI settings. |
| `system_prompt.text` | string | `""` | Injected before or instead of the base prompt. |
| `system_prompt.replace_base` | bool | `false` | `true` replaces the built-in base prompt (skills/project prompts still apply). |
| `tools.mode` | string | `all` | `allowlist` = only `list`; `denylist` = everything except `list`. |
| `tools.config.<name>.enabled` | bool | inherit | `false` removes the tool and hard-denies at dispatch (protected coordination tools exempt). |
| `tools.config.<name>.require_approval` | bool | inherit | Main agent gets a y/n prompt; background sub-agents follow `autoApproveSubagentTools`. |
| `tools.config.<name>.params` | map | — | Shallow merge of defaults; the model can still override. |
| `tools.config.<name>.pinned_params` | map | — | Always overwrite model-sent values (top-level keys). |
| `mcp.mode` / `mcp.servers` | string / list | `all` / `[]` | `selected` whitelists servers by name; `none` disables MCP. |
| `skills.mode` / `skills.list` | string / list | `all` / `[]` | Same pattern for skills in the system prompt. |
| `loop.*` | numbers/bools | see §5 | Each key overrides the matching `agent*` setting. |
| `compaction.*` | numbers/bool/string | see §5 | `enabled: false` disables only the periodic cycle. |
| `evaluation.*` | see above | disabled | Judge reads `read_file`/`list_files` to verify claimed artifacts; below-threshold results feed a gap list back for bounded fix rounds. Put the rubric here — it goes into the judge's system prompt. |
| `graph.enabled` / `graph.profile` | bool / string | follow global | Per-profile graph-gate override. |

---

## 8. Agent team YAML (`agents/*.yaml`)

Team configs define a swarm: agents, orchestration mode, workflow and inter-agent protocols. Select with `/agent <file>` or `--agent <file>` (mode switches to `auto`).

```yaml
system:
  name: Example Research Team
  orchestration_mode: hierarchical   # hierarchical | flat | mesh | hybrid | pipeline | p2p
  communication_protocol: structured_handoff
  context_passing: full_chain        # full_chain | limited
  p2p_governance:                    # p2p mode only
    consensus_mechanism: contract_net   # contract_net | voting
    bid_timeout_seconds: 30
    min_confidence_threshold: 0.5
    audit_log: true

agents:
  - id: agent_1                      # unique snake_case id
    name: Researcher
    role: worker                     # human | orchestrator | worker | checker | reporter | researcher | peer
    persona: >-
      You are a meticulous researcher...
    responsibilities:
      - Search for and collect relevant information
    model: ""                        # per-agent model override (empty = session model)
    system_prompt: ""                # appended to the agent's base prompt
    tools:    { mode: all, list: [], config: {} }   # same schema as §7 tools
    mcp_servers: []                  # [] = none, [a, b] = selected, omitted = all
    skills:   { mode: all, list: [] }
    loop:     {}                     # same knobs as §7 loop, per agent
    compaction: {}                   # same knobs as §7 compaction, per agent
    bus:  { enabled: false, topics: [] }   # pub/sub bus subscription
    mesh: { enabled: false }
    p2p:                             # p2p mode only
      confidence_domains: [research]
      reputation_score: 0.8

workflow:
  sequence:
    - step: 1
      agent: agent_1
      action: Gather sources and hand structured notes to the writer
      outputs_to: [agent_2]
      communication:
        enabled: true
        protocols: [tcp]             # tcp | queue | bus | blackboard
        participants: [agent_1, agent_2]
        permitted_topics: [findings]
      peer_socket:
        enabled: false
        protocol: bidirectional_async   # bidirectional_async | unidirectional
        participants: []
        permitted_topics: []

connections:
  - from: agent_1
    to: agent_2
    label: research handoff
    protocol: tcp                    # tcp | queue (point-to-point)
    topics: [findings]

communication:
  format: structured_json_in_yaml_envelope
  context_inheritance:
    mode: cumulative                 # cumulative | selective | none
    max_history_tokens: 8000
```

Key constraints:

- **Roles:** `human`, `orchestrator`, `worker`, `checker`, `reporter`, `researcher`, `peer`. The human entry point (`role: human`, `id: human`) is auto-generated when missing.
- **Orchestration modes:** `hierarchical`, `flat`, `mesh`, `hybrid`, `pipeline`, `p2p`.
- **Protocols:** point-to-point connections use `tcp` or `queue`; `bus` is per-agent pub/sub (`bus.topics`); `blackboard` is shared-state.
- **Per-agent `tools` / `skills` / `loop` / `compaction`** reuse the exact schemas from §7, so you can sandbox one agent tightly while another runs free.
- `mcp_servers` shorthand: omitted = all servers, `[]` = none, `[name, ...]` = only those.
- **Router mode** teams additionally draw per-agent models from the **Router Model Pool** (Settings → Sub-Agent), where each pool entry has its own `model`, `api_url`/`api_key`, tier (`fast`/`balanced`/`deep`) and strengths; a hard `model:` on an agent wins over routing, and provider failover retries the next pool model on rejection.

---

## 9. Graph profile YAML (`graph/*.yaml`) & rules files

A graph profile is an **evaluator-optimizer gate**: a judge panel reviews the final answer before it reaches you; failing verdicts send the worker back for bounded revisions. Select with `/graph <name>` or `--graph <name>` (activates graph mode), or gate every mode globally with `graphEnabled: true`.

```yaml
name: default
description: Judge panel reviews the final answer before delivery.

worker:
  mode: single                # single | auto | manual | fully_auto | auto_swarm | router
  agent_loop_profile: ""      # optional loop profile the worker runs under

judges:                       # at least one judge required
  - name: quality
    model: ""                 # dedicated judge model (empty = session model)
    api_url: ""
    api_key: ""
    rules: ""                 # inline rules text (appended to rules_file content)
    rules_file: default_rules.yaml   # file in graph/rules/
    weight: 1.0               # for weighted_average aggregation (≤0 = ignored)
    threshold: null           # per-judge pass override (null = aggregation.threshold)
    use_tools: true           # judge may read_file / list_files to verify artifacts
    allow_execute: false      # also grant run_python / run_shell (use with care)
    max_judge_rounds: 3       # judge's tool-loop rounds (1–6)

aggregation:
  policy: all_pass            # all_pass | majority | weighted_average
  threshold: 0.75             # pass score 0.0–1.0

loop:
  max_iterations: 2           # judge→revise cycles (clamped 1–5)
  max_fix_rounds: 5           # worker tool rounds per revision (clamped 1–10)
  judge_plain_answers: true   # judge answers even when no tools were called
```

Rules files (`graph/rules/*.yaml`) hold the criteria rendered into the judge's prompt:

```yaml
rules:
  - id: answers-all-parts
    severity: blocker          # blocker = can fail the verdict | warn = noted only
    description: Every distinct part of the user's request is answered.
  - id: no-fabrication
    severity: blocker
    description: Claims must be backed by tool evidence; no invented data or files.
  - id: artifacts-exist
    severity: warn
    description: Files the answer claims to have produced must exist on disk.
```

Behavior notes:

- **Fail-open:** a judge that errors is skipped; if *all* judges error, the answer is released — a misconfigured judge never traps your session.
- `worker.mode` lets the gate **wrap any sub-agent mode** — e.g. an `auto_swarm` team whose merged answer still passes the panel.
- Rules file paths are restricted to `graph/rules/` (traversal blocked).
- Thresholds are validated to 0.0–1.0; iteration knobs are clamped as noted.

---

## 10. Sub-agent modes

| Mode | Behavior |
|---|---|
| `single` | One agent, no sub-agents (default). |
| `auto` | Sub-agent team dispatch from a team YAML (`/agent <file>`). |
| `manual` | You pick agents/skills per task. |
| `fully_auto` | The system designs the team on the fly. |
| `router` | Orchestrator triages, routes sub-tasks to a heterogeneous model pool, merges answers, with provider failover. |
| `graph` | Judge-panel gate wraps the worker (see §9). |

---

*This reference matches TigrimOS v0.7.2. The desktop app, headless server, and CLI share the same engine and file formats — everything here (except `.env`, `cli_state.json`, and the folder overlay) applies to the other run modes too.*
