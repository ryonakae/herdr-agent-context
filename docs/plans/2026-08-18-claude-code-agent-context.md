# Claude Code Agent Context Implementation Plan

> **For implementers:** Execute tasks in order unless dependencies allow otherwise. Mark a task complete only after its validation succeeds. Reflect minor implementation differences in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Problem Statement

`herdr-agent-context` v0.1.0 reports privacy-bounded Pi session names and recent assistant activity to the Herdr sidebar, but Claude Code panes still expose only Herdr's built-in agent rows. Users cannot distinguish multiple Claude conversations or see their latest assistant activity without opening each pane. The implementation is currently wired directly to Pi types in `runtime.rs`, so adding Claude by copying the Pi pipeline would duplicate binding, TTL, clear, and reporting logic and make the planned Codex and OpenCode backends harder to add safely.

Claude Code has two useful identity paths: the official Herdr integration reports a native session ID, while hook-free installations persist local JSONL transcripts under Claude project directories. The feature must use official identity when available without installing hooks or changing agent settings, and must fail conservatively where hook-free same-project matching is ambiguous.

## Goal

Release a backward-compatible `v0.2.0` prerelease that:

- displays the same session-name and quoted recent-activity rows for persistent interactive Claude Code panes and Pi panes;
- prioritizes official Herdr session references for both agents, then uses agent-specific local fallback without writing inferred identity back to Herdr;
- introduces a small compile-time backend boundary suitable for later Codex and OpenCode implementations without creating a dynamic plugin framework;
- preserves Pi v0.1.0 behavior, privacy, TTL, reconnect, packaging, and four-target distribution contracts;
- supports the existing `pi_session_dirs` configuration while adding structured per-agent session roots;
- is validated with synthetic tests, hook-free live dogfooding, temporary official integrations, and the existing release pipeline.

## Out of Scope

- Implementing Codex or OpenCode context extraction in v0.2.0.
- A runtime-loadable backend ABI, user-authored backend scripts, or a generic external plugin framework.
- Claude `--print`, `--background`, `--no-session-persistence`, detached background agents, or non-persisted sessions.
- Reporting sidechain/subagent transcript text, tool calls, thinking, tool results, system records, or API error text as recent activity.
- Reading pane viewport/scrollback text for transcript scoring.
- Reading another process's environment or recursively discovering arbitrary `.claude` directories under the user's home.
- Persisting inferred pane-to-session bindings to disk.
- Filesystem watcher integration; the listener remains polling-based.
- Automatic installation of Herdr Pi/Claude integrations, automatic edits to Pi/Claude settings, or automatic Herdr sidebar configuration.
- Reporting inferred session paths or IDs through `pane.report_agent_session`, `pane.report_agent`, or another canonical identity API.
- Windows support; the existing macOS and GNU/Linux targets remain unchanged.
- Publishing a tag or GitHub Release without a separate explicit promotion confirmation after dogfooding and final validation.

## Requirements and Decisions

### Requirements

- **R1:** Support local, persistent, top-level interactive Claude Code sessions detected by Herdr, including normal starts, `--continue`, `--resume`, `--name`, worktree sessions, and Remote Control sessions when they produce a local top-level JSONL transcript.
- **R2:** Explicitly exclude Claude `--print`, `--background`, and `--no-session-persistence` processes from hook-free binding so a historical transcript cannot appear current.
- **R3:** Report Claude context using the existing `agent_context_session_name` and `agent_context_last_message` metadata tokens, with one-line values limited to 80 Unicode scalars and activity enclosed in ASCII double quotes.
- **R4:** Resolve the Claude session name as explicit custom title, then latest Claude `ai-title`, then first genuine human user text on the active branch, then canonical cwd basename.
- **R5:** Resolve recent activity as the latest top-level assistant text block after the latest genuine human user entry on the active branch; exclude thinking, tool calls/results, sidechains/subagents, metadata/system records, and `isApiErrorMessage` entries. Retain the previous activity for the same session until a replacement text appears.
- **R6:** Reconstruct the active Claude branch from `uuid` and `parentUuid`. Select the last parseable eligible top-level `user` or `assistant` record in physical JSONL order as the active leaf, then follow its ancestors; records on other leaves must not supply fallback user text or activity. Session-level title records may remain outside the message chain but must match the session ID.
- **R7:** Treat official Herdr session references as authoritative: Pi `kind=path` resolves directly; Claude `kind=id` resolves to the matching configured top-level JSONL. If an authoritative reference is missing, unreadable, or invalid, retry without heuristic fallback and do not refresh metadata TTL.
- **R8:** Treat exact UUID values from Claude `--session-id <uuid>` and `--resume <uuid>` as high-confidence local hints below Herdr authority. Do not directly bind `--resume <name>` or `--continue` from arguments.
- **R9:** Without authoritative identity, preserve valid in-memory sticky bindings. For one pane, initial fallback selects the compatible candidate with greatest filesystem mtime, breaking ties by lexicographically ascending path. A bound pane switches only when its bound fingerprint is unchanged since the prior scan and exactly one other compatible candidate is new/changed and newer than the bound file; zero or multiple changed alternatives keep the binding. If the bound file disappears or becomes cwd-incompatible, remove the binding and apply the initial single-pane rule. Multiple same-project panes at cold start remain unbound unless each pane has unique authoritative or exact-UUID evidence; do not guess by ordering or reduce ambiguity merely because another pane was directly bound.
- **R10:** Scope hook-free Claude discovery to project directories relevant to live pane cwd values. For ordinary discovery, stat direct-child JSONLs from the last 30 days, sort by descending mtime then ascending path, validate session structure/cwd in that order, skip invalid or cwd-incompatible files without consuming the quota, and stop after 25 compatible candidates. Authoritative IDs, exact UUID hints, and valid sticky paths bypass age/count limits.
- **R11:** Use `CLAUDE_CONFIG_DIR/projects` from the listener environment when set, otherwise `~/.claude/projects`; merge explicit structured roots. Never inspect per-pane process environments.
- **R12:** Introduce an internal static backend contract for Pi and Claude now, with extension points for compiled Codex and OpenCode backends. Keep Herdr transport, runtime lifecycle, and metadata reporting agent-neutral; keep transcript formats and discovery agent-specific.
- **R13:** Isolate local scan/read/parse failures per pane. A failed Claude pane must not block Pi or another Claude pane. Shared Herdr transport failures still trigger the existing reconnect/full-sync path.
- **R14:** Keep global `poll_interval_ms` and `metadata_ttl_ms`; preserve absolute polling deadlines, metadata sequence epochs, clear retries, duplicate-listener locking, and no-refresh-on-failure behavior.
- **R15:** Add structured `[agents.pi]` and `[agents.claude]` `session_dirs` configuration. Continue accepting legacy top-level `pi_session_dirs`; specifying both legacy and structured Pi roots is invalid. Config reload remains strict and atomic, retaining the previous valid config on any invalid or unknown field.
- **R16:** Preserve the privacy boundary: all parsing remains local; no runtime network dependency; no logs contain titles, prompts, assistant text, process environments, or full process arguments; fixtures are synthetic; custom metadata remains visual-only.
- **R17:** Document Herdr's official Pi and Claude integrations as optional but recommended for exact binding and native resume. Plugin installation remains hook-free and never invokes integration installation.
- **R18:** Claim Claude Code `2.1.x` format support as best effort and document verification against `2.1.220`. Unknown records are ignored; incompatible required structure fails closed for that pane and lets metadata expire.
- **R19:** Preserve the four existing release targets, checksum/archive/installer contracts, Herdr `0.8.0` minimum, and runtime independence from Cargo, Node.js, and Python.
- **R20:** Validate both hook-free and official-integration paths. Official Pi/Claude integrations are installed only temporarily after recording checksums/backups, then uninstalled and verified to restore the previous agent settings state.

