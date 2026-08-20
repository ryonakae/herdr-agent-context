# Herdr Tab Session Titles Implementation Plan

> **For implementers:** Execute tasks in order unless dependencies allow otherwise. Mark a task complete only after its validation succeeds. Reflect minor implementation differences in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Problem Statement

`herdr-agent-context` reports Pi session names and Claude display context in the Herdr Agents sidebar, but Herdr's tab bar still uses unrelated numeric or manually assigned labels. This makes it difficult to identify several agent sessions without opening their tabs. Herdr 0.8.0 and later expose stable tab IDs, per-tab focused-pane snapshots, `tab.rename`, and `tab.renamed`, so the plugin can synchronize tab labels, but that API writes persistent custom labels without source ownership, TTL, or a way to restore Herdr's true auto-named state.

Claude Code also distinguishes user-defined session names, default running-session display names, and AI-generated descriptive titles. The desired Claude label is the title Claude shows in the shell: a user-defined `--name`/`/rename` value when present, otherwise the AI-generated descriptive title. The current parser calls this a session name, still falls back to first-user text and cwd, and reads the legacy `custom-title.title` key rather than the current `custom-title.customTitle` key.

The implementation therefore needs an opt-in, durable ownership policy that keeps sidebar and tab title sources aligned, follows the focused pane, respects manual Herdr tab renames, restores user labels when ownership ends, and fails closed without regressing the existing metadata listener.

## Goal

Add an opt-in `[tab_name] enabled = true` feature that:

- synchronizes each Herdr tab to the display context of its internally focused Pi or Claude pane;
- uses Pi's existing resolved session name and Claude's verified shell title semantics;
- keeps Claude's sidebar token and tab label based on the same title source;
- follows focus and title changes without postponing the absolute metadata polling deadline;
- preserves user tab labels through session-scoped manual overrides and durable, privacy-bounded ownership state;
- restores the latest user baseline when the selected session disappears or the feature is disabled;
- preserves Herdr 0.8.0 / protocol 19 compatibility and all existing sidebar, privacy, reconnect, and packaging behavior.

## Out of Scope

- Enabling tab-name synchronization by default.
- Adding configurable width or debounce settings; the public config contains only `tab_name.enabled`.
- Showing Claude's default `project-xx` display name, first-user text, or cwd basename as a Claude title.
- Adding title extraction for Codex, OpenCode, or other unsupported agents.
- Changing `agent_context_last_message` behavior or renaming `$agent_context_session_name`.
- Adding a plugin action or CLI command to restore labels before plugin disable or uninstall.
- Guaranteeing automatic restoration after the listener is forcibly killed or the plugin is disabled or uninstalled.
- Modifying Herdr core to add source-owned tab labels, TTL labels, an `is_auto_named` field, or a clear-custom-label API.
- Persisting generated session titles, raw Pi paths, Claude UUIDs, prompts, or assistant text in plugin state.
- Encrypting local state; owner-only filesystem permissions and one-way digests are the selected boundary.
- Adding Windows support, changing release targets, publishing a release, or changing package/plugin versions.
- Committing real Pi or Claude transcripts, titles, session IDs, or user paths in fixtures.

## Requirements and Decisions

### Requirements

- **R1 — Opt-in configuration:** Add strict `[tab_name] enabled = <bool>` configuration. The default is `false`. Missing or removed configuration disables the feature. Unknown keys still reject the complete config atomically.
- **R2 — Focused-pane source:** For every tab, use the `focused_pane_id` from `session.snapshot.layouts`, including background tabs. A focus change is applied after a 150 ms trailing debounce. Rapid focus changes apply only the final pane.
- **R3 — Non-agent focus:** If focus moves to a shell, log, unsupported agent, or another pane without a resolved supported context, retain the last selected supported session's label while that session remains in the tab. If no supported session has been selected for that tab, retain the baseline.
- **R4 — Pi label:** A Pi pane uses the existing resolved `DisplayView.session_name`, including the current explicit-name, first-user, and cwd fallback behavior.
- **R5 — Claude title semantics:** A Claude pane has no display title until a valid matching `custom-title` or `ai-title` record exists. Do not use Claude's default running-session display name, first-user text, or cwd basename. The latest valid custom title takes precedence over the latest valid AI title.
- **R6 — Claude terminal-title preference:** Normalize the verified Claude JSONL title and Herdr's `terminal_title_stripped` to complete first-nonempty trimmed lines without applying either display bound. If those complete normalized lines match, use the terminal value as the shared source title; if the terminal value is absent or differs, use the verified JSONL value. Derive sidebar and tab strings independently from that shared source.
- **R7 — Claude format compatibility:** Read current `custom-title.customTitle`; accept `custom-title.title` only as a legacy fallback. Continue reading `ai-title.aiTitle`. Ignore title records for a different `sessionId` and follow later valid title changes in the same session.
- **R8 — Independent display bounds:** Derive sidebar metadata directly from the shared source title using the existing one-line maximum of 80 Unicode scalars. Independently derive generated tab labels from the same complete source using grapheme-cluster-safe terminal display width, at most 15 columns including a final `…`. Never derive the tab label from the 80-scalar sidebar string. User-authored Herdr labels are stored and restored exactly without either bound.
- **R9 — Status independence:** A resolved session remains eligible in `working`, `blocked`, `done`, `idle`, or `unknown` state. Agent lifecycle status does not itself acquire or release a tab label.
- **R10 — Manual override:** A `tab.renamed` value that does not match a plugin-expected rename becomes a user-authored override scoped to Herdr session, `tab_id`, and selected `session_identity`. It suppresses generated synchronization for that identity. Selecting another identity resumes synchronization; returning to the suppressed identity restores its exact manual label. A new identity in the same pane is not suppressed.
- **R11 — Baseline semantics:** The latest user-authored manual label becomes the tab's baseline. When no selected supported session remains, restore that baseline. If the initially captured baseline equals the tab's current positional number, record it as probably auto-named and restore the current position after reordering; otherwise restore the exact string. Herdr will still store the restored numeric label as custom because the API cannot clear custom naming.
- **R12 — Tab-local ownership:** Manual overrides and ownership do not follow a pane when `pane.move` transfers it to another `tab_id`. The source tab restores its own baseline when appropriate; the destination evaluates the moved pane under its own state.
- **R13 — Session transitions and failures:** A same-session transient read/parse failure retains the current generated label and does not refresh sidebar TTL. When a different session identity is observed but its title/name is not yet resolved, restore the baseline until the new display value becomes available.
- **R14 — Durable state:** Persist versioned ownership state per Herdr socket/session under `HERDR_PLUGIN_STATE_DIR`. Store baseline and user-authored override labels in plaintext; store socket identity, session identities, and plugin-generated/applied labels only as stable SHA-256 digests. State files and created directories are owner-only.
- **R15 — Write ordering and recovery:** Persist a recoverable pending transition before calling `tab.rename`. The transition records prior and target digests so restart can distinguish not-applied, applied, and externally changed labels without persisting generated title text. Atomically finalize after success. If state cannot be persisted, do not issue the rename. If state is malformed or unsupported, disable only tab-name synchronization for that listener while sidebar metadata continues.
- **R16 — Manual-event attribution:** Because `tab.renamed` has no source, recognize plugin-originated events by the persisted pending/applied target digest. Treat any other observed label as external/manual, including mismatches discovered from a later snapshot after a missed event or reconnect.
- **R17 — Disable cleanup:** A valid config transition from enabled to disabled stops new acquisitions, restores owned tabs to current user baselines, and removes state only after cleanup succeeds. Retry incomplete cleanup after transient socket errors. Do not overwrite a manual label observed while the listener was offline.
- **R18 — Lifecycle cleanup:** Remove state for closed tabs. Reconcile tab ordering, pane moves, changed focus, title changes, and missed events from `session.snapshot`; events are wake-up signals and the snapshot is authoritative.
- **R19 — Privacy and isolation:** Never log label/title text, session identity values, transcript text, full argv, or state contents. State or tab synchronization failures must not suppress another pane's metadata and must not turn a pane-local parse failure into unrelated label changes.
- **R20 — Compatibility:** Keep `min_herdr_version = "0.8.0"`, protocol 19 request/event compatibility, one socket connection per request, subscribe-before-snapshot/list ordering, buffered pre-acknowledgement events, separate event and RPC connections, duplicate-listener locking, reconnect behavior, and the absolute poll deadline.

