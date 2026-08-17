# Pi Agent Context Plugin Implementation Plan

> **For implementers:** Execute tasks in order unless dependencies allow otherwise. Mark a task complete only after its validation succeeds. Reflect minor implementation differences in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Problem Statement

Herdr detects running coding agents and exposes pane, agent, cwd, status, and optional native session references, but its sidebar does not resolve or display a Pi session's human-readable name or latest assistant activity by default. The official Pi integration can report an exact session reference, but requiring it would modify each agent's configuration. The plugin must instead work without installing Pi hooks or extensions, use Herdr's existing agent detection, resolve Pi's native JSONL store externally, and publish read-only sidebar metadata.

## Goal

Deliver a release-ready `v0.1.0` implementation of the `ryonakae.agent-context` Herdr plugin that:

- runs as a Rust native listener on macOS and Linux;
- detects Pi agents through Herdr's socket API;
- prefers an existing authoritative `agent_session` and otherwise binds panes to Pi session JSONL files with ZAM-compatible sticky/mtime behavior;
- publishes namespaced session-name and latest-assistant metadata with bounded staleness;
- requires no Pi integration, hook, extension, or automatic Herdr configuration edit;
- is testable through pure parser/resolver tests, a fake Herdr socket, installer tests, and a documented manual sidebar smoke test; and
- can be distributed as checksum-verified prebuilt binaries for four macOS/Linux targets.

The implementation is complete when all Final Validation items pass and the plan is moved unchanged to `docs/plans/archived/`.

## Out of Scope

- Agents other than Pi, including Claude Code, Codex, OpenCode, and Gemini CLI.
- Extracting or publishing a shared resolver crate with ZAM; reconsider this when adding a second agent.
- Installing or modifying Pi hooks, extensions, integrations, settings, or session files.
- Writing inferred bindings back through `pane.report_agent_session` or affecting Herdr native restore.
- Automatically modifying `~/.config/herdr/config.toml` or the user's sidebar rows.
- A custom plugin pane or standalone sidebar UI.
- Native Windows/named-pipe support.
- Reliable disambiguation of a session switch when multiple Pi panes share one cwd and no authoritative `agent_session` exists.
- A supervised daemon or automatic crash restart beyond reconnecting the listener's socket connection.
- Creating a git tag, pushing commits, or publishing a GitHub Release.
- Runtime telemetry, network requests, or logging conversation text.

## Requirements and Decisions

### Requirements

- **R1 — Hook-free discovery:** The plugin must work without installing Pi or Herdr agent integrations. It may read an already-present Herdr `agent_session`, but installation must not create one.
- **R2 — Pi-only pane selection:** Only Herdr agents whose normalized agent label is `pi` are eligible. A pane that closes, is released, or changes to another agent must lose plugin-owned metadata.
- **R3 — Session-name semantics:** `agent_context_session_name` must resolve in order from the latest Pi `session_info.name`, the first user text on the inferred active branch, and the session header cwd basename. Empty values are skipped.
- **R4 — Activity semantics:** `agent_context_last_message` must match ZAM's Pi activity behavior: the first non-empty line of the latest assistant text produced after the latest user message on the active branch. Thinking, tool calls, tool results, custom messages, compaction summaries, and branch summaries are excluded. Until a replacement assistant text is persisted, the prior valid activity remains displayed.
- **R5 — Bounded display values:** Both metadata values must be reduced to one non-empty line and truncated to at most 80 Unicode scalar values, adding an ellipsis when truncated. Missing values are omitted or cleared rather than replaced with invented placeholders.
- **R6 — Tree-aware parsing:** Pi v3 JSONL must be parsed as an `id`/`parentId` tree. The latest persisted tree entry is the inferred leaf, and only its ancestor chain contributes user/assistant fallback content. A branch move that writes no entry is inherently unobservable until Pi appends another entry.
- **R7 — Binding source priority:** An existing Pi `agent_session` with `kind = "path"` is authoritative. While that reference exists, an unreadable, missing, or temporarily malformed target must preserve any last valid value without TTL refresh and must not fall back to another heuristic candidate. Only a pane with no authoritative path reference uses fallback: group panes and candidate session files by canonical cwd, preserve valid prior bindings, then assign remaining candidates in descending mtime order deterministically.
- **R8 — Session switches:** When one Pi pane is active for a cwd, compare candidate `(size, mtime)` fingerprints between successful polls. Rebind only when the current bound file is unchanged and exactly one other compatible candidate changed or appeared with an mtime newer than the bound file; after rebinding, baseline every candidate before evaluating another switch. This follows observable `/new` and `/resume` writes without oscillating on unchanged history. With multiple Pi panes in the cwd, retain sticky bindings unless Herdr supplies an authoritative session reference.
- **R9 — Ephemeral sessions:** If foreground process information exposes `--no-session`, do not bind the pane. If wrappers hide the argument, the limitation must be documented; no inferred result may be promoted to canonical Herdr session identity.
- **R10 — Session roots:** Resolve roots from `PI_CODING_AGENT_SESSION_DIR`, then `PI_CODING_AGENT_DIR/sessions`, then `~/.pi/agent/sessions`; merge additional roots from plugin config `pi_session_dirs`. Paths are expanded/canonicalized where possible and deduplicated without scanning unrelated filesystem roots.
- **R11 — Runtime lifecycle:** `[[startup]]` starts one long-running listener per Herdr server/socket. On startup/reconnect it first obtains an acknowledged subscription to relevant pane/agent lifecycle events, then performs a full `agent.list` reconciliation while buffering or subsequently reconciling events received during the sync. This prevents a list-before-subscribe event-loss race. It then polls bound session candidates and reconnects with bounded backoff after socket interruption.
- **R12 — Metadata lifecycle:** Publish metadata with source `ryonakae.agent-context`, token names `agent_context_session_name` and `agent_context_last_message`, and a default TTL of 10,000 ms. Poll every 2,000 ms by default. Explicitly clear tokens on pane close/release, agent change, or binding loss.
- **R13 — Failure degradation:** A transient file read/parse failure preserves the last valid in-memory value but does not refresh its TTL. Persistent failure therefore removes stale sidebar content within the configured TTL. Malformed socket events are skipped; socket closure causes reconnect rather than process exit.
- **R14 — Config contract:** Read `${HERDR_PLUGIN_CONFIG_DIR}/config.toml` with optional keys `poll_interval_ms`, `metadata_ttl_ms`, and `pi_session_dirs`. Missing files/keys use defaults. Values must be positive, TTL must remain greater than the polling interval and within Herdr's 86,400,000 ms API limit, and invalid reloads must retain the last valid configuration while logging a content-free warning.
- **R15 — Privacy:** At runtime, read only Herdr socket data, process metadata needed for `--no-session`, plugin config, and matching Pi session files. Do not send runtime network requests. Logs may include pane IDs, paths, source selection, and error categories, but not session names or conversation text.
- **R16 — Sidebar ownership:** The plugin reports metadata only. README installation instructions must show the manual `ui.sidebar.agents.rows` entries; the plugin must not edit user configuration.
- **R17 — Platform/distribution:** Support `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu`, and `x86_64-unknown-linux-gnu`. Managed installation downloads the matching `v0.1.0` archive and verifies it against `SHA256SUMS` before installing `bin/herdr-agent-context`.
- **R18 — Release readiness:** Keep Cargo package version, plugin manifest version, asset names, and release tag convention synchronized. CI must validate Rust quality, installer behavior, release archive shape, and four-target builds. This plan does not publish the release.
- **R19 — Verification:** Use TDD. Automated tests cover parser, binding, config, socket protocol, reconnect/TTL/clear behavior, and installer integrity. Actual Herdr sidebar rendering remains a release-checklist manual smoke test.

