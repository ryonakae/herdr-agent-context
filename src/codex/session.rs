use crate::backend::DisplayView;
use crate::text::{complete_line, display_line};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexSessionHeader {
    pub session_identity: String,
    pub metadata_cwd: PathBuf,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexSessionView {
    pub header: CodexSessionHeader,
    pub display: DisplayView,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CodexSessionError {
    #[error("failed to read Codex session")]
    Read,
    #[error("Codex session has an incomplete trailing entry")]
    IncompleteTail,
    #[error("Codex session contains malformed JSONL")]
    MalformedJson,
    #[error("Codex session header is missing or invalid")]
    InvalidHeader,
    #[error("Codex session contains an invalid record")]
    InvalidRecord,
    #[error("Codex session identity does not match its filename")]
    IdentityMismatch,
    #[error("Codex session uses an incompatible history mode")]
    IncompatibleHistory,
}

pub fn parse_session(
    path: &Path,
    index_path: Option<&Path>,
) -> Result<CodexSessionView, CodexSessionError> {
    let input = fs::read_to_string(path).map_err(|_| CodexSessionError::Read)?;
    let identity = filename_identity(path).ok_or(CodexSessionError::IdentityMismatch)?;
    let index = index_path.and_then(|path| fs::read_to_string(path).ok());
    parse_session_text(&input, identity, index.as_deref())
}

pub fn parse_session_text(
    input: &str,
    filename_identity: &str,
    index: Option<&str>,
) -> Result<CodexSessionView, CodexSessionError> {
    let entries = parse_entries(input)?;
    let metadata_index = entries
        .iter()
        .position(|entry| entry.get("type").and_then(Value::as_str) == Some("session_meta"))
        .ok_or(CodexSessionError::InvalidHeader)?;
    let metadata = &entries[metadata_index];
    let payload = metadata
        .get("payload")
        .and_then(Value::as_object)
        .ok_or(CodexSessionError::InvalidHeader)?;
    let identity = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| is_uuid(value))
        .ok_or(CodexSessionError::InvalidHeader)?;
    if !is_uuid(filename_identity) {
        return Err(CodexSessionError::InvalidHeader);
    }
    if identity != filename_identity {
        return Err(CodexSessionError::IdentityMismatch);
    }
    let metadata_cwd = absolute_cwd(payload.get("cwd")).ok_or(CodexSessionError::InvalidHeader)?;
    if payload.get("source").and_then(Value::as_str) != Some("cli") {
        return Err(CodexSessionError::InvalidHeader);
    }

    let mut effective_cwd = metadata_cwd.clone();
    let mut first_user = None;
    let mut saw_user = false;
    let mut last_message = None;
    for entry in entries.iter().skip(metadata_index + 1) {
        match entry.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                let cwd = entry
                    .get("payload")
                    .and_then(Value::as_object)
                    .and_then(|payload| absolute_cwd(payload.get("cwd")))
                    .ok_or(CodexSessionError::InvalidRecord)?;
                effective_cwd = cwd;
            }
            Some("event_msg") => {
                let payload = entry
                    .get("payload")
                    .and_then(Value::as_object)
                    .ok_or(CodexSessionError::InvalidRecord)?;
                match payload.get("type").and_then(Value::as_str) {
                    Some("user_message") => {
                        let message = payload
                            .get("message")
                            .and_then(Value::as_str)
                            .ok_or(CodexSessionError::InvalidRecord)?;
                        if let Some(line) = complete_line(message) {
                            first_user.get_or_insert(line);
                            saw_user = true;
                            last_message = None;
                        }
                    }
                    Some("agent_message") => {
                        let message = payload
                            .get("message")
                            .and_then(Value::as_str)
                            .ok_or(CodexSessionError::InvalidRecord)?;
                        let phase = payload
                            .get("phase")
                            .and_then(Value::as_str)
                            .ok_or(CodexSessionError::InvalidRecord)?;
                        if saw_user && matches!(phase, "commentary" | "final_answer") {
                            if let Some(line) = display_line(message) {
                                last_message = Some(line);
                            }
                        }
                    }
                    Some("history_mode" | "thread_rolled_back") => {
                        return Err(CodexSessionError::IncompatibleHistory);
                    }
                    Some(_) => {}
                    None => return Err(CodexSessionError::InvalidRecord),
                }
            }
            Some("session_meta" | "response_item") => {}
            Some(_) | None => {}
        }
    }

    let header = CodexSessionHeader {
        session_identity: identity.to_owned(),
        metadata_cwd,
        cwd: effective_cwd,
    };
    let cwd_name = header
        .cwd
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(complete_line);
    let tab_name_source = index
        .and_then(|index| index_name(index, identity))
        .or(first_user)
        .or(cwd_name);
    let session_name = tab_name_source.as_deref().and_then(display_line);
    Ok(CodexSessionView {
        display: DisplayView {
            session_identity: identity.to_owned(),
            session_name,
            tab_name_source,
            last_message,
        },
        header,
    })
}