### Implementation Decisions

- **D1 — Explicit label ownership:** Once enabled and eligible, the plugin owns generated labels for a tab except for session-scoped manual overrides. This is more predictable than heuristically avoiding all manually named tabs, which Herdr's API cannot identify reliably.
- **D2 — One shared Claude title candidate:** Claude parsing and backend reconciliation produce one verified display title used by both sidebar metadata and tab naming. The public token name remains `$agent_context_session_name` for compatibility; documentation calls the Claude value a session title.
- **D3 — JSONL verification gate:** A terminal title alone is insufficient because it may be inherited from a shell. A matching `custom-title` or `ai-title` record establishes that Claude has a meaningful title. A mismatch falls back to JSONL rather than showing arbitrary OSC title content.
- **D4 — Separate tab-name domain module:** Keep backend parsing and Herdr transport independent. A dedicated tab-name manager owns topology selection, debounce state, manual-override policy, transition planning, and durable state; `runtime.rs` coordinates it with backend outcomes.
- **D5 — Snapshot authority:** Use `session.snapshot` only while tab naming is enabled or cleanup remains pending. `agent.list` remains the existing backend input. Add optional topology/title fields defensively so missing future/legacy fields skip tab naming rather than breaking sidebar reporting.
- **D6 — Event-driven focus, polling recovery:** Subscribe to focus, rename, move, close, and layout events to wake reconciliation. Continue relying on the absolute poll for title/file changes and missed events. Ignore metadata-only `pane.updated` as a wake-up to avoid a reporting loop.
- **D7 — Trailing focus debounce:** Focus changes replace a per-tab pending deadline at `last_focus_event + 150 ms`. This deadline participates in the listener's next wake-up calculation but never resets or extends the absolute polling deadline.
- **D8 — Stateful manual overrides:** Store user-authored overrides by digest of session identity within a tab record. The most recently observed user label also updates the tab baseline, while earlier identity-specific labels remain available when focus returns to those sessions.
- **D9 — Fail-closed persistence:** A tab is not renamed unless its ownership transition is durable. Corrupt state is not reset automatically because doing so could erase restoration data and misclassify existing labels.
- **D10 — Fixed Unicode policy:** Add grapheme segmentation and terminal-width dependencies. The 15-column limit is a fixed product behavior, not a config option.
- **D11 — TDD and synthetic fixtures:** Implement each behavioral task Red → Green → Refactor. Tests use synthetic titles, UUIDs, paths, snapshots, and Unix sockets only.
- **D12 — No release operation:** Update public capability/configuration documentation and the plugin description, but do not bump versions, tag, publish, or alter archive contracts in this task.

### Contracts

#### Configuration

```toml
[tab_name]
enabled = true
```

- Omitted table or omitted `enabled` means `false`.
- `max_width`, `focus_debounce_ms`, per-agent switches, or unknown fields are invalid.
- A valid enabled-to-disabled reload initiates restoration immediately; an invalid reload retains the previous complete config and does not change ownership.

#### Claude display title

For the selected bound Claude transcript:

