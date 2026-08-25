# Pane and Aggregate Tab Labels Implementation Plan

> **For implementers:** Execute tasks in order unless dependencies allow otherwise. Mark a task complete only after its validation succeeds. Record minor implementation differences in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Problem Statement

The listener can name a tab from the supported agent pane that has focus. A tab with several agents therefore changes its sidebar label on each focus change. The plugin does not assign labels to the individual panes, so split panes remain hard to identify.

## Goal

Give every resolved Pi or Claude pane its own session-derived label and give each tab one stable label that joins all resolved agent titles in visual pane order. Preserve manual names, durable ownership recovery, metadata polling deadlines, and the existing privacy boundary.

## Out of Scope

- Changing Pi or Claude title selection rules.
- Using `pane.report_metadata.title` instead of the persistent `pane.rename` API.
- Supporting agents other than Pi and Claude.
- Reporting inferred session paths or IDs to Herdr.
- Making label width or the ` + ` separator configurable.
- Changing sidebar token values or their 80-scalar limit.
- Editing archived plans to match the new behavior.
- Bumping package versions, creating tags, or publishing a release.

## Requirements and Decisions

### Requirements

- **R1:** Add independent, default-off pane naming through `[pane_name].enabled`. Keep `[tab_name].enabled` independent and default-off. Strict unknown-key rejection and atomic config reload behavior must remain unchanged.
- **R2:** Derive pane and tab components from the same complete source title already selected by each backend. Pi keeps explicit name, active-branch first user text, then cwd basename. Claude keeps verified custom or AI title and has no first-user or cwd fallback.
- **R3:** Generated pane labels and each generated tab component must occupy at most 20 terminal columns, include a final `…` when truncated, and preserve grapheme clusters. Sidebar values remain independently bounded to 80 Unicode scalars.
- **R4:** Use `pane.rename` to label each resolved supported pane. Do not use display metadata title overrides.
- **R5:** Build each automatic tab label from every supported pane in that tab that has a resolved source title. Order components by visual position, top to bottom and then left to right, retain duplicate titles, and join them with the literal separator ` + `. Do not apply another bound after joining the 20-column components.
- **R6:** Pane focus must not affect tab selection, tab component order, tab rename scheduling, or the absolute metadata polling deadline. Remove the 150 ms focus debounce from naming behavior.
- **R7:** A newly resolved session without a source title contributes no pane or tab generated label. If no tab component is available, keep or restore the tab baseline. A transient read or parse failure for an already known identity retains the last owned pane label and tab contribution without refreshing metadata TTL.
- **R8:** A manual pane rename, including clearing the label, overrides only the current session identity in that pane. Another session can acquire an automatic label; returning to the overridden identity restores its manual pane label. Pane manual labels never become tab components.
- **R9:** A manual tab rename overrides only the current ordered composition of contributing session identities. A different composition uses its generated aggregate; returning to the overridden composition restores the manual tab label. A manual rename with no contributing composition updates only the tab baseline.
- **R10:** When an owned session leaves, a pane closes, a pane moves, panes swap, a terminal identity is replaced, a tab composition changes, or either naming feature is disabled, restore or recompute only the affected labels. Preserve the latest manual baseline. Clearing an originally unnamed pane must restore an absent/null observed label rather than an empty generated title.
- **R11:** Persist recoverable ownership transitions before each rename RPC. On restart, distinguish not-applied, applied, and externally changed labels before retrying or accepting manual ownership. Never persist generated titles, raw session identities, or socket paths in plaintext.
- **R12:** Isolate pane ownership state from tab ownership state. A malformed or unavailable pane state store disables pane naming only; a tab state failure disables tab naming only; sidebar metadata continues in both cases. A malformed shared Herdr snapshot may disable both naming features without stopping metadata.
- **R13:** Preserve the current one-connection-per-request Herdr transport, subscribe-before-snapshot startup order, pre-acknowledgement event handling, missing-pane/tab classification, reconnect recovery, and no-loop handling for plugin-generated `pane_updated` events.
- **R14:** Document the new pane option, aggregate tab behavior, 20-column component rule, manual override scopes, persistence/privacy behavior, and live validation gates.

### Implementation Decisions

