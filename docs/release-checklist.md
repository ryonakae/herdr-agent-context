# Release checklist

Run this checklist from a clean checkout before promoting the `v0.2.0` prerelease. Use synthetic prompts and a disposable named Herdr session. Do not read or copy real transcripts into release evidence.

## Automated gates

- [x] `cargo fmt --check`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo test --all-targets --locked`
- [x] `cargo build --release --locked`
- [x] `sh scripts/verify-version.sh v0.2.0`
- [x] `sh tests/installer.sh`
- [x] `sh tests/release-assets.sh`
- [x] `shellcheck scripts/*.sh tests/*.sh`
- [x] `actionlint .github/workflows/*.yml`
- [x] `git diff --check`
- [x] The nonpublishing CI quality job and all four target jobs passed for the proposed release SHA.
- [x] Downloaded CI archives passed `scripts/verify-release-assets.sh 0.2.0 <dist>`.
- [x] Both Linux archives passed `scripts/verify-glibc-baseline.sh <binary> 2.18`.

## Evidence and source plugin setup

Open one dedicated validation shell and keep it open through source setup, integration installation, and final cleanup. Paste the fenced commands into that shell in order; do not run a fenced block as a standalone script or subshell because the cleanup functions and traps must persist. Use a second terminal only for the named Herdr TUI and pane commands, exporting the same evidence-directory path there.

Use one directory outside the repository for pane IDs, checksums, run IDs, and redacted `agent.list` results:

```sh
set -eu
export AGENT_CONTEXT_EVIDENCE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/agent-context-v020-evidence.XXXXXX")
printf '%s\n' "$(git rev-parse HEAD)" > "$AGENT_CONTEXT_EVIDENCE_DIR/release-sha.txt"
test -z "$(git status --porcelain)"
test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"
```

Record the existing plugin state, build the same clean SHA, then replace the managed v0.1.0 installation with a source link:

```sh
set -eu
evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?}
herdr plugin list --plugin ryonakae.agent-context --json > "$evidence/plugin-before.json"
jq -e '.result.plugins | length == 1 and .[0].enabled == true and
  .[0].version == "0.1.0" and .[0].source.kind == "github" and
  .[0].source.requested_ref == "v0.1.0"' "$evidence/plugin-before.json" >/dev/null
restore_plugin_baseline() {
  herdr plugin unlink ryonakae.agent-context >/dev/null 2>&1 || true
  herdr plugin install ryonakae/herdr-agent-context --ref v0.1.0 --yes >/dev/null
  herdr plugin list --plugin ryonakae.agent-context --json | jq -e '
    .result.plugins | length == 1 and .[0].enabled == true and
    .[0].version == "0.1.0" and .[0].source.kind == "github" and
    .[0].source.requested_ref == "v0.1.0"' >/dev/null
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
  .[0].version == "0.2.0" and .[0].source.kind == "local"' >/dev/null
```

- [x] `herdr plugin list --plugin ryonakae.agent-context --json` reports one enabled local `0.2.0` plugin.
- [x] No second `herdr-agent-context listen` process is running for the same socket.
- [x] Any code or tracked documentation change found during smoke invalidated this evidence directory and restarted validation from a new clean SHA.

## Disposable Herdr session

Create a temporary Herdr configuration that contains the README's shared agent sidebar rows. From a separate terminal, launch or attach the named session:

```sh
HERDR_CONFIG_PATH="$AGENT_CONTEXT_EVIDENCE_DIR/herdr-config.toml" \
  herdr --session agent-context-v020
```

Run the remaining pane commands from a shell inside that session. Parse every pane ID from the JSON response instead of predicting it:

```sh
pi_pane=$(herdr pane split --current --direction right --cwd "$PWD" --no-focus \
  | jq -er '.result.pane.pane_id')
claude_pane=$(herdr pane split --current --direction down --cwd "$PWD" --no-focus \
  | jq -er '.result.pane.pane_id')
printf '%s\n' "$pi_pane" > "$AGENT_CONTEXT_EVIDENCE_DIR/pi-pane-id.txt"
printf '%s\n' "$claude_pane" > "$AGENT_CONTEXT_EVIDENCE_DIR/claude-pane-id.txt"
herdr agent start context_pi --kind pi --pane "$pi_pane"
herdr agent start context_claude --kind claude --pane "$claude_pane" -- --name agent-context-smoke
```

Use synthetic prompts only:

```sh
herdr agent prompt context_pi "Reply with a short synthetic Pi status." --wait --timeout 120000
herdr agent prompt context_claude "Reply with a short synthetic Claude status." --wait --timeout 120000
```

## Hook-free sidebar behavior

### Pi

- [x] An unnamed session uses the first user text, then cwd basename, as the name fallback.
- [x] An explicit Pi session name replaces the fallback within one poll interval.
- [x] The latest assistant text appears without added surrounding quotes.
- [x] A new user entry retains prior activity until the next assistant text arrives.
- [x] `/new` and `/resume` update a single-pane sticky binding without showing another cwd's transcript.
- [x] Two same-cwd Pi panes keep established bindings stable.
- [x] A visible `--no-session` process clears both plugin-owned tokens.

### Claude Code

- [x] Current `customTitle` and legacy `title` records take precedence over the latest matching `ai-title`.
- [x] Without a matching custom or AI title, the session-name token stays empty even when first-user text and cwd exist.
- [x] A matching normalized terminal title is accepted; a missing or mismatched terminal title falls back to the verified JSONL title.
- [x] Live: a newly started Claude session without a custom or AI title leaves the existing tab baseline unchanged.
- [x] The latest top-level assistant text after the latest human entry appears without added surrounding quotes.
- [x] Thinking, tool activity, tool results, sidechains, API errors, and abandoned branches never appear.
- [x] A new human entry retains prior activity; switching to another session does not carry it across.
- [x] `--session-id <uuid>` or UUID `--resume` binds the exact local file without claiming an official source.
- [x] Resume by name and `--continue` use local fallback rather than direct identity.
- [x] `--print`, `--background`, and `--no-session-persistence` leave the plugin rows empty.
- [x] Two same-project Claude panes on a hook-free cold start stay empty without direct evidence.
- [x] Established same-project sticky bindings do not reshuffle after unrelated file activity.
- [x] A bound incomplete tail does not switch to an older candidate or refresh TTL; repair restores the same binding.

### Shared behavior

- [x] Multiline values stay on one row; exactly 80 scalars remain unchanged and longer values truncate to 79 scalars plus an ellipsis.
- [x] Sidebar and tab-component bounds derive independently from one complete title; a grapheme over 80 scalars stays intact when it fits the 20-column component limit.
- [x] Stopping the listener lets metadata expire after TTL; restart performs a full sync.
- [x] Replacing a pane terminal identity clears the prior terminal's owned metadata.
- [x] Socket disconnect/reconnect performs a new full sync with a fresh sequence epoch.
- [x] Invalid plugin config keeps the previous timing and both agents' roots.
- [x] Plugin logs contain no synthetic title, prompt, or assistant text.

## Optional tab-label behavior

Keep the default-off check separate from the opt-in checks. For the opt-in smoke, back up the plugin's `config.toml`, add `[tab_name] enabled = true`, and restore the exact original file before cleanup. Use only the disposable named session and synthetic agents created above.

- [x] With `[tab_name]` omitted, the listener sends no `session.snapshot` or `tab.rename` request and tab-only events do not refresh metadata.
- [x] Generated components use the Pi name or verified Claude title, preserve grapheme clusters, and occupy at most 20 terminal columns; the aggregate retains every component joined with ` + `.
- [x] Background tabs follow visual pane order (top to bottom, then left to right); focus changes cause no rename and do not move the absolute poll deadline.
- [x] Shell, unsupported, and untitled panes contribute nothing without hiding resolved panes in the same tab.
- [x] A manual rename suppresses only the current ordered session composition. Another composition can acquire the tab, and returning restores the exact manual label.
- [x] Pane moves recompute source and destination aggregates while keeping baselines and overrides tab-local. Closing a tab removes its ownership state.
- [x] Setting `enabled = false` restores the latest baseline. An inferred numeric baseline uses the current workspace-local position after reordering.
- [x] State and topology failures stop tab synchronization without blocking sidebar metadata. State files contain manual labels but no plaintext generated title, session identity, or socket path.
- [x] Live: Pi and Claude generated/custom labels, focus switching, shell retention, manual override, config disable, and listener restart match the rules above in an isolated disposable Herdr session.
- [x] Live: after the selected Pi process exits, the tab restores the exact baseline captured before acquisition.
- [x] Live: force-stopping the listener leaves the generated custom label; restarting and then setting `enabled = false` restores the saved numeric baseline.
- [x] The listener, synthetic agents and transcripts, temporary pane, isolated server, state/config directories, and disposable session are removed after the smoke.

## Temporary official integrations

Run these checks only after hook-free behavior passes. Return to the persistent validation shell from the setup section; do not run this block in the disposable pane shell. The validation shell must use the same `PI_CODING_AGENT_DIR` and `CLAUDE_CONFIG_DIR` as the test agents.

```sh
set -eu
evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?}
pi_dir=${PI_CODING_AGENT_DIR:-"$HOME/.pi/agent"}
claude_dir=${CLAUDE_CONFIG_DIR:-"$HOME/.claude"}
pi_hook="$pi_dir/extensions/herdr-agent-state.ts"
claude_settings="$claude_dir/settings.json"
claude_hook="$claude_dir/hooks/herdr-agent-state.sh"
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
  herdr integration status > "$evidence/integration-status-after.txt" || cleanup_status=1
  grep -q '^pi: not installed ' "$evidence/integration-status-after.txt" || cleanup_status=1
  grep -q '^claude: not installed ' "$evidence/integration-status-after.txt" || cleanup_status=1
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
```

- [x] `herdr integration status` reports Pi and Claude as `current`.
- [x] Restart fresh synthetic Pi and Claude sessions so both integrations initialize.
- [x] Pi reports `kind=path`, Claude reports `kind=id`, and both values are nonempty.
- [x] Metadata reports use `applies_to_source=herdr:pi` or `herdr:claude` for the matching pane.
- [x] A newer same-cwd fallback cannot replace an authoritative session.
- [x] Missing or malformed authoritative targets do not fall back and do not refresh TTL.
- [x] Native Pi and Claude resume keep the exact context after restarting the disposable named session.

In a shell inside the disposable named session, export the same `AGENT_CONTEXT_EVIDENCE_DIR` path and save only identity shape, not identity values or metadata tokens:

```sh
set -eu
evidence=${AGENT_CONTEXT_EVIDENCE_DIR:?}
pi_pane_id=$(cat "$evidence/pi-pane-id.txt")
claude_pane_id=$(cat "$evidence/claude-pane-id.txt")
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

Return to the persistent validation shell. Uninstall both integrations, restore the managed plugin, and compare the exact preinstall state. Do not overwrite a mismatch from the backup:

```sh
if ! cleanup_validation; then
  trap - EXIT HUP INT TERM
  echo "integration/plugin restoration failed; evidence: $evidence" >&2
  exit 1
fi
trap - EXIT HUP INT TERM
```

- [x] File presence, link targets, and checksums match exactly after uninstall.
- [x] No restoration mismatch occurred; any mismatch would have stopped release work for manual review.

## Cleanup and promotion evidence

- [x] Temporary validation cleanup completed before promotion; the verified managed v0.2.0 plugin is enabled after promotion.
- [x] No source listener, temporary integration, disposable pane, or named test session remains.
- [x] The repository is clean and `HEAD == origin/main` before exact-SHA CI validation.

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
find "$dist/download" -type f -name 'herdr-agent-context-v0.2.0-*.tar.gz' \
  -exec cp {} "$dist/" \;
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$dist" && sha256sum herdr-agent-context-v0.2.0-*.tar.gz > SHA256SUMS)
else
  (cd "$dist" && shasum -a 256 herdr-agent-context-v0.2.0-*.tar.gz > SHA256SUMS)
fi
sh scripts/verify-release-assets.sh 0.2.0 "$dist"
for target in aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  mkdir "$dist/$target"
  tar -xzf "$dist/herdr-agent-context-v0.2.0-$target.tar.gz" \
    -C "$dist/$target" herdr-agent-context
  sh scripts/verify-glibc-baseline.sh "$dist/$target/herdr-agent-context" 2.18
done
```

- [x] Record the exact SHA and CI run ID under `$AGENT_CONTEXT_EVIDENCE_DIR`.
- [x] All four downloaded CI archives pass checksum, content, executable, and Linux glibc `2.18` checks.
- [x] Independent implementation and distribution review has no unresolved finding.
- [x] Obtain explicit promotion approval before creating or pushing `v0.2.0`.
- [x] After approval, tag CI and the Release workflow both pass for the recorded SHA.
- [x] The public prerelease contains four archives plus `SHA256SUMS`.
- [x] Public assets pass `scripts/verify-release-assets.sh 0.2.0`.
- [x] The public URL installer installs a host binary byte-identical to its archive.
- [x] Replace the managed v0.1.0 plugin with the verified managed v0.2.0 release.

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