### Implementation Decisions

- **D1:** Reuse the existing generic metadata tokens rather than adding Claude-specific tokens or overriding Herdr `title`/`display-agent`.
- **D2:** Use a compile-time backend registry/interface, not a dynamic backend ABI. The interface exposes agent labels, eligibility, root resolution, candidate discovery, authoritative/hint resolution, and transcript parsing; runtime owns pane lifecycle, TTL, sequence, and reporting.
- **D3:** Adapt the ZAM reference selectively: use project-scoped discovery, direct-child transcripts, recent candidate limits, sticky bindings, fingerprint caching, and conservative cold-start ambiguity. Do not copy viewport text scoring, tool activity previews, flat-tail branch handling, or opaque UUID handling.
- **D4:** Keep authoritative identity source-aware. Metadata resolved through an official reference uses that session source as `applies_to_source`; fallback and process-hint bindings do not claim an official source.
- **D5:** Keep inferred bindings in memory only. Listener/server restart establishes identity again from official references, exact UUID hints, or safe fallback.
- **D6:** Cache candidate header/fingerprint information and parse transcript bodies only for changed bound files. Polling remains the recovery mechanism for missed events and partial writes.
- **D7:** Use full-file, branch-aware parsing for a changed bound Claude transcript rather than ZAM's final-16-KiB flat tail, because rewind correctness is part of the display contract.
- **D8:** Ship the feature as `v0.2.0`; dogfood via source link before promotion and use the existing tag-triggered prerelease workflow only after explicit approval.

### Contracts

#### Metadata

| Token | Pi | Claude |
|---|---|---|
| `agent_context_session_name` | explicit Pi name → first active-branch user text → cwd basename | explicit custom title → latest `ai-title` → first genuine active-branch user text → cwd basename |
| `agent_context_last_message` | latest active-turn assistant text | latest top-level active-turn assistant text |

- Both values are one line and at most 80 Unicode scalars.
- Activity includes leading and trailing ASCII `"`; truncation reserves both quotes and the ellipsis.
- `pane.report_metadata.params.agent` is the selected backend's canonical Herdr label: `pi` for Pi and `claude` for Claude. Tests assert the complete payload so Claude tokens cannot be routed to Pi rows.
- Missing activity reports a nullable token. A new user entry does not clear the retained activity for the same bound session.
- A changed binding never carries activity from the previous session.

#### Binding precedence

1. Official Herdr `agent_session` resolvable by the selected backend.
2. Agent-specific exact local identity hint (Claude UUID arguments).
3. Existing valid in-memory sticky binding.
4. Safe single-pane local fallback.
5. Unbound; never arbitrary multi-pane assignment.

An official reference blocks every lower level even when its file is absent or malformed.

#### Backend boundary

Representative Claude records used by synthetic contract tests are structurally equivalent to:

```json
{"type":"custom-title","title":"Explicit name","sessionId":"session-uuid"}
{"type":"ai-title","aiTitle":"Generated name","sessionId":"session-uuid"}
{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"session-uuid","isSidechain":false,"message":{"role":"assistant","content":[{"type":"thinking","thinking":"excluded"},{"type":"text","text":"Visible activity"}]}}
```

The internal backend contract must support these responsibilities without exposing transcript-format details to `runtime.rs`:

- identify canonical Herdr agent labels and excluded process modes;
- resolve default and configured roots;
- map authoritative `path` or `id` references and exact process hints to local files;
- discover bounded candidates for relevant live panes;
- validate candidate/session cwd and identity;
- parse a file into a common display view containing stable session identity, session name, and optional activity.

The registry contains Pi and Claude in v0.2.0. Codex and OpenCode are not stubbed with empty implementations.

#### Configuration

Preferred structured form:

```toml
poll_interval_ms = 2000
metadata_ttl_ms = 10000

[agents.pi]
session_dirs = ["~/additional/pi/sessions"]

[agents.claude]
session_dirs = ["~/additional/claude/projects"]
```

Backward-compatible Pi-only alternative:

```toml
pi_session_dirs = ["~/legacy/pi/sessions"]
```

- The two Pi forms are alternatives and are invalid when combined.
- Root paths must be absolute or start with `~`; merge and canonical deduplication follow the current Pi contract.
- Unknown root/agent fields, unknown agent tables, invalid paths, legacy/structured Pi conflicts, nonpositive polling, and TTL not greater than polling reject the complete file.
- An invalid initial load uses timing defaults and default roots. An invalid reload retains all previous valid values.

#### Claude record filtering

- Session ID must be a nonempty string and agree with an authoritative/UUID-selected file when that evidence exists.
- Top-level message-chain records require valid `uuid`/`parentUuid` relationships; cycles or missing ancestors fail the changed file rather than selecting another branch.
- Genuine user fallback/boundary records exclude `isMeta=true`, `isSidechain=true`, agent-authored records, and tool-result-only content.
- Assistant activity excludes `isSidechain=true`, API error messages, and content without a nonempty `text` block.
- `custom-title` and `ai-title` records are session metadata; the latest valid custom title wins over the latest valid AI title.
- Incomplete final JSONL data is retryable and does not refresh TTL. A completed malformed record or invalid tree is a parse failure for that refresh.

## Current Context

### Confirmed

- Repository HEAD `66ffab6` is the clean `v0.1.0` prerelease commit, and the managed Herdr plugin is installed from that release.
- Herdr `0.8.0` protocol `19` exposes `agent_session { source, agent, kind, value }`, process metadata, metadata tokens, and lifecycle events used by the listener.
- The current Pi implementation already prioritizes official `kind=path` references and refuses fallback on authoritative read/parse failure.
- Herdr's official Pi integration reports an absolute session path when available; Herdr represents Claude's official report as `kind=id` even though the hook receives both `session_id` and `transcript_path`.
- The plugin currently filters only Pi panes in `runtime.rs`, stores binding logic in `src/pi/resolver.rs`, and reports through a shared Herdr transport.
- Installed Claude Code is `2.1.220`. Its CLI supports `--name`, `--continue`, `--resume`, `--session-id`, `--fork-session`, `--background`, `--print`, and `--no-session-persistence`.
- Current local Claude top-level JSONLs use UUID filenames under `~/.claude/projects`, contain `user`/`assistant` records with `uuid`, `parentUuid`, `sessionId`, `cwd`, `isSidechain`, and typed content blocks, and contain `ai-title` records. Only structural keys/types were inspected; no real transcript values were read or copied.
- Official Claude hooks receive `session_id`, `transcript_path`, and `cwd`. The plugin must not install or depend on those hooks.
- ZAM prior art uses project-scoped direct-child scans, a 30-day/25-file cap, sticky bindings, mtime/text scoring, and disk cache. It does not parse UUID arguments, official integration identity, sidechain markers, or active branches. The selected design adopts only the safe filesystem/caching ideas listed in D3.
- Existing release validation includes Rust tests, fake Unix sockets, installer/archive negative tests, four target builds, glibc `2.18`, public-asset installer smoke, and a tag-triggered GitHub prerelease.

### Assumptions

- Internal type and file names may be adjusted during implementation to match Rust module boundaries, provided the backend responsibilities, public configuration, and task/file ownership remain unchanged and the plan records the difference.
- Candidate age/count constants may remain private implementation constants because their values and bypass rules are fixed by R10; no public tuning keys are added.

## File Structure

