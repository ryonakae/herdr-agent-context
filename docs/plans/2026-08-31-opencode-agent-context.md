# OpenCode Agent Context Implementation Plan

> **For implementers:** Execute tasks in order unless dependencies allow otherwise. Mark a task complete only after its validation succeeds. Reflect minor implementation differences in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Problem Statement

`herdr-agent-context` reports privacy-bounded session names and recent assistant activity for Pi, Claude Code, and Codex, but OpenCode panes remain unsupported. Users cannot see the active OpenCode conversation in the Herdr sidebar, automatic tab names, or automatic pane names. OpenCode 1.x persists many sessions in one SQLite database rather than one JSONL file per session, so adding it requires session-ID-aware SQLite resolution that does not confuse same-directory sessions or refresh metadata after failed reads.

## Goal

Add best-effort OpenCode 1.x support for persistent root TUI sessions. Prefer Herdr's official OpenCode session ID when available, retain hook-free operation through conservative local SQLite attribution, and expose the resolved OpenCode title and current assistant text through the existing sidebar, tab-name, and pane-name paths without weakening privacy, TTL, or ambiguity handling for existing backends.

## Out of Scope

- OpenCode `run`, `attach`, `serve`, `web`, ACP, MCP, GitHub automation, and other non-root or non-TUI modes.
- OpenCode child/subagent sessions where `session.parent_id` is non-null.
- The development/preview `session_message` persistence schema; this change supports the verified OpenCode 1.x `session`, `message`, and `part` tables only.
- Reading history through a live OpenCode HTTP server, plugin callback, export command, `sqlite3` subprocess, or `opencode db` subprocess.
- Installing or modifying the optional Herdr OpenCode integration or the user's OpenCode configuration.
- Reporting inferred OpenCode IDs through `pane.report_agent_session` or another canonical Herdr identity API.
- OpenCode session restore, lifecycle-state detection, transcript search, history export, or UI changes in Herdr itself.
- A release version, `CHANGELOG.md` entry, tag, GitHub Release, or managed-plugin promotion. Those require a separately approved release version and the repository release procedure.
- Compatibility claims for unverified OpenCode versions or future database schemas.

## Requirements and Decisions

### Requirements

- **R1:** Register `opencode` as a fourth static backend without changing Pi, Claude Code, Codex, Herdr transport, metadata TTL, clear retry, or naming ownership behavior.
- **R2:** Support persistent root OpenCode TUI sessions started normally or with `--continue`, `--session <ID>`, or `--fork`; reject excluded subcommands and non-root database sessions.
- **R3:** Bind in this order: valid official Herdr OpenCode `kind = "id"` reference; valid non-fork `--session <ID>` CLI hint; valid in-memory sticky binding for the same OpenCode process generation; one uniquely new or changed same-cwd root session after pane observation; otherwise remain unbound.
- **R4:** Treat matching official identity and a visible non-fork `--session` identity as authoritative. Missing, malformed, duplicate, wrong-cwd, unreadable, or otherwise incompatible authoritative identity must block every lower precedence rather than select a different session.
- **R5:** Never treat the source ID in `--session <ID> --fork` or `--continue --fork` as the forked session. Wait until the new root session is uniquely observable, unless the official Herdr integration reports its new ID.
- **R6:** Ordinary fallback must not bind on its baseline scan, must require exactly one changed/new compatible candidate, and must fail closed when candidates, configured databases, or OpenCode panes sharing the canonical cwd make attribution ambiguous. A failed database scan preserves its prior observations, prevents fallback from every other configured database during that reconciliation, and—when no prior observation exists—makes the first successful recovery baseline-only.
- **R7:** Resolve the session name as meaningful `session.title`, then the first genuine user text, then the canonical session-directory basename. Treat the verified OpenCode 1.x `New session - <ISO-8601 UTC timestamp>` value as a default rather than a meaningful title.
- **R8:** Resolve activity as the latest nonblank ordinary assistant text after the latest genuine user input, including text still being streamed into a part. Exclude reasoning, tool calls/results, errors, ignored text, synthetic text, files, patches, step records, and unknown part types.
- **R9:** When a new genuine user input has no replacement assistant text, return no replacement activity so the runtime retains the previous activity only for the same terminal, agent, binding database, and session ID. A terminal, agent, database, or session change must never inherit prior OpenCode display state.
- **R10:** Read OpenCode data from the primary database selected by `OPENCODE_DB` or the standard XDG data location, plus normalized and deduplicated `[agents.opencode].database_paths`. Capture `OPENCODE_DB` and `XDG_DATA_HOME` once at listener startup; never inspect pane environments.
- **R11:** Add bundled SQLite support through `rusqlite`, open databases read-only without creating or mutating them, and keep WAL-visible current activity readable. Read all rows contributing to one database result from one read transaction/snapshot. Do not wait on `SQLITE_BUSY`; fail that refresh immediately and retry on the next reconciliation so one database cannot postpone other backends. SQLite open, schema, query, busy, decode, and required-structure failures must not refresh metadata TTL.
- **R12:** Support only the OpenCode 1.x `session`, `message`, and `part` contract verified against OpenCode 1.18.23. Safely ignore unknown non-display part types, but fail closed for missing required tables/columns or malformed required session/message/text fields.
- **R13:** Use the resolved OpenCode title for the sidebar session-name token and, when enabled, existing automatic tab and pane labels. Use the OpenCode session ID—not the shared database path—as naming contributor identity.
- **R14:** Bound sidebar values remain one line and at most 80 Unicode scalars. Tab and pane components retain the existing 20-column grapheme-safe bound.
- **R15:** Tests must construct synthetic temporary SQLite databases and fake Herdr inputs only. Do not copy, commit, snapshot, log, or assert against real OpenCode session IDs, directories, titles, prompts, assistant text, database files, process environments, or full process arguments.
- **R16:** Public documentation must explain OpenCode scope, binding precedence, display rules, optional official integration, database configuration, compatibility boundary, privacy behavior, and conservative ambiguity handling.