- **D1:** Call `pane.rename` with `label: string` for acquisition and omit the optional `label` member for clearing, matching Herdr 0.8.0 `PaneRenameParams`. Herdr exposes the resulting pane label as an optional field that deserializes to `None` when absent or null.
- **D2:** Keep tab and pane ownership managers separate. They may share a narrowly scoped secure-file primitive if extraction reduces duplication, but they must use separate state namespaces and independent failure states.
- **D3:** Sort layout panes by `(rect.y, rect.x, pane_id)`. The pane ID supplies a deterministic tie-breaker for malformed or equal coordinates. Focus does not participate in the key.
- **D4:** Compute a tab composition identity from the ordered sequence of domain-separated `(agent, session_identity)` identities. Preserve the existing identity digest for a singleton composition so existing one-session manual tab overrides and pending state remain valid after upgrade.
- **D5:** Keep the current tab state document compatible where possible. The stored selection anchor may refer to the first contributing pane while its identity digest represents the full ordered composition. Introduce a schema migration only if implementation proves the existing serialized contract cannot represent this safely; such a migration must preserve valid singleton state and receive focused tests.
- **D6:** Store pane state under `HERDR_PLUGIN_STATE_DIR/pane-name/<socket-digest>.json`, with the same 0700 directory, 0600 file, no-follow validation, atomic replace, file and directory sync, and socket scoping used for tab state.
- **D7:** Represent pane baseline and manual override labels as nullable values because Herdr panes can have no manual label. Digest null and non-null targets with distinct domain-separated encodings.
- **D8:** Continue ignoring `pane_updated` as an immediate reconcile trigger because metadata and pane rename reports emit it. The next periodic snapshot observes manual pane label changes without creating a reporting loop or postponing the absolute poll deadline. Topology, lifecycle, title, and config events may still request an immediate reconciliation under the existing event policy.

### Contracts

The public plugin configuration adds this optional table:

```toml
[pane_name]
enabled = true # default: false
```

The existing tab option remains independent:

```toml
[tab_name]
enabled = true # default: false
```

The generated label contract is:

```text
pane label        = bounded_20(complete_session_title)
tab component     = bounded_20(complete_session_title)
tab generated     = component_1 + " + " + component_2 + ...
component order   = ascending (rect.y, rect.x, pane_id)
```

A tab composition contains only resolved components. It retains duplicate session identities and duplicate display strings. The tab aggregate has no post-join width cap.

Pane rename transport accepts an optional label and verifies the returned pane ID and label:

```text
rename_pane(pane_id, Some(label)) -> request includes "label"; response label equals Some(label)
rename_pane(pane_id, None)        -> request omits "label"; response label equals None
```

Ownership state follows these invariants:

- Persist a pending transition before issuing `tab.rename` or `pane.rename`.
- Finalize the pending transition only after the response confirms the target or reports the target resource missing.
- Plaintext state may contain user-authored baselines and manual overrides. It may not contain generated titles, raw session identities, or raw socket paths.
- Tab and pane state failures have separate disable and cleanup paths.

## Current Context

### Confirmed

- `src/text.rs` currently bounds generated tab labels at 15 terminal columns and derives sidebar and tab values independently.
- `src/backend.rs::DisplayView` retains the complete backend title source in `tab_name_source`; both naming features can derive their own bounded labels from it.
- `src/tab_name/` owns tab baselines, session-scoped overrides, pending transitions, crash recovery, probable numeric baselines, and socket-scoped durable state.
- `src/runtime.rs` currently obtains one `session.snapshot` only while tab naming owns or cleans labels. It validates tab, layout, and pane relationships before handing snapshots to `TabNameManager`.
- Current `TabLayout` deserialization discards layout pane rectangles, and `SnapshotPane` discards the optional pane label. The Herdr v0.8.0 source contract, protocol 19, already includes `PaneLayoutPane.rect`, `PaneInfo.label: Option<String>`, `PaneRenameParams { pane_id, label: Option<String> }`, and `pane.rename`; no minimum-version increase or protocol fallback is required.
- `src/main.rs` treats focus as a naming deadline, ignores `pane_updated` to avoid metadata loops, and keeps an absolute polling schedule separate from event-driven reconciliations.
- Herdr v0.8.0 `pane.rename` returns pane information and clears a manual label when its request omits `label`; absent and JSON-null response labels both deserialize as `None`.
- Existing integration tests use a fake `HerdrApi` and temporary Unix socket fixtures. They do not require a live user Herdr session.

### Assumptions

- Internal type and helper names may change during implementation if their responsibilities and public behavior remain as specified.
- The pane state manager may reuse an extracted secure persistence helper or keep a small pane-specific wrapper around the existing pattern. State namespaces and failure isolation remain fixed.

