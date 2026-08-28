# Codex Agent Context Implementation Plan

> **For implementers:** Execute tasks in order unless dependencies allow otherwise. Use red → green vertical slices at the agreed test seams. Mark a task complete only after its validation succeeds. Reflect minor implementation differences in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Problem Statement

`herdr-agent-context` reports privacy-bounded Pi and Claude Code session context, but Herdr panes detected as `codex` are unsupported and any previously reported plugin metadata is cleared. Users cannot see a Codex session name or recent assistant response in the sidebar, automatic tab labels, or automatic pane labels.

Codex CLI persists active interactive sessions as nested rollout JSONL files and stores optional thread names in a separate append-only index. Exact Herdr integration identity is available when installed, while hook-free attribution must avoid assigning the wrong transcript when several candidates or panes share a cwd. The rollout format is an internal Codex format rather than a stable public API, so parsing must be version-bounded, conservative, and isolated from Pi, Claude, and Herdr transport behavior.

## Goal

Add a statically compiled Codex backend that:

- reports a resolved Codex session name and latest assistant text through the existing sidebar metadata tokens;
- contributes the same resolved name to automatic tab and pane labels;
- prioritizes official Herdr session IDs, then exact `codex resume <UUID>` hints, then a conservative hook-free local fallback;
- supports active persistent interactive Codex TUI sessions without requiring an integration or changing user settings;
- preserves all Pi, Claude, naming ownership, TTL, polling, privacy, packaging, and release behavior.

## Out of Scope

- `codex exec`, review, remote, ephemeral, subagent, internal, MCP, VS Code, app-server, or other non-interactive/non-root session sources.
- Archived sessions, compressed rollouts, SQLite-selected rollout heads, or files outside active `sessions/**/*.jsonl` roots.
- Treating a `codex fork <UUID>` parent UUID as the new child session identity.
- Binding `codex resume <name>` or `codex resume --last` directly from process arguments.
- Guessing among multiple same-cwd panes, multiple eligible candidates, duplicate active rollouts for one identity, or malformed identity evidence.
- Writing inferred rollout paths or IDs through `pane.report_agent_session`, `pane.report_agent`, or any canonical Herdr identity API.
- Persisting inferred bindings, rollout text, or session names outside existing Herdr metadata and naming ownership state.
- A dynamic backend ABI, external backend scripts, or refactoring Pi/Claude beyond the minimum common-registry changes.
- Package version changes, tags, releases, integration installation, or edits to Codex/Herdr settings.

## Requirements and Decisions

### Requirements

- **R1:** Treat `codex` as a supported backend in the existing static registry and report through `agent_context_session_name` and `agent_context_last_message`. A resolved Codex name must also drive the existing automatic tab and pane label paths when enabled.
- **R2:** Support persistent root interactive TUI sessions when process metadata is sufficient to establish an eligible Codex command: normal starts, UUID resume, and fork after the child identity is observable. Missing or truncated process metadata is unbound, and the modes and sources listed in Out of Scope are excluded.
- **R3:** Apply binding precedence: valid official Herdr `agent_session { agent: "codex", kind: "id" }`; structured `codex resume <UUID>`; existing valid in-memory sticky binding; one uniquely changed/new same-cwd rollout after pane observation; otherwise unbound.
- **R4:** Never perform ordinary fallback when more than one Codex pane has the same canonical cwd, when candidate evidence is not unique, or while a Codex authoritative reference exists but is missing, malformed, ambiguous, unreadable, or identity-incompatible.
- **R5:** Resolve the display name as the latest nonblank `thread_name` for the thread ID, then the first genuine user message, then the session cwd basename. Bound sidebar values remain one line and at most 80 Unicode scalars; tab/pane labels continue using the existing 20-column grapheme-safe bound.
- **R6:** Resolve recent activity as the latest eligible assistant text after the latest genuine user message, including commentary and final-answer messages. Exclude reasoning, developer/system records, tool calls/results, task-completion echoes, and nontext content. If a new user message has no replacement assistant text, return no replacement so runtime retains the prior activity for that same session only.
- **R7:** Parse canonical identity from the first valid root `session_meta` thread ID and validate filename identity, cwd, root `cli` source, and required structure. Use the latest valid `turn_context` cwd when present, otherwise canonical session metadata cwd. Do not let copied later metadata in fork history replace the child identity.
- **R8:** Limit ordinary discovery to active non-symlink JSONL rollouts beneath controlled year/month/day directories. Use the existing conservative discovery budget of files modified within 30 days and at most 25 compatible candidates per relevant cwd; exact official IDs, exact UUID hints, and existing sticky paths bypass age/count limits but not root, type, identity, or cwd validation.
- **R9:** Resolve the primary Codex home from listener-level `CODEX_HOME`, otherwise `~/.codex`. Accept additional active rollout roots through `[agents.codex].session_dirs`, normalize and deduplicate them like existing agent roots, and locate a configured root's optional `session_index.jsonl` beside its `sessions` directory. Invalid relative paths and unknown fields reject the complete config atomically.
- **R10:** Treat an incomplete trailing rollout record as retryable. A failed rollout read/parse may retain in-memory display state but must not report a refresh or extend TTL. Ignore only an incomplete trailing `session_index.jsonl` line, use the last completed nonblank exact-ID name or the agreed user/cwd fallback, and continue the normal rollout/activity refresh. Any completed malformed index line invalidates the explicit-name tier for that refresh and uses the user/cwd fallback while still refreshing rollout activity and TTL. Completed malformed required rollout structure, invalid UUID/cwd/source, root escape, symlink, duplicate exact identity, or unknown incompatible history contract fails closed for that pane without blocking other backends.
- **R11:** Only successful official identity carries `applies_to_source`. Exact process hints and local fallback remain visual-only and never claim an official source.
- **R12:** Preserve terminal replacement invalidation, binding replacement isolation, metadata clear retry, sequence epoch, reconnect, absolute polling deadlines, `pane_updated` loop prevention, manual naming overrides, and Pi/Claude behavior.
- **R13:** Use only synthetic UUIDs, paths, names, and transcript text in tests. Production logs may retain the repository's existing pane-ID/path/error-category policy, but must not contain session names, user/assistant text, process environments, or full process arguments.
- **R14:** Describe support as best effort for the current rollout structure and verified against Codex CLI `0.149.1`. Safely ignorable unknown records may be skipped; incompatible required structure must fail closed. Do not enforce an exact `cli_version` gate that rejects otherwise compatible resumed sessions.

