# Repository guide

`herdr-agent-context` is a Rust Herdr plugin that reads local Pi, Claude Code, and Codex JSONL sessions and reports privacy-bounded sidebar metadata.

## Common commands

```sh
cargo test --all-targets --locked
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --release --locked
sh tests/installer.sh
sh tests/release-assets.sh
actionlint .github/workflows/*.yml
shellcheck scripts/*.sh tests/*.sh
```

Run the focused Rust test first while developing, then run the full validation set above before committing. Use `git diff --check` as the final whitespace check.

## Structure

- `src/backend.rs`: shared static backend contracts and Pi/Claude/Codex registry.
- `src/pi/`: Pi v3 JSONL parsing, discovery, and sticky binding.
- `src/claude/`: branch-aware Claude JSONL parsing, bounded discovery, CLI eligibility, and conservative binding.
- `src/codex/`: Codex rollout/index parsing, bounded discovery, CLI eligibility, and conservative binding.
- `src/herdr/`: protocol 19 values and Unix socket transport.
- `src/runtime.rs`: reconciliation, TTL refresh/clear behavior, and runtime caches.
- `src/tab_name/`: durable tab-label ownership, manual overrides, and crash recovery.
- `src/main.rs`: listener lifecycle, polling deadline, reconnect backoff, and socket-scoped lock.
- `scripts/`: binary installer and release-contract checks.
- `tests/listener.rs`: fake socket, runtime, reconnect, and duplicate-listener integration tests.
- `docs/release-checklist.md`: manual Herdr smoke and release promotion gates.

Read `README.md` for the public installation/configuration contract and `docs/release-checklist.md` for release-specific gates.

## Implementation constraints

- Keep Pi, Claude, and Codex parsing/resolution independent from Herdr transport. Backends are compiled into the static registry; do not add a dynamic ABI or external backend scripts.
- Herdr 0.8 raw RPC uses one socket connection per request. Keep the long-lived event subscription separate, subscribe before `agent.list`, and preserve events received before acknowledgement.
- `pane.report_metadata` emits `pane_updated`. Do not let that event create a reporting loop or postpone the absolute polling deadline.
- A failed session read/parse may retain in-memory display state, but it must not refresh metadata TTL. Retry transient metadata clears.
- Never report inferred paths through `pane.report_agent_session` or another canonical identity API.
- Logs may contain pane IDs, paths, and error categories. Never log transcript text, process environments, or full process arguments.

## Testing and release rules

- Use TDD for behavior changes. Add synthetic fixtures only; never commit real Pi/Claude/Codex conversations or user session paths.
- Keep socket tests on temporary Unix sockets. Do not require a running user Herdr session in automated tests.
- Shell scripts are POSIX `sh`; keep `shellcheck` and negative installer/archive tests passing.
- Keep package/plugin versions synchronized and the non-publishing four-target matrix green. Follow `docs/release-checklist.md` for packaging changes.
- Do not create tags or publish releases unless explicitly requested. Do not commit generated `bin/`, `target/`, or `dist/` content.

## Documentation

Update `README.md` only when installation, sidebar rows, configuration, privacy, or user-visible limitations change. Update `docs/release-checklist.md` when validation or release packaging changes. Keep public prose and code comments in English.