fn parse_entries(input: &str) -> Result<Vec<Value>, CodexSessionError> {
    let mut entries = Vec::new();
    let has_trailing_newline = input.ends_with('\n');
    let line_count = input.lines().count();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value)
                if value.as_object().is_some()
                    && value.get("type").and_then(Value::as_str).is_some() =>
            {
                entries.push(value);
            }
            Ok(_) => return Err(CodexSessionError::InvalidRecord),
            Err(_) if index + 1 == line_count && !has_trailing_newline => {
                return Err(CodexSessionError::IncompleteTail);
            }
            Err(_) => return Err(CodexSessionError::MalformedJson),
        }
    }
    Ok(entries)
}

fn index_name(input: &str, identity: &str) -> Option<String> {
    let has_trailing_newline = input.ends_with('\n');
    let line_count = input.lines().count();
    let mut name = None;
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(line) {
            Ok(entry) => entry,
            Err(_) if index + 1 == line_count && !has_trailing_newline => break,
            Err(_) => return None,
        };
        let object = entry.as_object()?;
        let id = object.get("id").and_then(Value::as_str)?;
        let thread_name = object.get("thread_name").and_then(Value::as_str)?;
        if id == identity {
            if let Some(value) = complete_line(thread_name) {
                name = Some(value);
            }
        }
    }
    name
}

fn absolute_cwd(value: Option<&Value>) -> Option<PathBuf> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub(super) fn filename_identity(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let body = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let timestamp = body.get(..19)?;
    if !valid_timestamp(timestamp) || body.as_bytes().get(19) != Some(&b'-') {
        return None;
    }
    let identity_and_suffix = body.get(20..)?;
    let identity = identity_and_suffix.get(..36)?;
    if !is_uuid(identity) {
        return None;
    }
    match identity_and_suffix.get(36..)? {
        "" => Some(identity),
        suffix
            if suffix.len() == 37
                && suffix.starts_with('_')
                && suffix.get(1..).is_some_and(is_uuid) =>
        {
            Some(identity)
        }
        _ => None,
    }
}