- Create: `src/backend.rs` — common backend identity, candidate/display contracts, static Pi/Claude registry, and agent-neutral binding evidence types.
- Create: `src/claude/mod.rs` — Claude backend composition and exports.
- Create: `src/claude/session.rs` — branch-aware Claude JSONL parsing, title/name/activity extraction, and synthetic parser tests.
- Create: `src/claude/resolver.rs` — Claude root/project discovery, ID/UUID resolution, bounded candidate scan, eligibility, and resolver tests.
- Modify: `src/pi/resolver.rs` — implement the common backend/discovery contracts while preserving Pi path and heuristic behavior.
- Modify: `src/pi/session.rs` — return the common display view without changing Pi parsing semantics.
- Modify: `src/pi/mod.rs` — expose the Pi backend through the registry.
- Modify: `src/config.rs` — structured agent sections, legacy Pi compatibility, Claude roots, strict atomic validation/reload, and tests.
- Modify: `src/runtime.rs` — reconcile all registered backends, isolate pane-local failures, retain per-session activity, and report source-scoped metadata.
- Modify: `src/herdr/mod.rs` — accept the backend's canonical agent label in the agent-neutral metadata reporting contract.
- Modify: `src/herdr/protocol.rs` — serialize `pi` or `claude` in the complete metadata payload instead of hardcoding Pi.
- Modify: `src/herdr/socket.rs` — forward the canonical agent label without changing socket lifecycle semantics.
- Modify: `src/lib.rs` — register new backend and Claude modules.
- Modify: `src/main.rs` — pass Claude environment/root inputs to configuration/backend initialization without reading pane environments.
- Modify: `tests/listener.rs` — mixed Pi/Claude fake-socket behavior, authority, TTL, failure isolation, reconnect, and subprocess integration tests.
- Modify: `README.md` — Pi/Claude capability, shared sidebar rows, optional official integrations, structured config, compatibility, privacy, and limitations.
- Modify: `docs/release-checklist.md` — hook-free and temporary-integration live scenarios, checksum restoration, ambiguity, and v0.2 promotion gates.
- Modify: `AGENTS.md` — only if the architecture map or validation commands materially change; do not duplicate README/release prose.
- Modify: `Cargo.toml`, `Cargo.lock`, `herdr-plugin.toml` — synchronize version `0.2.0`; dependency changes are permitted only when justified by the implementation and must not add a runtime network client.
- Modify: `tests/installer.sh`, `tests/release-assets.sh`, `.github/workflows/*.yml`, or release scripts only where version/contract assertions require it; four targets and archive contents remain unchanged.
- Modify: `docs/plans/2026-08-18-claude-code-agent-context.md` — maintain progress and implementation differences; archive only after every final validation succeeds.

## Testing Decisions

- **Primary behavior seam:** `Runtime<HerdrApi>` with synthetic Pi/Claude files and fake Herdr API values. Verify externally reported tokens, `applies_to_source`, TTL refresh/expiry behavior, clear retries, and mixed-pane isolation rather than internal cache fields.
- **Parser seam:** public/backend session-reader result from synthetic Claude JSONL. Fixtures cover titles, active/abandoned branches, meta/tool/sidechain/API-error filtering, partial tails, malformed trees, Unicode formatting, and same-session identity.
- **Resolver seam:** backend binding result from synthetic pane/process/reference/candidate inputs. Verify precedence, exact UUID handling, exclusions, bounded scans, sticky behavior, and conservative multi-pane cold start.
- **Config seam:** `ConfigWatcher` initial/reload outcomes from temporary TOML files. Verify legacy migration and atomic retention.
- **Transport seam:** existing temporary Unix socket and real listener subprocess tests. Add mixed agent lists and Claude `kind=id`; do not require a live user Herdr server in automated tests.
- **Live seam:** disposable Herdr session and synthetic prompts. Test hook-free Claude first; temporarily install official Pi/Claude integrations only after backup/checksum capture, then verify authoritative binding and native resume before uninstall/restoration.
- **Prior art:** retain `src/pi/session.rs`, `src/pi/resolver.rs`, `src/config.rs`, and `tests/listener.rs` test patterns. Use ZAM's `crates/zam/src/agents/claude/parser.rs`, `worker/process/commands.rs`, and `worker/process/scoring.rs` only as behavioral reference, not copied transcript fixtures.
- **Avoid:** real user transcripts, real session paths, snapshots of private sidebar content, mocks of backend internals, viewport scraping, tests that depend on filesystem mtime ties without explicit timestamps, or tests that install integrations into the developer's real home during automated runs.

## Progress

- [x] Task 1: Add the extension-ready backend/config boundary while preserving all Pi behavior.
- [x] Task 2: Parse and discover Claude Code sessions with bounded, branch-aware local behavior.
- [x] Task 3: Reconcile mixed Pi/Claude panes with conservative binding, source authority, and failure isolation.
- [x] Task 4: Publish the v0.2 public contract in docs, versions, and package checks.
- [ ] Task 5: Complete automated, hook-free, temporary-integration, CI, and release validation.

Implementation-time minor file changes or internal naming differences must be recorded in the relevant task. Ask the user before changing requirements, Out of Scope, configuration schema, display precedence, binding behavior, privacy boundaries, compatibility claims, or release contracts.

## Tasks

### Task 1: Extension-Ready Backend and Configuration Boundary

**Covers:** R12, R14, R15, R16, D2

**Objective:** The existing Pi feature runs through an agent-neutral compile-time backend/config boundary with no externally observable regression, and both legacy and structured config forms have an explicit tested contract.

**Files:**
- Create: `src/backend.rs`
- Modify: `src/config.rs`
- Modify: `src/pi/mod.rs`
- Modify: `src/pi/session.rs`
- Modify: `src/pi/resolver.rs`
- Modify: `src/runtime.rs`
- Modify: `src/lib.rs`
- Test: module tests in the modified Rust files and existing Pi cases in `tests/listener.rs`

**Dependencies:** Existing v0.1.0 Pi parser/resolver/runtime and protocol contracts.

**Implementation notes:**
- Begin with regression tests at the runtime reporting seam before moving Pi types behind the backend boundary.
- Keep the registry static and compiled. Do not add Codex/OpenCode placeholders, dynamic dispatch configuration, or external backend loading.
- Separate common pane key, candidate identity/fingerprint, display view, binding evidence, and backend outcome from Pi JSONL details.
- Keep source authority distinguishable from exact local hints: only successful official references carry `applies_to_source`.
- Parse config into one atomic value. Legacy `pi_session_dirs` and `[agents.pi].session_dirs` are alternatives, not merged forms.
- Preserve invalid-initial-load defaults and invalid-reload retention exactly.
- Keep global timing validation and absolute scheduling unchanged.

**Test cases:**
- Existing legacy config with only `pi_session_dirs` → same resolved roots and timing as v0.1.0.
- `[agents.pi].session_dirs` → roots expand, merge with defaults, and deduplicate.
- Both Pi forms, unknown agent table/key, relative root, invalid timing → complete config rejection; reload retains the prior valid Pi/Claude configuration.
- Existing Pi authoritative path, sticky fallback, `/new`/`/resume`, `--no-session`, activity retention, TTL, clear retry, reconnect, sequence epoch, and duplicate lock tests → unchanged expected reports.
- Backend registry → contains exactly Pi and Claude only after Task 2 adds Claude; Task 1 may temporarily contain Pi only without public behavior change.

**Complete when:**
- All existing Pi tests pass through the new boundary without weakened assertions.
- Legacy config remains accepted and structured Pi config is fully validated.
- Runtime and Herdr transport no longer depend on `PiSessionView` or Pi-only resolver types.
- No new runtime dependency or log content expands the privacy boundary.