### Implementation Decisions

- **D1:** Add `src/opencode/` as an independent parser/resolver/backend. Keep OpenCode SQL and JSON details out of `runtime.rs` and Herdr transport.
- **D2:** Reuse `DisplayView`, `Binding`, and `BackendOutcome` as the backend-neutral lifecycle contract. `Binding.path` identifies the authorized database; `DisplayView.session_identity` identifies the OpenCode row.
- **D3:** Use bundled `rusqlite` rather than a system SQLite library or subprocess. Open a real read-only SQLite connection and do not use immutable mode because active OpenCode writes may reside in the WAL. Use SQLite's immediate busy behavior (zero busy timeout) and one read transaction per database result; a busy/locked refresh fails without sleeping and retries at the next normal reconciliation.
- **D4:** Resolve the primary database as follows: nonempty `OPENCODE_DB` wins; an absolute value is used directly and a relative value is resolved under the OpenCode data directory. The data directory is nonempty absolute `XDG_DATA_HOME` plus `opencode`, otherwise `~/.local/share/opencode`. Without `OPENCODE_DB`, the primary database is `<data-directory>/opencode.db`. Configured database paths are additional files.
- **D5:** Track fallback observations per canonical database path and session ID using session/message/part row fingerprints. Preserve observations across busy/read/schema failures; never interpret recovery from an unobserved or failed database as a new-session signal, and never bind from a partial set of configured databases. Do not use only the SQLite database or WAL file mtime: one shared database contains many sessions and unrelated writes must not mark every candidate as changed.
- **D6:** Order logical messages by `(message.time_created, message.id)` and parts within a message by `(part.time_created, part.id)`, ascending. The lexicographic ID is a deterministic tie-breaker only; `time_updated` never reorders a logical message or part. A streamed update changes the selected text value and fingerprint without changing logical order.
- **D7:** Validate exact/sticky candidates outside ordinary age/count discovery limits, while preserving database membership, unique identity, root-session, cwd, and required-schema checks. Ordinary discovery is bounded to live pane cwd values and compatible root rows; final numeric bounds should follow existing Claude/Codex constants unless SQLite query characteristics justify a smaller equivalent recorded in Task 2.
- **D8:** Preserve official `BindingEvidence::Official { source }`; only official bindings set `applies_to_source`. Exact CLI and inferred bindings remain visual metadata only.
- **D9:** A failed read of an already bound same identity may retain in-memory display state but sends no refresh. An observed different authoritative identity clears incompatible prior state before retrying, using existing runtime outcomes.
- **D10:** Follow TDD with synthetic databases: establish red behavior at parser/resolver/runtime seams, implement the smallest backend change, then refactor without introducing a dynamic backend ABI or OpenCode-specific naming manager.
- **D11:** Carry Herdr's existing foreground process PID through the common `ProcessCommand` input. The OpenCode resolver selects the eligible root command's PID as its process generation; a changed PID retires sticky and fallback state even when pane, terminal, cwd, executable, and argv are unchanged. Existing backends receive the field without changing their binding semantics.

### Contracts

#### Configuration

```toml
[agents.opencode]
database_paths = ["~/additional/opencode/opencode.db"]
```

- `database_paths` contains database files, not session directories.
- Config entries must be absolute after `~` expansion; relative entries and unknown OpenCode keys reject the complete config atomically.
- `OPENCODE_DB` follows OpenCode semantics: absolute values are direct paths; relative values are relative to the resolved OpenCode data directory.
- Primary and additional paths are normalized and deduplicated before reconciliation.

#### Binding authority

1. Matching Herdr `agent_session { agent: "opencode", kind: "id" }` is authoritative.
2. Structured visible `opencode --session <ID>` is an exact local hint only when `--fork` is absent.
3. A valid same-pane/same-terminal/same-process-PID sticky database-plus-session binding is next.
4. Hook-free fallback may bind only after baseline observation to one uniquely changed/new compatible root session, with one OpenCode pane for that cwd, every configured database scanned successfully for that reconciliation, and no cross-database ambiguity.
5. Otherwise the pane is unbound and reports no generated metadata or naming component.

#### OpenCode 1.x rows

The supported schema requires these columns and relationships:

- `session(id TEXT PRIMARY KEY, parent_id TEXT NULL, directory TEXT NOT NULL, title TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, time_archived INTEGER NULL)` for identity and discovery.
- `message(id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL)`, with `message.session_id` referring to `session.id`.
- `part(id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL)`, with both IDs agreeing with the selected message and session.

The reader contract is:

- `session.id` is the canonical nonblank session identity.
- `session.directory` must canonically equal the live pane cwd.
- `session.parent_id IS NULL` is required for this feature.
- `session.title` supplies the preferred name only when nonblank and not the verified default timestamp form.
- `message.data` must be a JSON object whose string `role` distinguishes `user` and `assistant`; an assistant `error` value other than absent/null makes that assistant message ineligible.
- `part.data` for selected text must be a JSON object with string `type = "text"` and string `text`; boolean `synthetic = true` or `ignored = true` makes it ineligible. Missing `synthetic`/`ignored` is false; a present non-boolean value is malformed required text data.
- Logical messages are ordered ascending by `(message.time_created, message.id)` and parts within one message by `(part.time_created, part.id)`. `time_updated` contributes to change fingerprints but not logical order.
- Unknown records may be ignored only when they cannot affect identity, ordering, role, or selected text. Missing required columns, broken row relationships, or malformed required JSON fail the affected database result closed.
- Candidate fingerprints include the selected session's identity-relevant values and the maximum relevant message/part `(time_created, time_updated, id)` tuples, so a streamed update is visible while another session's write is not.
- Candidate discovery and display derivation for one database reconciliation use one read transaction/snapshot. A busy/locked database returns failure immediately; it never sleeps inside the listener loop.