### Implementation Decisions

- **D1 — Rust with deferred sharing:** Implement the Pi adapter locally in Rust. Preserve separation between Pi parsing/resolution and Herdr transport, but do not introduce a generic multi-agent trait or shared ZAM crate in v0.1.0.
- **D2 — Raw socket API:** Use Herdr protocol 19 methods `agent.list`, `events.subscribe`, `pane.process_info`, and `pane.report_metadata` directly over `HERDR_SOCKET_PATH`. Deserialize defensively and ignore unknown response/event fields.
- **D3 — Long-running startup:** Use `[[startup]] command = ["bin/herdr-agent-context", "listen"]`. The process is not described as supervised; a socket-scoped lock prevents duplicates caused by repeated startup/handoff invocation.
- **D4 — Incremental work:** Cache file identity/size/mtime and parsed values. Reparse only changed bound/candidate files. The implementation may read a changed file in full for correctness initially, but must not rescan every historical session file's contents each polling tick.
- **D5 — Read-only canonical identity:** `agent_session` is input only. All output uses `pane.report_metadata` with TTL and nullable token values for clearing.
- **D6 — ZAM-compatible fallback with targeted fixes:** Preserve sticky/mtime behavior and prior activity retention, while adding active-branch parsing and safe single-pane session-switch handling as improvements suitable for later upstreaming to ZAM.
- **D7 — Thread-to-tab distribution pattern:** Follow `toyamarinyon/herdr-thread-to-tab`'s prebuilt installer/checksum/release structure, adapted to this repository and binary name.
- **D8 — Stable public names:** Repository and binary are `herdr-agent-context`; plugin ID and metadata source are `ryonakae.agent-context`; display name is `Agent Context`; custom tokens are namespaced as specified in R12.
- **D9 — Manual Herdr config:** Documentation gives a copyable sidebar example, but installation and startup perform no config mutation.

### Contracts

#### Plugin config

Path: `${HERDR_PLUGIN_CONFIG_DIR}/config.toml`

```toml
poll_interval_ms = 2000
metadata_ttl_ms = 10000
pi_session_dirs = ["/absolute/or/tilde/path/to/sessions"]
```

All fields are optional. Unknown fields should be rejected with a clear warning so misspellings do not silently change behavior. An invalid changed file leaves the prior valid configuration active. On first startup with an invalid file, use safe defaults for timing and only environment/default session roots; do not accept partially parsed custom roots.

#### Metadata report

```text
method: pane.report_metadata
source: ryonakae.agent-context
agent: pi
applies_to_source: optional source of the currently observed Pi ownership
TTL default: 10000 ms
tokens:
  agent_context_session_name: string | null
  agent_context_last_message: string | null
```

A report never includes title, display-agent, state-label, or canonical session fields. `null` clears a token owned by this source. Sequence numbers must increase per pane/source so delayed reports cannot restore stale values.

#### Binding identity

A live pane is identified by Herdr `pane_id` plus `terminal_id`. Cached in-memory bindings are valid only while both identifiers and normalized agent kind remain unchanged. Bindings are not required to persist across listener/server restarts.

#### Pi parsed view

```text
PiSessionView
- path
- session_id
- cwd
- explicit_name?
- first_user_line?
- latest_turn_assistant_line?
- modified_at
```

The parsed view contains only normalized display fields needed by the resolver; full transcript content must not enter logs or persisted plugin state.

#### Listener state transitions

```text
startup/reconnect -> subscribe acknowledged -> full agent.list reconciliation
                   -> process buffered events/final reconcile -> report/clear
pane/agent event  -> targeted/full reconciliation -> report/clear
poll tick         -> config reload check -> changed-file resolve -> refresh valid TTL
parse failure     -> retain value in memory, do not report/refresh TTL
socket closure    -> reconnect with bounded backoff -> full reconciliation
pane release      -> clear tokens -> remove binding/cache for pane
```