**Validation:**
- Run: `cargo test config::tests --lib --locked && cargo test pi:: --lib --locked`
- Expected: all focused config and Pi tests pass.
- Run: `cargo test --test listener --locked`
- Expected: all v0.1.0 runtime/socket/subprocess behaviors pass unchanged.
- Run: `cargo clippy --all-targets -- -D warnings`
- Expected: exit 0 with no warnings.

### Task 2: Claude Session Parsing and Scoped Discovery

**Covers:** R1-R6, R8, R10, R11, R18, D3, D6, D7

**Objective:** The Claude backend resolves relevant local files and produces the agreed branch-aware display view from synthetic Claude `2.1.x` transcripts without reading unrelated projects or excluded execution modes.

**Files:**
- Create: `src/claude/mod.rs`
- Create: `src/claude/session.rs`
- Create: `src/claude/resolver.rs`
- Modify: `src/backend.rs`
- Modify: `src/config.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`
- Test: module tests in `src/claude/session.rs` and `src/claude/resolver.rs`

**Dependencies:** Task 1 common display/config/backend contracts.

**Implementation notes:**
- Resolve default root from listener-level `CLAUDE_CONFIG_DIR` plus `/projects`, otherwise `~/.claude/projects`; merge structured additions without process-environment inspection.
- Scope project discovery from canonical live pane cwd. Treat Claude's encoded project directory name as a lookup hint only; validate candidate `cwd` from JSONL before binding to avoid lossy-name collisions.
- Scan direct-child `*.jsonl` only. Do not descend into `subagents/`.
- Apply the 30-day/newest-25 limit only to ordinary discovery. Resolve authoritative IDs, exact UUID hints, and existing sticky paths directly even when old.
- Parse active message ancestry from the latest valid top-level message leaf. Reject cycles/missing ancestors and distinguish incomplete final writes from completed malformed records consistently with Pi.
- Parse latest matching `custom-title` and `ai-title` metadata outside the branch chain, with custom title precedence.
- Genuine user text excludes meta, sidechain, agent-authored, and tool-result entries. Assistant text excludes sidechain, API error, thinking, tool-use, fallback-only, and nontext blocks.
- Parse only changed bound transcripts fully; cache only structure/fingerprint/display state in memory and never transcript text in logs or disk state.
- Recognize full and short Claude flags safely (`--session-id`, `--resume`, `-r`, `--print`, `-p`, `--background`, `--bg`, `--no-session-persistence`) without logging argv.

**Test cases:**
- `custom-title`, then `ai-title`, then active first human user text, then cwd basename → exact name priority.
- Rewind fixture where branch A records appear first and the final persisted eligible record is branch B's leaf → branch B ancestry supplies user/activity text and branch A is ignored, regardless of record timestamps; trailing non-message metadata does not change the selected leaf.
- Latest human user followed by thinking/tool-use/tool-result/sidechain/API error and then top-level text → only the final eligible text appears, quoted.
- New human user without assistant text → parser returns no replacement activity so runtime can retain the prior same-session value.
- Multiline, whitespace-only, embedded quote, 78/79+ Unicode scalar activity → one line, balanced ASCII quotes, exact 80-scalar cap including ellipsis.
- Direct child versus nested subagent JSONL → only direct child is discovered.
- Relevant cwd encoding collision → candidate header cwd validation prevents cross-project binding.
- 26 recent candidates plus one old authoritative/UUID candidate → ordinary list caps at 25 while direct identity bypasses limits.
- `CLAUDE_CONFIG_DIR`, default home, structured roots, missing roots → deterministic merged roots without home-wide search.
- `--print`, `-p`, `--background`, `--bg`, `--no-session-persistence` → ineligible; normal, continue, resume, name, worktree, and remote-control forms remain eligible when persistent.
- UUID resume/session-id → exact file hint; resume name and continue → no direct identity hint.
- Partial final line → retryable failure/no refresh; malformed complete entry or invalid chain → parse failure; unknown record types → ignored.

**Complete when:**
- Synthetic tests prove every name/activity/filter/branch/limit/root/argument contract.
- No test or fixture contains a copied local Claude transcript or actual user path.
- Candidate discovery does not recursively scan unrelated Claude project directories for a live pane.
- Claude Code compatibility is structural and fails closed without an exact binary version gate.

**Validation:**
- Run: `cargo test claude:: --lib --locked`
- Expected: all Claude parser/resolver tests pass, including negative and Unicode boundaries.
- Run: `cargo test text::tests --lib --locked`
- Expected: shared Pi/Claude display formatting remains bounded and balanced.
- Run: `cargo clippy --all-targets -- -D warnings`
- Expected: exit 0 with no warnings.

### Task 3: Mixed-Agent Runtime, Authority, and Failure Isolation

**Covers:** R3, R5, R7-R9, R12-R14, R16, D1, D4, D5

**Objective:** One listener concurrently reports correct context for Pi and Claude panes, uses source-scoped authority where available, refuses ambiguous or broken identity, and isolates local failures without disturbing transport recovery.

**Files:**
- Modify: `src/backend.rs`
- Modify: `src/pi/resolver.rs`
- Modify: `src/claude/resolver.rs`
- Modify: `src/runtime.rs`
- Modify: `src/herdr/mod.rs`
- Modify: `src/herdr/protocol.rs`
- Modify: `src/herdr/socket.rs`
- Test: protocol tests in `src/herdr/protocol.rs` and mixed runtime/socket behavior in `tests/listener.rs`

**Dependencies:** Tasks 1 and 2.

**Implementation notes:**
- Group pane inputs by selected backend while keeping one shared Herdr `agent.list`, process-info collection, event subscription, poll deadline, and metadata reporter.
- Pass the selected backend's canonical agent label through `HerdrApi::report_metadata` and the protocol serializer. Assert `agent: "pi"` and `agent: "claude"` plus both token values, source, sequence, TTL, and `applies_to_source` in complete request payload tests.
- Preserve `PaneKey` terminal identity invalidation. Sticky state is in memory and per backend/session; never transfer retained activity across backend, terminal, or session identity changes.
- Official Pi path and Claude ID block lower precedence even when unreadable. Retry their exact target and let TTL expire.
- A resolved official binding reports with its exact source as `applies_to_source`; exact UUID/local fallback reports with no official source claim.
- For single-pane fallback, switch only when one new/changed candidate is uniquely supported. For multi-pane cold start, leave unsupported panes unbound rather than assigning by sort order.
- Handle local scanner/parser outcomes per pane. Continue reconciling other panes, then report/clear successful outcomes. Preserve transient clear retry.
- Herdr request/subscription failure remains a cycle/connection failure and uses existing backoff/full sync; do not hide transport failures as pane-local errors.
- Metadata-triggered `pane_updated` events must not create reporting loops or postpone the absolute poll deadline.

**Test cases:**
- One Pi and one Claude pane in the same fake `agent.list` → each backend reports the shared tokens from its own transcript.
- Pi official path and Claude official ID → exact files win over newer fallback candidates and reports carry their respective integration sources.
- Missing/malformed official Claude ID target with valid same-cwd candidate → no fallback and no TTL refresh; repair restores the exact report.
- Exact Claude UUID argument without integration → direct local binding, but `applies_to_source` remains unset.
- Resume name/continue with one pane → newest compatible candidate by mtime/path tie-break; with multiple same-project cold-start panes → every pane lacking direct evidence stays unbound, even if another pane's direct binding leaves one ordinary candidate.
- Single-pane sticky switching → unchanged bound file plus exactly one newer changed alternative switches; a changed bound file, no changed alternative, or multiple changed alternatives does not switch; removed/incompatible bound file re-runs deterministic initial selection.
- More than 25 recent files with malformed/cwd-incompatible entries ahead of valid entries → invalid entries do not consume the 25-compatible-candidate quota.
- Existing sticky multiple-pane bindings → no reshuffle when another file changes; listener restart without authority → no disk restoration.
- One malformed Claude pane plus healthy Claude and Pi panes → healthy panes refresh while failed pane expires.
- New human Claude message without replacement text → prior quoted activity remains; changed Claude session → prior activity is not inherited.
- Claude process changes to excluded headless/background mode, non-Claude agent, or new terminal identity → owned tokens clear/retry according to existing rules.
- Event burst, reconnect, sequence epoch, duplicate listener → existing absolute deadline and transport behavior remain valid with mixed agents.