## File Structure

- Create: `src/pane_name/mod.rs` — pane label ownership, manual override attribution, transition effects, and recovery.
- Create: `src/pane_name/state.rs` — nullable pane baseline/override schema and socket-scoped durable state.
- Modify: `src/lib.rs` — expose the pane naming module and any shared persistence module.
- Modify: `src/text.rs` — replace the 15-column tab helper with a shared 20-column context-label contract.
- Modify: `src/config.rs` — parse and expose independent `[pane_name].enabled` configuration.
- Modify: `src/herdr/protocol.rs` — deserialize pane labels and layout rectangles; encode nullable pane rename requests; parse pane rename responses.
- Modify: `src/herdr/mod.rs` — add the nullable pane rename operation and independent error/status contracts.
- Modify: `src/herdr/socket.rs` — send `pane.rename` through the existing one-request connection path.
- Modify: `src/tab_name/mod.rs` — replace focus selection with ordered composition selection while preserving ownership and recovery rules.
- Modify: `src/tab_name/state.rs` — adjust validation or migrate schema only if ordered composition cannot remain serialization-compatible.
- Modify: `src/runtime.rs` — build one validated naming snapshot, drive both managers, apply independent effects, and preserve failed display state.
- Modify: `src/main.rs` — remove focus deadlines and report pane/tab naming failures independently without changing poll deadlines.
- Modify: `tests/listener.rs` — cover runtime behavior and exact Unix socket RPC/event contracts for aggregate tabs and pane labels.
- Modify: `README.md` — document configuration, aggregate labels, pane labels, manual overrides, and privacy limits.
- Modify: `docs/release-checklist.md` — replace focus-driven tab checks and add default-off, ownership, recovery, and live pane checks.
- Modify: `herdr-plugin.toml` — mention optional pane labels in the user-facing description without changing the version.
- Optional create/modify: `src/name_state.rs` — shared secure state-file primitive only if used by both managers without coupling their schemas or failure states.

## Testing Decisions

- **TDD workflow:** For Tasks 1–4, add the listed behavior tests before implementation, run the task's Red command, and record the expected assertion, compile, or missing-behavior failure in that task's implementation notes. Then implement the minimum change for Green, refactor under passing focused tests, and run the task's full validation. A task cannot be complete without recorded Red evidence and a passing Green validation.
- **Test seam:** Test label formatting and each ownership manager as pure state transitions. Test runtime orchestration through `FakeApi`. Test JSON methods, response validation, event handling, and one-request socket behavior through the temporary Unix socket server in `tests/listener.rs`.
- **Behavior:** Use synthetic Pi and Claude transcripts. Cover two and three resolved panes, vertical and horizontal coordinates, duplicate titles, untitled Claude sessions, transient failures, manual rename and clear operations, composition changes, moves, swaps, close, disable, listener restart, and interrupted pending transitions.
- **Prior art:** Extend the ownership and recovery cases in `src/tab_name/mod.rs`, secure file checks in `src/tab_name/state.rs`, runtime fake tests and socket fixtures in `tests/listener.rs`, and the manual Herdr smoke gates in `docs/release-checklist.md`.
- **Avoid:** Do not assert private collection iteration order, plaintext generated labels in state fixtures, real user session paths, a running user Herdr socket, or focus timing that no longer belongs to naming.

## Progress

- [x] Task 1: Define the 20-column, configuration, and Herdr pane-label contracts
- [ ] Task 2: Convert tab ownership from focus selection to ordered composition ownership
- [ ] Task 3: Add durable session-scoped pane label ownership
- [ ] Task 4: Reconcile pane and aggregate tab labels without metadata loops
- [ ] Task 5: Publish the user contract and complete repository validation

Implementers must update this list only after each task's validation succeeds. Record minor file changes or implementation differences in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Tasks

### Task 1: Define the 20-column, configuration, and Herdr pane-label contracts

**Covers:** R1, R2, R3, R4, R13, D1

**Objective:** Expose independent pane configuration, one shared 20-column generated-label rule, and typed nullable pane rename/snapshot data before adding ownership behavior.

**Files:**
- Modify: `src/config.rs`
- Modify: `src/text.rs`
- Modify: `src/herdr/protocol.rs`
- Modify: `src/herdr/mod.rs`
- Modify: `src/herdr/socket.rs`
- Test: module tests in those files
- Test: `tests/listener.rs`

**Dependencies:** None.

