# Changelog

All notable changes to herdr-agent-context are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.4.0

### Added

- Added Codex session context to the Herdr sidebar, automatic tab names, and automatic pane names.
- Added conservative Codex rollout resolution through authoritative session IDs, structured UUID resume hints, sticky bindings, and uniquely changed same-directory rollouts.
- Added Codex session-name and visible assistant-activity parsing while excluding reasoning, tool calls, and non-user-visible records.

### Changed

- Added Codex-specific session roots through `CODEX_HOME` and plugin configuration, with bounded active-rollout discovery and optional session-index names.

## v0.3.0

### Added

- Added opt-in automatic tab names aggregated from resolved Pi and Claude Code sessions in visual pane order.
- Added opt-in automatic pane names based on resolved session names or verified Claude titles.
- Added durable ownership state for generated tab and pane labels, manual overrides, and crash recovery.

### Fixed

- Preserved manual and generated label ownership across terminal replacement, delayed Herdr events, session composition changes, and listener restarts.

## v0.2.0

### Added

- Added Claude Code session context alongside Pi, including verified titles and filtered assistant activity.
- Added authoritative Claude session-ID binding, bounded local JSONL discovery, and agent-specific session roots.

### Changed

- Shared sidebar rows and backend contracts across supported agents and displayed assistant activity without surrounding quotation marks.

### Fixed

- Cleared stale Pi context when panes switch sessions and waited for authoritative Pi binding instead of briefly displaying heuristic fallback data.
- Preserved prior Claude activity until replacement text appears and failed closed for unsupported or malformed session data.

## v0.1.0

### Added

- Added Pi session names and recent assistant activity to the Herdr sidebar.
- Added authoritative Pi path binding with conservative same-directory fallback discovery.
- Added a Herdr plugin listener with configurable polling and metadata TTL.
- Added checksum-verified binary distribution for macOS and Linux.

### Changed

- Bounded displayed metadata to one line and 80 Unicode scalar values.