**Complete when:**
- Runtime has no agent-specific conditional parsing outside backend selection/dispatch.
- All mixed-agent external reports match the precedence and failure contracts.
- Pi v0.1.0 regression tests remain green.
- No inferred identity is sent through canonical Herdr APIs.

**Validation:**
- Run: `cargo test --test listener --locked`
- Expected: mixed Pi/Claude, authority, ambiguity, failure isolation, reconnect, and subprocess tests all pass.
- Run: `cargo test --all-targets --locked`
- Expected: all Rust tests pass with no ignored failures.
- Run: `rg -n 'report_agent_session|report_agent\b' src`
- Expected: no production code writes inferred canonical identity; any protocol fixture/comment match is reviewed.

### Task 4: v0.2 Public Configuration, Documentation, and Packaging Contract

**Covers:** R3, R15, R17-R19, D8

**Objective:** Users can install/configure Pi and Claude rows without automatic agent/Herdr edits, understand optional integration accuracy and limitations, and build/package synchronized `0.2.0` artifacts under the unchanged release contract.

**Files:**
- Modify: `README.md`
- Modify: `docs/release-checklist.md`
- Modify: `AGENTS.md` only if architecture/commands changed materially.
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `herdr-plugin.toml`
- Modify: release/installer tests or scripts only for explicit version assertions.

**Dependencies:** Tasks 1-3 establish final public behavior.

**Implementation notes:**
- Show one `rows_by_agent` example with identical Pi and Claude token rows.
- Keep one-command plugin installation first. Explain that official integrations are optional and separately user-controlled; include exact `herdr integration install pi` and `herdr integration install claude` commands in an accuracy/native-resume section, not as automatic setup.
- Document name precedence, quoted activity, persistent interactive scope, same-project multi-pane conservative behavior, 30-day/25-candidate fallback limits with direct-identity bypass, `CLAUDE_CONFIG_DIR`, tested Claude version, and TTL failure behavior.
- Document structured config as preferred and legacy `pi_session_dirs` as accepted but mutually exclusive with `[agents.pi]`.
- State that the listener reads matching local transcripts and process metadata but never logs title/message text or sends runtime network requests.
- Synchronize package/plugin version to `0.2.0`; keep plugin ID, metadata token names, Herdr minimum, archive filenames/content, target matrix, and glibc baseline unchanged.
- Do not publish during this task.

**Test cases:**
- `verify-version.sh v0.2.0` → Cargo and plugin versions agree.
- README config examples → strict parser accepts each valid alternative when tested independently; no example contains both Pi config forms in one active snippet.
- Existing installer/archive negative suites → exact four target names, executable/license rules, checksum contract, and corrupt/symlink/hardlink rejection remain green at 0.2.0.
- Public privacy scan → no dependency or production source introduces runtime HTTP/DNS; logs do not accept display text.

**Complete when:**
- README is sufficient for a fresh Pi/Claude install and states all user-visible limitations.
- Release checklist covers both fallback and temporary official-integration validation plus restoration checks.
- Versions are synchronized at `0.2.0`, and existing package contracts pass unchanged.
- No automatic config or integration edit was added.

**Validation:**
- Run: `sh scripts/verify-version.sh v0.2.0`
- Expected: reports version `0.2.0` is consistent.
- Run: `sh tests/installer.sh && sh tests/release-assets.sh`
- Expected: all positive and negative distribution cases pass.
- Run: `shellcheck scripts/*.sh tests/*.sh && actionlint .github/workflows/*.yml`
- Expected: exit 0 with no diagnostics.
- Run: `git diff --check`
- Expected: no whitespace errors.

### Task 5: Dogfooding, Official-Integration Restoration, CI, and Promotion

**Covers:** R1-R20, D1-D8

**Objective:** Prove the exact v0.2 commit locally, in a disposable Herdr environment, through both identity paths, across all release targets, and—only after explicit user approval—through the published prerelease artifacts.

**Files:**
- Modify: `docs/release-checklist.md` checkboxes/evidence as implementation progresses.
- Modify: `docs/plans/2026-08-18-claude-code-agent-context.md` progress, actual file differences, and final evidence.
- Move after all validation: `docs/plans/2026-08-18-claude-code-agent-context.md` → `docs/plans/archived/2026-08-18-claude-code-agent-context.md`

**Dependencies:** Tasks 1-4 complete and reviewed.

**Implementation notes:**
- Build/stage the source binary and replace the managed v0.1.0 plugin with a local link only for dogfooding; preserve the user's manual sidebar rows and do not auto-edit Herdr config.
- Use synthetic/disposable Pi and Claude sessions. Never copy real transcripts into fixtures or logs.
- First validate hook-free Pi/Claude behavior, including Claude title/activity/rewind/exclusions, exact UUID hint, single-pane switching, and conservative same-project multi-pane cold start.
- Before integration tests, hash/back up the exact Pi extension directory entries and Claude settings/hook files affected by Herdr. Install official integrations with Herdr CLI, verify Pi path and Claude ID appear through `agent.list`, authoritative reports use matching sources, and native resume works after a disposable server restart.
- Uninstall both temporary integrations and verify checksums/file presence return exactly to the recorded state. A restoration mismatch blocks completion and requires user review; do not overwrite unrelated settings to force a match.
- Run an exact-HEAD nonpublishing CI matrix before tag creation. Download all four matrix artifacts together and run the release-asset verifier.
- Ask for explicit promotion confirmation after presenting dogfood, review, local, CI, artifact, and restoration evidence. Only then create/push `v0.2.0` and watch the tag CI and release workflow.
- Download the public Release assets, verify all four plus `SHA256SUMS`, run the public URL installer into a temporary root, and compare the installed host binary byte-for-byte with the archive.
- Restore the desired managed plugin installation after source-link dogfooding. Do not leave a duplicate listener or temporary integration.
- Expand `docs/release-checklist.md` with the exact pane creation/prompt/read commands used in the disposable session. Store actual pane IDs, integration sources, run IDs, immutable release SHA, backup files, and checksum outputs outside the repository under one `$AGENT_CONTEXT_EVIDENCE_DIR`; summarize links/results in the final report. After the release commit is frozen, do not edit tracked evidence until public verification finishes.

**Operational validation procedures:**