#### Failure and retention

- Successful resolution reports metadata with `agent = "opencode"`; official resolution also reports `applies_to_source`.
- Unbound or identity-changing failure clears incompatible previously reported metadata through the existing retryable clear path.
- Same-identity read/parse/query failure does not call `report_metadata`, does not extend TTL, and may retain only in-memory display/naming state. Failed ordinary scans retain prior baselines but cannot produce a fallback binding from any database in that reconciliation.
- A successful same-session parse with no post-user assistant replacement returns `last_message = None`; `Runtime` alone applies retention.

## Current Context

### Confirmed

- `BackendRegistry` statically dispatches Pi, Claude, and Codex and filters unsupported agents before process inspection (`src/backend.rs`).
- `Runtime::reconcile_at` already transports Herdr `agent_session` references into generic `PaneInput`, reports generic `DisplayView` values, retries metadata clears, and coordinates tab/pane names (`src/runtime.rs`). No Herdr protocol change is required.
- Runtime activity retention already requires the same terminal, agent, binding path, and session identity. OpenCode therefore needs no dedicated retention cache.
- Existing naming contributor logic uses session IDs for Claude and Codex; OpenCode must join that ID-based branch because all of its sessions can share one database path.
- The listener captures only an environment allowlist. It currently omits `OPENCODE_DB` and `XDG_DATA_HOME` (`src/main.rs`).
- Herdr process metadata already contains `Process.pid`, but `Runtime` currently drops it while constructing `ProcessCommand`; OpenCode process-generation invalidation requires carrying that existing field through the common input (`src/herdr/protocol.rs`, `src/runtime.rs`, `src/backend.rs`).
- Configuration deserialization is strict and invalid reloads keep the previous complete valid configuration (`src/config.rs`).
- The installed OpenCode 1.18.23 command exposes root TUI `--continue`, `--session`, and `--fork`, plus excluded subcommands.
- OpenCode source defines the primary database and `session`, `message`, and `part` persistence fields. Local schema-only inspection confirmed those tables and role/part-type shapes without reading conversation values.
- Herdr 0.8.2 offers an optional `opencode` integration and reports OpenCode native identity as `agent = "opencode"`, `kind = "id"`, source `herdr:opencode`.
- The release matrix builds standalone binaries for two macOS and two Linux GNU targets and enforces a glibc 2.18 baseline on Linux (`.github/workflows/ci.yml`, `.github/workflows/release.yml`).

### Assumptions

- Internal Rust type and helper names inside `src/opencode/` may follow the closest existing backend conventions as long as the contracts above remain unchanged.
- Unit tests may choose either in-memory SQLite or temporary on-disk SQLite where the behavior under test does not depend on WAL or read-only file opening. WAL/read-only behavior itself must use a temporary on-disk database.

## File Structure

- Create: `src/opencode/mod.rs` — compose OpenCode authority, sticky/fallback state, parser results, and generic backend outcomes.
- Create: `src/opencode/session.rs` — read and validate OpenCode 1.x SQLite rows and derive the common display view.
- Create: `src/opencode/resolver.rs` — resolve database paths/candidates, parse process eligibility, maintain fallback observations, and enforce ambiguity rules.
- Modify: `Cargo.toml` — add bundled read-only SQLite support through `rusqlite`.
- Modify: `Cargo.lock` — lock the SQLite dependency graph.
- Modify: `src/lib.rs` — export the OpenCode module.
- Modify: `src/backend.rs` — carry process PID, register and dispatch the OpenCode backend, and include its binding/authority lookup.
- Modify: `src/config.rs` — add strict OpenCode database-path configuration and XDG/`OPENCODE_DB` resolution.
- Modify: `src/main.rs` — capture listener-level `OPENCODE_DB` and `XDG_DATA_HOME`.
- Modify: `src/runtime.rs` — carry foreground process PID and use session-ID naming contributor identity for OpenCode.
- Modify: `tests/listener.rs` — verify externally reported OpenCode metadata, TTL/clear behavior, and tab/pane integration using synthetic databases.
- Modify: `README.md` — publish supported OpenCode behavior, setup, configuration, compatibility, privacy, and limitations.
- Modify: `herdr-plugin.toml` — include OpenCode in the user-visible plugin description without changing the version.
- Modify: `AGENTS.md` — record the new backend structure and synthetic OpenCode database testing/privacy constraints.
- Maintain then archive: `docs/plans/2026-08-31-opencode-agent-context.md` — record task progress and implementation differences; move unchanged in name to `docs/plans/archived/` only after every final validation succeeds.

## Testing Decisions

- **Parser seam:** Synthetic SQLite schema and rows → validated identity, cwd, root status, title fallback, and latest eligible assistant text. SQL/schema/JSON failures remain distinguishable from a valid session with no replacement activity. Writer/read-only-reader tests prove one-transaction snapshots, WAL visibility, and immediate busy failure without using real data.
- **Resolver/backend seam:** Synthetic databases, explicit row timestamps, process metadata, official references, pane keys, and cwd values → generic `BackendOutcome` and binding evidence. Assert externally visible binding results, not private map layout.
- **Config seam:** `Config::from_toml` and OpenCode database-path resolution → strict structured config, XDG default, relative/absolute `OPENCODE_DB`, additional path normalization, deduplication, and unchanged existing agent forms.
- **Runtime seam:** `Runtime<HerdrApi>` with synthetic SQLite files and `FakeApi` → complete metadata, authority source scoping, no-refresh/clear behavior, activity retention, and tab/pane ownership by session ID.
- **Packaging seam:** Existing native and `cross` release builds → bundled SQLite links successfully on all four targets and Linux binaries retain the glibc 2.18 baseline.
- **Prior art:** Follow `src/codex/{session,resolver,mod}.rs`, strict agent config tests in `src/config.rs`, runtime identity/retention handling in `src/runtime.rs`, and Codex/mixed-agent cases in `tests/listener.rs` while keeping SQL logic independent.
- **Avoid:** Real OpenCode databases or conversations, transcript snapshots, shelling out to OpenCode/SQLite, tests of private cache shape, file-mtime-only candidate tests, a running Herdr/OpenCode dependency in automated tests, or assertions containing real user paths/IDs.

