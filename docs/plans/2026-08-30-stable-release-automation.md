# Stable Release Automation Implementation Plan

> **For implementers:** Execute tasks in order unless dependencies allow otherwise. Mark a task complete only after its validation succeeds. Reflect minor implementation differences in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Problem Statement

`herdr-agent-context` currently publishes every `v*` tag as a GitHub prerelease through `softprops/action-gh-release` with GitHub-generated notes. Release notes are not reviewable as a tracked source artifact, stable tag validity and `main` ancestry are not enforced, version preparation is manual across multiple files, and rerunning publication does not fail closed when an existing release differs from the intended body or assets. The sibling `zerdr` and `shepherd` repositories provide stronger release contracts through tracked changelogs, pre-publication validation, generated notes, and idempotent publication.

## Goal

Establish a tested stable-release process in which `CHANGELOG.md` is the source of truth, one preparation command synchronizes release-owned versions, CI validates release contracts continuously, stable tags build the existing four archives, and GitHub Release publication uses deterministic notes and safely verifies an existing release on reruns.

## Out of Scope

- Creating or pushing a new version tag.
- Publishing a new GitHub Release or modifying the existing `v0.1.0` through `v0.4.0` prereleases.
- Migrating packaging to `cargo-dist`, adding Homebrew, or changing the four supported targets and `.tar.gz` archive format.
- Changing the installer URL, plugin installation contract, runtime behavior, or Herdr integration protocol.
- Automatically choosing the next semantic version or deriving authored changelog bullets from commits or pull requests.
- Adding prerelease-tag support to the new workflow.

## Requirements and Decisions

### Requirements

- **R1:** Add a public `CHANGELOG.md` containing accurate, descending `v0.1.0` through `v0.4.0` history derived only from existing public releases, repository history, and public documentation.
- **R2:** Validate that changelog headings are unique stable `vX.Y.Z` versions in descending order, the target is the latest section, each section has at least one allowed Keep a Changelog category, and every category contains a bullet.
- **R3:** Render deterministic GitHub Release notes from the target changelog section plus exact Install, Validation, and previous-tag comparison blocks.
- **R4:** Provide `scripts/prepare-release.sh X.Y.Z` that validates an already-authored latest changelog section and atomically synchronizes `Cargo.toml`, the root package entry in `Cargo.lock`, and `herdr-plugin.toml`; invalid input or inconsistent source versions must leave every file unchanged.
- **R5:** Remove release-to-release version churn from installer and release-asset tests and make the release checklist use `VERSION`/`TAG` rather than requiring edits for every new release.
- **R6:** Accept only stable tags of the exact form `vX.Y.Z`; require tag, Cargo package, plugin manifest, lockfile package, and latest changelog versions to match; require the tagged commit to be an ancestor of `origin/main`.
- **R7:** On a stable tag, preserve the current four-target build, archive validation, checksum generation, Linux glibc `2.18` verification, and installer smoke before publication.
- **R8:** Publish a non-draft, non-prerelease GitHub Release marked latest with deterministic generated notes and the exact four archives plus `SHA256SUMS`.
- **R9:** Publication must be rerun-safe: an existing release succeeds only when its tag/state, required generated note blocks, and exact asset-name set match; any mismatch fails without overwriting public state.
- **R10:** CI must exercise changelog, release-note, version-preparation, tag-validation, and workflow contracts without creating a tag or release.
- **R11:** Document the stable release preparation, promotion, verification, recovery, and the fact that `v0.4.0` and older releases remain prereleases.

### Implementation Decisions

- **D1:** Starting with the next release, GitHub Releases are stable; existing prereleases remain unchanged.
- **D2:** `CHANGELOG.md`, not GitHub's generated notes, is the authored source of release changes.
- **D3:** Release notes contain the target changelog section, plugin Install command, workflow-backed Validation claims, and `vPREVIOUS...vCURRENT` comparison link.
- **D4:** Version selection remains an explicit maintainer decision; the preparation command does not infer SemVer impact.
- **D5:** Publication follows Shepherd's fail-closed rerun model rather than overwriting or rejecting every existing release.
- **D6:** Retain the repository's POSIX `sh` script convention and existing custom packaging rather than importing Shepherd's Node toolchain or Zerdr's `cargo-dist` pipeline.
- **D7:** Release tests derive fixture versions from manifests or explicit test inputs, so normal releases do not require mechanical test edits.