### Implementation Decisions

- **D1:** Add `src/codex/` as an independent parser/resolver/backend and compile it into `BackendRegistry`; keep Codex transcript details out of `runtime.rs` and Herdr transport.
- **D2:** Use `DisplayView` and `BackendOutcome` as the shared display/lifecycle contract. Extend common binding lookup and naming contributor identity only where Codex's ID-based identity requires it.
- **D3:** Use newline-terminated JSONL order as the activity order. Prefer Codex agent/user event records that expose commentary/final phases and deliberately ignore duplicate persistence representations so one logical response is not selected twice.
- **D4:** Parse `session_index.jsonl` independently from rollout validity. The latest completed nonblank entry for an exact thread ID wins. Ignore an incomplete trailing line and continue from the completed prefix. A missing/unreadable index or any completed malformed line invalidates the explicit-name tier for that refresh and selects the user/cwd fallback. No index failure blocks an otherwise valid rollout refresh or TTL update.
- **D5:** Establish an ordinary-scan baseline before hook-free binding. A cold listener does not immediately attach an old newest transcript; exactly one compatible candidate must become new/changed after observation. A valid sticky binding remains until invalidated and never reshuffles solely because another pane exists.
- **D6:** Cache only bounded structural/display results keyed by file fingerprints. Changes to either a bound rollout or its relevant name index must be observable on the next poll; an index-only rename must refresh sidebar, tab, and pane display even when the rollout fingerprint is unchanged.
- **D7:** Reuse existing text bounding, metadata reporting, tab/pane naming, config reload, and scan-limit patterns rather than creating Codex-specific copies of shared lifecycle behavior.

### Contracts

#### Display

| Value | Codex precedence |
|---|---|
| `agent_context_session_name` | latest nonblank indexed `thread_name` → first genuine user text → effective cwd basename |
| `agent_context_last_message` | latest eligible commentary/final assistant text after the latest genuine user input |
| automatic tab/pane component | same resolved source as `agent_context_session_name` |

- Sidebar values are trimmed to the first nonblank line and bounded to 80 Unicode scalars including any ellipsis.
- Existing naming managers bound the same source to 20 terminal columns and preserve manual overrides.
- A new user input yields no replacement activity until an eligible assistant message appears; `Runtime` retains the previous value only when terminal, agent, binding, and session identity are unchanged.
- A terminal, backend, binding, or session change never carries the previous Codex activity or name.

#### Binding state

1. A matching official Codex `kind=id` reference is authoritative and blocks every lower level even when resolution fails.
2. A structured `codex resume <UUID>` argument is an exact local hint; names and `--last` are not exact hints.
3. A valid sticky path may be reused after root/cwd/identity revalidation.
4. Hook-free fallback may bind only after baseline observation, for one Codex pane in that cwd, when exactly one compatible candidate is new or changed.
5. Every other case is unbound or failed; no sorted arbitrary assignment is allowed.

An official binding reports `applies_to_source`; exact and fallback bindings do not. Inferred identity is never sent to a canonical Herdr API.

