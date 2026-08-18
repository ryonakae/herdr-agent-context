<div align="center">

# Agent Context

**Show Pi session names and recent activity in the Herdr sidebar**

Install one Herdr plugin to resolve Pi JSONL sessions without changing Pi hooks or settings.

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

Add Pi-specific rows to your Herdr `config.toml`:

```toml
[ui.sidebar.agents.rows_by_agent]
pi = [
  ["state_icon", "workspace", "tab"],
  ["$agent_context_session_name"],
  ["$agent_context_last_message"],
  ["agent"],
]
```

Restart Herdr or reload its configuration. Pi panes then show the explicit Pi session name, or the first user message/cwd fallback, followed by the latest assistant text in ASCII double quotes.

## How context is resolved

- **Authoritative sessions:** An existing Herdr `agent_session` path always wins. The plugin never writes an inferred path back to Herdr.
- **Hook-free fallback:** Without an authoritative path, the listener matches Pi session files by cwd and keeps pane bindings sticky.
- **Branch-aware text:** Only the active Pi JSONL branch supplies the first user message and latest assistant activity.
- **Bounded metadata:** Values are one line and at most 80 Unicode characters, including the activity quotes. Metadata expires after 10 seconds if the listener cannot refresh it.

## Configuration

Find the plugin configuration directory:

```sh
herdr plugin config-dir ryonakae.agent-context
```

Create `config.toml` there when you need non-default values:

```toml
poll_interval_ms = 2000
metadata_ttl_ms = 10000
pi_session_dirs = ["~/additional/pi/sessions"]
```

| Option | Type | Default | Description |
|---|---|---:|---|
| `poll_interval_ms` | integer | `2000` | Interval between session reconciliations. |
| `metadata_ttl_ms` | integer | `10000` | Herdr metadata lifetime; must exceed the poll interval. |
| `pi_session_dirs` | string array | `[]` | Additional Pi session roots. |

Pi session roots follow `PI_CODING_AGENT_SESSION_DIR`, then `PI_CODING_AGENT_DIR/sessions`, then `~/.pi/agent/sessions`. Additional configured roots are merged and deduplicated. Root paths must be absolute or start with `~`; a relative configured root rejects the file, while a relative environment root falls back to the next source. The poll interval must be positive; TTL must exceed it and cannot exceed `86400000`. Unknown keys reject the entire file. An invalid first load uses timing defaults without custom roots, while an invalid reload retains the previous valid settings.

## Privacy and limitations

- **Local processing:** The listener reads Herdr process metadata and matching Pi JSONL files. It sends no telemetry or runtime network requests.
- **Safe logs:** Logs may identify panes, paths, and error categories, but never session names or conversation text.
- **Same-cwd panes:** Multiple Pi panes in one cwd use sticky mtime-based matching and can remain ambiguous without an authoritative session path.
- **Ephemeral sessions:** Visible `--no-session` arguments prevent binding. A wrapper that hides process arguments can make a historical fallback file appear current.
- **Unsupervised listener:** Herdr startup hooks do not supervise daemons. If the listener exits, metadata expires and a Herdr restart starts it again.

## License

MIT
