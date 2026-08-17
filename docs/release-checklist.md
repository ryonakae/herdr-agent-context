# Release checklist

Run this checklist from a clean checkout before promoting a `v0.1.x` prerelease. Use a disposable named Herdr session and synthetic Pi prompts; do not modify Pi integrations or publish a release during the smoke test.

## Automated gates

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --all-targets --locked`
- [ ] `cargo build --release --locked`
- [ ] `sh tests/installer.sh`
- [ ] `sh tests/release-assets.sh`
- [ ] The non-publishing CI matrix built all four supported targets.
- [ ] `scripts/verify-release-assets.sh` accepted the downloaded matrix artifacts and generated checksums.
- [ ] The Linux artifacts passed `scripts/verify-glibc-baseline.sh` with baseline `2.17`.

## Local plugin setup

1. Record checksums for the Pi settings directory and Herdr config file so the no-auto-edit claim can be checked later.
2. Build and stage the listener:

   ```sh
   cargo build --release --locked
   mkdir -p bin
   cp target/release/herdr-agent-context bin/herdr-agent-context
   herdr plugin link .
   ```

3. Add the README's Pi rows to the disposable Herdr configuration.
4. Start or restart the disposable Herdr session and confirm the plugin log has no startup error.

## Sidebar behavior

- [ ] Start an unnamed Pi session. The first user text appears as the session name fallback.
- [ ] Start a Pi session with no user text fixture. The cwd basename appears as the final fallback.
- [ ] Set a Pi session name. The explicit name replaces the fallback within one poll interval.
- [ ] Complete an assistant reply. Its first non-empty text line appears as recent activity.
- [ ] Enter a new user message without waiting for a reply. The prior assistant activity remains visible.
- [ ] Complete the reply. The new assistant activity replaces the retained value.
- [ ] Confirm multiline and over-80-character text stays on one row and truncates safely.
- [ ] Use `/new` in a cwd with one Pi pane, then send a prompt. The binding moves to the newly active session.
- [ ] Use `/resume` for an older session, then send a prompt. The binding follows the changed session file.
- [ ] Open two Pi panes in the same cwd. Existing sticky bindings do not reshuffle when another session file changes; note that exact disambiguation requires an authoritative Herdr session path.
- [ ] Run Pi with a visible `--no-session` argument. Both metadata tokens remain empty or clear.

## Failure and recovery

- [ ] Stop a manually launched listener. Existing metadata disappears after the configured TTL.
- [ ] Restart the listener. A full sync restores current metadata without waiting for a pane event.
- [ ] Temporarily make a bound JSONL tail incomplete. The prior value is not refreshed and expires; repairing the file restores reporting.
- [ ] Restart the listener while Pi remains running. New sequence values are accepted immediately rather than waiting for old metadata to expire.
- [ ] Close or replace a Pi pane. Plugin-owned tokens clear or disappear with the pane.
- [ ] Disconnect and restore an isolated Herdr socket. The listener reconnects and performs a new full sync.
- [ ] Put an unknown key in plugin `config.toml`. The previous valid timing/root settings stay active and the log contains no conversation text.

## Privacy and cleanup

- [ ] Search plugin logs for synthetic session-name/message strings; none are present.
- [ ] Confirm no runtime HTTP/DNS request was made by the listener.
- [ ] Confirm Pi settings/integration checksums are unchanged.
- [ ] Confirm Herdr config differs only by the manual sidebar rows added for this test.
- [ ] Run `herdr plugin unlink ryonakae.agent-context`.
- [ ] Remove the disposable sidebar rows and session data.
- [ ] Confirm no plugin listener remains and metadata expires within the TTL.