#### Rollout and index inputs

- Active rollout shape is `<root>/YYYY/MM/DD/rollout-<timestamp>-<thread-id>[ _<rollout-id>].jsonl`, where `<root>` is the primary `<CODEX_HOME>/sessions` or a configured active session root.
- The first canonical root `session_meta` supplies the thread identity and metadata fallback cwd. A latest valid `turn_context` may update effective cwd without changing identity.
- Only root interactive `cli` source is eligible for local fallback. Missing/unknown source does not default to another source.
- `session_index.jsonl` entries are matched by exact thread UUID; later nonblank names replace earlier names.
- A partial final rollout line is retryable and blocks that refresh. A partial trailing name-index line is skipped while completed prefix entries and the valid rollout continue to refresh normally. A completed malformed name-index line invalidates the explicit-name tier for that refresh, selects the user/cwd fallback, and still refreshes the valid rollout normally. Broken completed required rollout records fail the refresh. Unknown records are ignored only when they cannot affect identity, cwd, genuine-user boundaries, assistant text, or history semantics.

#### Configuration

```toml
[agents.codex]
session_dirs = ["~/additional/codex/sessions"]
```

- `CODEX_HOME/sessions` or `~/.codex/sessions` is always the primary active root.
- Additional roots are active `sessions` directories. Their parent directory's `session_index.jsonl` is the optional explicit-name index; a missing or unreadable index leaves rollout fallback naming available.
- Existing `[agents.pi]`, `[agents.claude]`, and legacy `pi_session_dirs` behavior remains unchanged.

## Current Context

### Confirmed

- Repository baseline is clean `main` at `9b08d9e63533740d798d8f7930f91a01fde44bd3`, equal to `origin/main` when planning began.
- `BackendRegistry` currently contains only `PiBackend` and `ClaudeBackend`; `supports_agent` filters unknown agents before process inspection and reconciliation.
- `Runtime` already maps generic `BackendOutcome` values to metadata TTL/clear behavior and backend-neutral tab/pane naming contexts.
- `Runtime::report_view` retains prior activity only for the same terminal, agent, binding path, and session identity.
- Codex official Herdr integration v8 reports a UUID session ID as `agent_session.kind == "id"`; installed Herdr is `0.8.2`.
- Installed Codex is `codex-cli 0.149.1`. Public OpenAI source tag `rust-v0.149.1` at commit `ff29a44391deccde0aba0f8390337d7f3c319ea4` defines rollout JSONL, session metadata, session index, resume/fork, and session-source structures.
- Codex rollout JSONL is not a stable versioned external API. The compatibility statement must therefore be best effort and fail closed on required-structure changes.
- Existing Claude discovery already supplies the 30-day/25-compatible-candidate bounded-scan pattern, sticky fingerprints, exact identity, cwd validation, and conservative multi-pane behavior that Codex can adapt without sharing parser code.
- Repository validation commands are defined in `AGENTS.md`; no new dependency is known to be required.

### Assumptions

- Private Rust type and helper names may change to match existing conventions if the responsibilities and observable contracts above remain unchanged and the plan records the difference.
- Codex UUID validation may reuse an existing local validation pattern or add a narrowly justified parser dependency; this must not add a runtime network client or weaken `Cargo.lock`/release validation.

## File Structure

- Create: `src/codex/mod.rs` — compose Codex reconciliation, binding/cache ownership, and common `BackendOutcome` mapping.
- Create: `src/codex/session.rs` — rollout and session-index parsing, effective display derivation, and synthetic parser tests.
- Create: `src/codex/resolver.rs` — controlled nested discovery, exact-ID/path resolution, process eligibility, fallback baseline/stickiness, and resolver tests.
- Modify: `src/backend.rs` — add `CODEX_AGENT`, `CodexBackend`, registry dispatch, and generic binding/authority lookup.
- Modify: `src/config.rs` — add strict `[agents.codex].session_dirs`, `CODEX_HOME` root resolution, and config tests.
- Modify: `src/main.rs` — collect listener-level `CODEX_HOME` without reading pane environments.
- Modify: `src/lib.rs` — export the Codex module.
- Modify: `src/runtime.rs` — use ID-based naming contributor identity for Codex and preserve generic failure/retention behavior.
- Modify: `tests/listener.rs` — Codex fake-pane/runtime metadata, authority, TTL, mixed-backend, tab, and pane behavior.
- Modify: `README.md` — supported agents, Codex semantics, optional integration, config, compatibility, privacy, and limitations.
- Modify: `docs/plans/2026-08-28-codex-agent-context.md` — progress and minor implementation differences; archive only after all final validation succeeds.
- Modify only if justified: `Cargo.toml`, `Cargo.lock` — narrowly scoped parsing dependency; no package version change.

## Testing Decisions