1. Select the latest valid `custom-title` value for the same session, reading `customTitle` first and legacy `title` second.
2. Otherwise select the latest valid `ai-title.aiTitle` for the same session.
3. If neither exists, the Claude display title is absent.
4. Normalize the selected JSONL title and `terminal_title_stripped` to complete first-nonempty trimmed lines, without applying the sidebar or tab bound.
5. If the complete normalized terminal line equals the complete verified JSONL line, choose the terminal line as the shared source; otherwise choose the JSONL line.
6. Derive `$agent_context_session_name` from the shared source with the existing 80-scalar metadata bound.
7. Independently derive the tab label from the same complete shared source with the 15-column grapheme/display-width bound.

The sidebar value must never be the input to tab truncation. This matters for long combining-character graphemes whose scalar count can exceed 80 while their terminal width remains below 15.

#### Herdr topology and mutation surface

The minimum internal Herdr contract adds:

- agent fields: optional `workspace_id`, `tab_id`, and `terminal_title_stripped`;
- `session.snapshot`: ordered tab records plus per-tab `focused_pane_id` layout snapshots;
- `tab.rename { tab_id, label }`: returns the updated `TabInfo`;
- subscriptions for existing pane lifecycle events plus `pane.focused`, `pane.moved`, `tab.created`, `tab.closed`, `tab.renamed`, `tab.moved`, and `layout.updated`.

Event parsing must retain `tab_id`, `pane_id`, and `label` only where supplied and ignore unknown fields. The event stream remains subscribed before the first authoritative snapshot/list call.

#### Durable state

Store one versioned JSON document at a socket-scoped path equivalent to:

```text
${HERDR_PLUGIN_STATE_DIR}/tab-name/<sha256(HERDR_SOCKET_PATH)>.json
```

The exact Rust type names may follow existing conventions, but the serialized contract must include:

- schema version;
- tab records keyed by stable `tab_id`;
- plaintext baseline kind/value and plaintext user override labels;
- digested session-identity keys;
- digests of last expected/applied generated labels;
- recoverable pending transition with prior and target digests.

Writes use a same-directory temporary file, owner-only permissions, flush/sync appropriate for crash recovery, and atomic rename. A generated title or raw session identity must never appear in the serialized document. Unsupported versions and malformed documents fail closed without overwrite.

#### Ownership state transitions

- **Untracked + eligible selected session:** capture/persist baseline, then apply bounded generated label.
- **Owned + same identity/title update:** persist transition, then apply the new bounded label.
- **Owned + non-agent focus:** retain the last selected supported session while it remains in the same tab.
- **Owned + different unresolved identity:** restore baseline until a display value resolves.
- **Owned + unexpected tab rename:** persist the exact label as latest baseline and identity-specific override; suppress that identity.
- **Suppressed identity selected:** display its exact manual override.
- **Different unsuppressed identity selected:** display its generated label while retaining prior suppression records.
- **No selected/live supported identity, tab close, pane move out, or feature disable:** restore or clean state according to R11, R12, R17, and R18.
- **Restart with pending transition:** compare snapshot label digest to prior and target digests; finalize, retry from recomputable current context, or classify an unrelated value as manual without guessing.

## Current Context

### Confirmed

- The worktree was clean before this plan was created.
- Herdr 0.8.2 is installed locally and reports protocol 20; the checked v0.8.0 source/schema reports protocol 19.
- Herdr v0.8.0 already provides `session.snapshot`, per-tab `focused_pane_id`, `tab.rename`, and `tab.renamed`, so the current minimum version can remain unchanged.
- `tab.rename` stores an exact custom string, schedules session persistence, emits `tab.renamed`, and has no source, TTL, sequence, clear-custom-label operation, or label length validation.
- Herdr auto tab labels are positional numbers. The stable `TabInfo.number`/public ID number can differ from the displayed auto position. `TabInfo` does not expose whether a label is auto-named.
- Current `agent.list` records include `workspace_id`, `tab_id`, and `terminal_title_stripped`; the plugin's minimal `AgentInfo` currently ignores them.
- Current event subscriptions cover only `pane.created`, `pane.updated`, `pane.closed`, `pane.exited`, and `pane.agent_detected`. `pane.report_metadata` emits `pane_updated`, which the listener deliberately ignores as a reconciliation wake-up.
- `Runtime::reconcile` owns backend outcomes and metadata lifecycle; `main.rs` owns config polling, reconnect, event polling, and the absolute polling deadline.
- `HERDR_PLUGIN_STATE_DIR` is injected for plugin runtime commands but is not currently read by the listener.
- The plugin currently reads Claude `custom-title.title`, then `ai-title.aiTitle`, then first active-branch user text, then cwd basename.
- Current Claude Code documentation distinguishes user names, default display names, and AI-generated titles. The installed Claude Code 2.1.220 documents `--name` as visible in the prompt box, resume picker, and terminal title.
- Structural inspection of live local Claude records found `ai-title` entries with `aiTitle`. No transcript values were copied. For both live Claude panes inspected, Herdr `terminal_title_stripped` exactly matched the plugin's latest AI-title-derived metadata value.
- Current Claude `custom-title` records use `customTitle`; public reports also show `sessionId`. Existing synthetic tests use the legacy `title` key and need both positive current-format and legacy compatibility coverage.
- Existing automated seams include parser unit tests, `Runtime<HerdrApi>` fake tests, temporary Unix-socket transport tests, listener subprocess reconnect tests, config watcher tests, installer/archive tests, and release validation commands documented in `AGENTS.md`.

### Assumptions

- Private Rust type and helper names may change during implementation to match ownership and borrowing constraints, provided the serialized/public contracts and file responsibilities remain intact and minor differences are recorded in this plan.
- A stable SHA-256 implementation and well-maintained Unicode grapheme/display-width crates may be selected during implementation without changing observable behavior.

## File Structure