## Current Context

### Confirmed

- The repository currently contains only `README.md` and an MIT `LICENSE`; there is no existing Rust or plugin structure to preserve.
- The installed Herdr version is `0.8.0`, protocol `19`, and supports external plugins, `agent.list`, `events.subscribe`, `pane.process_info`, `pane.report_metadata`, TTL metadata, and namespaced custom token values.
- Herdr plugin startup hooks run asynchronously once after session restoration and socket readiness, but are not supervised daemons.
- Herdr `AgentInfo` exposes agent, status, cwd/foreground_cwd, pane/tab/workspace/terminal IDs, title, and optional `agent_session { source, agent, kind, value }`.
- `pane.report_metadata` allows at most 16 tokens per report; token names are 1–32 characters matching `[A-Za-z0-9_-]`; TTL is 1–86,400,000 ms. Both selected token names satisfy the name limit.
- Pi 0.83.0 stores v3 tree-structured JSONL under a cwd-encoded session root, supports `session_info.name`, and can override its config/session directories with `PI_CODING_AGENT_DIR` and `PI_CODING_AGENT_SESSION_DIR`.
- ZAM's Pi parser uses the first user message as task fallback, the latest assistant text after the latest user as activity, and sticky/mtime pane binding. It retains prior activity when a new user message has no assistant response yet.
- `toyamarinyon/herdr-thread-to-tab` demonstrates a Rust long-running `[[startup]]` listener and checksum-verified prebuilt assets for the same four targets.

### Assumptions

- Internal module boundaries may shift during TDD if responsibilities remain separated and the public contracts above do not change.
- A full read of a changed bound JSONL file is acceptable for the first correct implementation; incremental byte parsing is an optimization only if tests show it preserves tree/name/activity semantics.
- Standard Rust stable and edition 2021 or newer may be selected during implementation; this does not change the plugin's public behavior.

## File Structure

- Create: `Cargo.toml` — Rust package metadata and runtime/test dependencies.
- Create: `Cargo.lock` — reproducible dependency lockfile used by CI/release builds.
- Create: `src/main.rs` — `listen` entrypoint, environment validation, process exit behavior.
- Create: `src/config.rs` — defaults, TOML schema, validation, environment/root resolution, reload behavior.
- Create: `src/herdr/mod.rs` — Herdr-facing domain types and transport interface used by the runtime.
- Create: `src/herdr/protocol.rs` — protocol 19 request/response/event envelopes and defensive decoding.
- Create: `src/herdr/socket.rs` — Unix socket request/response, subscription, reconnect boundary, and metadata reports.
- Create: `src/pi/mod.rs` — Pi-specific module boundary without a speculative generic adapter API.
- Create: `src/pi/session.rs` — v3 JSONL tree parsing and bounded display extraction.
- Create: `src/pi/resolver.rs` — root scanning, cwd grouping, source priority, sticky binding, and single-pane session switching.
- Create: `src/runtime.rs` — initial sync, event reconciliation, polling, TTL refresh/clear, config reload, and in-memory state.
- Create: `src/text.rs` — shared one-line/80-character normalization used by both public tokens.
- Create: `tests/fixtures/pi/*.jsonl` — synthetic Pi sessions for names, branches, messages, malformed tails, and switches; no copied private conversations.
- Create: `tests/listener.rs` — fake Unix socket integration tests for protocol, lifecycle, reconnect, TTL, and privacy-safe errors.
- Create: `tests/installer.sh` — target selection, unsupported platform, checksum rejection, and successful local-fixture install.
- Create: `tests/release-assets.sh` — expected four archives, archive contents, exact checksum set, and corruption rejection.
- Create: `scripts/install-binary.sh` — release asset selection, download, checksum verification, extraction, and executable installation.
- Create: `scripts/verify-release-assets.sh` — release archive/checksum contract validation.
- Create: `scripts/verify-glibc-baseline.sh` — GNU/Linux compatibility check used by release CI.
- Create: `herdr-plugin.toml` — plugin identity, platform constraints, build installer, and startup listener.
- Modify: `README.md` — product description, installation, manual sidebar config, plugin config, behavior, privacy, limitations, troubleshooting, and local development.
- Create: `.github/workflows/ci.yml` — formatting, linting, tests, installer/release-contract tests, and a non-publishing four-target build matrix runnable on pull requests, branch pushes, or `workflow_dispatch`.
- Create: `.github/workflows/release.yml` — tag-gated version verification, four-target builds, archive/checksum generation, validation, and prerelease publication definition; not invoked by this implementation task.
- Create: `docs/release-checklist.md` — manual Herdr smoke scenarios and release promotion checks.
- Modify: `docs/plans/2026-08-18-pi-agent-context-plugin.md` — progress and any minor implementation-file differences; archive only after complete validation.

## Testing Decisions

- **Test seam:** Keep Pi parsing, display normalization, config validation, and binding as pure Rust functions. Put socket I/O behind a transport boundary and test the listener against a temporary Unix socket speaking newline-delimited Herdr envelopes.
- **Behavior:** Fixtures cover explicit and fallback names, active versus abandoned branches, assistant text filtering, Unicode truncation, malformed/incomplete final lines, authoritative and inferred bindings, same-cwd collisions, single-pane `/new`/`/resume`, TTL expiration, explicit clears, malformed events, and reconnect/full-sync behavior.
- **Prior art:** Adapt ZAM's Pi parser/binding test cases and `herdr-thread-to-tab`'s listener, installer, release-asset, and four-target workflow patterns. Do not copy real session content or unrelated agent/UI logic.
- **Avoid:** Tests must not assert private internal struct layouts, wall-clock sleeps longer than a small bounded fake-clock/test interval, real user session paths/content, global Herdr plugin registry mutations, or network access.
- **Manual seam:** Use an isolated/local linked plugin with the installed Herdr binary for sidebar rendering because agent detection and TUI layout are not stable CI boundaries.