### Contracts

- `scripts/release-notes.sh check [X.Y.Z]` validates the changelog; without an argument it also requires the latest changelog version to equal the package version.
- `scripts/release-notes.sh render X.Y.Z` writes the complete deterministic release body to stdout.
- `scripts/release-notes.sh verify X.Y.Z BODY_FILE` accepts the exact rendered body or that complete body followed by operator notes. It rejects text before the generated title, text inserted between mandatory blocks, duplicated blocks, and any altered/missing/reordered block.
- `scripts/prepare-release.sh X.Y.Z` accepts stable bare versions only. The target changelog section must already exist and be latest. All release-owned files are replaced only after every source and generated output has validated.
- `scripts/validate-release-tag.sh vX.Y.Z COMMIT [ROOT]` validates stable syntax, all synchronized versions, the latest changelog entry, and `COMMIT` ancestry under `origin/main`; a test-only Git command seam may be used without weakening production behavior.
- `scripts/check-github-release.sh X.Y.Z BODY_FILE [REPOSITORY]` owns the complete pre-publication decision. It queries the tag release and latest-release endpoint through `gh`, prints `absent` only for a confirmed tag-release HTTP 404, and prints `existing` only after verifying `tagName=vX.Y.Z`, `name=vX.Y.Z`, `isDraft=false`, `isPrerelease=false`, latest identity, the exact five asset names, and the deterministic body. Every other API/transport/parse/state/body/asset failure exits nonzero. A `GH_COMMAND` test seam permits a fake CLI without weakening production defaults.
- GitHub Releases created by the workflow satisfy the same state/body contract; the workflow branches only on the tested script's `absent` or `existing` result.

## Current Context

### Confirmed

- Current package and managed plugin version is `0.4.0`; `Cargo.lock` has one root `herdr-agent-context` package entry at that version.
- `.github/workflows/release.yml` currently matches all `v*` tags, builds four target archives, verifies assets and installer behavior, and always creates a prerelease with GitHub-generated notes.
- `.github/workflows/ci.yml` already runs quality checks plus a nonpublishing four-target build matrix.
- `scripts/verify-version.sh` currently compares only tag, `Cargo.toml`, and `herdr-plugin.toml`.
- `tests/installer.sh` and `tests/release-assets.sh` hardcode `0.4.0` fixtures.
- `docs/release-checklist.md` mixes reusable procedure, version-specific commands, and immutable historical promotion records.
- `zerdr` uses a tracked Keep a Changelog file and `cargo-dist`; `shepherd` uses a tracked changelog, deterministic release-note rendering, stable tag/main validation, and rerun-safe GitHub Release creation.
- Existing public `v0.1.0` through `v0.4.0` releases are prereleases and must not be edited.

### Assumptions

- The release-note implementation may share private helper scripts when that keeps POSIX entrypoints small; no new runtime dependency may be required by installed plugin users.
- Historical changelog wording may consolidate implementation-level commits into user-visible bullets as long as it remains factually supported by public repository history.

## File Structure

- Create: `CHANGELOG.md` — public authored release history and future release-note source.
- Create: `scripts/release-notes.sh` — changelog checking, deterministic rendering, and existing-body verification entrypoint.
- Create: `scripts/prepare-release.sh` — atomic release-owned version synchronization.
- Create: `scripts/validate-release-tag.sh` — stable tag, synchronized version, changelog, and main-ancestry validation.
- Create: `scripts/check-github-release.sh` — tested GitHub API acquisition, confirmed-404 absence decision, latest/state/body/asset verification, and rerun result.
- Create: `tests/release-notes.sh` — changelog grammar, rendering, body verification, and malformed-input tests.
- Create: `tests/prepare-release.sh` — successful synchronization and failure atomicity tests in temporary repositories.
- Create: `tests/release-tag.sh` — stable tag and ancestry contract tests with isolated Git repositories.
- Create: `tests/github-release.sh` — fixture-driven existing-release state, body, asset, and API-failure tests.
- Modify: `scripts/verify-version.sh` — include lockfile and latest changelog consistency where appropriate without duplicating tag ancestry responsibility.
- Modify: `tests/installer.sh` — derive fixture asset names from the manifest version.
- Modify: `tests/release-assets.sh` — derive fixture versions and cover expanded version consistency.
- Modify: `.github/workflows/ci.yml` — run release-contract tests continuously.
- Modify: `.github/workflows/release.yml` — stable-tag validation, deterministic notes, stable publication, and fail-closed rerun verification.
- Modify: `docs/release-checklist.md` — convert the reusable procedure to `VERSION`/`TAG`, preserve historical records, and describe stable promotion gates.
- Modify: `AGENTS.md` — list release automation commands and the changelog/release ownership contract.
- Modify: `README.md` only if a public stable install or release link currently requires correction; otherwise leave it unchanged.

