<div align="center">

# Agent Context

**Show Pi, Claude Code, Codex, and OpenCode session context in the Herdr sidebar, tab bar, and pane borders**

Install one Herdr plugin to read local JSONL sessions and OpenCode SQLite data without changing agent hooks or settings.

</div>

## Install

```sh
herdr plugin install ryonakae/herdr-agent-context --yes
```

The plugin downloads a checksum-verified binary for macOS or Linux. It does not require Cargo, Node.js, or Python at runtime. Restart the Herdr session if the startup listener does not begin immediately.

### Build from source

```sh
git clone https://github.com/ryonakae/herdr-agent-context.git
cd herdr-agent-context
cargo build --release --locked
mkdir -p bin
cp target/release/herdr-agent-context bin/herdr-agent-context
herdr plugin link .
```

## Quickstart

Configure context rows once for all Herdr agents in your `config.toml`:

```toml
[ui.sidebar.agents]
rows = [
  ["state_icon", { token = "agent", bold = true, dim = false }],
  [{ token = "workspace", bold = false, dim = true }, "tab", "pane"],
  ["$agent_context_session_name"],
  ["$agent_context_last_message"],
]
```

Restart Herdr or reload its configuration. Each supported pane shows its agent, location, resolved session name or title, and recent assistant activity.

## Exact session binding

The plugin works without agent integrations. Herdr's official integrations improve attribution when several sessions share a cwd and enable native resume tracking:

```sh
herdr integration install pi
herdr integration install claude
herdr integration install codex
herdr integration install opencode
```

These commands are optional and separate from plugin installation. The plugin never installs integrations or edits agent or Herdr settings.

An official Pi session path or Claude, Codex, or OpenCode session ID takes priority over local fallback. Codex next uses a structured `codex resume <UUID>` hint. OpenCode next uses a structured non-fork `opencode --session <ID>` hint. Both backends then keep a valid in-memory binding or wait for one uniquely new or changed same-cwd session after observing the pane. OpenCode never treats the source ID passed with `--fork` as the forked session. When the official Pi integration is installed, the listener waits for its session path during startup instead of showing a heuristic match. If an authoritative reference is missing or malformed, the listener waits for the exact session instead of showing another transcript. The plugin reports visual metadata only; it never writes inferred paths or IDs back to Herdr.

## Context rules

### Pi

- Name: explicit Pi session name, first user text on the active branch, then cwd basename.
- Activity: latest assistant text in the active turn.
- Local fallback: cwd matching with in-memory sticky bindings.
- `--no-session` disables local binding when it is visible in process metadata.

### Claude Code

- Title: current or legacy custom title, then latest `ai-title`. Until Claude writes one of these records, the session title stays empty; first-user text and cwd are not title fallbacks.
- Activity: latest top-level assistant text block after the latest genuine user entry.
- Filtering: thinking, tool calls and results, fallback blocks, sidechains, metadata, and API error text never become activity.
- Scope: persistent top-level interactive sessions, including continue, resume, named, worktree, and Remote Control sessions that write a local transcript.
- Exclusions: `--print`, `--background`, and `--no-session-persistence` do not use local fallback.

### Codex

- Name: latest nonblank exact-ID `thread_name`, first genuine user message, then effective cwd basename.
- Activity: latest commentary or final assistant text after the latest genuine user message. Reasoning, system and developer records, tool calls and results, task-completion echoes, and nontext content are excluded.
- Scope: persistent root interactive TUI sessions, including normal starts; targetless, UUID, named, and `--last` resume; and fork after the child session becomes observable.
- Exclusions: `exec`, review, remote, ephemeral, subagent, internal, MCP, app-server, and other noninteractive or non-root sources do not bind.
- Retention: after a new user message, the same session keeps its prior activity until Codex writes replacement commentary or final text.

### OpenCode

- Name: stored title unless it is the default `New session - <timestamp>`, then first genuine user text, then cwd basename.
- Activity: latest ordinary assistant text after the latest genuine user input, including text still being streamed. Reasoning, tools and results, errors, synthetic or ignored text, files, patches, step records, and unknown parts are excluded.
- Scope: persistent root TUI sessions started normally or with `--continue`, `--session <ID>`, or `--fork`. A fork binds only after the child session becomes observable unless the official integration reports the child ID.
- Exclusions: `run`, `attach`, `serve`, `web`, ACP, MCP, GitHub automation, child/subagent sessions, and other non-root or non-TUI modes do not bind.
- Retention: after a new genuine user message, the same session keeps its prior activity until OpenCode writes replacement assistant text.

