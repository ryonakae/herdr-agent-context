# Release checklist

Use this procedure for each future stable release. `CHANGELOG.md` is the authored source of release changes; generated GitHub Release notes are derived from it. Historical releases through `v0.4.0` remain prereleases and must not be changed. The next release and later releases use the stable policy.

Run only from a clean local `main` checkout. Do not read or copy real transcripts into release evidence; use synthetic prompts, synthetic fixtures, and a disposable named Herdr session only.

## Release identity

Choose the version explicitly; `scripts/prepare-release.sh` does not infer SemVer impact.

```sh
set -eu
VERSION=X.Y.Z
TAG=v$VERSION
plugin_before=$(herdr plugin list --plugin ryonakae.agent-context --json)
BASELINE_VERSION=$(printf '%s\n' "$plugin_before" | jq -er '
  .result.plugins | if length == 1 then .[0].version else error("expected one managed plugin") end')
BASELINE_REF=$(printf '%s\n' "$plugin_before" | jq -er '
  .result.plugins[0].source |
  if .kind == "github" and (.requested_ref | type) == "string" then .requested_ref
  else error("expected one managed GitHub plugin") end')
unset plugin_before
export AGENT_CONTEXT_EVIDENCE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/agent-context-${VERSION}-evidence.XXXXXX")
```

## Prepare the release commit

- [ ] Confirm `git status --porcelain` is empty and `HEAD` equals `origin/main`.
- [ ] Author the latest `CHANGELOG.md` section for `$TAG` with reviewed, user-visible changes.
- [ ] Check and preview the deterministic notes before changing release-owned versions:

```sh
sh scripts/release-notes.sh check "$VERSION"
sh scripts/release-notes.sh render "$VERSION" > "$AGENT_CONTEXT_EVIDENCE_DIR/$TAG-notes.md"
```

- [ ] Synchronize the package, lockfile, and managed-plugin versions:

```sh
sh scripts/prepare-release.sh "$VERSION"
sh scripts/release-notes.sh check "$VERSION"
git diff -- Cargo.toml Cargo.lock herdr-plugin.toml CHANGELOG.md
```