## Progress

- Review base: `8518acc4972f8a8a85b780aa428d511f68f3bf77`
- [x] Task 1: Read OpenCode 1.x sessions into privacy-bounded display views.
- [x] Task 2: Resolve OpenCode database configuration, CLI eligibility, authority, and conservative fallback binding.
- [x] Task 3: Integrate OpenCode metadata and automatic tab/pane labels without regressing existing agents.
- [ ] Task 4: Publish the OpenCode contract, complete independent review, and pass every repository validation gate.

Implementation-time minor file changes or internal differences must be reflected in the relevant task. Ask the user before changing a requirement, Out of Scope item, or public contract.

## Tasks

### Task 1: OpenCode SQLite Session and Display Parsing

**Covers:** R7, R8, R9, R11, R12, R14, R15, D1, D2, D3, D6, D9, D10

**Objective:** Synthetic OpenCode 1.x SQLite rows produce a validated common display view with canonical identity/cwd, meaningful title fallback, and streaming assistant activity while malformed or unreadable input cannot refresh metadata.

**Files:**
- Create: `src/opencode/session.rs`
- Create: `src/opencode/mod.rs` only for the minimum module/type surface required by parser tests
- Modify: `src/lib.rs` — exported the module in Task 1 rather than Task 2 so the agreed `--lib` parser seam could compile
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Test: module tests in `src/opencode/session.rs`

**Dependencies:** Existing `DisplayView` and text-bounding helpers.

**Implementation notes:**
- Begin with failing tests that build only synthetic temporary schemas and rows; never import or copy the local OpenCode database.
- Add `rusqlite` with bundled SQLite support. Open production database paths read-only and without immutable mode or create flags. Keep the busy timeout at zero, and perform candidate/session/message/part reads that form one result in one read transaction. Keep SQL parameterized; session IDs and paths must never be interpolated into SQL text.
- Validate required `session` identity, directory, root status, title, and timestamps before deriving display content. Canonical cwd mismatch or non-root `parent_id` is incompatible.
- Recognize only the verified exact default-title structure `New session - <ISO-8601 UTC timestamp>` as default. A blank/default title falls through to first genuine user text, then cwd basename.
- Require the exact columns, row relationships, JSON object fields, and `(time_created, id)` ordering stated in Contracts. Genuine user text and visible assistant text require a nonblank `type = "text"` part with neither `synthetic` nor `ignored` true.
- Select activity only from error-free assistant messages after the latest genuine user input. Keep streamed text eligible before assistant completion. Return no replacement when the latest user has no eligible later assistant text.
- Ignore reasoning, tools, patches, files, step markers, and safe unknown part types. Missing required schema/columns, malformed selected role/text JSON, invalid field types, or read/query/busy failures fail closed.
- Apply existing one-line/80-scalar metadata bounds through shared text helpers while preserving an unbounded source for the existing 20-column tab/pane path.

**Test cases:**
- Valid root session with meaningful title and assistant text → exact session ID, title, and bounded latest activity.
- Verified default title → first genuine user text; no genuine user text → canonical cwd basename.
- Manual/AI title update without message change → next read returns the new title.
- Multiple messages and multiple text parts inserted out of row order, including equal timestamps → `(time_created, id)` yields deterministic latest eligible assistant text after the latest genuine user input; `time_updated` does not reorder it.
- Streaming update to an existing text part → next read returns the updated text before message completion and changes only that session's fingerprint.
- Writer commits to WAL while a read-only connection is active → a fresh read transaction sees the committed update; one parser result never mixes pre- and post-commit session/message/part generations.
- Database held busy/locked → refresh fails immediately without a busy wait and another backend can reconcile normally.
- Latest user input without later assistant text → valid view with no replacement activity.
- Synthetic/ignored user text → not used for latest-user boundary or title fallback.
- Synthetic/ignored assistant text, reasoning, tools/results, files, patches, step records, unknown safe parts, and assistant error → never displayed.
- Non-root session, cwd mismatch, blank/invalid identity, malformed selected JSON, missing table/column, busy/unreadable database, and unsupported `session_message`-only schema → failed outcome with no display refresh.
- Unicode, multiline, whitespace-only, and over-limit text → existing normalization and bounds remain exact.

**Complete when:**
- The parser exposes only validated OpenCode session/display data needed by the backend.
- Every eligible and excluded display source is covered with synthetic data.
- Read-only/WAL-safe opening is exercised without mutating the database.
- Existing shared text tests remain green.

**Validation:**
- Run: `cargo test opencode::session:: --lib --locked`
- Expected: all synthetic OpenCode SQLite parser/display tests pass, including read-only, malformed, filtering, streaming, and Unicode boundaries.
- Run: `cargo test text::tests --lib --locked`
- Expected: all existing shared text-boundary tests pass unchanged.

**Implementation record (2026-08-31):** Complete. Red evidence began with the first synthetic title/activity test returning `OpenCodeSessionError::Read`; later slices exposed invalid calendar dates being treated as default titles, missing required `session.time_created` being accepted, and nonexistent cwd values being normalized. The implementation uses bundled `rusqlite`, read-only zero-timeout connections, one transaction snapshot, required 1.x row/JSON validation, deterministic logical ordering, per-session fingerprints, WAL-visible streaming updates, and shared display bounds. `src/lib.rs` was pulled forward from Task 2 only to expose the agreed parser test seam. `cargo test opencode::session:: --lib --locked` passes 11 tests, `cargo test text::tests --lib --locked` passes 5 tests, and focused Clippy, format, and whitespace checks pass.