All values use one line and at most 80 Unicode scalars. The activity limit includes any ellipsis. A new Claude, Codex, or OpenCode user entry retains prior activity only for the same session until replacement text appears. Metadata expires after the configured TTL (10 seconds by default) when the listener cannot refresh a bound session.

Claude Code support is best effort for the current 2.1.x JSONL structure and was verified with 2.1.220. Codex support is best effort for the internal rollout structure verified with Codex CLI 0.149.1. OpenCode support is best effort for the 1.x `session`, `message`, and `part` SQLite schema verified with OpenCode 1.18.23. The parsers ignore unknown records only when safe; broken required structure fails closed for that pane.

## Tab labels

Automatic tab labels are disabled by default. Enable them in the plugin's `config.toml`:

```toml
[tab_name]
enabled = true
```

The listener labels each tab from all supported Pi, Claude Code, Codex, and OpenCode panes in it, including background tabs. Components follow visual pane order: top to bottom, then left to right. Pi, Codex, and OpenCode use their resolved session names. Claude uses its verified custom or generated title. Untitled and unsupported panes contribute nothing; a tab with no resolved title keeps its baseline label. Agent status and pane focus do not affect label eligibility.

Each component occupies at most 20 terminal columns and preserves grapheme clusters. The listener joins components with ` + ` and does not apply another limit to the aggregate.

A manual Herdr tab rename overrides the current ordered session composition in that tab. Another composition may use its generated label; returning to the overridden composition restores the manual label. When no supported composition remains, or when you set `enabled = false`, the listener restores the latest manual baseline. If the initially captured label equals the tab's current workspace-local position, the listener treats it as Herdr's positional label and restores the new position after reordering. A numeric label entered later stays exact. Herdr stores a restored position as a custom label because version 0.8 has no API to clear custom naming.

## Pane labels

Automatic pane labels are also disabled by default and configured independently:

```toml
[pane_name]
enabled = true
```

Each supported pane uses its own Pi, Codex, or OpenCode session name or verified Claude title, bounded to 20 terminal columns. A manual pane rename, including clearing the label, overrides only the current session in that pane. Another session may use its generated label; returning restores the manual override. The listener restores the latest manual pane baseline when the session leaves or pane naming is disabled. Pane overrides do not change tab components.

## Configuration

Find the plugin configuration directory:

```sh
herdr plugin config-dir ryonakae.agent-context
```

Create `config.toml` there when you need non-default values:

```toml
poll_interval_ms = 2000
metadata_ttl_ms = 10000

[agents.pi]
session_dirs = ["~/additional/pi/sessions"]

[agents.claude]
session_dirs = ["~/additional/claude/projects"]

[agents.codex]
session_dirs = ["~/additional/codex/sessions"]

[agents.opencode]
database_paths = ["~/additional/opencode/opencode.db"]

[tab_name]
enabled = true

[pane_name]
enabled = true
```

| Option | Type | Default | Description |
|---|---|---:|---|
| `poll_interval_ms` | integer | `2000` | Interval between session reconciliations. |
| `metadata_ttl_ms` | integer | `10000` | Herdr metadata lifetime; must exceed the poll interval. |
| `agents.pi.session_dirs` | string array | `[]` | Additional Pi session roots. |
| `agents.claude.session_dirs` | string array | `[]` | Additional Claude `projects` roots. |
| `agents.codex.session_dirs` | string array | `[]` | Additional active Codex `sessions` roots. |
| `agents.opencode.database_paths` | string array | `[]` | Additional OpenCode SQLite database files. |
| `tab_name.enabled` | boolean | `false` | Synchronize tab labels with the ordered supported sessions in each tab. |
| `pane_name.enabled` | boolean | `false` | Synchronize each pane label with its supported session. |

