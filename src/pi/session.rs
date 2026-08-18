use crate::text::{display_line, quoted_display_line};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PiSessionView {
    pub path: PathBuf,
    pub session_id: String,
    pub cwd: PathBuf,
    pub explicit_name: Option<String>,
    pub first_user_line: Option<String>,
    pub latest_turn_assistant_line: Option<String>,
    pub modified_at: SystemTime,
}

impl PiSessionView {
    pub fn session_name(&self) -> Option<String> {
        self.explicit_name
            .clone()
            .or_else(|| self.first_user_line.clone())
            .or_else(|| {
                self.cwd
                    .file_name()
                    .and_then(|value| value.to_str())
                    .and_then(display_line)
            })
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("failed to read Pi session")]
    Read(#[source] std::io::Error),
    #[error("Pi session has an incomplete trailing entry")]
    IncompleteTail,
    #[error("Pi session contains malformed JSONL")]
    MalformedJson,
    #[error("Pi session header is missing or invalid")]
    InvalidHeader,
    #[error("Pi session tree is invalid")]
    InvalidTree,
}

#[derive(Debug)]
struct Node {
    parent_id: Option<String>,
    kind: NodeKind,
}

#[derive(Debug)]
enum NodeKind {
    User(Option<String>),
    Assistant(Option<String>),
    SessionInfo(Option<String>),
    Other,
}

pub fn parse_session(path: &Path) -> Result<PiSessionView, SessionError> {
    let input = fs::read_to_string(path).map_err(SessionError::Read)?;
    let metadata = fs::metadata(path).map_err(SessionError::Read)?;
    parse_session_text(
        path,
        &input,
        metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    )
}

pub fn parse_session_header(path: &Path) -> Result<(String, PathBuf), SessionError> {
    let file = fs::File::open(path).map_err(SessionError::Read)?;
    let mut lines = BufReader::new(file).lines();
    let first = lines
        .next()
        .ok_or(SessionError::InvalidHeader)?
        .map_err(SessionError::Read)?;
    parse_header(&serde_json::from_str(&first).map_err(|_| SessionError::MalformedJson)?)
}

pub fn parse_session_text(
    path: &Path,
    input: &str,
    modified_at: SystemTime,
) -> Result<PiSessionView, SessionError> {
    let mut entries = Vec::new();
    let has_trailing_newline = input.ends_with('\n');
    let line_count = input.lines().count();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => entries.push(value),
            Err(_) if index + 1 == line_count && !has_trailing_newline => {
                return Err(SessionError::IncompleteTail);
            }
            Err(_) => return Err(SessionError::MalformedJson),
        }
    }

    let header = entries.first().ok_or(SessionError::InvalidHeader)?;
    let (session_id, cwd) = parse_header(header)?;
    let mut nodes = HashMap::new();
    let mut order = Vec::new();
    let mut explicit_name = None;

    for entry in entries.iter().skip(1) {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let parent_id = entry
            .get("parentId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let kind = parse_node_kind(entry);
        if let NodeKind::SessionInfo(Some(name)) = &kind {
            explicit_name = display_line(name);
        }
        nodes.insert(id.to_owned(), Node { parent_id, kind });
        order.push(id.to_owned());
    }

    let mut chain_ids = Vec::new();
    if let Some(leaf_id) = order.last() {
        let mut visited = HashSet::new();
        let mut cursor = Some(leaf_id.as_str());
        while let Some(id) = cursor {
            if !visited.insert(id.to_owned()) {
                return Err(SessionError::InvalidTree);
            }
            let node = nodes.get(id).ok_or(SessionError::InvalidTree)?;
            chain_ids.push(id.to_owned());
            cursor = node.parent_id.as_deref();
        }
        chain_ids.reverse();
    }

    let mut first_user_line = None;
    let mut latest_user_index = None;
    for (index, id) in chain_ids.iter().enumerate() {
        if let NodeKind::User(line) = &nodes[id].kind {
            if first_user_line.is_none() {
                first_user_line = line.clone();
            }
            latest_user_index = Some(index);
        }
    }

    let mut latest_turn_assistant_line = None;
    if let Some(user_index) = latest_user_index {
        for id in chain_ids.iter().skip(user_index + 1) {
            if let NodeKind::Assistant(Some(line)) = &nodes[id].kind {
                latest_turn_assistant_line = Some(line.clone());
            }
        }
    }

    Ok(PiSessionView {
        path: path.to_owned(),
        session_id,
        cwd,
        explicit_name,
        first_user_line,
        latest_turn_assistant_line,
        modified_at,
    })
}

fn parse_header(value: &Value) -> Result<(String, PathBuf), SessionError> {
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return Err(SessionError::InvalidHeader);
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(SessionError::InvalidHeader)?;
    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(SessionError::InvalidHeader)?;
    Ok((id.to_owned(), PathBuf::from(cwd)))
}