### Task 2: OpenCode Configuration, Eligibility, and Conservative Binding

**Covers:** R2, R3, R4, R5, R6, R10, R11, R12, R15, D1, D2, D4, D5, D7, D8, D10

**Objective:** The OpenCode backend resolves configured databases and exact authority safely, identifies only supported root TUI commands, and binds hook-free panes only after unique post-observation evidence.

**Files:**
- Create: `src/opencode/resolver.rs`
- Modify: `src/opencode/mod.rs`
- Modify: `src/config.rs`
- Modify: `src/main.rs`
- Modify: `src/backend.rs`
- Modify: `src/lib.rs`
- Modify: `src/runtime.rs` for process PID propagation only
- Test: module tests in `src/opencode/resolver.rs`, `src/opencode/mod.rs`, `src/config.rs`, `src/backend.rs`, and `src/main.rs`

**Dependencies:** Task 1's validated session reader and fingerprint inputs.

**Implementation notes:**
- Add a dedicated strict OpenCode config type because `database_paths` is a file list and existing agent config uses `session_dirs`.
- Resolve the primary path exactly as D4 specifies, merge configured additions, normalize/deduplicate them, and reject relative config entries or unknown fields atomically. Capture only `OPENCODE_DB` and `XDG_DATA_HOME` at listener level.
- Carry each foreground `Process.pid` into `ProcessCommand`. Parse structured argv without retaining or logging it, and return the eligible root command's PID as process generation. Accept root TUI normal/project starts, `--continue`, `--session <ID>`, and `--fork`; reject known subcommands, help/version-only invocations, missing session values, duplicate/conflicting exact hints, and malformed visible structure.
- With `--fork`, never treat a source `--session` value as the active identity. Official child identity may resolve immediately; otherwise use the ordinary observation path for the new root row.
- Official matching OpenCode `kind = "id"` authority overrides all lower evidence on success and failure. Foreign-agent references do not claim OpenCode authority.
- Exact identity lookup must be unique across configured databases and validate root status, canonical cwd, required schema, and session identity before binding. Duplicate exact identity fails closed.
- Sticky bindings remain keyed by `PaneKey`, eligible root process PID, database path, and session identity and must be revalidated. PID change, terminal replacement, ineligible command, disappeared pane, changed cwd, or incompatible database retires sticky and observation state.
- Build ordinary fingerprints from the specific session plus relevant message/part state. Record a baseline without binding; on later scans bind only one changed/new compatible root candidate, with exactly one OpenCode pane for that canonical cwd. Unrelated writes to the same database must not change another candidate's fingerprint. Preserve successful observations when any database scan fails, block all ordinary fallback from the partial scan, and baseline a database on its first successful recovery when it had no prior successful observation.
- Bound/exact validation may bypass ordinary scan age/count bounds. Keep ordinary SQL discovery bounded and indexed around live cwd values; record any minor constant difference from Claude/Codex in this task without weakening the one-candidate ambiguity rule.
- Map missing/read/schema/query failures to existing `BackendOutcome` categories so runtime owns retention, TTL, clear, and pane isolation.

**Test cases:**
- Default XDG path, custom nonempty absolute XDG path, empty/relative `XDG_DATA_HOME` fallback, absolute `OPENCODE_DB`, relative `OPENCODE_DB`, empty `OPENCODE_DB` fallback, configured additions, normalization, and deduplication → exact ordered database list.
- Relative configured database path, unknown OpenCode key, `session_dirs` under OpenCode, and unknown agent table → complete config rejection; prior valid reload remains active.
- Normal/project, `--continue`, non-fork `--session <ID>`, and fork forms → eligible; every excluded subcommand/help/version/malformed form → ineligible or invalid as specified.
- Official valid ID → official source-scoped binding; missing/malformed/wrong-cwd/non-root/duplicate official ID → no lower fallback.
- Non-fork `--session` valid ID → exact non-source-scoped binding; missing/malformed/wrong-cwd/non-root/duplicate exact ID → no lower fallback.
- `--session <parent> --fork` and `--continue --fork` → parent is never bound; a pre-existing sticky binding to that parent is retired, and a uniquely created child can bind only after observation.
- First ordinary scan with one candidate → baseline only; one later changed/new candidate → local binding; zero or multiple changes → unbound.
- Multiple same-cwd OpenCode panes, duplicate sessions across databases, or multiple compatible candidates → no arbitrary assignment.
- An unrelated session update in the same database → unchanged candidate does not become eligible.
- Sticky binding, terminal replacement, cwd change, process replacement where only PID changes, pane removal, and database disappearance → exact expected retain/retire behavior; a restarted process cannot inherit the prior session solely because argv is unchanged.
- Busy/read/schema failure after a baseline → prior baseline survives, unchanged recovery does not bind, and a later genuine candidate change may bind normally.
- Failure of one configured database while another has one changed candidate → no partial-set fallback; a database with no successful prior scan gets a baseline-only first recovery.
- One broken OpenCode pane/database plus healthy Pi/Claude/Codex panes → backend outcomes remain isolated.

**Complete when:**
- Registry supports exactly Pi, Claude, Codex, and OpenCode and dispatches each backend independently.
- Every authority, fork, observation, ambiguity, and stale-state transition is externally testable through resolver/backend outcomes.
- OpenCode path configuration is strict and existing configuration forms remain unchanged.
- No pane environment, transcript, full argv, or inferred canonical ID is reported or logged.

**Validation:**
- Run: `cargo test opencode::resolver:: --lib --locked && cargo test opencode::tests --lib --locked`
- Expected: all CLI, database discovery, authority, fallback, fork, ambiguity, and lifecycle tests pass.
- Run: `cargo test config::tests --lib --locked && cargo test backend::tests --lib --locked && cargo test --bin herdr-agent-context --locked`
- Expected: all OpenCode env/config/registry tests and every binary test pass; strict existing behavior remains green and the binary command does not report zero tests solely because of a name filter.
- Run: `cargo test pi:: --lib --locked && cargo test claude:: --lib --locked && cargo test codex:: --lib --locked`
- Expected: every existing backend suite passes unchanged.