## Progress

- [x] Task 1: Establish the Rust package, config contract, and tree-aware Pi parser.
- [x] Task 2: Resolve Pi session roots and implement authoritative/sticky pane binding.
- [ ] Task 3: Implement the Herdr protocol 19 socket client and metadata contract.
- [ ] Task 4: Deliver the long-running listener with reconciliation, polling, TTL, and failure recovery.
- [ ] Task 5: Package the plugin and document installation, configuration, privacy, and manual validation.
- [ ] Task 6: Add release-grade installer, CI, four-target artifacts, and release checks.

Implementation-discovered minor file changes or internal differences must be recorded in the relevant task. Changing requirements, Out of Scope, or public contracts requires user confirmation before editing the plan or implementation.

## Tasks

### Task 1: Rust Foundation, Config, and Pi Session Parsing

**Covers:** R3–R6, R10, R14, R15, D1, D4, D6, D8

**Objective:** Create a testable Rust package that loads the public config contract and converts Pi v3 JSONL into privacy-bounded session name/activity values without reading abandoned branches.

**Files:**
- Create: `Cargo.toml`
- Create: `Cargo.lock`
- Create: `src/main.rs`
- Create: `src/config.rs`
- Create: `src/pi/mod.rs`
- Create: `src/pi/session.rs`
- Create: `src/text.rs`
- Create: `tests/fixtures/pi/*.jsonl`

**Dependencies:** None.

**Implementation notes:**
- Begin with failing tests for the agreed config and parser outputs, then add implementation.
- Parse JSONL entries defensively by `type`, retaining IDs/parents only as needed to reconstruct the inferred active branch.
- Resolve the latest `session_info.name` according to Pi's session metadata semantics; derive user/assistant fallbacks from the inferred active branch.
- Accept user content as either a string or text blocks; accept assistant text blocks only. Ignore image-only messages and all non-text content types.
- Preserve the previous activity outside the parser/runtime merge layer when the new parsed session has no assistant text after its latest user message.
- Treat a trailing incomplete line as transient rather than converting partial content into output. A structurally invalid completed entry produces a parse failure for TTL handling.
- Use synthetic fixtures and generic paths; no private transcript text enters the repository.
- Validate `poll_interval_ms > 0`, `metadata_ttl_ms > poll_interval_ms`, and `metadata_ttl_ms <= 86_400_000`.
- Expand `~` only at the start of configured paths. Canonicalization failure for a not-yet-created path must not panic; preserve a normalized absolute candidate or reject it with a content-free warning.

**Test cases:**
- Explicit `session_info.name` plus user fallback → explicit name wins.
- No explicit name → first active-branch user line wins; no user text → cwd basename wins.
- Two branches where the physically later abandoned branch contains different text → inferred active branch alone supplies the fallback/activity.
- Assistant text mixed with thinking/tool calls, plus tool-result, custom-message, compaction, and branch-summary entries → only assistant text blocks after the latest user contribute; latest non-empty first line is returned.
- New user entry with no following assistant → parsed activity is absent so runtime can retain the prior value.
- Empty/multiline/over-80-character Unicode text → first non-empty line, scalar-safe truncation, and ellipsis contract hold.
- Incomplete trailing JSONL line → transient parse error with no invented value.
- Missing config → defaults; valid partial config → merged defaults; invalid timing/unknown key → rejected without partial activation.
- Root precedence and deduplication → `PI_CODING_AGENT_SESSION_DIR`, `PI_CODING_AGENT_DIR/sessions`, the default `~/.pi/agent/sessions`, and configured additional roots are each covered; the explicit session-dir env selects the primary root while configured additions remain included and canonical duplicates collapse.

**Complete when:**
- The crate builds and all parser/config tests pass.
- Public config defaults and validation match R14.
- No generic multi-agent trait or ZAM-specific UI/process layer is introduced.
- Test fixtures contain no private conversation data.

**Validation:**
- Run: `cargo test --lib`
- Expected: All library-level config, text, and Pi session parser tests pass with no ignored failures.
- Run: `cargo fmt --check`
- Expected: Exit 0 with no formatting diff.

### Task 2: Pi Root Scanning and Pane-to-Session Resolution

**Covers:** R1, R2, R7–R10, R15, D1, D4–D6

**Objective:** Resolve each eligible Pi pane to an authoritative or inferred session path while preserving ZAM-compatible behavior and safe single-pane switching.

**Files:**
- Create: `src/pi/resolver.rs`
- Modify: `src/pi/mod.rs`
- Modify: `src/pi/session.rs`
- Modify: `src/config.rs`
- Test: module tests in `src/pi/resolver.rs` and fixtures under `tests/fixtures/pi/`

**Dependencies:** Task 1.