- [ ] Run all local gates:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
sh tests/release-notes.sh
sh tests/prepare-release.sh
sh tests/release-tag.sh
sh tests/github-release.sh
sh tests/installer.sh
sh tests/release-assets.sh
shellcheck scripts/*.sh tests/*.sh
actionlint .github/workflows/*.yml
git diff --check
```

- [ ] Commit the reviewed release-owned changes and push that commit to `main`. Record its immutable SHA:

```sh
RELEASE_SHA=$(git rev-parse HEAD)
git push origin main
test "$RELEASE_SHA" = "$(git rev-parse origin/main)"
printf '%s\n' "$RELEASE_SHA" > "$AGENT_CONTEXT_EVIDENCE_DIR/release-sha.txt"
```

## Exact-SHA CI and approval

- [ ] Wait for the nonpublishing CI quality job and all four target jobs for `$RELEASE_SHA`, not merely the latest branch run.
- [ ] Download and validate that exact run's artifacts:

```sh
run_id=$(gh run list --workflow ci.yml --commit "$RELEASE_SHA" --limit 10 \
  --json databaseId,headBranch,headSha \
  --jq '.[] | select(.headBranch == "main" and .headSha == "'"$RELEASE_SHA"'") | .databaseId' \
  | head -n 1)
test -n "$run_id"
test "$(gh run view "$run_id" --json headSha --jq .headSha)" = "$RELEASE_SHA"
gh run watch "$run_id" --exit-status
printf '%s\n' "$run_id" > "$AGENT_CONTEXT_EVIDENCE_DIR/pre-release-ci-run-id.txt"

dist=$(mktemp -d "${TMPDIR:-/tmp}/agent-context-${VERSION}-ci-assets.XXXXXX")
gh run download "$run_id" --pattern 'herdr-agent-context-*' --dir "$dist/download"
find "$dist/download" -type f -name "herdr-agent-context-${TAG}-*.tar.gz" -exec cp {} "$dist/" \;
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$dist" && sha256sum "herdr-agent-context-${TAG}-"*.tar.gz > SHA256SUMS)
else
  (cd "$dist" && shasum -a 256 "herdr-agent-context-${TAG}-"*.tar.gz > SHA256SUMS)
fi
sh scripts/verify-release-assets.sh "$VERSION" "$dist"
for target in aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  mkdir "$dist/$target"
  tar -xzf "$dist/herdr-agent-context-${TAG}-$target.tar.gz" -C "$dist/$target" herdr-agent-context
  sh scripts/verify-glibc-baseline.sh "$dist/$target/herdr-agent-context" 2.18
done
```

- [ ] Obtain explicit promotion approval after the exact-SHA CI and artifact gates pass. Record the approval and residual risks in `$AGENT_CONTEXT_EVIDENCE_DIR`.
- [ ] Do not create or move a tag, create a release, or update the managed plugin before this approval.

## Tag, publish, and verify

- [ ] Create and push an annotated stable tag pointing at `$RELEASE_SHA`:

```sh
test "$(git rev-parse HEAD)" = "$RELEASE_SHA"
git tag -a "$TAG" "$RELEASE_SHA" -m "Release $TAG"
git push origin "$TAG"
```

- [ ] Wait for tag CI and the tag-driven Release workflow. The workflow validates `scripts/validate-release-tag.sh`, builds four archives, validates assets and Linux glibc compatibility, smoke-tests the installer, and creates a non-draft, non-prerelease latest release with deterministic notes.
- [ ] Verify public stable/latest state, exact notes, and assets without mutation:

```sh
sh scripts/release-notes.sh render "$VERSION" > "$AGENT_CONTEXT_EVIDENCE_DIR/$TAG-notes.md"
sh scripts/check-github-release.sh "$VERSION" "$AGENT_CONTEXT_EVIDENCE_DIR/$TAG-notes.md" ryonakae/herdr-agent-context
```

- [ ] Verify the public installer on the release host and compare its installed binary with the matching `$TAG` archive.
- [ ] Update the managed plugin only after public verification:

```sh
herdr plugin install ryonakae/herdr-agent-context --ref "$TAG" --yes
herdr plugin list --plugin ryonakae.agent-context --json
```

## Rerun recovery

A rerun is safe only when `scripts/check-github-release.sh` reports `existing`: the tag, title, stable/latest state, deterministic generated-note blocks, and exact five assets must match. An `absent` result allows the workflow to create the release once.

Stop promotion on an API or transport error, malformed response, conflicting release state/body/assets, or any other nonzero result. Do not move, delete, overwrite, or recreate a tag or GitHub Release to recover. Investigate the conflicting public state and obtain explicit approval for any subsequent action.

## Optional live synthetic smoke

Live source and integration smoke is optional when it would mutate a shared Herdr plugin registry or otherwise be unsafe. Automated coverage and exact-SHA artifact checks do not count as executed live smoke. If skipped, leave the smoke boxes unchecked and record the specific risk and approval in `$AGENT_CONTEXT_EVIDENCE_DIR`.

- [ ] Before linking a source build, record the one managed plugin's version and requested ref as `$BASELINE_VERSION` and `$BASELINE_REF`; restore that exact baseline during cleanup.
- [ ] Use only a disposable named Herdr session and synthetic Pi, Claude, and Codex prompts; keep pane IDs, checksums, run IDs, and redacted `agent.list` results outside the repository.
- [ ] Confirm hook-free sidebar behavior, session binding, TTL clearing, and privacy-bounded logs for the supported backends.
- [ ] If testing temporary official integrations, back up and restore exact file presence, link targets, and checksums; do not overwrite a restoration mismatch.
- [ ] Remove the source listener, temporary integrations, synthetic agents/transcripts, disposable pane/session, and temporary state/config directories before promotion.
- [ ] Confirm the managed `$TAG` plugin is enabled after public verification and that no duplicate listener remains.

## Historical promotion records

The following records are immutable evidence for prerelease promotions. Do not edit them.

## v0.2.0 promotion record

- Release SHA: `6f4ed7e918538276c252044b0638c18e1deb368b`.
- Exact-HEAD pre-release CI: `32337430470`; quality and all four target jobs passed.
- Tag CI: `32338069478`; quality and all four target jobs passed.
- Release workflow: `32338069510`; all four builds, asset validation, installer smoke, and prerelease publication passed.
- Public prerelease: <https://github.com/ryonakae/herdr-agent-context/releases/tag/v0.2.0>.
- Public assets: four target archives plus `SHA256SUMS`; archive contents, checksums, executable bits, and both Linux glibc `2.18` baselines passed.
- Public installer host: `Darwin arm64`; asset `herdr-agent-context-v0.2.0-aarch64-apple-darwin.tar.gz`; installed and archived binary SHA-256 `8fa57bf2c5a71706faf5bb6e55db0f693fbf595934dac4433c11ddc5a1e6fa03`.
- Managed plugin: enabled `v0.2.0`, requested ref `v0.2.0`, resolved commit `6f4ed7e918538276c252044b0638c18e1deb368b`; the running listener uses the managed release binary.
- Integrations after promotion: Pi and Claude `current (v8)`; Codex remains integration-only and is outside the v0.2.0 transcript backend scope.
- Independent implementation reviews completed with no unresolved findings; live Herdr dogfood confirmed Pi startup fail-closed behavior and unquoted recent activity.

## v0.3.0 promotion record

- Release SHA: `d37c9dc4d60415b2f8edf0e0023b1b2f32b60d4e`.
- Exact-HEAD pre-release CI: `32853915694`; quality and all four target jobs passed.
- Tag CI: `32855068242`; quality and all four target jobs passed.
- Release workflow: `32855068270`; all four builds, asset validation, installer smoke, and prerelease publication passed.
- Public prerelease: <https://github.com/ryonakae/herdr-agent-context/releases/tag/v0.3.0>.
- Public assets: four target archives plus `SHA256SUMS`; archive contents, checksums, executable bits, and both Linux glibc `2.18` baselines passed.
- Public installer host: `Darwin arm64`; asset `herdr-agent-context-v0.3.0-aarch64-apple-darwin.tar.gz`; installed and archived binary SHA-256 `95ffbcd2cb06ba5faa7f657181c43d311dc1d7b4c55637d633ad9ec6f6f5bd8c`.
- Managed plugin: enabled `v0.3.0`, requested ref `v0.3.0`, resolved commit `d37c9dc4d60415b2f8edf0e0023b1b2f32b60d4e`; no duplicate listener was running after installation.
- Integrations after promotion: Pi, Claude, and Codex `current (v8)`.
- Independent review found no unresolved implementation or distribution defect. Live source and integration smoke was not run because Herdr's plugin registry is global; promotion proceeded after explicit acceptance of that residual risk.

## v0.4.0 promotion record

- Release SHA: `4d8daae4262a12a49b5d8094480daa17bb9a4f29`.
- Exact-HEAD pre-release CI: `33281544682`; quality and all four target jobs passed.
- Tag CI: `33281731261`; quality and all four target jobs passed.
- Release workflow: `33281731259`; all four builds, asset validation, installer smoke, and prerelease publication passed.
- Public prerelease: <https://github.com/ryonakae/herdr-agent-context/releases/tag/v0.4.0>.
- Public assets: four target archives plus `SHA256SUMS`; archive contents, checksums, executable bits, and both Linux glibc `2.18` baselines passed.
- Public installer host: `Darwin arm64`; asset `herdr-agent-context-v0.4.0-aarch64-apple-darwin.tar.gz`; installed, archived, and managed-plugin binary SHA-256 `dfad13887da907dc701a1460f7f9b20a95603814cd53065079ccf40643273ebc`.
- Managed plugin: enabled `v0.4.0`, requested ref `v0.4.0`, resolved commit `4d8daae4262a12a49b5d8094480daa17bb9a4f29`; its installed binary is byte-identical to the public archive.
- Integrations after promotion: Pi, Claude, and Codex `current (v8)`.
- Independent implementation and release-candidate reviews found no unresolved blocking/high finding. Live source and integration smoke was not run because Herdr's plugin registry is global; promotion proceeded after explicit user acceptance of that residual risk.