- Create: `src/tab_name/mod.rs` — tab topology selection, focus debounce, ownership/manual-override state machine, transition planning, generated-label bounding, and unit tests.
- Create: `src/tab_name/state.rs` — versioned socket-scoped state schema, SHA-256 digests, owner-only atomic persistence/recovery, and failure tests.
- Modify: `src/lib.rs` — register the tab-name module.
- Modify: `src/text.rs` — add grapheme-safe terminal display-width truncation while preserving existing 80-scalar metadata helpers.
- Modify: `src/config.rs` — add strict `[tab_name].enabled`, default-off behavior, reload tests, and config equality support.
- Modify: `src/backend.rs` — carry terminal title/topology input needed for agent-neutral display and tab coordination without exposing Claude record details.
- Modify: `src/claude/session.rs` — expose only verified Claude titles, support `customTitle` plus legacy `title`, remove first-user/cwd name fallbacks, and retain activity parsing.
- Modify: `src/claude/mod.rs` — combine verified JSONL title with `terminal_title_stripped` and emit the shared Claude display value.
- Modify: `src/herdr/protocol.rs` — add defensive topology/title structs, snapshot/rename params, richer event parsing, and exact payload tests.
- Modify: `src/herdr/mod.rs` — extend `HerdrApi` with the minimum snapshot and tab-rename operations used by runtime/tab naming.
- Modify: `src/herdr/socket.rs` — call `session.snapshot` and `tab.rename` while preserving one-connection-per-request and event-stream ordering.
- Modify: `src/runtime.rs` — coordinate backend results, topology snapshots, metadata, tab manager input/effects, config transitions, and pane-local failure isolation.
- Modify: `src/main.rs` — read `HERDR_PLUGIN_STATE_DIR`, integrate focus-debounce deadlines with the absolute poll schedule, route richer events, and trigger enabled/disabled reconciliation.
- Modify: `tests/listener.rs` — synthetic runtime, manual override, restart recovery, exact socket, debounce/reconnect, and metadata/tab isolation coverage.
- Modify: `Cargo.toml`, `Cargo.lock` — add only the digest and Unicode dependencies required by D10/R14; keep runtime network-free and Rust 1.85 compatible.
- Modify: `README.md` — document opt-in config, Pi name versus Claude title semantics, focus/manual-override behavior, persistence/privacy, auto-name limitation, and disable/uninstall behavior.
- Modify: `herdr-plugin.toml` — update the description to mention sidebar and tab context without changing ID, version, platforms, startup, or build contracts.
- Modify: `docs/release-checklist.md` — add a disposable-session live smoke for focus switching, Claude generated/custom titles, Pi naming, manual overrides, config disable restoration, and listener restart recovery.
- Maintain then archive: `docs/plans/2026-08-20-herdr-tab-session-titles.md` — reflect progress and minor implementation differences; move unchanged by name to `docs/plans/archived/` only after every final validation succeeds.

## Testing Decisions

- **Primary policy seam:** Pure/deterministic tab-manager tests consume synthetic ordered tabs, per-tab focus, pane/session/title candidates, observed labels, events, and explicit instants. Assert planned persistence and rename effects rather than private map layout.
- **Persistence seam:** Temporary directories with synthetic labels/digests verify versioning, owner-only modes, atomic replacement, pending-transition recovery, socket scoping, corruption handling, and absence of generated plaintext.
- **Backend seam:** Synthetic Claude JSONL plus synthetic Herdr terminal titles verify current/legacy custom-title keys, AI-title updates, no-title behavior, terminal match preference, mismatch fallback, and unchanged activity extraction.
- **Runtime seam:** `Runtime<HerdrApi>` fake snapshots and agents verify metadata and tab effects together, focus/non-agent rules, identity transitions, status independence, pane moves, config disable cleanup, and isolation when state or transcript operations fail.
- **Transport seam:** Temporary Unix sockets assert exact subscription objects, subscribe-before-RPC behavior, buffered events, `session.snapshot`, `tab.rename`, response validation, and no canonical session reporting.
- **Scheduler seam:** Explicit `Instant` values verify 150 ms trailing debounce, rapid focus replacement, wake-up minimums, and that events/debounce never postpone the absolute poll deadline.
- **Live seam:** A disposable named Herdr session and synthetic/disposable Pi/Claude conversations verify visible tab behavior without touching real transcripts or the default session. Follow the new release-checklist section and close only resources created for the smoke.
- **Prior art:** Reuse current `ConfigWatcher`, `Runtime<HerdrApi>`, protocol full-payload assertions, listener reconnect tests, source-scoped lock, and session parser fixture builders.
- **Avoid:** sleeps in policy unit tests, real transcript fixtures, printing local titles/paths, direct edits to Herdr session files, depending on sidebar row configuration for tab tests, or treating a successful command without behavioral assertions as sufficient.

## Progress

- [x] Task 1: Align Claude's shared sidebar/tab display title and Unicode bounding contracts.
- [x] Task 2: Add opt-in configuration and the Herdr topology/rename/event transport contract.
- [ ] Task 3: Implement durable tab ownership, manual overrides, restoration, and crash recovery.
- [ ] Task 4: Integrate focus-debounced tab synchronization with runtime and listener lifecycle.
- [ ] Task 5: Publish the user-visible contract and complete automated and disposable-session validation.

Implementation-time minor file changes or internal naming differences must be recorded in the relevant task. Ask the user before changing requirements, Out of Scope, configuration schema, title precedence, manual-override scope, state format/privacy boundary, compatibility claims, or release contracts.

## Tasks

### Task 1: Shared Claude Display Title and Unicode Bounds

**Covers:** R4-R8, R13, R19, D2, D3, D10, D11

**Objective:** Claude sidebar metadata exposes exactly the user-defined or AI-generated title that drives its shell title, with current-format compatibility and no first-user/cwd fallback, and the repository has a tested grapheme/display-width helper for future tab labels.

