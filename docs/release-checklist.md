# Release checklist

Run this checklist from a clean checkout before promoting the `v0.4.0` prerelease. Use synthetic prompts and a disposable named Herdr session. Do not read or copy real transcripts into release evidence.

## Automated gates

- [x] `cargo fmt --check`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo test --all-targets --locked`
- [x] `cargo build --release --locked`
- [x] `sh scripts/verify-version.sh v0.4.0`
- [x] `sh tests/installer.sh`
- [x] `sh tests/release-assets.sh`
- [x] `shellcheck scripts/*.sh tests/*.sh`
- [x] `actionlint .github/workflows/*.yml`
- [x] `git diff --check`
- [ ] The nonpublishing CI quality job and all four target jobs passed for the proposed release SHA.
- [ ] Downloaded CI archives passed `scripts/verify-release-assets.sh 0.4.0 <dist>`.
- [ ] Both Linux archives passed `scripts/verify-glibc-baseline.sh <binary> 2.18`.

## Evidence and source plugin setup

Open one dedicated validation shell and keep it open through source setup, integration installation, and final cleanup. Paste the fenced commands into that shell in order; do not run a fenced block as a standalone script or subshell because the cleanup functions and traps must persist. Use a second terminal only for the named Herdr TUI and pane commands, exporting the same evidence-directory path there.

Use one directory outside the repository for pane IDs, checksums, run IDs, and redacted `agent.list` results:

```sh
set -eu
export AGENT_CONTEXT_EVIDENCE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/agent-context-v040-evidence.XXXXXX")
printf '%s\n' "$(git rev-parse HEAD)" > "$AGENT_CONTEXT_EVIDENCE_DIR/release-sha.txt"
test -z "$(git status --porcelain)"
test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"
```

Record the existing plugin state, build the same clean SHA, then replace the managed v0.3.0 installation with a source link:

```sh
set -eu
evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?}
herdr plugin list --plugin ryonakae.agent-context --json > "$evidence/plugin-before.json"
jq -e '.result.plugins | length == 1 and .[0].enabled == true and
  .[0].version == "0.3.0" and .[0].source.kind == "github" and
  .[0].source.requested_ref == "v0.3.0"' "$evidence/plugin-before.json" >/dev/null
restore_plugin_baseline() {
  herdr plugin unlink ryonakae.agent-context >/dev/null 2>&1 || true
  herdr plugin install ryonakae/herdr-agent-context --ref v0.3.0 --yes >/dev/null
  herdr plugin list --plugin ryonakae.agent-context --json | jq -e '
    .result.plugins | length == 1 and .[0].enabled == true and
    .[0].version == "0.3.0" and .[0].source.kind == "github" and
    .[0].source.requested_ref == "v0.3.0"' >/dev/null
}
cleanup_validation() { restore_plugin_baseline; }
trap 'cleanup_validation || { echo "plugin restoration failed; evidence: $evidence" >&2; exit 1; }' EXIT
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
  .[0].version == "0.4.0" and .[0].source.kind == "local"' >/dev/null
```

- [ ] `herdr plugin list --plugin ryonakae.agent-context --json` reports one enabled local `0.4.0` plugin.
- [ ] No second `herdr-agent-context listen` process is running for the same socket.
- [ ] Any code or tracked documentation change found during smoke invalidated this evidence directory and restarted validation from a new clean SHA.

## Disposable Herdr session

Create a temporary Herdr configuration that contains the README's shared agent sidebar rows. From a separate terminal, launch or attach the named session:

```sh
HERDR_CONFIG_PATH="$AGENT_CONTEXT_EVIDENCE_DIR/herdr-config.toml" \
  herdr --session agent-context-v040
```

Run the remaining pane commands from a shell inside that session. Parse every pane ID from the JSON response instead of predicting it:

```sh
pi_pane=$(herdr pane split --current --direction right --cwd "$PWD" --no-focus \
  | jq -er '.result.pane.pane_id')
claude_pane=$(herdr pane split --current --direction down --cwd "$PWD" --no-focus \
  | jq -er '.result.pane.pane_id')
codex_pane=$(herdr pane split --pane "$pi_pane" --direction down --cwd "$PWD" --no-focus \
  | jq -er '.result.pane.pane_id')
printf '%s\n' "$pi_pane" > "$AGENT_CONTEXT_EVIDENCE_DIR/pi-pane-id.txt"
printf '%s\n' "$claude_pane" > "$AGENT_CONTEXT_EVIDENCE_DIR/claude-pane-id.txt"
printf '%s\n' "$codex_pane" > "$AGENT_CONTEXT_EVIDENCE_DIR/codex-pane-id.txt"
herdr agent start context_pi --kind pi --pane "$pi_pane"
herdr agent start context_claude --kind claude --pane "$claude_pane" -- --name agent-context-smoke
herdr agent start context_codex --kind codex --pane "$codex_pane"
```

Use synthetic prompts only:

```sh
herdr agent prompt context_pi "Reply with a short synthetic Pi status." --wait --timeout 120000
herdr agent prompt context_claude "Reply with a short synthetic Claude status." --wait --timeout 120000
herdr agent prompt context_codex "Reply with a short synthetic Codex status." --wait --timeout 120000
```

## Hook-free sidebar behavior

### Pi

- [ ] An unnamed session uses the first user text, then cwd basename, as the name fallback.
- [ ] An explicit Pi session name replaces the fallback within one poll interval.
- [ ] The latest assistant text appears without added surrounding quotes.
- [ ] A new user entry retains prior activity until the next assistant text arrives.
- [ ] `/new` and `/resume` update a single-pane sticky binding without showing another cwd's transcript.
- [ ] Two same-cwd Pi panes keep established bindings stable.
- [ ] A visible `--no-session` process clears both plugin-owned tokens.

### Claude Code

- [ ] Current `customTitle` and legacy `title` records take precedence over the latest matching `ai-title`.
- [ ] Without a matching custom or AI title, the session-name token stays empty even when first-user text and cwd exist.
- [ ] A matching normalized terminal title is accepted; a missing or mismatched terminal title falls back to the verified JSONL title.
- [ ] Live (not run in this review): a newly started Claude session without a custom or AI title leaves the existing tab baseline unchanged.
- [ ] The latest top-level assistant text after the latest human entry appears without added surrounding quotes.
- [ ] Thinking, tool activity, tool results, sidechains, API errors, and abandoned branches never appear.
- [ ] A new human entry retains prior activity; switching to another session does not carry it across.
- [ ] `--session-id <uuid>` or UUID `--resume` binds the exact local file without claiming an official source.
- [ ] Resume by name and `--continue` use local fallback rather than direct identity.
- [ ] `--print`, `--background`, and `--no-session-persistence` leave the plugin rows empty.
- [ ] Two same-project Claude panes on a hook-free cold start stay empty without direct evidence.
- [ ] Established same-project sticky bindings do not reshuffle after unrelated file activity.
- [ ] A bound incomplete tail does not switch to an older candidate or refresh TTL; repair restores the same binding.

### Codex

- [ ] The latest nonblank exact-ID `thread_name` wins, followed by first genuine user text and cwd basename fallbacks.
- [ ] The latest commentary or final assistant text appears; reasoning, system/developer records, tools, completion echoes, and nontext content never appear.
- [ ] A new genuine user message retains prior activity for the same session; switching identity never carries it across.
- [ ] An official ID binds exactly and reports the matching source; UUID `resume` binds exactly without claiming an official source.
- [ ] Normal starts wait for one uniquely new or changed same-cwd rollout after pane observation; a cold listener does not attach an old transcript.
- [ ] Targetless, named, `--last`, and UUID resume remain interactive; fork binds only after the child identity becomes observable.
- [ ] `exec`, review, remote, ephemeral, subagent, internal, MCP, app-server, and non-root sources leave plugin rows empty.
- [ ] Multiple same-cwd Codex panes or multiple changed candidates remain empty without official or exact evidence.
- [ ] A bound partial or completed structurally invalid rollout does not refresh TTL; repair restores the same identity.
- [ ] An index-only rename refreshes sidebar, tab, and pane labels; a malformed completed index entry falls back without suppressing valid rollout activity.

### Shared behavior

- [ ] Multiline values stay on one row; exactly 80 scalars remain unchanged and longer values truncate to 79 scalars plus an ellipsis.
- [ ] Sidebar and tab-component bounds derive independently from one complete title; a grapheme over 80 scalars stays intact when it fits the 20-column component limit.
- [ ] Stopping the listener lets metadata expire after TTL; restart performs a full sync.
- [ ] Replacing a pane terminal identity clears the prior terminal's owned metadata.
- [ ] Socket disconnect/reconnect performs a new full sync with a fresh sequence epoch.
- [ ] Invalid plugin config keeps the previous timing and all three agents' roots.
- [ ] Plugin logs contain no synthetic title, prompt, or assistant text.

## Optional tab and pane label behavior

Keep the default-off check separate from the opt-in checks. For the opt-in smoke, back up the plugin's `config.toml`, enable `[tab_name]` and `[pane_name]` independently and together, then restore the exact original file before cleanup. Use only the disposable named session and synthetic agents created above. If the live smoke is not applicable, leave its boxes unchecked and record the reason; automated coverage does not count as an executed live check.

Current v0.4.0 review status: live source and integration smoke will not run because named sessions share the global Herdr plugin registry, and replacing it with the unreleased build would mutate the active user's plugin state. The user explicitly accepted this residual risk on 2026-08-30; automated and exact-SHA distribution coverage do not count as live smoke.

- [ ] With both naming tables omitted, the listener sends no `session.snapshot`, `tab.rename`, or `pane.rename` request and naming-only events do not refresh metadata.
- [ ] Generated components use the Pi or Codex name or verified Claude title, preserve grapheme clusters, and occupy at most 20 terminal columns; the aggregate retains every component joined with ` + `.
- [ ] Background tabs follow visual pane order (top to bottom, then left to right); focus changes cause no rename and do not move the absolute poll deadline.
- [ ] Shell, unsupported, and untitled panes contribute nothing without hiding resolved panes in the same tab.
- [ ] A manual rename suppresses only the current ordered session composition. Another composition can acquire the tab, and returning restores the exact manual label.
- [ ] Pane moves recompute source and destination aggregates while keeping baselines and overrides tab-local. Closing a tab removes its ownership state.
- [ ] Each generated pane label uses its own 20-column session title; manual rename and clear override only that pane's current session and never change the tab aggregate.
- [ ] Pane session and terminal replacement, move, close, listener restart, and `[pane_name] enabled = false` restore or recompute the exact owned label.
- [ ] Setting either naming feature to `false` restores its latest baseline. An inferred numeric tab baseline uses the current workspace-local position after reordering.
- [ ] Pane and tab state failures disable only their own synchronizer without blocking the other synchronizer or sidebar metadata. State files contain manual labels but no plaintext generated title, session identity, terminal/binding generation, or socket path.
- [ ] Live (not run in this review): Pi, Claude, and Codex generated/custom labels, focus switching, shell retention, manual override, config disable, and listener restart match the rules above in an isolated disposable Herdr session.
- [ ] Live (not run in this review): after the selected Pi process exits, the tab recomputes from any remaining resolved components; it restores the exact baseline only when no resolved component remains.
- [ ] Live (not run in this review): force-stopping the listener leaves the generated custom label; restarting and then setting `enabled = false` restores the saved numeric baseline.
- [ ] Live cleanup (not applicable because the smoke was not run): remove the listener, synthetic agents and transcripts, temporary pane, isolated server, state/config directories, and disposable session.

## Temporary official integrations

Run these checks only after hook-free behavior passes. Return to the persistent validation shell from the setup section; do not run this block in the disposable pane shell. The validation shell must use the same `PI_CODING_AGENT_DIR`, `CLAUDE_CONFIG_DIR`, and `CODEX_HOME` as the test agents.

```sh
set -eu
evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?}
pi_dir=${PI_CODING_AGENT_DIR:-"$HOME/.pi/agent"}
claude_dir=${CLAUDE_CONFIG_DIR:-"$HOME/.claude"}
codex_dir=${CODEX_HOME:-"$HOME/.codex"}
pi_hook="$pi_dir/extensions/herdr-agent-state.ts"
claude_settings="$claude_dir/settings.json"
claude_hook="$claude_dir/hooks/herdr-agent-state.sh"
codex_settings="$codex_dir/config.toml"
codex_hook="$codex_dir/herdr-agent-state.sh"
sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}
file_state() {
  for path in "$@"; do
    if [ -L "$path" ]; then
      printf 'L\t%s\t%s\t' "$path" "$(readlink "$path")"
      sha256_file "$path"
    elif [ -f "$path" ]; then
      printf 'F\t%s\t' "$path"
      sha256_file "$path"
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
herdr integration status > "$evidence/integration-status-before.txt"
grep -q '^pi: not installed ' "$evidence/integration-status-before.txt"
grep -q '^claude: not installed ' "$evidence/integration-status-before.txt"
grep -q '^codex: not installed ' "$evidence/integration-status-before.txt"
file_state "$pi_hook" "$claude_settings" "$claude_hook" "$codex_settings" "$codex_hook" > "$evidence/files-before.txt"
backup_file "$pi_hook"
backup_file "$claude_settings"
backup_file "$claude_hook"
backup_file "$codex_settings"
backup_file "$codex_hook"
cleanup_integrations() {
  cleanup_status=0
  herdr integration uninstall codex >/dev/null 2>&1 || true
  herdr integration uninstall claude >/dev/null 2>&1 || true
  herdr integration uninstall pi >/dev/null 2>&1 || true
  file_state "$pi_hook" "$claude_settings" "$claude_hook" "$codex_settings" "$codex_hook" > "$evidence/files-after.txt" || cleanup_status=1
  cmp "$evidence/files-before.txt" "$evidence/files-after.txt" || cleanup_status=1
  herdr integration status > "$evidence/integration-status-after.txt" || cleanup_status=1
  grep -q '^pi: not installed ' "$evidence/integration-status-after.txt" || cleanup_status=1
  grep -q '^claude: not installed ' "$evidence/integration-status-after.txt" || cleanup_status=1
  grep -q '^codex: not installed ' "$evidence/integration-status-after.txt" || cleanup_status=1
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
herdr integration install codex
```

- [ ] `herdr integration status` reports Pi, Claude, and Codex as `current`.
- [ ] Restart fresh synthetic Pi, Claude, and Codex sessions so all integrations initialize.
- [ ] Pi reports `kind=path`; Claude and Codex report `kind=id`; all values are nonempty.
- [ ] Metadata reports use `applies_to_source=herdr:pi`, `herdr:claude`, or `herdr:codex` for the matching pane.
- [ ] A newer same-cwd fallback cannot replace an authoritative session.
- [ ] Missing or malformed authoritative targets do not fall back and do not refresh TTL.
- [ ] Native Pi, Claude, and Codex resume keep the exact context after restarting the disposable named session.

In a shell inside the disposable named session, export the same `AGENT_CONTEXT_EVIDENCE_DIR` path and save only identity shape, not identity values or metadata tokens:

```sh
set -eu
evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?}
pi_pane_id=$(cat "$evidence/pi-pane-id.txt")
claude_pane_id=$(cat "$evidence/claude-pane-id.txt")
codex_pane_id=$(cat "$evidence/codex-pane-id.txt")
agent_json=$(herdr agent list)
printf '%s\n' "$agent_json" | jq --arg pi "$pi_pane_id" --arg claude "$claude_pane_id" --arg codex "$codex_pane_id" '
  [.result.agents[] | select(.pane_id == $pi or .pane_id == $claude or .pane_id == $codex) |
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
jq -e --arg pane "$codex_pane_id" '
  [.[] | select(.pane_id == $pane and .agent == "codex")] |
  length == 1 and .[0].session_kind == "id" and
  .[0].session_source == "herdr:codex" and .[0].identity_nonempty == true
' "$evidence/agent-authority-redacted.json" >/dev/null
```

Return to the persistent validation shell. Uninstall all three integrations, restore the managed plugin, and compare the exact preinstall state. Do not overwrite a mismatch from the backup:

```sh
if ! cleanup_validation; then
  trap - EXIT HUP INT TERM
  echo "integration/plugin restoration failed; evidence: $evidence" >&2
  exit 1
fi
trap - EXIT HUP INT TERM
```

- [ ] File presence, link targets, and checksums match exactly after uninstall.
- [ ] No restoration mismatch occurred; any mismatch would have stopped release work for manual review.

## Cleanup and promotion evidence

- [ ] Temporary source validation cleanup completed before promotion (not applicable; live source validation was not run).
- [ ] The verified managed v0.4.0 plugin is enabled after promotion.
- [ ] No source listener, temporary integration, disposable pane, or named test session remains.
- [ ] The repository is clean and `HEAD == origin/main` before exact-SHA CI validation.

Select the CI run by immutable SHA, flatten all target artifacts, generate their checksum manifest, and run the same release validators:

```sh
set -eu
evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?}
sha=$(cat "$evidence/release-sha.txt")
test "$(git rev-parse HEAD)" = "$sha"
test "$(git rev-parse origin/main)" = "$sha"
run_id=$(gh run list --workflow ci.yml --commit "$sha" --limit 10 \
  --json databaseId,headBranch,headSha \
  --jq '.[] | select(.headBranch == "main" and .headSha == "'"$sha"'") | .databaseId' \
  | head -n 1)
test -n "$run_id"
test "$(gh run view "$run_id" --json headSha --jq .headSha)" = "$sha"
gh run watch "$run_id" --exit-status
printf '%s\n' "$run_id" > "$evidence/pre-release-ci-run-id.txt"
dist=$(mktemp -d "${TMPDIR:-/tmp}/agent-context-ci-assets.XXXXXX")
gh run download "$run_id" --pattern 'herdr-agent-context-*' --dir "$dist/download"
find "$dist/download" -type f -name 'herdr-agent-context-v0.4.0-*.tar.gz' \
  -exec cp {} "$dist/" \;
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$dist" && sha256sum herdr-agent-context-v0.4.0-*.tar.gz > SHA256SUMS)
else
  (cd "$dist" && shasum -a 256 herdr-agent-context-v0.4.0-*.tar.gz > SHA256SUMS)
fi
sh scripts/verify-release-assets.sh 0.4.0 "$dist"
for target in aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  mkdir "$dist/$target"
  tar -xzf "$dist/herdr-agent-context-v0.4.0-$target.tar.gz" \
    -C "$dist/$target" herdr-agent-context
  sh scripts/verify-glibc-baseline.sh "$dist/$target/herdr-agent-context" 2.18
done
```

- [ ] Record the exact SHA and CI run ID under `$AGENT_CONTEXT_EVIDENCE_DIR`.
- [ ] All four downloaded CI archives pass checksum, content, executable, and Linux glibc `2.18` checks.
- [ ] Independent implementation and distribution review has no unresolved finding.
- [x] Obtain explicit promotion approval before creating or pushing `v0.4.0` (approved 2026-08-30).
- [ ] After approval, tag CI and the Release workflow both pass for the recorded SHA.
- [ ] The public prerelease contains four archives plus `SHA256SUMS`.
- [ ] Public assets pass `scripts/verify-release-assets.sh 0.4.0`.
- [ ] The public URL installer installs a host binary byte-identical to its archive.
- [ ] Replace the managed v0.3.0 plugin with the verified managed v0.4.0 release.

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