- **Parser seam:** synthetic rollout/index text or temporary files → validated session identity, effective cwd, display name, and optional latest assistant line; malformed/incomplete outcomes remain distinguishable.
- **Resolver/backend seam:** synthetic `PaneInput`, process metadata, references, roots, timestamps, and files → `BackendOutcome`/binding evidence without inspecting cache internals.
- **Config seam:** `Config::from_toml` and root-resolution functions → strict structured config, listener-level `CODEX_HOME`, default home, path normalization, and unchanged Pi/Claude forms.
- **Runtime seam:** `Runtime<HerdrApi>` with synthetic files and `FakeApi` → complete externally reported metadata, TTL/no-refresh, source scoping, mixed-agent isolation, and naming effects.
- **Prior art:** follow `src/claude/session.rs`, `src/claude/resolver.rs`, `src/claude/mod.rs`, config module tests, and mixed cases in `tests/listener.rs` while keeping Codex parsing/resolution independent.
- **Avoid:** tests of private cache shape, real Codex conversations or paths, snapshots containing transcript content, mtime races without explicit timestamps, a running user Herdr session, integration installation, and mocks that bypass the public parser/resolver/runtime seams.

## Progress

- [x] Task 1: Parse active Codex rollouts and names into the agreed privacy-bounded display view.
- [x] Task 2: Resolve Codex roots, eligibility, exact identity, and conservative hook-free bindings.
- [x] Task 3: Integrate Codex metadata and automatic tab/pane labels without regressing Pi or Claude.
- [x] Task 4: Publish the Codex contract, complete independent review, run every validation gate, and archive this plan.

Implementation-time minor file changes or internal naming differences must be recorded in the relevant task. Ask the user before changing requirements, Out of Scope, configuration schema, display precedence, binding behavior, privacy boundaries, compatibility claims, or release contracts.

## Tasks

### Task 1: Codex Rollout and Session-Name Parsing

**Covers:** R5-R7, R10, R13-R14, D3-D4, D6

**Objective:** Synthetic active Codex rollout and name-index inputs produce a validated common display view with stable identity, effective cwd, agreed title fallback, and latest eligible assistant text.

**Files:**
- Create: `src/codex/session.rs`
- Create: `src/codex/mod.rs` only for the minimum module/type surface needed by parser tests
- Modify: `src/lib.rs`
- Test: module tests in `src/codex/session.rs`

**Dependencies:** Existing `DisplayView` and `src/text.rs` bounds.

**Implementation notes:**
- Work in red → green vertical slices: canonical header/identity, name index, user/cwd fallback, assistant activity, then malformed/incomplete boundaries.
- Validate the first canonical root metadata before deriving display content. Do not let later copied fork metadata replace identity.
- Treat latest valid `turn_context` cwd as effective cwd while retaining root metadata cwd as fallback.
- Read name index by exact UUID and latest nonblank entry. Index failure removes only that name tier.
- Accept genuine user and eligible assistant event records in physical JSONL order. Ignore duplicate persistence representations, reasoning, developer/system records, tools, task-complete echoes, and unknown phases.
- Return no replacement assistant line after a later genuine user record until commentary/final text appears; runtime retention is Task 3.
- Reuse `display_line`; do not log error payloads or text.

**Test cases:**
- Valid root `cli` metadata plus matching filename ID and cwd → exact identity/effective cwd.
- Forked child with copied later parent metadata → child identity remains canonical.
- Latest `turn_context` cwd present/absent → latest cwd/meta cwd respectively.
- Duplicate name-index entries and blank rename → latest completed nonblank exact-ID name.
- Incomplete final index line → ignore only the tail and use the latest completed exact-ID name or fallback without suppressing a normal refresh.
- Completed malformed index line or missing/unreadable index → invalidate the explicit-name tier for that refresh, use user/cwd fallback, and continue valid rollout activity/TTL refresh.
- `thread_name`, first genuine user, cwd basename → exact display-name precedence and same tab-name source.
- Developer/system/tool/reasoning records before genuine input → never become a name or activity.
- Commentary followed by final answer → latest eligible assistant text; duplicate response representation does not change semantics.
- New genuine user without assistant replacement → parser returns no replacement activity.
- Multiline, whitespace-only, 80/81 scalar Unicode → existing one-line 80-scalar contract.
- Partial final JSON, malformed completed required record, missing metadata, invalid UUID/cwd/source, unknown incompatible history mode → retryable or fail-closed outcome as contracted.
- Unknown unrelated record → safely ignored.

**Complete when:**
- Parser tests cover all display precedence, filtering, identity, cwd, and failure contracts with synthetic data.
- No fixture contains a real path, ID, prompt, response, or copied rollout.
- Parser errors contain categories only, never transcript values.