**Files:**
- Modify: `src/backend.rs`
- Modify: `src/claude/session.rs`
- Modify: `src/claude/mod.rs`
- Modify: `src/text.rs`
- Modify: `tests/listener.rs`
- Modify: `Cargo.toml`, `Cargo.lock`

**Dependencies:** Existing Claude resolver and metadata reporting behavior.

**Implementation notes:**
- Start with failing parser/backend/runtime tests before changing production behavior.
- Keep activity branch reconstruction and filtering unchanged. Removing first-user/cwd fallback applies only to Claude's display title.
- Select latest valid custom and AI title values independently, then preserve custom-over-AI precedence. Read `customTitle` first per record and legacy `title` second; do not accept a mismatched `sessionId`.
- Pass `terminal_title_stripped` through agent-neutral pane input. Missing title/topology fields must not block existing metadata.
- Compare complete first-nonempty trimmed title lines before either display bound. A mismatch returns the JSONL source; it never returns arbitrary terminal text.
- Preserve a shared complete source title long enough to derive the 80-scalar sidebar value and 15-column tab value independently; do not feed `display_line` output into tab truncation.
- Add a separate helper whose contract is grapheme-safe display width <= 15 columns including `…`; do not alter existing `display_line` results.

**Test cases:**
- Current `custom-title.customTitle`, legacy `custom-title.title`, and `ai-title.aiTitle` → expected title precedence and latest valid value.
- Title record for another session, blank title, or no title record with a valid first user and cwd → no Claude session-name token; activity remains independently available.
- Matching normalized terminal title → shared display title; missing or mismatched terminal title → verified JSONL title.
- Later AI/custom title in the same identity → sidebar token updates; same-session transient incomplete tail → no refresh and previous runtime value remains.
- Exact 15-column ASCII/CJK/emoji grapheme values → unchanged; over-limit values → <=15 display columns with one trailing ellipsis and no split grapheme.
- One narrow grapheme containing more than 80 Unicode scalars → sidebar follows the 80-scalar contract while the tab is independently derived from the complete grapheme and remains intact within 15 display columns.
- Existing 80-scalar metadata tests → unchanged except Claude no-title fallback expectations.

**Complete when:**
- Claude's reported `$agent_context_session_name` follows the agreed title contract and activity behavior has no regression.
- The current and legacy custom-title formats have synthetic coverage.
- Generated tab-label bounding is deterministic across ASCII, CJK, combining characters, and emoji sequences.

**Validation:**
- Run: `cargo test claude::session::tests --locked`
- Expected: all Claude parser tests pass, including current `customTitle`, legacy `title`, no-fallback, and title-update cases.
- Run: `cargo test text::tests --locked`
- Expected: existing 80-scalar and new 15-column grapheme/display-width tests pass.
- Run: `cargo test --test listener claude --locked`
- Expected: Claude metadata title, authority, failure, and activity tests pass with no first-user/cwd title fallback.

**Implementation record:**
- Added complete-line and independent grapheme/display-width tab-label helpers with `unicode-segmentation` and `unicode-width`; Claude now preserves a complete verified title source while reporting the existing 80-scalar metadata value.
- Added current `customTitle`, legacy `title`, no-title, terminal-title verification, and >80-scalar narrow-grapheme coverage. Updated `README.md` for the changed Claude title contract.
- Minor file difference: `src/herdr/protocol.rs`, `src/runtime.rs`, `src/pi/session.rs`, and `src/pi/resolver.rs` were also updated to carry the terminal title and shared tab source without waiting for Task 2.
- Focused parser, text, Claude listener, full Rust/shell/workflow/release checks, formatting, and clippy validation passed. Task-level review found no blocking/high issue; its mixed-key legacy fallback observation was fixed and revalidated.

### Task 2: Configuration and Herdr Tab Control Contract

**Covers:** R1, R2, R16, R18, R20, D5, D6, D11

**Objective:** The plugin can opt into authoritative tab topology, receive the relevant events, and issue exact tab renames over the protocol 19-compatible socket API without changing default-off runtime traffic.

**Files:**
- Modify: `src/config.rs`
- Modify: `src/herdr/protocol.rs`
- Modify: `src/herdr/mod.rs`
- Modify: `src/herdr/socket.rs`
- Modify: `tests/listener.rs`

**Dependencies:** Task 1's expanded agent input/title fields.

**Implementation notes:**
- Add a strict defaultable `[tab_name]` config object with only `enabled`.
- Preserve complete-config atomic reload behavior. Invalid reloads cannot enable, disable, or clean up tab ownership.
- Deserialize only the snapshot/tab/layout fields needed by policy and ignore unknown future fields. Keep sidebar operations functional when optional topology/title fields are absent.
- Add exact request serializers/parsers for `session.snapshot` and `tab.rename`; validate response IDs through the existing RPC path.
- Replace the pane-only event representation with a minimal typed event that can carry pane focus/move, tab rename/move/close/create, and layout wake-ups without exposing transcript data.
- Subscribe before any list/snapshot call and retain events received before acknowledgement. Keep one socket connection per RPC request.
- Default-disabled reconciliation must not call `session.snapshot` or `tab.rename`.

**Test cases:**
- Empty config and `[tab_name]` without `enabled` → disabled; explicit true/false → expected; unknown nested key → complete config rejected.
- Valid enabled-to-disabled reload → represented as a real config transition; invalid reload → prior enabled state retained.
- Protocol 19-shaped snapshot with ordered tabs/layout focus and unknown fields → exact minimal topology.
- `tab.rename` request → exact `{tab_id,label}` and updated `TabInfo`; `tab_not_found` remains classifiable as a missing resource.
- Subscription request → contains all required dotted event types exactly once; snake-case envelopes parse to the correct typed event and label.
- Buffered pre-ack focus/rename event → delivered before later stream items.