fn parse_node_kind(entry: &Value) -> NodeKind {
    match entry.get("type").and_then(Value::as_str) {
        Some("session_info") => NodeKind::SessionInfo(
            entry
                .get("name")
                .and_then(Value::as_str)
                .and_then(display_line),
        ),
        Some("message") => {
            let Some(message) = entry.get("message") else {
                return NodeKind::Other;
            };
            let role = message.get("role").and_then(Value::as_str);
            let content = message.get("content");
            match role {
                Some("user") => NodeKind::User(content.and_then(user_message_text)),
                Some("assistant") => NodeKind::Assistant(content.and_then(assistant_message_text)),
                _ => NodeKind::Other,
            }
        }
        _ => NodeKind::Other,
    }
}

fn user_message_text(content: &Value) -> Option<String> {
    content
        .as_str()
        .and_then(display_line)
        .or_else(|| block_text(content, display_line))
}

fn assistant_message_text(content: &Value) -> Option<String> {
    block_text(content, quoted_display_line)
}

fn block_text(content: &Value, format: fn(&str) -> Option<String>) -> Option<String> {
    let blocks = content.as_array()?;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(line) = block.get("text").and_then(Value::as_str).and_then(format) {
                return Some(line);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn parse(input: &str) -> PiSessionView {
        parse_session_text(
            Path::new("/tmp/session.jsonl"),
            input,
            SystemTime::UNIX_EPOCH + Duration::from_secs(42),
        )
        .unwrap()
    }

    #[test]
    fn explicit_name_wins_over_active_branch_user_and_cwd() {
        let view = parse(concat!(
            "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"fallback task\"}}\n",
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"u1\",\"name\":\"Named session\"}\n"
        ));
        assert_eq!(view.session_name(), Some("Named session".into()));
        assert_eq!(view.first_user_line, Some("fallback task".into()));
    }

    #[test]
    fn follows_latest_persisted_leaf_and_ignores_abandoned_branch() {
        let view = parse(concat!(
            "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"active task\"}}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"secret\"},{\"type\":\"text\",\"text\":\"active answer\\nextra\"}]}}\n",
            "{\"type\":\"message\",\"id\":\"x1\",\"parentId\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":\"abandoned answer\"}}\n",
            "{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"a1\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"continue\"}]}}\n",
            "{\"type\":\"message\",\"id\":\"a2\",\"parentId\":\"u2\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"name\":\"bash\"},{\"type\":\"text\",\"text\":\"latest answer\"}]}}\n"
        ));
        assert_eq!(view.first_user_line, Some("active task".into()));
        assert_eq!(
            view.latest_turn_assistant_line,
            Some("\"latest answer\"".into())
        );
    }

    #[test]
    fn latest_user_without_assistant_has_no_replacement_activity() {
        let view = parse(concat!(
            "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"one\"}}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":\"old answer\"}}\n",
            "{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"a1\",\"message\":{\"role\":\"user\",\"content\":\"two\"}}\n"
        ));
        assert_eq!(view.latest_turn_assistant_line, None);
    }

    #[test]
    fn skips_empty_user_content_when_selecting_name_fallback() {
        let view = parse(concat!(
            "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"image\"}]}}\n",
            "{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"u1\",\"message\":{\"role\":\"user\",\"content\":\"first textual task\"}}\n"
        ));
        assert_eq!(view.session_name(), Some("first textual task".into()));
    }

    #[test]
    fn header_only_session_uses_cwd_basename() {
        let view = parse("{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}\n");
        assert_eq!(view.session_name(), Some("project".into()));
        assert_eq!(view.latest_turn_assistant_line, None);
    }

    #[test]
    fn assistant_string_content_is_not_activity() {
        let view = parse(concat!(
            "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"task\"}}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":\"not a text block\"}}\n"
        ));
        assert_eq!(view.latest_turn_assistant_line, None);
    }

    #[test]
    fn cwd_basename_is_last_name_fallback() {
        let view = parse(concat!(
            "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":null,\"message\":{\"role\":\"assistant\",\"content\":\"answer\"}}\n"
        ));
        assert_eq!(view.session_name(), Some("project".into()));
    }

    #[test]
    fn distinguishes_incomplete_tail_from_completed_malformed_entry() {
        let incomplete = "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work\"}\n{";
        assert!(matches!(
            parse_session_text(Path::new("x"), incomplete, SystemTime::UNIX_EPOCH),
            Err(SessionError::IncompleteTail)
        ));
        let malformed = format!("{incomplete}\n");
        assert!(matches!(
            parse_session_text(Path::new("x"), &malformed, SystemTime::UNIX_EPOCH),
            Err(SessionError::MalformedJson)
        ));
    }

    #[test]
    fn skips_non_message_activity_entries() {
        let view = parse(concat!(
            "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"task\"}}\n",
            "{\"type\":\"tool_result\",\"id\":\"t1\",\"parentId\":\"u1\",\"text\":\"private tool text\"}\n",
            "{\"type\":\"custom_message\",\"id\":\"c1\",\"parentId\":\"t1\",\"text\":\"custom\"}\n",
            "{\"type\":\"compaction\",\"id\":\"c2\",\"parentId\":\"c1\",\"summary\":\"summary\"}\n",
            "{\"type\":\"branch_summary\",\"id\":\"b1\",\"parentId\":\"c2\",\"summary\":\"branch\"}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"b1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"visible\"}]}}\n"
        ));
        assert_eq!(view.latest_turn_assistant_line, Some("\"visible\"".into()));
    }
}