## Testing Decisions

- **Test seam:** Invoke public POSIX scripts as subprocesses against temporary roots and temporary Git repositories; run the complete publication-state script against a fake `gh` CLI that returns controlled HTTP/API responses; inspect exit status, stdout/body output, invoked API operations, and exact file contents.
- **Behavior:** Cover valid current history, future-version preparation, malformed/duplicate/out-of-order changelog entries, invalid SemVer/tag forms, version divergence, non-main ancestry, deterministic notes, exact comparison links, strict mandatory body placement, confirmed release absence, release state/asset mismatches, API/transport failures, and no partial writes.
- **Prior art:** Follow `tests/installer.sh` and `tests/release-assets.sh` for portable temporary-directory setup and negative assertions; follow Shepherd's release-note and tag contracts semantically without importing its Node implementation.
- **Avoid:** Tests must not call GitHub mutation APIs, create remote tags/releases, modify global Herdr state, depend on real transcripts, or assert private helper implementation details.

## Progress

- Review base: `07165944aed8bebd0baf217d46603783f61c79d5`
- [x] Task 1: Establish changelog and deterministic release-note contracts.
- [ ] Task 2: Add atomic release preparation and stable-tag validation.
- [ ] Task 3: Convert CI and publication workflow to tested stable releases.
- [ ] Task 4: Make release operations reusable and document the new process.
- [ ] Task 5: Complete independent review and full validation.

Implementation may reflect minor file changes in the relevant task. Ask the user before changing requirements, Out of Scope, or public contracts.

## Tasks

### Task 1: Establish changelog and deterministic release-note contracts

**Covers:** R1, R2, R3, D2, D3, D6

**Objective:** A tracked changelog can be validated and rendered into one deterministic, reviewable GitHub Release body.

**Files:**
- Create: `CHANGELOG.md`
- Create: `scripts/release-notes.sh`
- Create: `tests/release-notes.sh`

**Dependencies:** Public GitHub releases/tags and repository history for factual backfill.