**Implementation notes:**

- Add `PaneNameConfig { enabled: bool }` and `Config::pane_name`; parse only `enabled` under a deny-unknown-fields table.
- Replace `TAB_LABEL_WIDTH = 15` with a 20-column context-label contract used by both tab components and panes. Keep derivation from `complete_line`, not the 80-scalar sidebar result.
- Extend snapshot protocol types with nullable pane labels and layout pane rectangles. Retain fields required for strict workspace/tab/pane validation.
- Add a minimal typed pane rename result rather than coupling ownership code to every `PaneInfo` field.
- Encode `Some(label)` with a JSON `label` member and encode `None` by omitting that member, exactly matching Herdr v0.8.0 `PaneRenameParams`. Assert omission clears the label against the versioned protocol fixture and fake server.
- Keep socket requests on separate RPC connections and preserve current response ID and API error checks.

**Test cases:**

- Config omitted → both naming features remain disabled.
- `[pane_name] enabled = true` with tab disabled, and the inverse → independent values load.
- Unknown pane-name key → the whole config is rejected; reload tests retain the previous valid config.
- ASCII, CJK, combining sequences, and emoji at or over 20 display columns → exact width or grapheme-safe ellipsis; sidebar results remain unchanged.
- Snapshot JSON with `label: null` and positioned layout panes → typed values preserve null and coordinates.
- Pane rename with a string and with clear → exact `pane.rename` method/params, including an omitted `label` member for clear; mismatched returned pane ID or label fails validation.
- `pane_not_found` and `unknown_pane` → missing-pane classification remains distinct from transient errors.

**Complete when:**

- Configuration, text, protocol, and transport contracts compile and their focused tests pass.
- Existing tab calls and metadata reports remain byte-compatible except for the intentional 20-column output change.

**Implementation record (2026-08-25):**

- Changed `src/config.rs`, `src/text.rs`, `src/herdr/protocol.rs`, `src/herdr/mod.rs`, `src/herdr/socket.rs`, `src/tab_name/mod.rs`, `tests/listener.rs`, `README.md`, and `docs/release-checklist.md`.
- Red evidence: config failed because `Config::pane_name` was absent; text failed because `context_label` was absent; protocol failed because pane labels, layout rectangles, and pane rename helpers were absent; socket integration failed because `rename_pane` and response mismatch checks were absent. Commit review also exposed the old 15-column tab expectation as a failing regression test.
- Green evidence: Task 1 validation, all 24 existing tab ownership tests, all 33 listener integration tests, formatting, and whitespace checks passed. The harness PATH omitted Cargo, so commands used the equivalent absolute binary `$HOME/.cargo/bin/cargo`.
- Documentation impact: updated the existing tab width statements from 15 to 20 columns. Pane and aggregate behavior remain assigned to Task 5 after those features exist.

**Validation:**

- Red: `cargo test config::tests --locked && cargo test text::tests --locked && cargo test herdr::protocol::tests --locked && cargo test --test listener --locked`
- Expected before implementation: at least one new assertion or compile check fails because pane config, 20-column labels, protocol-19 pane fields, or pane rename transport is absent. Record the failing case in this task.
- Green: `cargo test config::tests --locked && cargo test text::tests --locked && cargo test herdr::protocol::tests --locked && cargo test herdr::tests --locked && cargo test --test listener --locked`
- Expected after implementation: all focused unit and socket integration tests pass, including pane config, 20-column Unicode, v0.8.0/protocol-19 fixtures, omitted-label clear requests, response validation, and error classification.

### Task 2: Convert tab ownership from focus selection to ordered composition ownership

**Covers:** R3, R5, R6, R7, R9, R10, R11, D3, D4, D5

**Objective:** Give each tab one stable aggregate based on its resolved agent panes and preserve manual overrides per ordered composition.

**Files:**
- Modify: `src/tab_name/mod.rs`
- Modify: `src/tab_name/state.rs` only if validation or an explicit compatible migration requires it
- Modify: `src/runtime.rs` — switch tab-only orchestration from focused selection to positioned compositions
- Modify: `src/main.rs` — remove focus deadlines and callers in the same compilable change
- Modify: `tests/listener.rs` — replace focus-driven tab runtime cases with aggregate tab cases
- Test: module tests in `src/tab_name/mod.rs` and `src/tab_name/state.rs`

**Dependencies:** Task 1.

**Implementation notes:**