**Validation:**
- Run: `cargo test codex::session:: --lib --locked`
- Expected: every Codex parser/index test passes, including negative and Unicode boundaries.
- Run: `cargo test text::tests --lib --locked`
- Expected: shared display and naming bounds remain green.

**Implementation record (2026-08-28):** Complete. `src/codex/session.rs` exposes a file parser that extracts and validates the filename identity plus a synthetic-text seam that accepts the expected filename identity directly. Missing or unreadable index files map to the same explicit-name fallback state as an absent index; completed malformed index content remains independently fail-soft. A valid final index entry is accepted without a trailing newline, while only a malformed non-newline tail is ignored. Display and effective-cwd derivation begins after the validated canonical metadata record, so pre-metadata conversation and turn records are excluded while safe unknown preambles remain allowed. Red → green slices covered canonical metadata/effective cwd, index naming and tail completion, user/cwd fallback, assistant activity, rollout failures, canonical-metadata ordering, and file-shape validation. Task 4 review correction now requires every completed rollout record to be an object with a string outer `type`, while preserving safe string-typed unknown records and the distinct malformed/incomplete errors. The file parser now accepts only the Codex 0.149.1 rollout filename shape, validates the real timestamp and both UUID positions, and returns only the thread UUID. `cargo test codex::session:: --lib --locked` passes 14 tests; the earlier shared text suite remains 5 tests.

### Task 2: Codex Discovery, Eligibility, Configuration, and Binding

**Covers:** R2-R4, R7-R11, R13-R14, D1-D2, D5-D7

**Objective:** The Codex backend resolves exact evidence and safe hook-free candidates from configured active roots, returns common backend outcomes, and never guesses across ambiguous panes or sessions.

**Files:**
- Create: `src/codex/resolver.rs`
- Modify: `src/codex/mod.rs`
- Modify: `src/backend.rs`
- Modify: `src/config.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`
- Test: module tests in `src/codex/resolver.rs`, `src/codex/mod.rs`, and `src/config.rs`

**Dependencies:** Task 1 validated session/header parser.

**Implementation notes:**
- Continue red → green slices: config/root resolution, process eligibility, official/exact resolution, controlled scanning, baseline fallback, then stickiness/failure.
- Traverse only the controlled year/month/day layout under active roots; reject symlinks, root escapes, archives, compressed files, and unexpected file shapes.
- Apply 30-day/25-compatible-candidate limits to ordinary fallback only. Exact/sticky resolution bypasses age/count but validates root, regular file, filename/meta identity, cwd, and root source.
- Parse structured argv before any bounded fallback representation. Never log argv or preserve it in errors/state.
- `resume <UUID>` is exact. Named/`--last` resume remains eligible but non-exact. Fork waits for the child ID/file and never uses the parent UUID.
- Authoritative Codex ID blocks lower precedence on every error. A foreign-agent reference does not claim Codex authority.
- Record candidate fingerprints on the initial scan but leave ordinary panes unbound. Bind only one changed/new compatible candidate after observation and only when exactly one Codex pane has that cwd.
- Revalidate sticky path on every relevant poll and invalidate on terminal replacement, root/cwd/identity mismatch, disappearance, or ineligible process.
- Keep failures pane-local and map them to existing `BackendOutcome` categories so runtime controls TTL/reporting.

**Test cases:**
- Default home, valid/invalid `CODEX_HOME`, configured additions, tilde/absolute/relative paths, deduplication, and unknown config fields → exact strict config outcomes without Pi/Claude changes.
- Controlled nested active JSONL versus archive/compressed/symlink/root escape/unexpected depth → only eligible active file is considered.
- Normal TUI, UUID/named/last resume, fork child, and excluded exec/review/remote/ephemeral/subagent/internal/source variants → agreed eligibility and exact-hint outcomes.
- Missing/truncated process metadata → unbound and remove any sticky binding; malformed or conflicting UUID evidence → fail closed with no fallback; switching an existing sticky pane to an excluded mode → remove the binding and return unbound.
- Valid official ID versus newer candidate → official wins and carries source.
- Missing/malformed/duplicate/cwd-incompatible official target → failed identity, no fallback.
- UUID resume without integration → exact binding with no official source claim; named/last resume remains non-exact.
- Initial unique same-cwd historical candidate → baseline only and unbound; exactly one later new/changed candidate → bound.
- Zero or multiple changed candidates, multiple same-cwd Codex panes, duplicate active identity, or ambiguous roots → unbound/fail-closed with no arbitrary assignment.
- Valid sticky binding remains stable; replacement terminal, missing file, changed identity/cwd/source, or excluded process invalidates it.
- 30-day and 25-compatible boundaries, malformed candidates ahead of valid ones, and exact old candidate → ordinary limit applies while exact evidence bypasses it.
- One broken Codex pane plus another healthy backend outcome → reconciliation map remains isolated.