**Implementation record (2026-08-31):** Complete. Red slices began with missing strict config fields/resolution, then exposed a partial-database fallback when a second database lacked required schema and macOS canonical-cwd lookup gaps. The final backend carries Herdr process PID, accepts documented OpenCode 1.18.23 root-TUI options, excludes non-root commands, applies official/exact/sticky/observed precedence, discards fork-parent hints, scans 30-day/25-compatible root candidates with per-session fingerprints, and preserves fail-closed baselines across failed databases. `ProcessCommand.pid` was added mechanically to existing backend tests without changing their semantics. `cargo test opencode:: --lib --locked` passes 30 tests; config passes 14, registry 3, binary 7, Pi 18, Claude 27, and Codex 31. All-target Clippy, format, and whitespace checks pass.

### Task 3: Sidebar, Tab, Pane, TTL, and Retention Integration

**Covers:** R1, R3, R4, R8, R9, R11, R13, R14, R15, D2, D8, D9, D10, D11

**Objective:** One listener reports correct OpenCode context beside existing agents and uses OpenCode session identity consistently for sidebar metadata, automatic tab labels, automatic pane labels, retention, clears, and manual overrides.

**Files:**
- Modify: `src/backend.rs`
- Modify: `src/runtime.rs`
- Modify: `tests/listener.rs`
- Test: runtime unit tests in `src/runtime.rs` and integration-style fake API tests in `tests/listener.rs`

**Dependencies:** Task 2's registered OpenCode backend and outcomes.

**Implementation notes:**
- Treat OpenCode as session-ID-based in naming contributor identity. Do not introduce OpenCode branches in tab/pane ownership managers.
- Report `agent = "opencode"`; only official binding evidence supplies `applies_to_source`.
- Reuse `Runtime::report_view` activity retention. Confirm equality includes terminal, agent, database binding path, and session ID so activity never crosses databases or sessions.
- On same-identity SQLite failure, preserve in-memory state without metadata refresh. On changed official/exact identity or unsupported process, clear incompatible reported state and retry transient clear failures.
- Keep naming and metadata backend-neutral, preserve absolute polling deadlines, and do not let `pane_updated` create a report loop.
- Extend `FakeApi` helpers with configurable synthetic OpenCode process PIDs/argv and temporary databases; do not rely on Pi defaults or a running OpenCode/Herdr integration.

**Test cases:**
- Mixed Pi, Claude, Codex, and OpenCode panes → four correctly agent-labeled independent reports.
- Official OpenCode ID → exact `applies_to_source`; exact CLI and local fallback → no source scope.
- Meaningful title, default-title fallback, streamed assistant text, and later title/text updates → next poll updates sidebar values.
- New user input without replacement assistant text → prior activity retained only for the same OpenCode identity; later text replaces it.
- Terminal, agent, database, or session change → no previous OpenCode activity/name carryover.
- Same pane/terminal/cwd/argv with a changed foreground OpenCode PID → old sticky/display state is not adopted by the replacement process; it must establish fresh exact or post-baseline evidence.
- Busy/unreadable/malformed bound DB → no TTL refresh; repair recovers; unrelated panes continue refreshing.
- New unresolved official/exact identity → old metadata clears, transient clear failure retries, and lower fallback remains blocked.
- Excluded OpenCode process after a prior report → clear is sent and retried on transient failure.
- Enabled pane naming → OpenCode title labels the pane; manual override is scoped by session ID even when two sessions share a DB.
- Enabled tab naming with mixed panes → OpenCode component follows visual order and existing composition/manual-baseline behavior.
- Same DB but changed OpenCode session → a previous session's tab/pane override does not suppress or label the new session.
- Ambiguous/unbound OpenCode pane → no generated component; baseline is preserved/restored.
- 80-scalar sidebar and 20-column tab/pane limits → exact existing bounds.

**Complete when:**
- OpenCode appears correctly in all three requested display surfaces through common runtime paths.
- Official authority, TTL/no-refresh, clear retry, retention, and naming identity are proven through public fake API effects.
- The complete listener suite shows no Pi/Claude/Codex, reconnect, scheduling, or naming regression.

**Validation:**
- Run: `cargo test runtime::tests::opencode --lib --locked`
- Expected: OpenCode session-ID naming and runtime transition tests pass.
- Run: `cargo test --test listener opencode --locked`
- Expected: all OpenCode metadata, authority, activity, failure, tab, and pane tests pass.
- Run: `cargo test --test listener --locked`
- Expected: the complete listener integration suite passes without existing-agent or lifecycle regressions.

**Implementation record (2026-08-31):** Complete. The Red runtime seam showed OpenCode contributing its shared database path instead of `ses_runtime_one`; adding OpenCode to the existing session-ID contributor branch fixed both naming managers without backend-specific manager code. Eight synthetic SQLite listener scenarios cover mixed-agent reporting and official source scope, exact/streaming updates, same-session retention and replacement isolation, fresh fallback evidence after PID replacement, busy/malformed/unreadable no-refresh and healthy-pane continuation, authority/excluded-process clear retry, same-database pane override isolation, mixed visual-order tab behavior and ambiguity restoration, plus shared display bounds. Runtime identity has 1 focused test, OpenCode listener coverage has 8, the complete listener suite passes 73, the library suite passes 229, and all-target Clippy, format, and whitespace checks pass.

### Task 4: Public Contract, Review, and Release-Matrix Validation

**Covers:** R1, R10, R11, R12, R14, R15, R16, D3, D4, D10

**Objective:** Users can understand and configure OpenCode support, repository guidance reflects SQLite privacy constraints, and independent review plus every local/release matrix gate confirms the implementation and bundled SQLite packaging.