- Replace focused-pane selection and debounce deadlines with an ordered list of contributing pane contexts supplied by runtime.
- Build each component through the shared 20-column helper, join with ` + `, and do not truncate the aggregate.
- Use `(rect.y, rect.x, pane_id)` order prepared from the authoritative layout. Exclude unsupported and unresolved panes. Retain duplicates.
- Preserve the last generated component for a known failed identity. Do not turn failure retention into metadata TTL refresh.
- Use the existing singleton digest formula for one contributor. Use a domain-separated, length-framed digest for two or more ordered identities so composition boundaries cannot collide.
- Attribute manual tab observations to the active composition digest. Keep a manual rename with no composition as baseline-only.
- Preserve pending-before-RPC persistence, expected self-event handling, probable numeric baseline restoration, missing-tab completion, and restart reconciliation.
- Remove obsolete focus queues, deadlines, runtime/listener callers, and retention rules in the same task instead of leaving compatibility shims.
- Wire the aggregate tab manager through runtime before this task's Green validation. Task 4 will extend the already compiling tab-only path with pane ownership and shared snapshot application.

**Test cases:**

- One pane → its 20-column component becomes the tab label and valid existing singleton override state still applies.
- Two or three panes with vertical/horizontal coordinates → labels join in top-to-bottom, then left-to-right order.
- Equal coordinates → pane ID tie-break produces deterministic output.
- Duplicate title or duplicate session identity in separate panes → both entries remain.
- Focus changes across supported and shell panes → no effect and no rename.
- One untitled new Claude pane beside a titled Pi pane → only the Pi component appears; no titled panes → baseline remains.
- Known transient failure → prior component remains; resolved title change replaces it.
- Add, close, move, or swap a contributing pane → only the affected tab compositions change.
- Manual aggregate rename → current ordered composition restores it; reordered, added, or removed composition uses its own generated/override value.
- Pending tab rename at process death → restart handles prior, target, and externally changed observations without losing manual baseline.
- Disable → latest baseline restoration still handles probable numeric labels and tab reordering.

**Complete when:**

- Tab ownership has no focus deadline or focus-dependent selection path.
- Aggregate, manual override, migration compatibility, recovery, and cleanup tests pass.
- Existing privacy assertions still find no generated title or raw session identity in state.

**Validation:**

- Red: `cargo test tab_name::tests --locked && cargo test --test listener --locked`
- Expected before implementation: new aggregate ordering, focus-invariance, or runtime cases fail against focused selection. Record the failing case in this task.
- Green: `cargo test tab_name::tests --locked && cargo test tab_name::state::tests --locked && cargo test --test listener --locked`
- Expected after implementation: tab ownership, state safety, runtime integration, aggregate ordering, manual composition, and restart recovery cases pass with no focus-debounce API or caller left.

### Task 3: Add durable session-scoped pane label ownership

**Covers:** R4, R7, R8, R10, R11, R12, D2, D6, D7

**Objective:** Manage each pane's automatic, manual, baseline, and pending labels without coupling pane failure to tab or sidebar behavior.

**Files:**
- Create: `src/pane_name/mod.rs`
- Create: `src/pane_name/state.rs`
- Modify: `src/lib.rs`
- Optional create/modify: `src/name_state.rs`
- Test: module tests in the new ownership/state files
- Test: existing tab state tests if a persistence primitive moves

**Dependencies:** Task 1. May proceed in parallel with Task 2 after Task 1.

**Implementation notes:**

- Model observed and target labels as `Option<String>`. Preserve manual clear as a real session override rather than treating it as absence of an override.
- Scope overrides to pane-local session identity digests. A session change selects a new generated/override target; returning to an identity restores its override.
- Keep pane manual values independent from generated tab components.
- Record enough terminal ownership evidence to avoid carrying an automatic label onto a replacement terminal without first reconciling the observed baseline and session identity.
- Restore baseline for unsupported, unbound, or removed ownership while the pane exists. Treat a missing pane as completed cleanup and remove its state.
- Persist a pending transition before each effect. Recover prior, target, and third-party observations after restart using nullable label digests.
- Use an independent `pane-name` directory and state schema. Reuse secure file mechanics only if tab and pane schemas, cleanup flags, and error paths remain independent.

**Test cases:**