**Implementation notes:**
- Use TDD: start with valid/malformed changelog fixtures and expected rendered blocks.
- Allowed categories are `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, and `Security`; require at least one bullet per present category.
- Ignore headings hidden inside fenced code and comments sufficiently to prevent false release sections; do not build a general Markdown parser.
- Require strictly descending stable versions and a next-older entry before rendering a comparison link.
- Render install instructions using `herdr plugin install ryonakae/herdr-agent-context --ref vX.Y.Z --yes` and state only validations actually enforced by the workflow.
- Backfill historical entries in English and keep public prose concise.

**Test cases:**
- Valid `v0.4.0` through `v0.1.0` changelog → check succeeds and prints the selected version.
- Missing target, non-latest target, duplicate/out-of-order/invalid version, unknown/duplicate/empty category, or category without a bullet → check fails.
- Render `0.4.0` fixture → exact title, release changes, install command, validation block, and `v0.3.0...v0.4.0` link.
- Verify an exact rendered body or body with permitted trailing operator notes → succeeds; changed/missing/reordered/duplicated mandatory block, prefixed text, or text inserted between blocks → fails.

**Complete when:**
- Historical entries are factual and ordered.
- Release-note output is deterministic and fully covered by positive and negative tests.
- No GitHub API or repository mutation is needed to preview notes.

**Implementation outcome:**
- Added factual `v0.1.0` through `v0.4.0` history from public tags, README contracts, and release metadata.
- Added POSIX `check`, `render`, and strict `verify` commands with temporary-root test coverage.
- Focused validation passed: `sh tests/release-notes.sh`, real changelog check/render, `shellcheck`, and `git diff --check`.

**Validation:**
- Run: `sh tests/release-notes.sh`
- Expected: all changelog and release-note contract tests pass.
- Run: `sh scripts/release-notes.sh check 0.4.0 && sh scripts/release-notes.sh render 0.4.0 >/tmp/herdr-agent-context-v0.4.0-notes.md`
- Expected: check prints `0.4.0`; render succeeds with no repository changes.

### Task 2: Add atomic release preparation and stable-tag validation

**Covers:** R4, R5, R6, D4, D7

**Objective:** Maintainers can synchronize a reviewed version safely, and tags fail before building when any release identity or ancestry condition is wrong.

**Files:**
- Create: `scripts/prepare-release.sh`
- Create: `scripts/validate-release-tag.sh`
- Create: `tests/prepare-release.sh`
- Create: `tests/release-tag.sh`
- Modify: `scripts/verify-version.sh`
- Modify: `tests/installer.sh`
- Modify: `tests/release-assets.sh`

**Dependencies:** Task 1 changelog checker.

**Implementation notes:**
- Use TDD and temporary roots; preserve file modes and clean temporary files through traps.
- Validate every input and current cross-file version before preparing temporary outputs. Commit replacements only after all outputs pass consistency checks.
- Restrict modifications to the root package version in `Cargo.lock`; dependency versions with the same text must remain unchanged.
- Make tests derive their baseline version from `herdr-plugin.toml` or explicit fixture values.
- Stable tag validation fetches `origin/main` without tags and uses `git merge-base --is-ancestor`; tests use isolated repositories or an explicit command seam rather than the real remote.

**Test cases:**
- Valid latest future changelog plus synchronized current files → all three release-owned versions update and validation succeeds.
- Invalid version syntax, missing/non-latest changelog, divergent current versions, missing/duplicate replacement anchor, or malformed lockfile → command fails and byte-for-byte file snapshots remain unchanged.
- `vX.Y.Z` matching all sources at a commit on `origin/main` → tag validation succeeds.
- Bare version, prerelease/build tag, mismatched source, stale changelog, unknown commit, or commit outside `origin/main` → validation fails.
- Installer and asset tests pass without hardcoded current version strings.

**Complete when:**
- Successful preparation changes only expected release-owned version fields.
- Every tested failure is atomic.
- Tag validation rejects all unsupported tag and ancestry cases before artifact build.

**Validation:**
- Run: `sh tests/prepare-release.sh && sh tests/release-tag.sh && sh tests/installer.sh && sh tests/release-assets.sh`
- Expected: all positive, negative, and atomicity cases pass.

### Task 3: Convert CI and publication workflow to tested stable releases

**Covers:** R7, R8, R9, R10, D1, D5

**Objective:** CI continuously checks release tooling, and a valid future stable tag publishes or safely verifies one deterministic stable release.

**Files:**
- Create: `scripts/check-github-release.sh`
- Create: `tests/github-release.sh`
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml`
- Test: `tests/release-notes.sh`
- Test: `tests/release-tag.sh`
- Test: `tests/release-assets.sh`

**Dependencies:** Tasks 1 and 2.

**Implementation notes:**
- Narrow the tag trigger to stable `vX.Y.Z` syntax as far as Actions glob syntax permits; the validation script remains authoritative.
- Checkout full history where ancestry validation requires it and render notes before expensive builds/publication.
- Preserve four builders, glibc gate, archive contents, checksum generation, asset validator, and installer smoke.
- Replace `softprops` generated prerelease publication with explicit `gh release` logic that can inspect an existing release.
- Move release lookup, confirmed-404 absence classification, latest endpoint lookup, normalization, and verification into `check-github-release.sh`; the workflow must not reimplement these decisions.
- API/transport/JSON failure and every non-404 tag lookup failure stop publication rather than resemble an absent release. Latest endpoint failure also stops an existing-release rerun.
- Before treating an existing release as success, verify exact tag/title, non-draft/non-prerelease/latest identity, strict required body placement, and the normalized exact five-name asset set. Never overwrite a conflicting release.
- Determine latest identity through the repository latest-release endpoint rather than assuming stable implies latest.
- New publication uses `--verify-tag`, exact tag/SHA target, deterministic notes, stable/latest flags, and five verified assets.
- Pin third-party actions to immutable commits where practical and keep least-privilege permissions per job.