**Implementation notes:**
- Define a small Herdr-agnostic pane input containing pane/terminal IDs, agent kind, cwd candidates, optional authoritative session reference, status/revision context, and observable process arguments.
- Normalize agent kind before selecting `pi`; reject released/non-Pi panes before scanning.
- Accept authoritative session references only when agent is Pi and kind is `path`. While that reference exists, a malformed, missing, or unreadable target blocks heuristic fallback: retain the last valid in-memory value without refreshing TTL, or emit no value if none was previously valid. Resume fallback only after Herdr removes the authoritative path reference.
- Use session header cwd as the authoritative cwd for a file; directory encoding is an index optimization/fallback, not the final truth.
- Scan directory entries/metadata for relevant cwd roots without reading all file contents each tick. Read candidate headers/content only when needed to validate or display a binding.
- Preserve a binding only while pane ID, terminal ID, agent kind, cwd compatibility, and candidate existence remain valid.
- For unbound panes sharing a cwd, sort candidates by descending mtime then stable path and sort panes by a stable Herdr identity before one-to-one assignment.
- For exactly one pane in a cwd, keep a `(size, mtime)` baseline from the prior successful scan. Rebind only when the bound file fingerprint is unchanged and exactly one other compatible file changed or appeared with an mtime newer than the bound file. Consume the change by replacing the binding and baselining all candidates before another switch decision. If the bound file changed, no alternative changed, or multiple alternatives changed, retain the current binding.
- For multiple panes, do not reassign valid sticky bindings based solely on a newer/changed file.
- Recognize an observable `--no-session` argument even through common wrapper argv arrays. Do not claim complete detection when process details omit arguments.
- Keep bindings in memory only; do not write transcript-derived state or canonical session identity to disk.

**Test cases:**
- Authoritative `agent_session path` and several newer candidates → authoritative path wins.
- Authoritative path is temporarily unreadable/malformed while heuristic candidates exist → no heuristic fallback; last valid value is retained without TTL refresh.
- Authoritative ID-only reference → fallback resolver runs; no guessed ID-to-path conversion.
- One pane/one cwd → newest compatible candidate binds.
- Two panes/two files → deterministic mtime assignment; next cycle preserves sticky mapping despite mtime changes.
- Pane terminal ID changes or agent becomes non-Pi → prior binding is removed and clear is requested.
- Single pane baseline sees one newer alternative change while bound file is unchanged → binding moves once, all fingerprints are rebaselined, and unchanged later polls do not move it back.
- Single pane sees bound-file activity, multiple changed alternatives, or an older changed alternative → binding remains stable.
- Multiple panes receive an additional newer file → existing valid mappings do not reshuffle.
- Process argv exposes `--no-session` → no binding; hidden argv case remains eligible and is documented as a limitation.
- Configured additional root and environment-selected primary root contain duplicate/canonical-equivalent files → candidate appears once.

**Complete when:**
- Resolver tests prove source priority, deterministic sticky behavior, safe single-pane switching, and cleanup.
- Resolver has no Herdr socket I/O and performs no metadata writes.
- No inferred path can be represented as canonical `agent_session` output.

**Validation:**
- Run: `cargo test pi::resolver`
- Expected: All resolver and binding cases pass deterministically.
- Run: `cargo clippy --all-targets -- -D warnings`
- Expected: Exit 0 with no warnings.

### Task 3: Herdr Socket Protocol and Metadata Reporting

**Covers:** R1, R2, R11–R13, R15, D2, D5, D8

**Objective:** Implement a defensive protocol 19 Unix-socket client that lists agents, observes process information, subscribes to lifecycle events, and reports/clears only the two plugin-owned metadata tokens.

**Files:**
- Create: `src/herdr/mod.rs`
- Create: `src/herdr/protocol.rs`
- Create: `src/herdr/socket.rs`
- Modify: `src/main.rs`
- Create: `tests/listener.rs` with protocol-focused fake-socket cases

**Dependencies:** Task 1; resolver-facing types from Task 2.

**Implementation notes:**
- Generate unique request IDs and match request/response envelopes without assuming response order on a shared connection. If separating request and subscription connections simplifies correctness, preserve one transport abstraction and document the choice in this task.
- Subscribe to `pane.created`, `pane.updated`, `pane.closed`, `pane.exited`, `pane.agent_detected`, and `pane.agent_status_changed`; unknown/malformed events must not terminate the listener.
- Treat pushed event discriminators according to Herdr's snake_case event envelopes even though subscription request names use dotted strings.
- Decode only required `AgentInfo`, process-info, event, and report response fields. Unknown fields and future enum values should degrade to a reconciliation rather than panic where feasible.
- `pane.report_metadata` must set source `ryonakae.agent-context`, agent `pi`, default/configured TTL, monotonic per-pane `seq`, and only namespaced tokens. Use nullable tokens to clear.
- Never invoke `pane.report_agent`, `pane.report_agent_session`, title, display-agent, or state-label mutation methods.
- Use `HERDR_SOCKET_PATH`; missing `HERDR_ENV=1` or socket path must produce a concise startup error without leaking environment contents.
- Separate log-safe identifiers/errors from display values so tests can prove names/message text do not reach stderr.

**Test cases:**
- Initial `agent.list` response with optional/unknown fields → required pane data decodes.
- `pane.process_info` response containing direct or wrapper argv → argument vectors decode without logging unrelated arguments or environment values.
- Subscription request → contains every required event type and receives acknowledgement before events.
- Dotted subscription plus snake_case pushed event → event targets the correct pane.
- Malformed event followed by valid event → malformed event is logged/skipped and valid event is processed.
- Metadata set report → exact source, agent, token keys, TTL, and increasing sequence; no forbidden fields.
- Clear report → both token values are null and later stale sequence cannot restore them.
- Socket closes → transport returns a reconnectable error rather than panicking.
- Error logging with secret-like session text in input → stderr contains category/pane/path only, not text value.

**Complete when:**
- Fake socket tests prove the public request/event/report contracts.
- Protocol decode has no dependency on real user configuration or global Herdr state.
- Forbidden canonical/session/title methods do not appear in production call sites.

**Validation:**
- Run: `cargo test --test listener protocol`
- Expected: Protocol-focused fake socket tests pass without network or live Herdr access.
- Run: `rg -n 'report_agent_session|report_agent\b|display_agent|clear_title|state_labels' src`
- Expected: No production mutation call exists; any matches are protocol comments/tests explicitly asserting absence.