- Unnamed pane plus resolved session → generated label; session leaves or feature disables → clear to null.
- Pre-named pane plus resolved session → generated label; release → exact original manual baseline.
- Manual rename for identity A → retained for A; identity B receives generated B; returning to A restores its manual value.
- Manual clear for identity A → null remains A's override; B still receives its generated value.
- Manual pane name never changes the tab component source fixture.
- Untitled identity without override → baseline; later title acquisition → generated label.
- Known read failure → current owned label remains without a rename effect.
- Pane close during pending rename → missing-pane completion removes ownership state.
- Terminal replacement using the same pane ID → old ownership does not rename the replacement from stale session state.
- Process death before and after pane RPC → restart distinguishes prior, target, and external labels.
- Unsafe permissions, symlink substitution, malformed JSON, wrong socket digest, unsupported version, and failed sync/rename → pane manager fails closed without exposing generated text.
- Pane state failure → tab manager fixture remains usable; tab state failure → pane manager fixture remains usable.

**Complete when:**

- Pane ownership and secure persistence tests cover nullable baselines, per-session overrides, cleanup, and crash recovery.
- State fixtures contain manual text only where allowed and contain no plaintext generated title, session identity, or socket path.

**Validation:**

- Red: `cargo test pane_name::tests --locked`
- Expected before implementation: the new pane ownership tests fail to compile or fail their first acquisition/manual-override assertion because the manager does not exist. Record the failing case in this task.
- Green: `cargo test pane_name::tests --locked && cargo test pane_name::state::tests --locked && cargo test tab_name::state::tests --locked`
- Expected after implementation: pane ownership, isolation, persistence safety, and any shared state-file regression tests pass.

### Task 4: Reconcile pane and aggregate tab labels without metadata loops

**Covers:** R5, R6, R7, R8, R10, R12, R13, D3, D8

**Objective:** Drive both ownership managers from one authoritative snapshot and apply independent rename effects while preserving metadata TTL and listener scheduling.

**Files:**
- Modify: `src/runtime.rs`
- Modify: `src/main.rs`
- Modify: `tests/listener.rs`
- Test: runtime module tests, listener integration tests, and temporary socket tests

**Dependencies:** Tasks 2 and 3. Task 2 has already removed focus APIs and wired aggregate tab-only runtime behavior; this task adds pane ownership and combines both managers around one snapshot.

**Implementation notes:**

- Request `session.snapshot` when either manager is enabled or has owned cleanup pending; do not request it when both are disabled and clean.
- Validate tabs, layouts, pane coordinates, pane labels, and workspace/tab membership once. Build visual-order pane inputs for tabs and observed-label pane inputs for pane ownership.
- Keep tab and pane manager errors/status flags separate. A shared malformed snapshot can disable both managers, but neither failure may abort already valid sidebar metadata reporting.
- Derive naming contexts from current backend outcomes. Use retained runtime pane identity only for the existing known-failure path; do not infer a different transcript.
- Apply and confirm pane and tab effects through their matching APIs. Classify missing resources as completed cleanup; propagate transient transport errors for reconnect/retry.
- Keep the focus APIs, scheduler wakeups, and focus-event special handling removed by Task 2. Focus events remain non-reconciling after pane ownership joins the runtime path.
- Keep `pane_updated` non-reconciling so metadata and pane rename reports cannot loop. Periodic snapshots observe user pane renames. `tab_renamed` expectation handling remains immediate enough to distinguish plugin and user changes.
- Keep the absolute poll deadline independent of event reconciliations and config reloads.

**Test cases:**

- Both features omitted/disabled and no cleanup → metadata RPCs occur without `session.snapshot`, `tab.rename`, or `pane.rename`.
- Pane-only, tab-only, and both enabled → one snapshot per reconciliation and only enabled/owned manager effects.
- Two panes in one tab with coordinates and synthetic Pi/Claude titles → two pane rename calls and one ordered aggregate tab rename.
- Repeated focus events → no immediate snapshot or rename and no polling deadline extension.
- Plugin `pane.report_metadata` and `pane.rename` produce `pane_updated` → no immediate reporting or rename loop.
- User pane rename/clear observed at the next poll → manager records the current identity override before issuing any replacement effect.
- Manual tab event for current aggregate → expected plugin event is ignored; external label becomes the composition override.
- Pane move/swap/layout event → source and destination aggregates update in layout order; pane session override follows its pane and identity.
- Pane close, tab close, terminal replacement, config disable, socket reconnect, and listener restart → exact baseline cleanup and fresh full synchronization.
- Malformed pane state → pane status reports disabled while aggregate tabs and sidebar continue; malformed tab state gives the inverse.
- Malformed shared topology → naming stops while the metadata already reported in that reconciliation remains valid.
- Exact socket fixture → `pane.rename` uses one connection per request and omits `label` for a clear request; subscription is established before `agent.list`; pre-ack events remain preserved.