**Test cases:**
- Workflow syntax passes `actionlint`.
- Fake `gh` returns a confirmed tag-release 404 → script prints `absent` and publication path remains eligible; exact existing release plus matching latest endpoint → script prints `existing` without overwrite.
- Tag lookup 401/403/500, transport failure, malformed JSON, latest endpoint failure, draft, prerelease, non-latest, wrong tag/title, changed body, and missing/extra/duplicate assets → script fails.
- Static contract checks confirm no `prerelease: true` or `generate_release_notes: true`, notes are rendered before publication, stable tag validation runs, and the tested existing-release verifier occurs before success.
- Existing archive/installer negative tests remain green.

**Complete when:**
- Pull requests and `main` exercise all release scripts without publishing.
- Only a validated stable tag can reach publication.
- The workflow is rerun-safe and keeps current distribution artifacts unchanged.

**Validation:**
- Run: `sh tests/github-release.sh`
- Expected: confirmed 404 returns `absent`, exact existing/latest release returns `existing`, and every non-404 API/transport/latest/state/body/asset/malformed-response case fails closed.
- Run: `actionlint .github/workflows/*.yml`
- Expected: both workflows are valid.
- Run: `shellcheck scripts/*.sh tests/*.sh`
- Expected: no shell diagnostics.
- Run: `rg -n 'prerelease: true|generate_release_notes: true' .github/workflows/release.yml`
- Expected: no matches.

### Task 4: Make release operations reusable and document the new process

**Covers:** R5, R11, D1, D4

**Objective:** Maintainers have one version-independent, auditable stable release procedure and repository guidance names the correct commands and source of truth.

**Files:**
- Modify: `docs/release-checklist.md`
- Modify: `AGENTS.md`
- Modify: `README.md` only if required by the public stable install contract

**Dependencies:** Tasks 1 through 3 define exact commands and workflow behavior.

**Implementation notes:**
- Preserve immutable `v0.2.0` through `v0.4.0` promotion records; do not rewrite prior release evidence.
- Replace current-version commands in reusable sections with validated `VERSION` and `TAG` variables.
- Document preparation order: author latest changelog section, preview notes, run prepare command, validate, push release commit, await exact-SHA CI, obtain explicit promotion approval, create annotated tag, verify stable release/assets, then update managed plugin.
- Document rerun recovery and conflict stop conditions.
- State explicitly that historical releases through `v0.4.0` remain prereleases and the next release starts the stable policy.
- Keep public install docs free of maintainer-only detail.

**Test cases:**
- Every documented command names a real script and uses accepted argument forms.
- Reusable checklist sections contain no assumed next version.
- Historical promotion records retain their exact SHAs and run IDs.

**Complete when:**
- A maintainer can prepare and verify a future release without editing test fixtures or procedural version literals.
- AGENTS and checklist agree on ownership and commands.

**Validation:**
- Run: `rg -n 'prepare-release|release-notes|validate-release-tag|CHANGELOG' AGENTS.md docs/release-checklist.md`
- Expected: all new contracts are documented.
- Run: `git diff --check`
- Expected: no whitespace errors.

### Task 5: Complete independent review and full validation

**Covers:** R1-R11, D1-D7

**Objective:** The complete release automation is independently reviewed and passes all repository and distribution gates without external mutation.

**Files:**
- Modify: this plan's progress and task status while implementing.
- Move after success: `docs/plans/2026-08-30-stable-release-automation.md` to `docs/plans/archived/2026-08-30-stable-release-automation.md`.

**Dependencies:** Tasks 1 through 4.

**Implementation notes:**
- Use a read-only reviewer to inspect fail-closed behavior, rerun safety, shell portability, workflow permissions, and scope compliance.
- Resolve all blocking/high findings and rerun focused tests after corrections.
- Do not create tags, releases, or mutate Herdr/plugin state during validation.

**Test cases:**
- Independent reviewer finds no unresolved blocking/high defect.
- Full Rust, shell, workflow, installer, archive, changelog, preparation, and tag-validation suites pass from the final tree.
- Only intended source/docs/workflow/test files are changed; no generated `target/`, `dist/`, or `bin/` content is tracked.

**Complete when:**
- All Final Validation items pass.
- Requirement Coverage has no gap.
- Plan is archived only after successful validation.