### Task 4: Long-Running Listener and Reconciliation

**Covers:** R1–R15, R19, D2–D6, D8

**Objective:** Combine transport and Pi resolution into one startup listener that converges sidebar metadata after startup, events, file changes, config changes, transient failures, and socket reconnection.

**Files:**
- Create: `src/runtime.rs`
- Modify: `src/main.rs`
- Modify: `src/config.rs`
- Modify: `src/herdr/*`
- Modify: `src/pi/*`
- Expand: `tests/listener.rs`

**Dependencies:** Tasks 1–3.

**Implementation notes:**
- Start only through the explicit `listen` subcommand; return usage status for unknown arguments.
- Acquire a lock scoped to the socket/session rather than a global plugin lock so multiple named Herdr sessions can each run a listener. A second listener for the same socket exits cleanly without clearing the active listener's metadata.
- Establish and acknowledge the event subscription before the initial full sync. Buffer events arriving during `agent.list`, then apply them and perform a final reconciliation before considering startup converged. Coalesce later bursts so repeated pane updates do not cause redundant full scans/reports.
- Maintain pane, binding, parsed-session, sequence, and last-valid-display state in memory. Refresh TTL only after a successful current binding parse; unchanged successful values may be re-reported before TTL expiration.
- Check `config.toml` identity/mtime on polling cycles. Apply a valid replacement atomically; retain prior settings and emit one deduplicated warning for an unchanged invalid file.
- Reconnect with bounded exponential backoff and jitter-free deterministic bounds suitable for tests. After reconnect, discard/revalidate stale Herdr pane ownership and perform a full sync.
- Parse/file failure must not emit a refreshing report. Explicit loss of pane/agent/binding must emit a clear immediately when connected.
- Ensure shutdown/socket fatal paths release the lock. TTL remains the fallback if the process is killed before explicit clear.
- Avoid wall-clock race tests by injecting a clock/tick source or using bounded test intervals.

**Test cases:**
- Startup with one unnamed Pi session → full sync emits fallback name and activity metadata.
- Named Pi session → explicit name replaces fallback after file change without restarting listener.
- Unchanged successful state across ticks → reports often enough that TTL does not expire, without reparsing unchanged JSONL.
- Incomplete/malformed changed file → prior values remain in memory but no TTL refresh report is sent; recovery resumes reports.
- Pane closed/released/non-Pi event → immediate clear and state eviction.
- Pi pane with no authoritative reference whose `pane.process_info` argv contains `--no-session` → resolver is skipped and any plugin-owned metadata is cleared; a later process-info change without the flag permits normal reconciliation.
- Listener starts twice for same socket → one active owner; different socket paths → both can run.
- Pane closes or changes agents between subscription acknowledgement and completion of initial `agent.list` → buffered event/final reconciliation prevents stale metadata from being refreshed.
- Socket disconnect/reconnect with changed pane list → resubscribe-first full reconciliation clears/removes old ownership and reports current panes.
- Valid config reload changes polling/TTL/roots; invalid reload retains prior values and emits no conversation text.
- Single-pane new session file → metadata switches; multiple-pane new file → no sticky reshuffle.

**Complete when:**
- Listener integration tests cover convergence and stale-value prevention.
- The binary remains alive through malformed events and reconnectable socket failures.
- Runtime performs no network requests and writes no transcript-derived persistent state.

**Validation:**
- Run: `cargo test --test listener`
- Expected: All listener lifecycle, reconnect, TTL, clear, config reload, and privacy cases pass.
- Run: `cargo test --all-targets`
- Expected: All package tests pass with no ignored failures.

### Task 5: Herdr Plugin Package, User Contract, and Manual Smoke Test

**Covers:** R11, R12, R14–R16, R19, D3, D8, D9

**Objective:** Make the listener usable as a linked Herdr plugin without modifying agent or Herdr configuration, and document exact user-visible behavior and limitations.

**Files:**
- Create: `herdr-plugin.toml`
- Modify: `README.md`
- Create: `docs/release-checklist.md`
- Modify: `src/main.rs` if local binary path/command contract requires it

**Dependencies:** Task 4.

**Implementation notes:**
- Manifest contract: ID `ryonakae.agent-context`, name `Agent Context`, version `0.1.0`, minimum Herdr `0.8.0`, platforms macOS/Linux, build via `sh scripts/install-binary.sh`, startup via `bin/herdr-agent-context listen`.
- README must start with user value and provide one managed install path plus a local build/link path. Explain that server restart is required to launch a new startup listener if Herdr does not start it immediately.
- Show a manual sidebar example using `$agent_context_session_name` and `$agent_context_last_message`; do not imply installation patches config.
- Document config location command, exact `config.toml` keys/defaults/validation, Pi root precedence, runtime privacy, `agent_session` preference, sticky same-cwd limitation, `--no-session` wrapper limitation, and log inspection.
- Manual checklist must use a disposable/local setup and include named/unnamed sessions, message update, `/new` or `/resume`, same-cwd panes, resolver restart/TTL, and removal/cleanup. It must not require publishing or changing Pi settings.
- Keep public repository documentation and code comments in English.

**Test cases / manual scenarios:**
- `herdr plugin link .` after copying a local debug/release binary → manifest validates and listener starts.
- Unnamed Pi → first user fallback appears; `/name` change → explicit name appears.
- Completed assistant reply → activity changes; new user before reply → prior ZAM-compatible activity remains.
- Listener termination → metadata disappears after TTL; restart/full sync restores it.
- Removing/unlinking plugin leaves Pi config untouched; sidebar custom rows simply render empty tokens.