**Complete when:**
- The fake socket proves exact snapshot, rename, and event contracts.
- Existing metadata RPC payload and reconnect/subscription ordering tests remain green.
- Default configuration creates no tab-name API dependency.

**Validation:**
- Run: `cargo test config::tests --locked`
- Expected: strict default-off table, valid reload, invalid retention, and all existing path/timing tests pass.
- Run: `cargo test herdr::protocol::tests --locked`
- Expected: snapshot, rename, richer event, existing metadata, and process-info protocol tests pass.
- Run: `cargo test --test listener socket_transport --locked`
- Expected: subscribe-first buffering and exact metadata/snapshot/rename requests pass on temporary Unix sockets.

**Implementation record:**
- Added strict default-off `TabNameConfig`, optional agent topology fields, minimal session snapshot/tab/layout values, exact `tab.rename`, missing-tab classification, and typed event context for pane/tab/workspace/label fields.
- Expanded subscriptions to the agreed 12 event types while preserving subscribe-before-RPC buffering and one socket per request. Existing default-disabled listener tests continue to use only metadata RPCs.
- Focused config/protocol/socket tests and the full Rust/shell/workflow/release validation set passed. Task-level review found no blocking/high issue; its missing-tab regression-test recommendation was added.

### Task 3: Durable Ownership and Restoration Policy

**Covers:** R8-R18, R19, D1, D4, D7-D11

**Objective:** A deterministic tab-name manager plans only durable, recoverable renames and implements focused-session ownership, manual overrides, baselines, reordering, moves, disable cleanup, and fail-closed recovery independently of socket and transcript details.

**Files:**
- Create: `src/tab_name/mod.rs`
- Create: `src/tab_name/state.rs`
- Modify: `src/lib.rs`
- Modify: `src/text.rs`
- Modify: `Cargo.toml`, `Cargo.lock`

**Dependencies:** Tasks 1-2 contracts.

**Implementation notes:**
- Begin with table-driven state-machine and persistence failures before implementing effects.
- Keep policy input agent-neutral: ordered tabs, focused pane per tab, pane-to-tab membership, stable terminal/session identity, optional display value, observed label, lifecycle/move/rename events, config state, and explicit time.
- Treat snapshot state as authoritative after reconnect and events as wake-ups/hints. Recover missed manual changes by digest comparison.
- Capture probable auto baseline only when the observed label equals the current ordered position. Restore its current position, acknowledging that Herdr stores it as custom.
- A manual rename updates the latest plaintext baseline and the current identity's plaintext override. It suppresses only that tab+identity; do not carry it through pane moves.
- Retain the last selected supported identity on non-agent focus only while that pane/session is still live in the same tab. Do not choose another unselected agent automatically.
- Persist a pending transition before returning a rename effect. On restart, prior/target/current digest classification must be deterministic without generated plaintext.
- Scope state filename by SHA-256 of the full socket path. Never serialize raw socket paths, generated titles, session IDs, or session paths.
- Use owner-only directory/file modes and same-directory atomic replacement. Unsupported version, malformed JSON, permission failure, or atomic write failure produces a tab-only disabled/error state without overwriting the source file.

**Test cases:**
- Initial/background tab whose focused pane has a resolved supported session → baseline captured and bounded generated rename planned; focused non-agent with no history → no rename.
- Two agents in one tab → focus A shows A, rapid focus A→B within 150 ms shows only B, shell focus retains B, B removal restores baseline rather than choosing A.
- All lifecycle statuses → same eligibility.
- Unexpected rename on A → exact manual label becomes baseline and A override; focus B shows B; return A restores A manual label; new identity in A is unsuppressed.
- Manual labels over 15 columns → stored/restored exactly; generated labels remain bounded.
- Pane moves to another tab → source and destination use independent baselines/overrides.
- Probable auto baseline tab reordered from position 1 to 3 → restore `3`; nonnumeric/custom baseline → restore exact string.
- Same identity parse failure → retain; different unresolved identity → restore baseline; later resolution → acquire new title.
- State write failure before rename → no rename effect. Finalization failure after API success → recoverable pending state remains.
- Restart with current digest equal prior, target, or neither → retry/finalize/manual classification respectively.
- Corrupt or future-version state → tab manager fail closed, file unchanged, no label/title leaked in errors.
- Two socket paths → different state files; serialized JSON contains baseline/manual strings but not synthetic generated title, raw identity, or socket path.
- Enabled-to-disabled → restore all non-manually-changed owned tabs and delete state only after successful cleanup; partial failure remains retryable.

**Complete when:**
- Every agreed ownership transition is represented by a deterministic unit test.
- No rename can be planned without a preceding durable transition.
- State restart/corruption/privacy behavior is tested without Herdr or real transcripts.

**Validation:**
- Run: `cargo test tab_name::tests --locked`
- Expected: focus, override, baseline, move, status, transition, and bounding policy tests pass.
- Run: `cargo test tab_name::state::tests --locked`
- Expected: atomic state, modes, socket scope, digest privacy, pending recovery, and corruption tests pass.

### Task 4: Runtime and Listener Lifecycle Integration

**Covers:** R2-R3, R8-R20, D4-D9, D11

**Objective:** Runtime combines backend outcomes and authoritative topology into visible, focus-debounced tab labels while preserving sidebar TTL, polling, reconnect, lock, and failure-isolation behavior.

**Files:**
- Modify: `src/runtime.rs`
- Modify: `src/main.rs`
- Modify: `src/backend.rs`
- Modify: `src/herdr/mod.rs`
- Modify: `tests/listener.rs`

**Dependencies:** Tasks 1-3.