**Complete when:**

- Runtime and listener integration tests prove stable focus-independent tabs, independently owned panes, no metadata loop, and independent disable paths.
- No focus-debounce scheduler API or test remains.

**Validation:**

- Red: `cargo test --test listener --locked && cargo test runtime::tests --locked`
- Expected before implementation: new pane-only, combined-manager, manual-pane, or no-loop runtime cases fail. Record the failing case in this task.
- Green: `cargo test --test listener --locked && cargo test runtime::tests --locked`
- Expected after implementation: fake API and temporary socket integration tests pass for all feature combinations, topology/lifecycle changes, reconnects, and no-loop scheduling.

### Task 5: Publish the user contract and complete repository validation

**Covers:** R1, R3, R5, R6, R8, R9, R11, R12, R14

**Objective:** Make the new behavior installable and understandable, then validate the complete release contract without publishing it.

**Files:**
- Modify: `README.md`
- Modify: `docs/release-checklist.md`
- Modify: `herdr-plugin.toml`
- Modify: this plan's Progress, task notes, and Final Validation status during implementation
- Move after all validation: `docs/plans/2026-08-25-pane-and-aggregate-tab-labels.md` to `docs/plans/archived/2026-08-25-pane-and-aggregate-tab-labels.md`

**Dependencies:** Task 4.

**Implementation notes:**

- Replace focus-driven tab prose with aggregate composition, visual ordering, duplicate retention, separator, and no post-join cap.
- State that each generated component and pane label has a 20-column bound.
- Add the independent pane config option, manual pane clear/override scope, manual tab composition scope, baseline restoration, force-stop limitation, and separate state privacy/failure behavior.
- Update the plugin description to mention optional tab and pane labels without changing its version.
- Rewrite release checklist cases that currently assert 15 columns, focused-pane selection, debounce, and shell focus retention. Add pane default-off, manual clear, restart recovery, move/swap, and independent failure checks.
- Use synthetic transcripts, temporary Unix sockets, and an isolated disposable Herdr session for live checks. Never rename panes or tabs in the user's active session.

**Test cases:**

- README example enables tab and pane independently and the option table lists both defaults.
- Release checklist distinguishes default-off checks from opt-in tab and pane checks.
- Live disposable tab with two supported panes → each pane shows its own 20-column title and the tab shows both in visual order.
- Focus switches → pane labels and tab aggregate stay unchanged.
- Manual pane rename and clear, manual aggregate tab rename, composition change, disable, restart, move, and swap → documented ownership rules hold.
- State inspection → only permitted manual labels appear in plaintext.

**Complete when:**

- Public docs and manifest match tested behavior.
- All automated validation passes.
- The isolated manual smoke passes and cleans up its listener, transcripts, panes, server, state, and config directories.
- The plan moves to `docs/plans/archived/` only after every Final Validation item succeeds.

**Validation:**

- Run: `cargo test --all-targets --locked && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo build --release --locked && sh tests/installer.sh && sh tests/release-assets.sh && actionlint .github/workflows/*.yml && shellcheck scripts/*.sh tests/*.sh && git diff --check`
- Expected: every command exits 0; no generated `bin/`, `target/`, or `dist/` content is committed.
- Run: follow the updated optional tab/pane label section in `docs/release-checklist.md` against an isolated disposable Herdr session.
- Expected: every applicable smoke item passes and all temporary resources are removed.

## Requirement Coverage