fn valid_timestamp(value: &str) -> bool {
    if value.len() != 19
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || value.as_bytes().get(10) != Some(&b'T')
        || value.as_bytes().get(13) != Some(&b'-')
        || value.as_bytes().get(16) != Some(&b'-')
        || !value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
    {
        return false;
    }
    let Some(year) = value.get(..4).and_then(|part| part.parse::<u16>().ok()) else {
        return false;
    };
    let Some(month) = value.get(5..7).and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let Some(day) = value.get(8..10).and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let Some(hour) = value.get(11..13).and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let Some(minute) = value.get(14..16).and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let Some(second) = value.get(17..19).and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day) && hour < 24 && minute < 60 && second < 60
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD_ID: &str = "10000000-0000-4000-8000-000000000001";

    #[test]
    fn parses_canonical_root_identity_and_metadata_cwd() {
        let input = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n"
        );

        let view = parse_session_text(input, CHILD_ID, None).unwrap();

        assert_eq!(view.header.session_identity, CHILD_ID);
        assert_eq!(
            view.header.metadata_cwd,
            std::path::PathBuf::from("/synthetic/project")
        );
        assert_eq!(
            view.header.cwd,
            std::path::PathBuf::from("/synthetic/project")
        );
    }

    #[test]
    fn latest_completed_nonblank_exact_index_name_wins() {
        let rollout = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Fallback task\"}}\n"
        );
        let index = concat!(
            "{\"id\":\"10000000-0000-4000-8000-000000000001\",\"thread_name\":\"Earlier\"}\n",
            "{\"id\":\"20000000-0000-4000-8000-000000000002\",\"thread_name\":\"Other\"}\n",
            "{\"id\":\"10000000-0000-4000-8000-000000000001\",\"thread_name\":\"Latest name\"}\n",
            "{\"id\":\"10000000-0000-4000-8000-000000000001\",\"thread_name\":\"   \"}\n",
            "{\"id\":\"10000000-0000-4000-8000-000000000001\",\"thread_name\":\"unfinished"
        );

        let view = parse_session_text(rollout, CHILD_ID, Some(index)).unwrap();

        assert_eq!(view.display.session_name.as_deref(), Some("Latest name"));
        assert_eq!(view.display.tab_name_source.as_deref(), Some("Latest name"));
    }

    #[test]
    fn accepts_a_complete_final_index_entry_without_a_trailing_newline() {
        let rollout = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Fallback task\"}}\n"
        );
        let index = concat!(
            "{\"id\":\"10000000-0000-4000-8000-000000000001\",\"thread_name\":\"Earlier\"}\n",
            "{\"id\":\"10000000-0000-4000-8000-000000000001\",\"thread_name\":\"Complete final name\"}"
        );

        let view = parse_session_text(rollout, CHILD_ID, Some(index)).unwrap();

        assert_eq!(
            view.display.session_name.as_deref(),
            Some("Complete final name")
        );
        assert_eq!(
            view.display.tab_name_source.as_deref(),
            Some("Complete final name")
        );
    }

    #[test]
    fn malformed_or_missing_index_uses_user_then_effective_cwd_fallback() {
        let with_user = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"developer_message\",\"message\":\"Private\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"  First task  \\nignored\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Later task\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Visible activity\",\"phase\":\"commentary\"}}\n"
        );
        let malformed_index = "{\"id\":42}\n";

        let user_view = parse_session_text(with_user, CHILD_ID, Some(malformed_index)).unwrap();
        assert_eq!(
            user_view.display.session_name.as_deref(),
            Some("First task")
        );
        assert_eq!(
            user_view.display.tab_name_source.as_deref(),
            Some("First task")
        );
        assert_eq!(
            user_view.display.last_message.as_deref(),
            Some("Visible activity")
        );

        let cwd_only = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/synthetic/latest-work\"}}\n"
        );
        let cwd_view = parse_session_text(cwd_only, CHILD_ID, None).unwrap();
        assert_eq!(
            cwd_view.display.session_name.as_deref(),
            Some("latest-work")
        );
        assert_eq!(
            cwd_view.display.tab_name_source.as_deref(),
            Some("latest-work")
        );
    }

    #[test]
    fn completed_malformed_index_invalidates_prior_name_but_keeps_rollout_activity() {
        let rollout = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Fallback task\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Visible activity\",\"phase\":\"final_answer\"}}\n"
        );
        let index = concat!(
            "{\"id\":\"10000000-0000-4000-8000-000000000001\",\"thread_name\":\"Indexed name\"}\n",
            "{\"id\":42}\n"
        );

        let view = parse_session_text(rollout, CHILD_ID, Some(index)).unwrap();

        assert_eq!(view.display.session_name.as_deref(), Some("Fallback task"));
        assert_eq!(
            view.display.tab_name_source.as_deref(),
            Some("Fallback task")
        );
        assert_eq!(
            view.display.last_message.as_deref(),
            Some("Visible activity")
        );
    }

    #[test]
    fn selects_latest_commentary_or_final_after_latest_genuine_user() {
        let answered = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Task\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_reasoning\",\"text\":\"Private\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Duplicate\"}]}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Commentary\",\"phase\":\"commentary\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"last_agent_message\":\"Echo\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"  Final answer  \\nignored\",\"phase\":\"final_answer\"}}\n"
        );
        let view = parse_session_text(answered, CHILD_ID, None).unwrap();
        assert_eq!(view.display.last_message.as_deref(), Some("Final answer"));

        let pending = format!(
            "{answered}{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"Next task\"}}}}\n"
        );
        let view = parse_session_text(&pending, CHILD_ID, None).unwrap();
        assert_eq!(view.display.last_message, None);
    }

    #[test]
    fn distinguishes_retryable_tail_from_fail_closed_rollout_errors() {
        let valid = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Task\"}}\n"
        );
        assert_eq!(
            parse_session_text(&format!("{valid}{{"), CHILD_ID, None),
            Err(CodexSessionError::IncompleteTail)
        );
        assert_eq!(
            parse_session_text(&format!("{valid}{{\n"), CHILD_ID, None),
            Err(CodexSessionError::MalformedJson)
        );
        assert_eq!(
            parse_session_text(
                &format!("{valid}{{\"type\":\"turn_context\",\"payload\":{{}}}}\n"),
                CHILD_ID,
                None
            ),
            Err(CodexSessionError::InvalidRecord)
        );
        assert_eq!(
            parse_session_text(
                &format!(
                    "{valid}{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"history_mode\"}}}}\n"
                ),
                CHILD_ID,
                None
            ),
            Err(CodexSessionError::IncompatibleHistory)
        );

        for invalid_header in [
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"not-a-uuid\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"relative\",\"source\":\"cli\"}}\n",
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"subagent\"}}\n",
        ] {
            assert_eq!(
                parse_session_text(invalid_header, CHILD_ID, None),
                Err(CodexSessionError::InvalidHeader)
            );
        }
        assert_eq!(
            parse_session_text("", CHILD_ID, None),
            Err(CodexSessionError::InvalidHeader)
        );
        assert_eq!(
            parse_session_text(valid, "10000000-0000-4000-8000-000000000099", None),
            Err(CodexSessionError::IdentityMismatch)
        );
    }

    #[test]
    fn completed_rollout_records_require_an_object_with_a_string_outer_type() {
        let metadata = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n"
        );
        for invalid in [
            "{}",
            "null",
            "[]",
            "42",
            "\"record\"",
            "{\"payload\":{}}",
            "{\"type\":null}",
            "{\"type\":42}",
            "{\"type\":{}}",
        ] {
            assert_eq!(
                parse_session_text(&format!("{invalid}\n{metadata}"), CHILD_ID, None),
                Err(CodexSessionError::InvalidRecord),
                "invalid pre-metadata record: {invalid}"
            );
            assert_eq!(
                parse_session_text(&format!("{metadata}{invalid}\n"), CHILD_ID, None),
                Err(CodexSessionError::InvalidRecord),
                "invalid post-metadata record: {invalid}"
            );
        }

        let safe_unknown = format!("{metadata}{{\"type\":\"future_unrelated\"}}\n");
        assert!(parse_session_text(&safe_unknown, CHILD_ID, None).is_ok());
    }

    #[test]
    fn ignores_conversation_and_turn_records_before_canonical_metadata() {
        let input = concat!(
            "{\"type\":\"future_unrelated_record\",\"payload\":{\"value\":1}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/synthetic/pre-metadata\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Pre-metadata task\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Pre-metadata answer\",\"phase\":\"final_answer\"}}\n",
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n"
        );

        let view = parse_session_text(input, CHILD_ID, None).unwrap();

        assert_eq!(view.header.cwd, PathBuf::from("/synthetic/project"));
        assert_eq!(view.display.session_name.as_deref(), Some("project"));
        assert_eq!(view.display.last_message, None);
    }

    #[test]
    fn filename_identity_accepts_only_the_official_rollout_shape() {
        let rollout_id = "20000000-0000-4000-8000-000000000002";
        for valid in [
            format!("rollout-2026-08-28T00-00-00-{CHILD_ID}.jsonl"),
            format!("rollout-2024-02-29T23-59-59-{CHILD_ID}_{rollout_id}.jsonl"),
        ] {
            assert_eq!(filename_identity(Path::new(&valid)), Some(CHILD_ID));
        }

        for invalid in [
            format!("rollout-copy-{CHILD_ID}.jsonl"),
            format!("prefix-rollout-2026-08-28T00-00-00-{CHILD_ID}.jsonl"),
            format!("rollout-2026-08-28T00-00-00-copy-{CHILD_ID}.jsonl"),
            format!("rollout-2026-02-29T00-00-00-{CHILD_ID}.jsonl"),
            format!("rollout-2026-13-01T00-00-00-{CHILD_ID}.jsonl"),
            format!("rollout-2026-08-28T24-00-00-{CHILD_ID}.jsonl"),
            format!("rollout-2026-08-28T00:00:00-{CHILD_ID}.jsonl"),
            format!("rollout-2026-08-28T00-00-00-{CHILD_ID}_not-a-uuid.jsonl"),
            format!("rollout-2026-08-28T00-00-00-{CHILD_ID}_{rollout_id}_extra.jsonl"),
            format!("rollout-2026-08-28T00-00-00-{CHILD_ID}.jsonl.gz"),
        ] {
            assert_eq!(
                filename_identity(Path::new(&invalid)),
                None,
                "invalid filename: {invalid}"
            );
        }
    }

    #[test]
    fn file_parser_requires_rollout_filename_and_treats_unreadable_index_as_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let rollout = directory
            .path()
            .join(format!("rollout-2026-08-28T00-00-00-{CHILD_ID}.jsonl"));
        std::fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Fallback\"}}\n"
            ),
        )
        .unwrap();
        let missing_index = directory.path().join("missing-index.jsonl");

        let view = parse_session(&rollout, Some(&missing_index)).unwrap();
        assert_eq!(view.display.session_name.as_deref(), Some("Fallback"));

        let wrong_shape = directory.path().join(format!("copy-{CHILD_ID}.jsonl"));
        std::fs::copy(&rollout, &wrong_shape).unwrap();
        assert_eq!(
            parse_session(&wrong_shape, None),
            Err(CodexSessionError::IdentityMismatch)
        );
    }

    #[test]
    fn filters_nonconversation_records_and_ignores_unrelated_unknown_records() {
        let input = concat!(
            "{\"type\":\"future_unrelated_record\",\"payload\":{\"text\":\"Private\"}}\n",
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"developer_message\",\"message\":\"Developer\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"system_message\",\"message\":\"System\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"tool_result\",\"message\":\"Tool\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_reasoning\",\"message\":\"Reasoning\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Before user\",\"phase\":\"final_answer\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"   \"}}\n"
        );

        let view = parse_session_text(input, CHILD_ID, None).unwrap();

        assert_eq!(view.display.session_name.as_deref(), Some("project"));
        assert_eq!(view.display.last_message, None);
    }

    #[test]
    fn applies_multiline_and_unicode_scalar_bounds_to_display_values() {
        let name = "界".repeat(81);
        let message = "語".repeat(81);
        let input = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{CHILD_ID}\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}}}\n\
             {{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"Task\"}}}}\n\
             {{\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"  {message}  \\nignored\",\"phase\":\"final_answer\"}}}}\n"
        );
        let index = format!("{{\"id\":\"{CHILD_ID}\",\"thread_name\":\"{name}\"}}\n");

        let view = parse_session_text(&input, CHILD_ID, Some(&index)).unwrap();

        assert_eq!(
            view.display.session_name.as_ref().unwrap().chars().count(),
            80
        );
        assert!(view.display.session_name.as_ref().unwrap().ends_with('…'));
        assert_eq!(view.display.tab_name_source.as_deref(), Some(name.as_str()));
        assert_eq!(
            view.display.last_message.as_ref().unwrap().chars().count(),
            80
        );
        assert!(view.display.last_message.as_ref().unwrap().ends_with('…'));
    }

    #[test]
    fn keeps_child_identity_and_uses_latest_turn_context_cwd() {
        let input = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}\n",
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"20000000-0000-4000-8000-000000000002\",\"cwd\":\"/synthetic/parent\",\"source\":\"cli\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/synthetic/first\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/synthetic/latest\"}}\n"
        );

        let view = parse_session_text(input, CHILD_ID, None).unwrap();

        assert_eq!(view.header.session_identity, CHILD_ID);
        assert_eq!(
            view.header.metadata_cwd,
            PathBuf::from("/synthetic/project")
        );
        assert_eq!(view.header.cwd, PathBuf::from("/synthetic/latest"));
    }
}