**Complete when:**
- Manifest identity and command paths match binary/installer contracts.
- README fully describes installation and required manual sidebar configuration without hidden setup.
- Manual checklist is executable and distinguishes expected limitations from failures.

**Validation:**
- Run: `cargo build --release --locked && mkdir -p bin && cp target/release/herdr-agent-context bin/herdr-agent-context`
- Expected: `bin/herdr-agent-context` exists and is executable for local linking.
- Run: `herdr plugin link .` followed by the documented smoke checklist in an isolated/local Herdr setup.
- Expected: Herdr accepts the manifest and both custom tokens behave as documented; Pi and Herdr config files are not automatically edited.
- Cleanup: `herdr plugin unlink ryonakae.agent-context`
- Expected: The local plugin registration is removed without deleting the checkout or Pi configuration.

### Task 6: Prebuilt Installer, CI, and Release Readiness

**Covers:** R17–R19, D7, D8

**Objective:** Make `v0.1.0` installable without Cargo/Node/Python at runtime and prove release asset integrity across the supported target matrix without publishing it.

**Files:**
- Create: `scripts/install-binary.sh`
- Create: `scripts/verify-release-assets.sh`
- Create: `scripts/verify-glibc-baseline.sh`
- Create: `tests/installer.sh`
- Create: `tests/release-assets.sh`
- Create: `.github/workflows/ci.yml`
- Create: `.github/workflows/release.yml`
- Modify: `herdr-plugin.toml`
- Modify: `README.md`

**Dependencies:** Task 5.

**Implementation notes:**
- Asset contract: `herdr-agent-context-v{version}-{target}.tar.gz`, each containing executable `herdr-agent-context` and `LICENSE`; one `SHA256SUMS` lists exactly the four expected archives.
- Installer selects target from `uname`, allows test-only repository/version/base-url overrides, downloads with curl or wget, verifies SHA-256 with `sha256sum` or `shasum`, extracts into a temporary directory, and atomically installs `bin/herdr-agent-context` only after successful verification.
- Unsupported OS/architecture, missing checksum entry, mismatched checksum, malformed archive, or missing executable must fail non-zero without leaving a partial executable.
- CI runs format, clippy with warnings denied, all tests, shell installer/release tests, and a non-publishing locked build/package matrix for all four targets on pull requests, branch pushes, and `workflow_dispatch`. Matrix artifacts are retained for inspection but never published as a Release.
- Release workflow is tag-triggered and reuses or mirrors the validated matrix contract, verifies `v{Cargo.toml version} == v{manifest version} == tag`, checks a declared GNU/Linux glibc baseline, packages/strips binaries, generates checksums, verifies assets, and defines prerelease publication/smoke installation. The workflow file may exist, but this task does not create/push a tag or trigger it.
- Pin maintained major versions of GitHub Actions and keep release permissions limited to contents write only where publication requires it.

**Test cases:**
- Darwin/Linux and arm64/x86_64 target mapping → exact expected asset names.
- Unsupported target → fails before network/download.
- Invalid checksum fixture → fails and leaves no installed binary.
- Valid local `file://` fixture → installs executable with expected bytes.
- Four synthetic archives plus exact checksums → verification passes; missing/extra/corrupt archive → fails.
- Version mismatch among Cargo, manifest, and simulated tag → release verification fails.
- Archive contents include anything beyond binary/LICENSE or omit either → verification fails.

**Complete when:**
- Automated installer and release-contract tests pass on supported CI runners.
- The non-publishing CI matrix successfully builds/packages all four expected targets before this task is marked complete; if the branch is not available to GitHub Actions, this validation remains pending and the plan is not archived.
- Release workflow defines the same four expected artifacts and checksum verification.
- Managed installation requires only standard download/archive/checksum tools at install time and no language runtime afterward.
- Repository is release-ready but no tag, push, or release publication has occurred.