Pi roots follow `PI_CODING_AGENT_SESSION_DIR`, then `PI_CODING_AGENT_DIR/sessions`, then `~/.pi/agent/sessions`. Claude roots use `$CLAUDE_CONFIG_DIR/projects` when the listener has that variable, then fall back to `~/.claude/projects`. Codex roots use the listener's `$CODEX_HOME/sessions`, then fall back to `~/.codex/sessions`; configured Codex roots are additional active `sessions` directories, with an optional `session_index.jsonl` in each parent directory.

OpenCode uses `$OPENCODE_DB` when set. An absolute value names the database directly; a relative value resolves under `$XDG_DATA_HOME/opencode` when `XDG_DATA_HOME` is absolute, otherwise under `~/.local/share/opencode`. Without `OPENCODE_DB`, the primary database is `opencode.db` in that data directory. Configured `database_paths` are additional database files. The listener normalizes and deduplicates all roots and database paths.

The legacy Pi form remains valid:

```toml
pi_session_dirs = ["~/additional/pi/sessions"]
```

Do not combine `pi_session_dirs` with `[agents.pi]`. Paths must be absolute or start with `~`. A relative path, unknown key or agent table, nonpositive poll interval, TTL no greater than the poll interval, or TTL above `86400000` rejects the complete file. An invalid first load uses defaults; an invalid reload keeps the previous valid settings.

## Privacy and limitations

- The listener reads Herdr process metadata, matching local Pi, Claude, or Codex JSONL files, and configured OpenCode SQLite databases. It sends no telemetry or runtime network requests.
- Logs contain error categories but no titles, prompts, assistant text, process environments, or full process arguments.
- Claude fallback scans direct-child JSONL files only in the project directories relevant to live pane cwd values. It considers compatible files from the last 30 days and stops after 25 candidates. Official IDs, exact UUID arguments, and existing sticky bindings bypass those age and count limits.
- Codex fallback scans non-symlink `rollout-*.jsonl` files only under controlled `sessions/YYYY/MM/DD` directories for live pane cwd values. Ordinary discovery considers compatible files from the last 30 days and stops after 25 candidates. Official IDs, exact UUID resume hints, and existing sticky bindings bypass age and count limits, but still require a valid active rollout, identity, root `cli` source, and matching cwd.
- Codex reads active rollout JSONL and an optional adjacent `session_index.jsonl`. Archived sessions, compressed rollouts, SQLite-selected heads, and files outside active session roots are unsupported.
- OpenCode opens SQLite databases read-only and reads the 1.x `session`, `message`, and `part` tables. Ordinary fallback considers at most 25 compatible, unarchived root sessions updated within 30 days. It inspects no more than 250 recent root rows per database and fails closed on overflow or when the bounded SQLite query budget expires. Official IDs, non-fork `--session` IDs, and sticky bindings bypass those ordinary discovery limits but still require one unique root identity and matching canonical cwd across every configured database.
- OpenCode development/preview `session_message` storage, databases outside the resolved path list, and child sessions are unsupported. A busy, unreadable, malformed, or incompatible database prevents hook-free attribution for that reconciliation and does not refresh metadata TTL.
- Multiple Claude panes in the same project stay empty on a hook-free cold start unless each pane has official or exact UUID evidence. Hook-free Codex and OpenCode fallback stays empty when more than one matching agent pane shares the canonical cwd or when multiple compatible sessions are new or changed after pane observation; official IDs and exact local hints remain eligible.
- Inferred bindings live in memory and provide visual metadata only. A listener restart resolves them again from current evidence.
- Tab and pane ownership state live under `HERDR_PLUGIN_STATE_DIR/tab-name` and `HERDR_PLUGIN_STATE_DIR/pane-name` with owner-only permissions. They store manual baselines and overrides as plaintext. Session identities, generated labels, terminal/binding generations, and socket identity use SHA-256 digests.
- A malformed or unsupported ownership file disables only that label synchronizer. Sidebar metadata and the other synchronizer continue.
- Setting either naming option to `false` restores its owned labels while the listener is running. Force-stopping the listener, disabling or uninstalling the plugin, or deleting its state can leave the last custom tab or pane label in Herdr.
- Herdr startup hooks do not supervise daemons. If the listener exits, metadata expires and a Herdr restart starts it again.

## License

MIT