**Files:**
- Modify: `README.md`
- Modify: `herdr-plugin.toml`
- Modify: `AGENTS.md`
- Modify: `docs/plans/2026-08-31-opencode-agent-context.md`
- Move after every final gate succeeds: `docs/plans/2026-08-31-opencode-agent-context.md` → `docs/plans/archived/2026-08-31-opencode-agent-context.md`

**Dependencies:** Tasks 1-3 complete.

**Implementation notes:**
- Update supported-agent header/quickstart, optional integration command, exact/fallback binding, OpenCode display semantics, root TUI scope/exclusions, database config/resolution, ambiguity behavior, TTL, privacy, and best-effort compatibility verified against OpenCode 1.18.23.
- Explain that `database_paths` points to files, `OPENCODE_DB` relative paths resolve under the XDG data directory, and development `session_message` storage is unsupported.
- Update the plugin description without changing package/plugin versions. Leave `CHANGELOG.md`, tags, releases, and promotion state untouched under Out of Scope.
- Update repository guidance for the new backend and synthetic SQLite-only fixtures. Keep live OpenCode sessions and real OpenCode databases outside validation; the existing native/cross build gates provide bundled-SQLite packaging evidence without adding a release-checklist process change.
- Request an independent code review after implementation. Correct blocking/high findings with focused red tests and rerun affected validation before final gates.
- Use only synthetic temporary database and fake-Herdr evidence. Do not inspect, copy, or validate against real OpenCode database rows, IDs, titles, prompts, assistant text, or integration state.
- Keep the plan's task records current. Archive only after every Final Validation item succeeds.

**Documentation record (2026-08-31):** Public and repository guidance now covers the optional Herdr integration, binding precedence and fork behavior, display/retention rules, root-TUI scope and exclusions, strict database configuration, XDG/`OPENCODE_DB` resolution, read-only SQLite privacy, conservative fallback, and the OpenCode 1.18.23 schema boundary. The plugin description includes OpenCode without changing version or released changelog history. `AGENTS.md` remains a 62-line repository entry point and adds only the new backend path, bundled read-only SQLite constraint, and synthetic-database privacy rule.

**Initial review correction record (2026-08-31):** Independent review of `8518acc..fc3ad19` found four blocking/high contract gaps: required identity primary keys were not validated, ordinary discovery prefiltered canonical cwd aliases, a streamed visible-text update before a later non-display part could leave the fallback fingerprint unchanged, and local sticky read failures lost the OpenCode session-ID naming generation. Focused Red tests reproduced all four. The correction validates single TEXT identity primary keys, canonical-filters recent root rows before stopping at 25 compatible sessions, hashes all session/message/part state into a privacy-bounded fingerprint, and carries optional session identity through generic failed bindings. OpenCode unit tests now pass 33, OpenCode listener tests 9, the complete listener suite 74, and the library suite 232; all-target Clippy, format, and whitespace checks pass before scoped re-review.

**Test cases / checks:**
- README examples and option table → exact `agents.opencode.database_paths`, XDG, `OPENCODE_DB`, supported modes, and limitations agree with code/tests.
- `herdr-plugin.toml` → description includes OpenCode while version remains synchronized at the existing value.
- Repository guidance → synthetic DB privacy and the new backend structure are explicit without changing release promotion authorization or adding a real-session smoke procedure.
- Dependency audit → no runtime `sqlite3`, `opencode`, network client, or dynamic backend script is introduced.
- Logging audit → no transcript/title/message, process environment, full argv, or SQL row JSON can reach logs.
- Full review and validation → no unresolved blocking/high finding and no baseline drift.

**Complete when:**
- Public and repository documentation exactly matches implemented behavior and exclusions.
- Independent implementation review has no unresolved blocking/high finding.
- All focused, full, lint, packaging, shell, and workflow validations pass.
- Requirement Coverage is confirmed against the final diff.
- The completed plan is archived under the same filename only after all gates pass.

**Validation:**
- Run: `cargo test opencode:: --lib --locked && cargo test --test listener opencode --locked`
- Expected: all focused OpenCode unit and listener tests pass.
- Run: the complete Final Validation command list below.
- Expected: every command succeeds; all four release targets link bundled SQLite, Linux remains at glibc 2.18, and review/audits report no unresolved blocking/high issue.

## Requirement Coverage

| Requirement / Decision | Task | Verification |
|---|---|---|
| R1 static backend and no regressions | Tasks 2, 3, 4 | Registry tests, full backend/listener/library suites, independent review |
| R2 root TUI scope | Task 2 | CLI eligibility and root/non-root database tests |
| R3 binding precedence | Task 2 | Official/exact/sticky/observed outcome tests |
| R4 authoritative failure blocks fallback | Tasks 2, 3 | Missing/malformed/duplicate/wrong-cwd authority and runtime clear tests |
| R5 fork parent exclusion | Task 2 | `--session --fork` and `--continue --fork` child-observation tests |
| R6 conservative ambiguity handling | Task 2 | Baseline, changed candidate, multiple pane/session/database tests |
| R7 session-name precedence | Task 1 | Meaningful/default/blank title, genuine user, cwd fallback tests |
| R8 streaming filtered activity | Tasks 1, 3 | Text update and excluded role/part/error tests plus runtime report tests |
| R9 same-session retention isolation | Tasks 1, 3 | No-replacement view and terminal/agent/database/session transition tests |
| R10 XDG, env, and additional DB paths | Tasks 2, 4 | Config/path/env tests and README contract check |
| R11 bundled read-only SQLite and no TTL refresh | Tasks 1, 3, 4 | WAL/read-only tests, failure runtime tests, four-target builds |
| R12 OpenCode 1.x-only compatibility | Tasks 1, 4 | Schema-negative tests and documented compatibility boundary |
| R13 all display surfaces and ID identity | Task 3 | Sidebar, mixed tab, pane, override-isolation tests |
| R14 display bounds | Tasks 1, 3, 4 | Unicode/one-line/80-scalar and 20-column tests/documentation |
| R15 synthetic privacy boundary | Tasks 1-4 | Fixture review, logging audit, no real DB/session evidence in diff |
| R16 public documentation | Task 4 | README/config/privacy/limitations review |
| D1 independent backend | Tasks 1, 2 | File boundaries and no SQL branches in runtime/transport |
| D2 common lifecycle contract | Tasks 1-3 | `DisplayView`/`BackendOutcome` tests and runtime effects |
| D3 bundled real read-only SQLite | Tasks 1, 4 | Dependency review, WAL test, native/cross builds |
| D4 exact path precedence | Tasks 2, 4 | Environment/config path table tests and docs |
| D5 per-session fingerprints | Task 2 | Unrelated same-DB update and changed-candidate tests |
| D6 deterministic logical ordering | Task 1 | Out-of-order insert/time and streamed-update tests |
| D7 exact versus bounded ordinary discovery | Task 2 | Old/exact/sticky and ordinary scan-limit tests |
| D8 source scope only for official binding | Tasks 2, 3 | Binding evidence and metadata `applies_to_source` tests |
| D9 failure/retention ownership | Tasks 1, 3 | No-refresh, recovery, identity-change clear tests |
| D10 TDD and minimal architecture | Tasks 1-4 | Red/green records in task notes, final diff review |
| D11 process-generation invalidation | Tasks 2, 3 | PID propagation and same-argv process-replacement tests |