1. Stage and link the exact source build while preserving the managed install identity:

   ```sh
   set -eu
   test -z "$(git status --porcelain)"
   : "${AGENT_CONTEXT_EVIDENCE_DIR:=$(mktemp -d "${TMPDIR:-/tmp}/agent-context-v020-evidence.XXXXXX")}"
   export AGENT_CONTEXT_EVIDENCE_DIR
   evidence=$AGENT_CONTEXT_EVIDENCE_DIR
   release_sha=$(git rev-parse HEAD)
   test "$release_sha" = "$(git rev-parse origin/main)"
   printf '%s\n' "$release_sha" > "$evidence/release-sha.txt"
   herdr plugin list --plugin ryonakae.agent-context --json > "$evidence/plugin-before.json"
   jq -e '.result.plugins | length == 1 and .[0].version == "0.1.0" and
     .[0].source.kind == "github" and .[0].source.requested_ref == "v0.1.0"' \
     "$evidence/plugin-before.json" >/dev/null
   restore_plugin_baseline() {
     herdr plugin unlink ryonakae.agent-context >/dev/null 2>&1 || true
     herdr plugin install ryonakae/herdr-agent-context --ref v0.1.0 --yes >/dev/null
     herdr plugin list --plugin ryonakae.agent-context --json | jq -e '
       .result.plugins | length == 1 and .[0].enabled == true and
       .[0].version == "0.1.0" and .[0].source.kind == "github" and
       .[0].source.requested_ref == "v0.1.0"' >/dev/null
   }
   cleanup_validation() { restore_plugin_baseline; }
   trap 'cleanup_validation || { echo "plugin baseline restoration failed; evidence: $evidence" >&2; exit 1; }' EXIT
   trap 'exit 130' HUP INT TERM
   cargo build --release --locked
   mkdir -p bin
   cp target/release/herdr-agent-context bin/.herdr-agent-context.new
   chmod 755 bin/.herdr-agent-context.new
   mv bin/.herdr-agent-context.new bin/herdr-agent-context
   if ! herdr plugin unlink ryonakae.agent-context || ! herdr plugin link .; then
     restore_plugin_baseline
     trap - EXIT HUP INT TERM
     exit 1
   fi
   herdr plugin list --plugin ryonakae.agent-context --json | jq -e '
     .result.plugins | length == 1 and .[0].enabled == true and
     .[0].version == "0.2.0" and .[0].source.kind == "local"' >/dev/null
   ```

   Keep this exact clean `release_sha` source link active through hook-free and official-integration smoke. Restore the managed baseline only after the integration cleanup comparison succeeds. Any code, test, manifest, or tracked documentation change discovered during smoke invalidates `$AGENT_CONTEXT_EVIDENCE_DIR`; commit/push the fix and restart procedure 1 from a new evidence directory.

2. Snapshot exactly the real files official integrations may touch, verify both integrations are initially absent, install them, and compare the post-uninstall state byte-for-byte. Run from a shell where the relevant `PI_CODING_AGENT_DIR`/`CLAUDE_CONFIG_DIR` values match the agents under test:

   ```sh
   set -eu
   evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?run procedure 1 in the same validation shell}
   mkdir -p "$evidence"
   pi_dir=${PI_CODING_AGENT_DIR:-"$HOME/.pi/agent"}
   claude_dir=${CLAUDE_CONFIG_DIR:-"$HOME/.claude"}
   pi_hook="$pi_dir/extensions/herdr-agent-state.ts"
   claude_settings="$claude_dir/settings.json"
   claude_hook="$claude_dir/hooks/herdr-agent-state.sh"
   file_state() {
     for path in "$@"; do
       if [ -L "$path" ]; then
         printf 'L\t%s\t%s\t' "$path" "$(readlink "$path")"
         shasum -a 256 "$path" | awk '{print $1}'
       elif [ -f "$path" ]; then
         printf 'F\t%s\t' "$path"
         shasum -a 256 "$path" | awk '{print $1}'
       elif [ -e "$path" ]; then
         printf 'UNSUPPORTED\t%s\n' "$path"; return 1
       else
         printf 'MISSING\t%s\n' "$path"
       fi
     done
   }
   backup_file() {
     path=$1
     if [ -e "$path" ] || [ -L "$path" ]; then
       mkdir -p "$evidence/backup$(dirname "$path")"
       cp -a "$path" "$evidence/backup$path"
     fi
   }
   herdr integration status > "$evidence/status-before.txt"
   grep -q '^pi: not installed ' "$evidence/status-before.txt"
   grep -q '^claude: not installed ' "$evidence/status-before.txt"
   file_state "$pi_hook" "$claude_settings" "$claude_hook" > "$evidence/files-before.txt"
   backup_file "$pi_hook"
   backup_file "$claude_settings"
   backup_file "$claude_hook"
   cleanup_integrations() {
     cleanup_status=0
     herdr integration uninstall claude >/dev/null 2>&1 || true
     herdr integration uninstall pi >/dev/null 2>&1 || true
     file_state "$pi_hook" "$claude_settings" "$claude_hook" > "$evidence/files-after.txt" || cleanup_status=1
     cmp "$evidence/files-before.txt" "$evidence/files-after.txt" || cleanup_status=1
     herdr integration status > "$evidence/status-after.txt" || cleanup_status=1
     grep -q '^pi: not installed ' "$evidence/status-after.txt" || cleanup_status=1
     grep -q '^claude: not installed ' "$evidence/status-after.txt" || cleanup_status=1
     return "$cleanup_status"
   }
   cleanup_validation() {
     cleanup_status=0
     cleanup_integrations || cleanup_status=1
     restore_plugin_baseline || cleanup_status=1
     return "$cleanup_status"
   }
   trap 'cleanup_validation || { echo "integration/plugin restoration failed; evidence: $evidence" >&2; exit 1; }' EXIT
   trap 'exit 130' HUP INT TERM
   herdr integration install pi
   herdr integration install claude
   herdr integration status > "$evidence/status-installed.txt"
   grep -q '^pi: installed ' "$evidence/status-installed.txt"
   grep -q '^claude: installed ' "$evidence/status-installed.txt"
   ```

   During the disposable-session smoke, set the pane IDs from the creation responses, save redacted authority evidence, and assert exact kinds/sources without retaining transcript values:

   ```sh
   set -eu
   : "${pi_pane_id:?set from the disposable Pi pane creation response}"
   : "${claude_pane_id:?set from the disposable Claude pane creation response}"
   agent_json=$(herdr agent list)
   printf '%s\n' "$agent_json" | jq --arg pi "$pi_pane_id" --arg claude "$claude_pane_id" '
     [.result.agents[] | select(.pane_id == $pi or .pane_id == $claude) |
       {pane_id, agent, session_source:.agent_session.source,
        session_kind:.agent_session.kind,
        identity_nonempty: ((.agent_session.value | type) == "string" and
                            (.agent_session.value | length) > 0)}]
   ' > "$evidence/agent-authority-redacted.json"
   unset agent_json
   jq -e --arg pane "$pi_pane_id" '
     [.[] | select(.pane_id == $pane and .agent == "pi")] |
     length == 1 and .[0].session_kind == "path" and
     .[0].session_source == "herdr:pi" and .[0].identity_nonempty == true
   ' "$evidence/agent-authority-redacted.json" >/dev/null
   jq -e --arg pane "$claude_pane_id" '
     [.[] | select(.pane_id == $pane and .agent == "claude")] |
     length == 1 and .[0].session_kind == "id" and
     .[0].session_source == "herdr:claude" and .[0].identity_nonempty == true
   ' "$evidence/agent-authority-redacted.json" >/dev/null
   ```

   After native-resume validation, call cleanup explicitly. The EXIT/signal trap runs the same comparison if the procedure is interrupted. Any mismatch blocks completion; retain `$evidence` and ask the user rather than copying backups over live settings. Restore the managed v0.1.0 plugin only after cleanup succeeds:

   ```sh
   set -eu
   if ! cleanup_validation; then
     trap - EXIT HUP INT TERM
     echo "integration/plugin restoration failed; evidence: $evidence" >&2
     exit 1
   fi
   trap - EXIT HUP INT TERM
   ```