**Complete when:**
- Registry supports `pi`, `claude`, and `codex` only and dispatches each backend independently.
- Every binding precedence and ambiguity case is externally testable through resolver/backend outcomes.
- Initial hook-free fallback cannot display an old transcript before post-observation evidence.
- Existing config and backend tests remain green.

**Validation:**
- Run: `cargo test codex::resolver:: --lib --locked && cargo test codex::tests --lib --locked`
- Expected: all eligibility, discovery, authority, ambiguity, and sticky-binding cases pass.
- Run: `cargo test config::tests --lib --locked && cargo test pi:: --lib --locked && cargo test claude:: --lib --locked`
- Expected: Codex config is accepted while all existing Pi/Claude behavior remains green.

**Implementation record (2026-08-28):** Complete. `CodexScanner` uses controlled year/month/day traversal and reparses validated candidates rather than adding a structural cache; this keeps rollout and adjacent `session_index.jsonl` changes observable without a separate index fingerprint cache. Process eligibility uses structured argv only, accepts targetless `resume`/`fork` picker starts, extracts only a `resume` UUID around supported options (including `--local-provider`), and rejects remote/help/version flags plus known noninteractive subcommands without retaining cmdline or argv text. A top-level `--` fixes normal prompt mode so following `resume`, excluded-subcommand, and UUID-like text cannot become identity evidence; a separator after an already selected `resume` mode may still delimit its target. Official IDs override conflicting lower-level UUID hints, exact/sticky paths bypass ordinary age/count limits while retaining root/type/identity/cwd/source validation, and ordinary fallback requires a PaneKey/canonical-cwd observation generation plus exactly one changed compatible candidate. Stale, nonordinary, disappeared-pane, and replacement-terminal observations are retired. Ordinary discovery filters nonregular entries before canonical deduplication so a symlink cannot hide its regular in-root target, while exact resolution still fails closed on matching symlink evidence. Task 4 review correction shares the session parser's official filename validator with controlled discovery and exact identity lookup, rejecting copied prefixes, invalid timestamps, invalid rollout-ID suffixes, and compressed files without changing root/depth/age/count behavior. Focused validation passes 9 resolver tests, 8 Codex backend tests, 2 static-registry tests, 11 config tests, 18 Pi tests, and 27 Claude tests.

### Task 3: Runtime Metadata and Naming Parity

**Covers:** R1, R3-R6, R10-R13, D2, D6-D7

**Objective:** One listener reports correct Pi, Claude, and Codex context while Codex names participate in existing sidebar, tab, and pane behavior with authority, retention, TTL, and clear semantics intact.

**Files:**
- Modify: `src/backend.rs`
- Modify: `src/runtime.rs`
- Modify: `tests/listener.rs`
- Test: existing runtime/naming module tests where the shared ID-based contributor path changes

**Dependencies:** Tasks 1-2.

**Implementation notes:**
- Add Codex to generic binding/authoritative lookup and use session ID rather than rollout path for naming contributor identity, as already done for Claude's ID-based sessions.
- Keep metadata reporting and naming managers backend-neutral. Do not add Codex branches to tab/pane ownership state machines.
- Report `agent: "codex"`; only official bindings set `applies_to_source`.
- On retryable parse/read failure, do not call `report_metadata`; preserve in-memory state only until TTL expires. Recovery of the same identity may report again.
- Let existing `report_view` retain activity when the parser returns no replacement for the same session. Verify that terminal/binding/session changes do not retain it.
- Extend fake process metadata per pane rather than relying on Pi defaults.

**Test cases:**
- Mixed Pi, Claude, and Codex panes → three correctly labeled metadata reports with independent name/activity values.
- Official Codex ID → exact source-scoped report; UUID/local fallback → no `applies_to_source`.
- New Codex user entry without assistant replacement → prior same-session activity retained; later commentary/final text replaces it.
- Rollout unchanged while `session_index.jsonl` gains a completed rename → the next poll updates sidebar, tab, and pane names; an incomplete index tail keeps the last completed name or fallback while activity and TTL continue refreshing.
- Changed Codex identity, rollout binding, terminal, or agent → no previous activity/name carryover and owned metadata clears/rebinds correctly.
- Incomplete/malformed/unreadable bound rollout → no TTL refresh; same-session repair recovers; another pane/backend continues refreshing.
- Unsupported/excluded Codex process after a prior report → clear is sent and transient clear failure retries.
- Enabled pane naming → resolved Codex fallback/title labels the pane and preserves manual override per session.
- Enabled tab naming with mixed supported panes → Codex component follows layout order and existing composition/manual-baseline behavior.
- Unbound/failed ambiguous Codex pane → no generated component; existing baseline is preserved/restored.
- 80-scalar sidebar and 20-column naming boundaries → unchanged shared limits.