## Final Validation

- [ ] `cargo test opencode:: --lib --locked` — Expected: all OpenCode parser, resolver, backend, config-adjacent unit tests pass.
- [ ] `cargo test --test listener opencode --locked` — Expected: all OpenCode runtime metadata and naming tests pass.
- [ ] `cargo test --all-targets --locked` — Expected: all Rust unit, binary, and integration tests pass.
- [ ] `cargo fmt --check` — Expected: no formatting differences.
- [ ] `cargo clippy --all-targets -- -D warnings` — Expected: no warnings.
- [ ] `cargo build --release --locked` — Expected: the local standalone release binary builds with bundled SQLite.
- [ ] `sh tests/installer.sh` — Expected: installer positive and negative tests pass.
- [ ] `sh tests/release-assets.sh` — Expected: four-target archive contract tests pass.
- [ ] `sh tests/release-notes.sh` — Expected: existing changelog/release-note contract remains valid without altering released history.
- [ ] `sh tests/prepare-release.sh` — Expected: release preparation contract tests pass.
- [ ] `sh tests/release-tag.sh` — Expected: tag validation contract tests pass.
- [ ] `sh tests/github-release.sh` — Expected: GitHub Release contract tests pass.
- [ ] `actionlint .github/workflows/*.yml` — Expected: all workflows are valid.
- [ ] `shellcheck scripts/*.sh tests/*.sh` — Expected: all POSIX shell files pass lint.
- [ ] Four-target CI-equivalent builds: `cargo build --release --locked --target aarch64-apple-darwin`, `cargo build --release --locked --target x86_64-apple-darwin`, `cross build --release --locked --target aarch64-unknown-linux-gnu`, and `cross build --release --locked --target x86_64-unknown-linux-gnu` — Expected: bundled SQLite builds on each supported target.
- [ ] `sh scripts/verify-glibc-baseline.sh target/aarch64-unknown-linux-gnu/release/herdr-agent-context 2.18 && sh scripts/verify-glibc-baseline.sh target/x86_64-unknown-linux-gnu/release/herdr-agent-context 2.18` — Expected: both Linux artifacts retain the glibc 2.18 baseline.
- [ ] `if rg -n 'Command::new|std::process::Command|report_agent_session|report_agent\b' src/opencode; then exit 1; fi` — Expected: exits zero because the OpenCode backend has no subprocess dependency or canonical identity reporting call.
- [ ] `if rg -n 'println!|eprintln!|dbg!' src/opencode; then exit 1; fi` — Expected: exits zero because the OpenCode backend has no direct transcript/title/message/environment/full-argv/row-JSON logging path.
- [ ] `git diff --check` — Expected: no whitespace errors.
- [ ] Independent code review — Expected: no unresolved blocking/high correctness, privacy, compatibility, or regression finding.
- [ ] Requirement Coverage has no unimplemented or unverified item.
- [ ] The plan and final diff agree, including any recorded minor implementation differences.
- [ ] Only after every item above succeeds, move this plan unchanged in name to `docs/plans/archived/2026-08-31-opencode-agent-context.md`.

## Risks and Open Questions

- Bundled SQLite adds C compilation and binary size. The native/cross release matrix and Linux symbol-baseline checks are mandatory evidence, not assumed compatibility.
- Active OpenCode writes may live in the WAL; immutable/file-copy shortcuts can miss current activity. Tests must exercise a writer plus a read-only reader on a temporary database.
- One database contains many sessions. File mtime is not session activity evidence; fallback correctness depends on per-session row fingerprints and explicit ambiguity tests.
- OpenCode SQLite and JSON payloads are internal, unversioned persistence contracts. Required-field drift must fail closed, and compatibility claims must remain pinned to the verified 1.18.23 shape.
- OpenCode development source includes a `session_message` schema not covered here. An empty or populated unsupported replacement schema must not be misread as 1.x history.
- Session title generation can lag or remain at the timestamp default. Name fallback and later title updates must stay independent from assistant-activity refresh.
- Multiple DBs can contain the same session ID or same-cwd recent sessions. Exact and fallback resolution must fail closed rather than rely on configured path order. A failed scan is unknown evidence, not an empty candidate set; observations must survive failure without allowing partial-database binding.
- Pane and terminal IDs can survive an OpenCode process restart. Sticky correctness therefore depends on the eligible root process PID carried from Herdr process metadata.
- No unresolved product or public-contract question remains. Release versioning and promotion are explicitly separate work.