3. Capture and validate the exact-HEAD nonpublishing CI and its four artifacts:

   ```sh
   set -eu
   evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?run procedure 1 in the same validation shell}
   sha=$(cat "$evidence/release-sha.txt")
   test -z "$(git status --porcelain)"
   test "$(git rev-parse HEAD)" = "$sha"
   test "$(git rev-parse origin/main)" = "$sha"
   run_id=$(gh run list --workflow ci.yml --commit "$sha" --limit 10 \
     --json databaseId,headBranch,headSha \
     --jq '.[] | select(.headBranch == "main" and .headSha == "'"$sha"'") | .databaseId' | head -n 1)
   test -n "$run_id"
   test "$(gh run view "$run_id" --json headSha --jq .headSha)" = "$sha"
   gh run watch "$run_id" --exit-status
   printf '%s\n' "$run_id" > "$evidence/pre-release-ci-run-id.txt"
   dist=$(mktemp -d "${TMPDIR:-/tmp}/agent-context-ci-assets.XXXXXX")
   gh run download "$run_id" --pattern 'herdr-agent-context-*' --dir "$dist/download"
   find "$dist/download" -type f -name 'herdr-agent-context-v0.2.0-*.tar.gz' \
     -exec cp {} "$dist/" \;
   (cd "$dist" && shasum -a 256 herdr-agent-context-v0.2.0-*.tar.gz > SHA256SUMS)
   sh scripts/verify-release-assets.sh 0.2.0 "$dist"
   for target in aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
     mkdir "$dist/$target"
     tar -xzf "$dist/herdr-agent-context-v0.2.0-$target.tar.gz" \
       -C "$dist/$target" herdr-agent-context
     sh scripts/verify-glibc-baseline.sh "$dist/$target/herdr-agent-context" 2.18
   done
   ```

4. After explicit promotion approval, tag only the validated SHA, watch both tag workflows, then verify the public assets and installer:

   ```sh
   set -eu
   evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?run procedure 1 in the same validation shell}
   sha=$(cat "$evidence/release-sha.txt")
   test "$(git rev-parse HEAD)" = "$sha"
   test "$(git rev-parse origin/main)" = "$sha"
   sh scripts/verify-version.sh v0.2.0
   git tag -a v0.2.0 -m 'v0.2.0'
   test "$(git rev-list -n1 v0.2.0)" = "$sha"
   git push origin refs/tags/v0.2.0
   tag_ci=
   release_run=
   for _ in $(seq 1 30); do
     tag_ci=$(gh run list --workflow ci.yml --commit "$sha" --limit 10 \
       --json databaseId,headBranch --jq '.[] | select(.headBranch == "v0.2.0") | .databaseId' | head -n 1)
     release_run=$(gh run list --workflow release.yml --commit "$sha" --limit 1 \
       --json databaseId --jq '.[0].databaseId')
     [ -n "$tag_ci" ] && [ -n "$release_run" ] && break
     sleep 2
   done
   test -n "$tag_ci"
   test -n "$release_run"
   gh run watch "$tag_ci" --exit-status
   gh run watch "$release_run" --exit-status
   printf '%s\n' "$tag_ci" > "$evidence/tag-ci-run-id.txt"
   printf '%s\n' "$release_run" > "$evidence/release-run-id.txt"
   release_json=$(gh release view v0.2.0 --json isDraft,isPrerelease,tagName)
   printf '%s\n' "$release_json" | jq -e '
     .isDraft == false and .isPrerelease == true and .tagName == "v0.2.0"
   ' >/dev/null
   public=$(mktemp -d "${TMPDIR:-/tmp}/agent-context-public.XXXXXX")
   mkdir -p "$public/assets"
   gh release download v0.2.0 --dir "$public/assets"
   sh scripts/verify-release-assets.sh 0.2.0 "$public/assets"
   host_asset=$(sh scripts/install-binary.sh --print-asset)
   HERDR_AGENT_CONTEXT_INSTALL_ROOT="$public/install" sh scripts/install-binary.sh
   mkdir "$public/expected"
   tar -xzf "$public/assets/$host_asset" -C "$public/expected" herdr-agent-context
   cmp "$public/expected/herdr-agent-context" "$public/install/bin/herdr-agent-context"
   verify_plugin_v02() {
     herdr plugin list --plugin ryonakae.agent-context --json | jq -e '
       .result.plugins | length == 1 and .[0].enabled == true and
       .[0].version == "0.2.0" and .[0].source.kind == "github" and
       .[0].source.requested_ref == "v0.2.0"' >/dev/null
   }
   restore_v01_if_v02_missing() {
     verify_plugin_v02 && return 0
     herdr plugin unlink ryonakae.agent-context >/dev/null 2>&1 || true
     herdr plugin install ryonakae/herdr-agent-context --ref v0.1.0 --yes >/dev/null
   }
   trap 'restore_v01_if_v02_missing || { echo "managed plugin restoration failed" >&2; exit 1; }' EXIT
   trap 'exit 130' HUP INT TERM
   herdr plugin unlink ryonakae.agent-context
   herdr plugin install ryonakae/herdr-agent-context --ref v0.2.0 --yes
   verify_plugin_v02
   trap - EXIT HUP INT TERM
   ```

   The release checklist must record the actual host and `$host_asset` used.

5. After every release, public-asset, integration-restoration, and managed-install check succeeds, update the tracked checklist/plan once, archive the plan, and push a docs-only commit. This commit is intentionally after the immutable release SHA and must receive its own successful CI:

   ```sh
   set -eu
   evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?run procedure 1 in the same validation shell}
   release_sha=$(cat "$evidence/release-sha.txt")
   test "$(git rev-parse 'v0.2.0^{}')" = "$release_sha"
   test "$(git rev-parse HEAD)" = "$release_sha"
   test "$(git rev-parse origin/main)" = "$release_sha"
   test -z "$(git diff --cached --name-only)"
   test -z "$(git ls-files --others --exclude-standard)"
   expected_worktree=$(printf '%s\n' \
     docs/plans/2026-08-18-claude-code-agent-context.md \
     docs/release-checklist.md | LC_ALL=C sort)
   test "$(git diff --name-only | LC_ALL=C sort)" = "$expected_worktree"
   mkdir -p docs/plans/archived
   git mv docs/plans/2026-08-18-claude-code-agent-context.md \
     docs/plans/archived/2026-08-18-claude-code-agent-context.md
   git add docs/release-checklist.md docs/plans/archived/2026-08-18-claude-code-agent-context.md
   expected_staged=$(printf '%s\n' \
     docs/plans/2026-08-18-claude-code-agent-context.md \
     docs/plans/archived/2026-08-18-claude-code-agent-context.md \
     docs/release-checklist.md | LC_ALL=C sort)
   test "$(git diff --cached --name-only | LC_ALL=C sort)" = "$expected_staged"
   test -z "$(git diff --name-only)"
   test -z "$(git ls-files --others --exclude-standard)"
   git commit -m 'docs: archive Claude Code implementation plan'
   docs_sha=$(git rev-parse HEAD)
   test "$(git rev-parse HEAD^)" = "$release_sha"
   git push origin main
   docs_ci=
   for _ in $(seq 1 30); do
     docs_ci=$(gh run list --workflow ci.yml --commit "$docs_sha" --limit 1 \
       --json databaseId --jq '.[0].databaseId')
     [ -n "$docs_ci" ] && break
     sleep 2
   done
   test -n "$docs_ci"
   gh run watch "$docs_ci" --exit-status
   test -z "$(git status --porcelain)"
   test "$(git rev-parse origin/main)" = "$docs_sha"
   ```

**Test cases:**
- Hook-free live Pi and Claude panes → correct shared rows; no agent settings changed.
- Claude explicit custom name/AI title/user/cwd fallbacks and quoted activity → match R4/R5 within one poll interval.
- Claude rewind, tool-only, sidechain, API error, new user without answer → match branch/filter/retention contracts.
- Two same-project Claude panes without integration on cold start → unsupported panes remain empty; adding official integration or exact UUID yields deterministic binding.
- Stop/restart listener and disconnect/reconnect disposable socket → TTL expiry/full sync/sequence behavior matches automated tests.
- Temporary official integrations → exact Pi path and Claude ID authority, native resume, and exact post-uninstall restoration.
- Four-target CI and release archives → all jobs/checksums/content/glibc/installer smoke pass.