**Complete when:**
- Codex appears in sidebar, automatic tab labels, and automatic pane labels using only common runtime paths.
- Source authority, activity retention, TTL, clear, and failure isolation match the approved contract.
- No Pi/Claude runtime or naming regression is observed.

**Validation:**
- Run: `cargo test --test listener codex --locked`
- Expected: all Codex runtime, metadata, TTL, clear, mixed-agent, and naming cases pass.
- Run: `cargo test --test listener --locked`
- Expected: the complete listener integration suite passes without Pi/Claude regressions.
- Run: `cargo test --lib --locked`
- Expected: all shared runtime, tab-name, pane-name, parser, resolver, and config unit tests remain green.

**Implementation record (2026-08-28):** Complete. `Runtime` now treats Codex naming contributors as session-ID based, alongside Claude, while metadata reporting and both naming managers remain backend-neutral. The listener `FakeApi` derives process `name` and `argv0` from each pane's synthetic argv so Pi, Claude, and Codex process evidence stays agent-correct. Thirteen synthetic Codex listener tests cover mixed-agent isolation; official, exact-resume, and post-baseline local authority; same-session activity retention plus commentary/final replacement and terminal/binding/session/agent isolation; rollout-independent index rename, incomplete-tail refresh, and completed-malformed fallback; incomplete, completed-structural-invalid, and unreadable rollout no-refresh with repair and healthy-pane continuation; unresolved new official/exact identity clear with transient retry and no old display carryover; excluded-process clear retry; pane manual overrides and tab ownership across same-session path changes; mixed layout order and ambiguous unbound baseline restoration; and the shared 80-scalar/20-column bounds. Red evidence was `cargo test runtime::tests::codex_naming_contributor_uses_session_identity --lib --locked`, which failed because Codex contributed its rollout path; it passes after the minimal common identity change. Task 4 review correction keeps same-identity failures retained without TTL refresh, but clears a reported same-terminal state when `FailedIdentity` changes the agent or session identity. Focused Codex listener validation passes 13 tests, the complete listener suite passes 65 tests, and the library suite passes 194 tests.

### Task 4: Public Contract, Independent Review, and Full Validation

**Covers:** R1-R14, D1-D7

**Objective:** Users can understand and configure Codex support and every repository gate plus an independent review confirms the implementation, privacy boundary, and regressions before the plan is archived.

**Files:**
- Modify: `README.md`
- Modify: `docs/plans/2026-08-28-codex-agent-context.md`
- Move after every final gate succeeds: `docs/plans/2026-08-28-codex-agent-context.md` → `docs/plans/archived/2026-08-28-codex-agent-context.md`
- Modify only if justified: `Cargo.toml`, `Cargo.lock`

**Dependencies:** Tasks 1-3 and a stable implementation diff.

**Implementation notes:**
- Document Pi, Claude Code, and Codex in the header/quickstart and supported-context rules.
- Document exact name/activity precedence, official integration as optional, conservative hook-free attribution, supported/excluded modes, `CODEX_HOME`, structured config, scan bounds, active-only limitation, TTL behavior, privacy, and verified Codex `0.149.1` compatibility.
- Keep install commands and package version unchanged; do not add integration installation as an automatic step.
- Run a fresh read-only Herdr reviewer over the stable diff. Blocking correctness, regression, privacy, or required-test findings return to the writer and are re-reviewed in the same context.
- Do not use real Codex sessions for validation. Automated synthetic/fake-socket tests are the supported seam.
- Update Progress and actual minor implementation differences before final validation. Archive only after every gate below succeeds.

**Documentation record (2026-08-28):** Complete. The public contract covers Codex sidebar metadata, automatic tab and pane labels, optional official integration and binding precedence, display derivation and same-session activity retention, supported and excluded CLI modes, Codex CLI 0.149.1 best-effort compatibility, `[agents.codex]` configuration and `CODEX_HOME` root precedence, controlled active-rollout discovery, ambiguity behavior, visual-only inference, and unsupported archive/compressed/SQLite sources.

**Review correction record (2026-08-28):** The first fresh implementation review found three blocking/high issues: unresolved replacement identities retained old metadata, completed records without a string outer `type` were treated as safe unknowns, and rollout filenames were accepted by prefix plus embedded UUID rather than the Codex 0.149.1 shape. The initial full gate run also found five Clippy diagnostics: four unit-struct default constructions and one cloned reference used as a one-element slice. Red tests reproduced all three findings plus the missing completed-malformed bound-rollout runtime seam. The corrections are recorded in Tasks 1-3; Clippy now constructs `CodexScanner` directly and uses `std::slice::from_ref`. The correction re-review marked all four findings resolved with no unresolved blocking/high findings. The complete release-gate rerun passed all 14 commands, Requirement Coverage was confirmed against the final diff, and no baseline drift occurred.