**Validation:**
- Run: `sh tests/installer.sh`
- Expected: Prints installer success and proves invalid checksum/unsupported targets fail safely.
- Run: `sh tests/release-assets.sh`
- Expected: Prints release asset success and proves missing/extra/corrupt assets fail.
- Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all-targets && cargo build --release --locked`
- Expected: Every command exits 0 without warnings or test failures.
- Run: `git diff --check`
- Expected: Exit 0 with no whitespace errors.
- Run after the user makes the exact HEAD available remotely and authorizes a non-publishing workflow run:
  ```sh
  branch=$(git branch --show-current)
  sha=$(git rev-parse HEAD)
  started=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
  gh workflow run ci.yml --ref "$branch"
  run_id=
  for _ in $(seq 1 30); do
    run_id=$(gh run list --workflow ci.yml --event workflow_dispatch --branch "$branch" --limit 30 --json databaseId,headSha,createdAt --jq ".[] | select(.headSha == \"$sha\" and .createdAt >= \"$started\") | .databaseId" | head -n 1)
    test -n "$run_id" && break
    sleep 2
  done
  test -n "$run_id"
  gh run watch "$run_id" --exit-status
  ```
- Expected: The newly dispatched run for the exact local HEAD exits successfully and its four target-build jobs produce artifacts without creating a tag or GitHub Release.

## Requirement Coverage

| Requirement / Decision | Task | Verification |
|---|---|---|
| R1 Hook-free discovery | Tasks 2–4 | Authoritative/fallback resolver tests; forbidden mutation scan |
| R2 Pi-only selection/cleanup | Tasks 2–4 | Non-Pi/release/close tests and clear payload assertions |
| R3 Session-name semantics | Task 1 | Explicit/user/cwd fixture tests |
| R4 ZAM activity semantics | Tasks 1, 4 | Assistant filtering and prior-value retention tests |
| R5 80-character one-line values | Task 1 | Unicode/multiline normalization tests |
| R6 Tree-aware parsing | Task 1 | Divergent branch fixtures |
| R7 Binding source priority | Task 2 | Authoritative path versus mtime fixtures |
| R8 Session switching | Tasks 2, 4 | Single-pane switch and multi-pane sticky tests |
| R9 Ephemeral sessions | Tasks 2–5 | Pure argv detection, fake process-info decoding, runtime skip/clear integration test, and documented limitation |
| R10 Session roots | Tasks 1, 2 | Environment/config precedence and deduplication tests |
| R11 Listener lifecycle | Tasks 3–5 | Subscription, lock, reconnect, and full-sync tests; manifest smoke |
| R12 Metadata lifecycle | Tasks 3, 4 | Exact payload, sequence, TTL refresh, and clear tests |
| R13 Failure degradation | Task 4 | Parse failure/no-refresh and reconnect tests |
| R14 Config contract | Tasks 1, 4, 5 | Config validation/reload tests and README contract |
| R15 Privacy | Tasks 1–4 | Synthetic fixtures, log redaction assertions, no runtime network path |
| R16 Manual sidebar ownership | Task 5 | README/config smoke and no config mutation check |
| R17 Platforms/distribution | Task 6 | Installer mapping and four-asset tests/workflow matrix |
| R18 Release readiness | Task 6 | Version/asset/checksum workflow verification |
| R19 Verification strategy | Tasks 1–6 | TDD-focused validations, fake socket integration, manual checklist |
| D1 Local Rust/deferred sharing | Tasks 1–2 | File/module review; no generic multi-agent/shared crate |
| D2 Raw socket API | Tasks 3–4 | Fake protocol 19 socket tests |
| D3 Long-running startup | Tasks 4–5 | Duplicate lock/listener tests and manifest command |
| D4 Incremental work | Tasks 1, 2, 4 | unchanged-file no-reparse integration assertion |
| D5 Read-only canonical identity | Tasks 2–4 | Payload assertions and forbidden method scan |
| D6 ZAM fallback plus fixes | Tasks 1, 2, 4 | compatibility, branch, and switch tests |
| D7 Thread-to-tab distribution | Task 6 | checksum installer and four-target release workflow |
| D8 Stable public names | Tasks 3, 5, 6 | protocol payload, manifest, installer asset tests |
| D9 Manual Herdr config | Task 5 | README and no-auto-edit smoke check |

## Final Validation

- [ ] `cargo fmt --check` — Expected: exit 0 with no formatting changes.
- [ ] `cargo clippy --all-targets -- -D warnings` — Expected: exit 0 with no warnings.
- [ ] `cargo test --all-targets` — Expected: all parser, resolver, config, and fake-socket tests pass with no ignored failures.
- [ ] `sh tests/installer.sh` — Expected: target/checksum/install cases pass and unsafe partial installs are rejected.
- [ ] `sh tests/release-assets.sh` — Expected: exact four-asset/checksum contract passes and corrupt variants fail.
- [ ] `cargo build --release --locked` — Expected: release binary builds reproducibly for the host.
- [ ] `git diff --check` — Expected: no whitespace errors.
- [ ] `rg -n 'report_agent_session|report_agent\b' src` — Expected: no production code writes canonical agent/session identity; test/comment matches, if any, are reviewed.
- [ ] Runtime privacy review — Expected: no telemetry/runtime HTTP dependency, no log statement accepts session-name/message text, and fixtures are synthetic.
- [ ] Manual isolated Herdr smoke checklist in `docs/release-checklist.md` — Expected: named/unnamed Pi, activity update, `/new`/`/resume`, same-cwd limitation, TTL disappearance, and plugin removal match documentation; skipped on unsupported CI with this explicit manual rationale.
- [ ] Non-publishing four-target CI run — After the user makes the exact HEAD available remotely and authorizes the run, use Task 6's timestamp-and-HEAD-filtered dispatch/watch commands; Expected: the captured new run ID matches the local HEAD, and both macOS architectures and both GNU/Linux architectures build/package successfully without creating a tag or Release. Until this succeeds, validation remains incomplete and the plan stays unarchived.
- [ ] Supported-target workflow review — Expected: release workflow covers the same four targets, verifies glibc/checksums, and is not triggered without a tag.
- [ ] Requirement Coverage has no unmapped requirement or decision.
- [ ] Plan and actual changed files/contracts agree; minor differences are reflected in the relevant task.
- [ ] No tag, push, GitHub Release, Pi integration, or automatic Herdr config edit occurred.
- [ ] After every item above succeeds, move this file unchanged to `docs/plans/archived/2026-08-18-pi-agent-context-plugin.md`.

## Risks and Open Questions

- Herdr startup hooks are not supervised. TTL prevents indefinitely stale UI, but a crashed listener requires a Herdr/plugin restart; this is an accepted v0.1.0 limitation.
- The Herdr socket schema may gain fields/events after protocol 19. Defensive decoding and `min_herdr_version = "0.8.0"` reduce coupling, but future incompatible protocol changes may require a plugin release.
- Without official Pi session reporting, same-cwd multi-pane mappings remain heuristic and may be wrong after listener startup or in-process session switching. The plugin limits impact to sidebar metadata and documents the limitation.
- A Pi `/tree` move without an appended entry does not persist the active leaf, so an external parser cannot observe it until a later entry is written.
- Wrapper processes may hide `--no-session`; this can lead to fallback binding against a historical file. Process-info detection and documentation mitigate but cannot eliminate the limitation without a Pi integration.
- GNU/Linux prebuilt compatibility depends on the selected glibc baseline and cross-build verification; release CI must fail rather than publish an unverified binary.
- No unresolved product/API/config decisions remain. Implementation discoveries that would change public behavior require user confirmation.