**Implementation notes:**
- Initialize the tab manager from `HERDR_PLUGIN_STATE_DIR` and the full socket path after config is known. Missing/unusable state dir while enabled disables tab naming only and emits a content-free warning.
- Keep metadata reconciliation as the primary per-pane operation. Feed successful backend outcomes, including stable session identity and display value, into tab policy without persisting raw values.
- Request snapshots only while enabled or cleanup is pending. Apply `tab.rename` effects after durable planning; classify missing tabs as cleanup, and treat shared socket failures consistently with reconnect/full sync.
- Route `pane.focused` into a 150 ms trailing deadline. Other lifecycle/title events may reconcile immediately except metadata-only `pane_updated`; the periodic poll recovers title updates and missed events.
- Compute the event wait as the minimum of the absolute poll deadline and any tab debounce/cleanup deadline. Reset only the poll schedule when the poll is actually due.
- A valid config reload must trigger reconciliation promptly. Disabling enters cleanup mode even though normal acquisition is off. Invalid config leaves all timing and ownership unchanged.
- On reconnect, subscribe, load authoritative snapshot/list state, recover pending transitions, and then converge labels without treating plugin labels as manual.
- Keep tab errors isolated where possible: corrupt/local state disables tab naming; pane parse failure affects only that pane; metadata continues. Transport failure still crosses the reconnect boundary.

**Test cases:**
- Enabled runtime with focused Pi and Claude panes → sidebar reports full values and tabs receive separately bounded values from the same source.
- Background tabs use their own snapshot `focused_pane_id` on initial reconciliation.
- Focus event at t=0, another at t=100 ms, deadlines at t=150/250 ms → only second selection renamed at/after t=250 ms.
- Continuous events and focus debounce → original absolute 2-second poll still occurs at its deadline.
- `pane_updated` from metadata → no report/rename loop; title change appears on periodic poll.
- Manual `tab_renamed` event versus plugin expected event → suppress versus acknowledge correctly, including event-before-ack buffering.
- Config true→false → prompt restoration and state cleanup; invalid reload → no change.
- Listener restart with pending/applied state → full sync converges without overwriting an offline manual rename.
- Corrupt state or missing state dir → metadata reports continue and no tab rename requests occur.
- Pane close/move/tab close and `tab_not_found` race → stale state removed without failing unrelated panes.
- Existing reconnect and duplicate-listener tests → fresh metadata sequence epoch and one owner remain unchanged.

**Complete when:**
- Fake runtime/socket tests demonstrate the complete feature from snapshot/backend input to exact rename requests.
- Scheduler tests prove debounce and absolute polling coexist.
- Existing metadata TTL, clear retry, reconnect, and duplicate-listener behavior remains green.

**Validation:**
- Run: `cargo test --test listener tab_name --locked`
- Expected: focused-pane, manual override, restart, cleanup, and isolation integration tests pass.
- Run: `cargo test --bin herdr-agent-context --locked`
- Expected: config reload, debounce scheduling, absolute deadline, lock, and existing binary unit tests pass.
- Run: `cargo test --test listener listener_binary --locked`
- Expected: subprocess reconnect/full-sync/duplicate-owner tests pass with state-dir handling and no metadata regression.

### Task 5: Public Contract, Live Smoke, and Full Validation

**Covers:** R1-R20, D1-D12

**Objective:** Users can understand and safely enable the feature, release validation covers the new behavior, and the complete repository validation suite passes before the plan is archived.

**Files:**
- Modify: `README.md`
- Modify: `herdr-plugin.toml`
- Modify: `docs/release-checklist.md`
- Maintain/archive: `docs/plans/2026-08-20-herdr-tab-session-titles.md`

**Dependencies:** Tasks 1-4 complete and validated.

**Implementation notes:**
- Document one opt-in config example and state that default behavior remains sidebar-only.
- Call the Pi value a session name and the Claude value a session title while retaining the existing token spelling.
- Explain focus behavior, 150 ms debounce, non-agent retention, 15-column tab bound, manual override scope, baseline restoration, numeric auto-name limitation, and title/status independence.
- Document that Claude title generation requires `custom-title`/`ai-title`; before it appears the sidebar title is absent and the tab baseline remains.
- Replace the existing checked release-checklist assertion that Claude falls back through first-user text and cwd. Add unchecked gates for current `customTitle`, legacy `title`, latest `ai-title`, no-title empty sidebar/baseline tab behavior, and independent sidebar/tab bounds; mark them complete only after the new validation runs.
- State that user labels are persisted locally, generated values/identities are digested, corruption fails tab sync closed, config disable restores labels, and forced stop/disable/uninstall can leave the last label.
- Update the plugin description without changing version or package/release matrices.
- Add a disposable named-session smoke that does not use real transcripts or modify the default Herdr session. Validate Pi, Claude generated/custom title, multi-pane focus, shell focus retention, manual overrides, agent exit, config disable, and listener restart.
- Do not tag, publish, install integrations globally, or commit generated `target/`, `bin/`, or `dist/` content.

**Test cases:**
- README config copied into a temporary config → parses and remains disabled when table omitted.
- Disposable tab with Pi and Claude panes → source/title and focus behavior matches contract visually within debounce/poll bounds.
- Manual rename in Claude identity A, focus identity B, return A → B generated title then A manual label.
- Disable config → latest user baseline restored; force-stop note verified by behavior/documentation.
- Full Rust, shell, workflow, release-asset, format, lint, release build, and whitespace checks → all pass.

**Complete when:**
- Public docs contain no claim that Herdr can restore true auto naming or clean up after uninstall.
- `docs/release-checklist.md` no longer contains the obsolete Claude first-user/cwd title-fallback assertion, and every replacement gate reflects the implemented contract and actual rerun status.
- The disposable-session smoke is recorded as passing without exposing transcript/title values in committed evidence.
- Every Final Validation item succeeds, Requirement Coverage is current, and the plan is moved unchanged by name to `docs/plans/archived/`.