**Validation:**
- Run the complete Final Validation command set below.
- Expected: every command succeeds and reviewer has no unresolved blocking/high finding.

## Requirement Coverage

| Requirement / Decision | Task | Verification |
|---|---|---|
| R1 | Task 1 | Historical changelog review and `release-notes.sh check` |
| R2 | Task 1 | Positive/negative changelog fixtures |
| R3 | Task 1 | Exact render and mandatory-block verification tests |
| R4 | Task 2 | Preparation success and byte-for-byte failure atomicity tests |
| R5 | Tasks 2, 4 | Dynamic installer/assets tests and version-independent checklist review |
| R6 | Task 2 | Stable syntax, source consistency, and isolated ancestry tests |
| R7 | Task 3 | Workflow review, actionlint, archive/glibc/installer gates |
| R8 | Task 3 | Static workflow contract and deterministic note render tests |
| R9 | Tasks 1, 3 | Strict body-placement tests plus fake-`gh` absence/existing/API/latest/state/asset fixtures |
| R10 | Task 3 | CI workflow includes release-note, tag, preparation, and complete publication-decision suites |
| R11 | Task 4 | AGENTS/checklist command and policy review |
| D1 | Tasks 3, 4 | Stable workflow flags and historical policy text |
| D2 | Task 1 | `CHANGELOG.md` parser/render contract |
| D3 | Task 1 | Render snapshot/structural assertions |
| D4 | Tasks 2, 4 | Explicit preparation argument and documented maintainer flow |
| D5 | Task 3 | Existing-release verification before success, no overwrite path |
| D6 | Tasks 1-3 | POSIX shellcheck and no added installed runtime dependency |
| D7 | Task 2 | Tests pass while deriving version dynamically |

## Final Validation

- [ ] `sh tests/release-notes.sh` — Expected: changelog check/render/verify positive and negative cases pass.
- [ ] `sh tests/prepare-release.sh` — Expected: synchronization and all failure-atomicity cases pass.
- [ ] `sh tests/release-tag.sh` — Expected: stable tag, version consistency, and ancestry cases pass.
- [ ] `sh tests/github-release.sh` — Expected: confirmed 404 and exact existing/latest paths return the correct decisions; all non-404 API/transport/latest/state/body/asset/malformed-response cases fail closed.
- [ ] `sh tests/installer.sh` — Expected: installer contract remains green with dynamic version fixtures.
- [ ] `sh tests/release-assets.sh` — Expected: archive/version contract remains green with dynamic version fixtures.
- [ ] `cargo test --all-targets --locked` — Expected: all Rust tests pass.
- [ ] `cargo fmt --check` — Expected: formatting passes.
- [ ] `cargo clippy --all-targets -- -D warnings` — Expected: no warnings.
- [ ] `cargo build --release --locked` — Expected: release build succeeds.
- [ ] `shellcheck scripts/*.sh tests/*.sh` — Expected: no diagnostics.
- [ ] `actionlint .github/workflows/*.yml` — Expected: both workflows are valid.
- [ ] `sh scripts/verify-version.sh v0.4.0` — Expected: manifests, lockfile, and latest changelog are consistent.
- [ ] `sh scripts/release-notes.sh check 0.4.0` — Expected: current tracked release identity is valid.
- [ ] `git diff --check` — Expected: no whitespace errors.
- [ ] Manual GitHub mutation test: N/A — no tag/release is created in this implementation; workflow behavior is verified through script tests, static review, and actionlint.
- [ ] Requirement Coverage has no unmatched item.
- [ ] Plan and actual changes agree.
- [ ] After every item succeeds, move this plan unchanged in name to `docs/plans/archived/`.

## Risks and Open Questions

- POSIX shell Markdown parsing can become brittle if expanded into a general parser; constrain supported changelog grammar and test hidden headings, fences, comments, and malformed sections explicitly.
- GitHub Actions tag globs are not full regular expressions; the workflow must treat the validation script as authoritative.
- `gh release view` asset ordering is not stable; compare normalized exact name sets rather than API order.
- Existing release body verification must permit only documented extra operator notes without allowing altered mandatory blocks.
- Existing `v0.4.0` and older GitHub prereleases remain intentionally unchanged.
- No unresolved product or release-policy question remains.
