<div align="center">

# Agent Context

**Show Pi and Claude Code session context in the Herdr sidebar and tab bar**

Install one Herdr plugin to read local JSONL sessions without changing agent hooks or settings.

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

Restart Herdr or reload its configuration. Each supported pane shows its agent, location, Pi session name or Claude session title, and recent assistant activity.

## Exact session binding

The plugin works without agent integrations. Herdr's official integrations improve attribution when several sessions share a cwd and enable native resume tracking:

```sh
herdr integration install pi
herdr integration install claude
```

These commands are optional and separate from plugin installation. The plugin never installs integrations or edits Pi, Claude, or Herdr settings.

An official Pi session path or Claude session ID takes priority over local fallback. When the official Pi integration is installed, the listener waits for its session path during startup instead of showing a heuristic match. If an authoritative reference is missing or malformed, the listener waits for the exact session instead of showing another transcript. The plugin reports visual metadata only; it never writes inferred paths or IDs back to Herdr.

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

Both values use one line and at most 80 Unicode scalars. The activity limit includes any ellipsis. A new user entry retains the prior activity for the same session until Claude writes replacement text. Metadata expires after the configured TTL (10 seconds by default) when the listener cannot refresh a bound transcript.

Claude Code support is best effort for the current 2.1.x JSONL structure and was verified with 2.1.220. Unknown records are ignored when safe; broken required structure fails closed for that pane.

## Tab labels

Automatic tab labels are disabled by default. Enable them in the plugin's `config.toml`:

```toml
[tab_name]
enabled = true
```

The listener labels each tab from the supported Pi or Claude Code pane that has focus inside it, including background tabs. Pi uses its resolved session name. Claude uses its verified custom or generated title. Until Claude writes one of those title records, the tab keeps its baseline label. Agent status does not affect label eligibility.

Generated labels occupy at most 20 terminal columns and preserve grapheme clusters. Focus changes wait 150 milliseconds so rapid switches apply only the final pane. Focusing a shell or unsupported pane keeps the last selected supported session while that session remains in the tab.

A manual Herdr tab rename overrides the selected session in that tab. Another session may use its generated label; returning to the overridden session restores the manual label. When the selected session leaves, or when you set `enabled = false`, the listener restores the latest manual baseline. If the initially captured label equals the tab's current workspace-local position, the listener treats it as Herdr's positional label and restores the new position after reordering. A numeric label entered later stays exact. Herdr stores a restored position as a custom label because version 0.8 has no API to clear custom naming.

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

[tab_name]
enabled = true
```

| Option | Type | Default | Description |
|---|---|---:|---|
| `poll_interval_ms` | integer | `2000` | Interval between session reconciliations. |
| `metadata_ttl_ms` | integer | `10000` | Herdr metadata lifetime; must exceed the poll interval. |
| `agents.pi.session_dirs` | string array | `[]` | Additional Pi session roots. |
| `agents.claude.session_dirs` | string array | `[]` | Additional Claude `projects` roots. |
| `tab_name.enabled` | boolean | `false` | Synchronize tab labels with the focused supported session. |

Pi roots follow `PI_CODING_AGENT_SESSION_DIR`, then `PI_CODING_AGENT_DIR/sessions`, then `~/.pi/agent/sessions`. Claude roots use `$CLAUDE_CONFIG_DIR/projects` when the listener has that variable, then fall back to `~/.claude/projects`. Configured roots are merged and deduplicated.

The legacy Pi form remains valid:

```toml
pi_session_dirs = ["~/additional/pi/sessions"]
```

Do not combine `pi_session_dirs` with `[agents.pi]`. Paths must be absolute or start with `~`. A relative path, unknown key or agent table, nonpositive poll interval, TTL no greater than the poll interval, or TTL above `86400000` rejects the complete file. An invalid first load uses defaults; an invalid reload keeps the previous valid settings.

## Privacy and limitations

- The listener reads Herdr process metadata and matching local Pi or Claude JSONL files. It sends no telemetry or runtime network requests.
- Logs contain error categories but no titles, prompts, assistant text, process environments, or full process arguments.
- Claude fallback scans direct-child JSONL files only in the project directories relevant to live pane cwd values. It considers compatible files from the last 30 days and stops after 25 candidates. Official IDs, exact UUID arguments, and existing sticky bindings bypass those age and count limits.
- Multiple Claude panes in the same project stay empty on a hook-free cold start unless each pane has official or exact UUID evidence. Existing sticky bindings remain stable.
- Inferred bindings live in memory. A listener restart resolves them again from current evidence.
- Tab ownership state lives under `HERDR_PLUGIN_STATE_DIR/tab-name` with owner-only permissions. It stores manual baselines and overrides as plaintext. Session identities, generated labels, and socket identity use SHA-256 digests.
- A malformed or unsupported ownership file disables tab synchronization for that listener. Sidebar metadata continues.
- Setting `tab_name.enabled = false` restores owned labels while the listener is running. Force-stopping the listener, disabling or uninstalling the plugin, or deleting its state can leave the last custom tab label in Herdr.
- Herdr startup hooks do not supervise daemons. If the listener exits, metadata expires and a Herdr restart starts it again.

## License

MIT