**Complete when:**
- Every automated and manual final-validation item below is checked with actual evidence.
- Independent review has no unresolved release-blocking findings.
- Temporary integrations/settings are fully restored and the intended plugin installation is active.
- If promotion was approved, public v0.2.0 assets pass post-publication verification; otherwise no tag/release exists and the plan remains unarchived until the agreed release goal is completed.
- The immutable `v0.2.0` tag points to the exact release commit validated by pre-release CI, tag CI, release workflow, and public artifact checks. After those checks, final evidence/checklist updates and plan archival occur in a separate docs-only commit whose parent contains the tagged release commit; that post-release commit is pushed and its CI passes before the repository is declared clean/synchronized.

**Validation:**
- Run: all commands in Final Validation.
- Expected: every command and manual gate succeeds; any skipped unsupported environment gate remains unchecked and blocks release/archive.

## Requirement Coverage

| Requirement / Decision | Task | Verification |
|---|---|---|
| R1, R2 | Tasks 2, 5 | Claude eligibility argument tests and hook-free live persistent/headless cases |
| R3, D1 | Tasks 2-4 | parser/runtime token assertions and Pi/Claude sidebar smoke |
| R4 | Tasks 2, 5 | synthetic custom/AI/user/cwd precedence plus live naming smoke |
| R5 | Tasks 2, 3, 5 | text/filter/retention tests and live turn progression |
| R6, D7 | Tasks 2, 5 | rewind parent-chain fixture and live rewind smoke |
| R7, D4 | Tasks 2, 3, 5 | Pi path/Claude ID fake socket tests and temporary integration smoke |
| R8 | Tasks 2, 3 | UUID versus name/continue resolver tests |
| R9, D5 | Tasks 3, 5 | sticky/single-pane/multi-pane/restart tests and smoke |
| R10, D3, D6 | Task 2 | direct-child, 30-day, 25-candidate, bypass, cache/fingerprint tests |
| R11 | Tasks 1, 2, 4 | config/root tests and README contract review |
| R12, D2 | Tasks 1, 3 | Pi regression through backend boundary and mixed registry tests |
| R13 | Task 3 | malformed Claude plus healthy mixed panes test |
| R14 | Tasks 1, 3 | existing deadline/reconnect/sequence/clear regression suite |
| R15 | Tasks 1, 4 | config watcher legacy/structured/conflict/atomic reload tests |
| R16 | Tasks 1-5 | privacy scans, synthetic fixture review, no canonical identity writes |
| R17 | Tasks 4, 5 | README optional integration section and no-auto-edit checksum checks |
| R18 | Tasks 2, 4, 5 | structural unknown/fail-closed tests and 2.1.220 live smoke |
| R19 | Tasks 4, 5 | version, installer/assets, four-target CI, glibc/public installer checks |
| R20 | Task 5 | recorded backup/install/authority/resume/uninstall/checksum evidence |
| D8 | Tasks 4, 5 | v0.2.0 version gate, explicit promotion approval, tag/release workflow |

## Final Validation

- [ ] `cargo fmt --check` — Expected: exit 0 with no formatting changes.
- [ ] `cargo clippy --all-targets -- -D warnings` — Expected: exit 0 with no warnings.
- [ ] `cargo test --all-targets --locked` — Expected: all Pi, Claude, backend, config, runtime, socket, reconnect, and lock tests pass with no ignored failures.
- [ ] `cargo build --release --locked` — Expected: host release binary builds successfully.
- [ ] `sh scripts/verify-version.sh v0.2.0` — Expected: Cargo/plugin/tag version contract is consistent.
- [ ] `sh tests/installer.sh` — Expected: supported-target install and every corrupt/checksum/archive/link negative case pass.
- [ ] `sh tests/release-assets.sh` — Expected: exact four-archive and checksum contract passes; malformed assets fail as intended.
- [ ] `shellcheck scripts/*.sh tests/*.sh` — Expected: exit 0 with no findings.
- [ ] `actionlint .github/workflows/*.yml` — Expected: exit 0 with no findings.
- [ ] `git diff --check` — Expected: no whitespace errors.
- [ ] `rg -n 'report_agent_session|report_agent\b' src` — Expected: no production canonical identity write; reviewed fixtures/comments only if present.
- [ ] Production privacy/dependency review — Expected: no runtime HTTP/DNS client, pane environment read, viewport scraping, transcript-bearing log call, real transcript fixture, or disk binding cache.
- [ ] Hook-free disposable Herdr smoke — Expected: Pi and persistent top-level Claude rows, name/activity/rewind/filter/switch/ambiguity/TTL/reconnect behavior match docs without changing agent settings.
- [ ] Temporary official integration smoke — Expected: Pi path and Claude ID are authoritative, source-scoped metadata and native resume work, and post-uninstall Pi/Claude files/checksums exactly match the backup.
- [ ] Source-link cleanup — Expected: no duplicate listener remains and the intended managed plugin/source state is restored.
- [ ] Exact-HEAD nonpublishing CI — Expected: quality and all four target build/package jobs pass for the commit proposed for `v0.2.0`.
- [ ] Downloaded matrix artifact verification — Expected: all four archives together pass `scripts/verify-release-assets.sh 0.2.0 <dist>` and Linux artifacts meet glibc `2.18`.
- [ ] Independent implementation/distribution review — Expected: no unresolved correctness, privacy, lifecycle, compatibility, or release finding.
- [ ] Explicit promotion approval recorded before tag creation.
- [ ] If approved, tag CI and Release workflow for `v0.2.0` — Expected: both succeed and publish a non-draft prerelease with four archives plus `SHA256SUMS` from the tagged commit.
- [ ] Public Release verification — Expected: downloaded public assets pass the verifier; public URL installer installs a byte-identical host binary into a temporary root.
- [ ] Requirement Coverage has no unmapped requirement or decision.
- [ ] Plan and actual changed files/contracts agree; minor differences are reflected in the relevant task.
- [ ] Release identity — Expected: `v0.2.0^{}` points to the immutable release SHA recorded before CI; `origin/main` contains that SHA and may be ahead only by the final docs-only evidence/archive commit.
- [ ] After every release/artifact/restoration item above succeeds, update final checklist evidence and move this file unchanged to `docs/plans/archived/2026-08-18-claude-code-agent-context.md` in one docs-only post-release commit; push it, require its CI to pass, and leave the worktree clean.

## Risks and Open Questions

- Claude JSONL is not a guaranteed stable public API. Structural parsing, explicit compatibility language, unknown-record tolerance, fail-closed required fields, and live validation mitigate drift.
- Claude project-directory encoding is lossy. Treat encoded paths only as scan hints and validate transcript cwd/session identity before binding.
- Official Claude identity is an ID in Herdr's public agent view, so ID-to-file resolution must search only configured/relevant roots and block fallback while the official reference exists.
- Multi-pane hook-free cold start intentionally sacrifices availability for attribution safety. README and live smoke must make this visible rather than presenting it as exact matching.
- Full branch parsing may be more expensive than ZAM's tail parser. Relevant-directory/candidate limits, fingerprint caching, and changed-bound-file parsing must keep the two-second poll sustainable; add measured synthetic large-file coverage if implementation profiling exposes a regression without changing the public timing contract.
- Temporary integration restoration is operationally sensitive. Any checksum mismatch is a blocker and must be reported rather than repaired destructively.
- No unresolved product or public-contract questions remain after the completed `dig`; implementation discoveries that would change these contracts require renewed user confirmation.