**Validation:**
- Run: `cargo test --all-targets --locked`
- Expected: all unit, integration, binary, protocol, parser, policy, persistence, and regression tests pass.
- Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
- Expected: formatting is clean and clippy reports no warnings.
- Run: `cargo build --release --locked`
- Expected: the release binary builds with Rust 1.85-compatible dependencies.
- Run: `sh tests/installer.sh && sh tests/release-assets.sh`
- Expected: installer and archive contracts remain unchanged and pass all positive/negative cases.
- Run: `actionlint .github/workflows/*.yml && shellcheck scripts/*.sh tests/*.sh && git diff --check`
- Expected: workflows and shell scripts pass static checks and no whitespace errors remain.
- Run: follow the new tab-name section in `docs/release-checklist.md` in a disposable named Herdr session.
- Expected: every listed focus/title/manual/restore/restart observation passes; only disposable resources are closed afterward.

## Requirement Coverage

| Requirement / Decision | Task | Verification |
|---|---|---|
| R1, D12 | Tasks 2, 5 | config default/strict reload tests; README parse example; no version diff |
| R2-R3, D5-D7 | Tasks 2, 3, 4 | snapshot/focus event tests; deterministic 150 ms policy and scheduler tests; background/non-agent cases |
| R4 | Tasks 1, 4 | existing Pi metadata tests plus runtime tab-name integration |
| R5-R7, D2-D3 | Tasks 1, 5 | current/legacy title parser tests; no-fallback cases; terminal match/mismatch backend tests; obsolete release-checklist fallback replaced |
| R8, D10 | Tasks 1, 3, 5 | independent-source Unicode scalar/grapheme/display-width tests; exact manual-label restoration tests; release-checklist bounds gate |
| R9 | Tasks 3, 4 | all-status eligibility table and runtime integration |
| R10-R12, D1, D8 | Tasks 3, 4 | manual override, identity return/change, baseline, reorder, and pane-move state-machine/runtime tests |
| R13 | Tasks 1, 3, 4 | same-identity failure retention and changed-unresolved identity restoration tests |
| R14-R16, D9 | Tasks 3, 4 | versioned owner-only state, digest privacy, pending transition, event attribution, restart, and corruption tests |
| R17-R18 | Tasks 3, 4, 5 | disable cleanup retry, closed-tab/move/snapshot convergence tests, documented forced-stop limitation |
| R19, D4, D11 | Tasks 1, 3, 4 | synthetic fixtures, content-free errors, state failure isolation, metadata continuation tests |
| R20 | Tasks 2, 4, 5 | protocol 19 fixtures, subscribe-first buffering, absolute poll, reconnect, lock, and full validation |

## Final Validation

- [ ] `cargo test claude::session::tests --locked` — Expected: current/legacy Claude title, no-fallback, branch, and activity tests pass.
- [ ] `cargo test text::tests --locked` — Expected: 80-scalar metadata, 15-column grapheme/display-width, and >80-scalar narrow-grapheme independent-derivation tests pass.
- [ ] `cargo test tab_name::tests --locked` — Expected: ownership, focus, override, baseline, move, transition, and recovery policy tests pass.
- [ ] `cargo test tab_name::state::tests --locked` — Expected: atomic state, permissions, digest privacy, socket scoping, pending recovery, and corruption tests pass.
- [ ] `cargo test --test listener tab_name --locked` — Expected: runtime/socket tab-name integration tests pass.
- [ ] `cargo test --all-targets --locked` — Expected: all Rust tests pass.
- [ ] `cargo fmt --check` — Expected: no formatting changes required.
- [ ] `cargo clippy --all-targets -- -D warnings` — Expected: no warnings.
- [ ] `cargo build --release --locked` — Expected: release build succeeds.
- [ ] `sh tests/installer.sh` — Expected: all installer positive and negative cases pass.
- [ ] `sh tests/release-assets.sh` — Expected: archive contract tests pass without version/matrix changes.
- [ ] `actionlint .github/workflows/*.yml` — Expected: workflow lint passes.
- [ ] `shellcheck scripts/*.sh tests/*.sh` — Expected: shell lint passes.
- [ ] `git diff --check` — Expected: no whitespace errors.
- [ ] Disposable named-session tab-name smoke in `docs/release-checklist.md` — Expected: generated/custom Claude title, Pi name, focus debounce, non-agent retention, manual override, exit restoration, config disable cleanup, and restart recovery all match the contract without using real transcript fixtures.
- [ ] Requirement Coverage has no unmatched requirement or decision.
- [ ] The plan and actual changed-file set agree, with minor differences recorded in the relevant task.
- [ ] After every item above succeeds, move this file without renaming it to `docs/plans/archived/2026-08-20-herdr-tab-session-titles.md`.

## Risks and Open Questions

### Risks

- Herdr cannot expose or restore true auto-named tab state. A restored positional number remains a custom label and may render with custom-label styling until Herdr adds a clear-label API.
- `tab.renamed` lacks an actor/source field. Digest-backed pending transitions make attribution recoverable but cannot distinguish an external actor deliberately writing the exact same expected label; that has no visible conflict.
- Claude JSONL is an evolving, best-effort format. Supporting `customTitle` plus legacy `title`, matching `sessionId`, and falling back from terminal title to verified JSONL title reduces but does not eliminate version risk.
- SHA-256 digests prevent plaintext persistence but do not make low-entropy generated titles cryptographically secret from an attacker who can already read the owner-only state file and guess candidate strings.
- Focus-driven `tab.rename` schedules Herdr session saves. The 150 ms debounce limits churn but cannot eliminate writes during deliberate repeated pane switching.
- A forced listener stop, plugin disable, uninstall, or unrecoverable corrupt state can leave the last custom tab label because no shutdown/uninstall hook or Herdr clear-label API exists.

### Open Questions

None. Public behavior, configuration, title precedence, focus timing, manual override scope, durable state/privacy, failure handling, compatibility, and cleanup limitations were resolved during design discussion.