**Test cases:**
- README config example → accepted by strict config parser.
- README claims → each maps to parser, resolver, config, or runtime tests and no excluded mode is presented as supported.
- Privacy search/review → no transcript value/path/full-argv logging and no real fixture.
- Independent review → no unresolved blocking/high findings.

**Complete when:**
- README accurately describes the shipped behavior and limitations.
- Independent review has no unresolved blocking/high findings.
- All focused and full validation commands pass.
- Requirement Coverage matches the actual diff and the completed plan is archived under the same filename.

**Validation:**
- Run all commands under Final Validation.
- Expected: every command exits 0, review has no unresolved blocking/high findings, and the archived plan records the evidence.

## Requirement Coverage

| Requirement / Decision | Task | Verification |
|---|---|---|
| R1, D1-D2 | Tasks 2-3 | Registry tests; mixed runtime metadata; tab/pane naming tests |
| R2 | Task 2 | Process/source eligibility and negative-mode tests |
| R3-R4, D5 | Task 2 | Authority, UUID, baseline fallback, ambiguity, and sticky tests |
| R5, D4 | Tasks 1, 3 | Name-index/fallback parser tests and naming/runtime reports |
| R6, D3 | Tasks 1, 3 | Message filtering/order tests and same-session retention tests |
| R7 | Tasks 1-2 | Canonical metadata, fork, effective-cwd, filename/identity tests |
| R8 | Task 2 | Controlled traversal, scan bounds, exact bypass, symlink negatives |
| R9 | Tasks 2, 4 | Config/root tests and README example validation |
| R10, D6 | Tasks 1-3 | Partial rollout no-refresh/recovery; partial index normal refresh; completed malformed index fallback; index-only rename across sidebar/tab/pane |
| R11 | Tasks 2-3 | `applies_to_source` exact payload assertions |
| R12, D7 | Task 3 | Full listener/runtime/naming regression suites |
| R13 | Tasks 1-4 | Synthetic fixture review, logging/privacy review, source search |
| R14 | Tasks 1-2, 4 | Unknown/required-record tests and README compatibility statement |

## Final Validation

- [x] `cargo test codex:: --lib --locked` — 31 passed.
- [x] `cargo test config::tests --lib --locked` — 11 passed.
- [x] `cargo test --test listener codex --locked` — 13 passed.
- [x] `cargo test --all-targets --locked` — 194 library, 6 binary, and 65 listener tests passed; none ignored.
- [x] `cargo fmt --check` — no formatting differences.
- [x] `cargo clippy --all-targets -- -D warnings` — no warnings.
- [x] `cargo build --release --locked` — release binary built successfully.
- [x] `sh tests/installer.sh` — all positive and negative cases passed.
- [x] `sh tests/release-assets.sh` — all archive, checksum, target, and version cases passed.
- [x] `actionlint .github/workflows/*.yml` — no workflow diagnostics.
- [x] `shellcheck scripts/*.sh tests/*.sh` — no shell diagnostics.
- [x] `rg -n 'report_agent_session|report_agent\b' src` — no matches; inferred Codex identity is not written to a canonical API.
- [x] `rg -n 'println!|eprintln!|dbg!' src/codex src/runtime.rs` — no matches; the reviewed paths add no transcript, environment, or full-argv logging.
- [x] `git diff --check` — no whitespace errors.
- [x] Fresh read-only Herdr review — all three blocking/high findings and the runtime test gap were resolved; no unresolved blocking/high findings remain.
- [x] Requirement Coverage contains no unsupported or unverified item.
- [x] Plan and actual changed files/contracts agree; `Cargo.toml` and `Cargo.lock` were not changed.
- [x] After every item above succeeded, moved this plan unchanged in name to `docs/plans/archived/2026-08-28-codex-agent-context.md`.

## Risks and Open Questions

- Codex rollout JSONL is an internal format. Required-field drift must fail closed and future compatibility claims require source/fixture revalidation.
- A separate mutable `session_index.jsonl` means rollout and title fingerprints can change independently; stale title caching is a regression risk.
- Controlled recursive discovery is broader than Claude's direct-child scan. Root containment, symlink rejection, depth checks, age/count bounds, and per-cwd scope are security and performance invariants.
- Resumed/forked histories can contain multiple metadata-like records. Selecting anything except the canonical root metadata can misattribute identity.
- Duplicate persistence representations can expose the same assistant response more than once. The parser must define one authoritative logical-message stream without dropping commentary/final updates.
- No unresolved user-facing questions remain. Any need to change supported modes, fallback eligibility, title/activity precedence, configuration schema, scan bounds, or compatibility wording requires user approval before editing this plan or implementation.