| Requirement / Decision | Task | Verification |
|---|---|---|
| R1 independent default-off config | Task 1, Task 5 | Config parse/reload cases; README option table |
| R2 unchanged backend title sources | Task 1 | Complete-title formatting tests and existing Pi/Claude suites in final validation |
| R3 20-column grapheme-safe components | Task 1, Task 2, Task 3 | Unicode unit tests; aggregate and pane manager tests |
| R4 actual pane rename API | Task 1, Task 3, Task 4 | Protocol payload tests; socket fixture and ownership tests |
| R5 ordered aggregate with duplicates and no final cap | Task 2, Task 4 | Coordinate ordering, duplicate, three-pane runtime cases |
| R6 no focus-driven naming | Task 2, Task 4 | Focus invariance and scheduler tests; removal of deadline API |
| R7 unresolved and failed behavior | Task 2, Task 3, Task 4 | Untitled Claude and known-failure retention tests |
| R8 session-scoped manual pane overrides | Task 3, Task 4 | Rename, clear, session switch, and return tests |
| R9 composition-scoped manual tab overrides | Task 2, Task 4 | Manual aggregate, reorder, add/remove, and return tests |
| R10 lifecycle and disable cleanup | Task 2, Task 3, Task 4 | Move, swap, close, replacement, disable, and missing-resource tests |
| R11 durable recovery and privacy | Task 2, Task 3, Task 5 | Pending transition restart matrices and state plaintext assertions |
| R12 failure isolation | Task 3, Task 4 | Independent malformed state and shared topology tests |
| R13 transport, startup, reconnect, and no-loop guarantees | Task 1, Task 4 | Exact Unix socket and listener scheduling tests |
| R14 documentation and release gates | Task 5 | README/manifest review and updated live smoke |
| D1 optional `pane.rename` label | Task 1, Task 3 | Omitted-member clear payload and null-baseline tests |
| D2 separate ownership managers | Task 3, Task 4 | Separate state paths and disable status tests |
| D3 visual ordering | Task 2, Task 4 | `(y, x, pane_id)` ordering cases |
| D4 composition digest with singleton compatibility | Task 2 | Existing singleton state and multi-identity digest tests |
| D5 compatible tab persistence | Task 2 | Existing fixture load plus migration test if schema changes |
| D6 secure pane state namespace | Task 3 | Permission, symlink, socket digest, and atomic-write tests |
| D7 nullable pane ownership values | Task 3 | Manual clear, unnamed baseline, and restart tests |
| D8 periodic pane rename observation and no loop | Task 4 | `pane_updated` suppression and next-poll override tests |

## Final Validation

- [ ] `cargo test text::tests --locked` — Expected: 80-scalar sidebar behavior and 20-column grapheme-safe label behavior pass independently.
- [ ] `cargo test tab_name::tests --locked` — Expected: aggregate ordering, composition override, lifecycle, and recovery tests pass with no focus dependency.
- [ ] `cargo test pane_name::tests --locked` — Expected: nullable baseline, manual override, cleanup, and recovery tests pass.
- [ ] `cargo test --test listener --locked` — Expected: runtime and Unix socket behavior passes without focus or metadata loops.
- [ ] `cargo test --all-targets --locked` — Expected: all Rust unit and integration tests pass.
- [ ] `cargo fmt --check` — Expected: no formatting differences.
- [ ] `cargo clippy --all-targets -- -D warnings` — Expected: no warnings.
- [ ] `cargo build --release --locked` — Expected: the release binary builds.
- [ ] `sh tests/installer.sh` — Expected: installer positive and negative cases pass.
- [ ] `sh tests/release-assets.sh` — Expected: release asset contract cases pass.
- [ ] `actionlint .github/workflows/*.yml` — Expected: workflow files pass validation.
- [ ] `shellcheck scripts/*.sh tests/*.sh` — Expected: shell scripts pass linting.
- [ ] `git diff --check` — Expected: no whitespace errors.
- [ ] Follow the updated optional tab/pane label smoke in `docs/release-checklist.md` using an isolated disposable Herdr session — Expected: aggregate, pane, manual, disable, restart, move, and cleanup cases pass without touching the active user session.
- [ ] Requirement Coverage has no missing requirement or decision.
- [ ] The plan and actual changed files agree; record any minor implementation differences in the relevant task.
- [ ] After every item above succeeds, move this file without renaming it to `docs/plans/archived/2026-08-25-pane-and-aggregate-tab-labels.md`.

## Risks and Open Questions

- Herdr emits `pane_updated` for both metadata and label changes. Immediate reconciliation would create a plugin feedback loop, so manual pane override adoption occurs on the next periodic snapshot.
- Long aggregates may exceed the visible tab or sidebar width by design. Herdr controls clipping and scrolling; the plugin preserves every 20-column component in the stored tab label.
- Layout coordinates can change on resize without semantic pane reordering. Sorting by top-left coordinate and pane ID keeps order deterministic, but integration tests must cover nested horizontal and vertical layouts.
- Existing tab ownership state was designed around one selected pane. The implementation must prove singleton serialization compatibility before reusing the selection shape; otherwise it must add and test an explicit migration without dropping valid manual baselines or overrides.
- No unresolved product questions remain.
