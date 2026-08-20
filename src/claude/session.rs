use crate::backend::DisplayView;
use crate::text::{complete_line, display_line};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeSessionHeader {
    pub session_identity: String,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeSessionView {
    pub header: ClaudeSessionHeader,
    pub display: DisplayView,
}

#[derive(Debug, Error)]
pub enum ClaudeSessionError {
    #[error("failed to read Claude session")]
    Read(#[source] std::io::Error),
    #[error("Claude session has an incomplete trailing entry")]
    IncompleteTail,
    #[error("Claude session contains malformed JSONL")]
    MalformedJson,
    #[error("Claude session header is missing or invalid")]
    InvalidHeader,
    #[error("Claude session contains an invalid record")]
    InvalidRecord,
    #[error("Claude session identity does not match")]
    IdentityMismatch,
    #[error("Claude session tree is invalid")]
    InvalidTree,
}

#[derive(Debug)]
struct Node {
    parent_uuid: Option<String>,
    session_identity: Option<String>,
    kind: NodeKind,
}

#[derive(Debug)]
enum NodeKind {
    User(Option<String>),
    Assistant(Option<String>),
    Transparent,
}

#[derive(Debug)]
struct ParsedMessage {
    uuid: String,
    parent_uuid: Option<String>,
    session_identity: String,
    cwd: PathBuf,
    sidechain: bool,
    kind: NodeKind,
}

pub fn parse_session(path: &Path) -> Result<ClaudeSessionView, ClaudeSessionError> {
    let input = fs::read_to_string(path).map_err(ClaudeSessionError::Read)?;
    parse_session_text(&input)
}

pub fn parse_session_header(path: &Path) -> Result<ClaudeSessionHeader, ClaudeSessionError> {
    let input = fs::read_to_string(path).map_err(ClaudeSessionError::Read)?;
    parse_session_header_text(&input)
}

pub fn parse_session_header_text(input: &str) -> Result<ClaudeSessionHeader, ClaudeSessionError> {
    let has_trailing_newline = input.ends_with('\n');
    let line_count = input.lines().count();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = match serde_json::from_str(line) {
            Ok(entry) => entry,
            Err(_) if index + 1 == line_count && !has_trailing_newline => {
                return Err(ClaudeSessionError::IncompleteTail);
            }
            Err(_) => return Err(ClaudeSessionError::MalformedJson),
        };
        if let Some(message) = parse_message(&entry)? {
            if !message.sidechain {
                return Ok(ClaudeSessionHeader {
                    session_identity: message.session_identity,
                    cwd: message.cwd,
                });
            }
        }
    }
    Err(ClaudeSessionError::InvalidHeader)
}

pub fn parse_session_text(input: &str) -> Result<ClaudeSessionView, ClaudeSessionError> {
    let entries = parse_entries(input)?;
    let mut header = None;
    let mut nodes = HashMap::new();
    let mut top_level_messages = Vec::new();
    let mut active_leaf = None;
    let mut custom_title = None;
    let mut ai_title = None;

    for entry in &entries {
        if let Some(message) = parse_message(entry)? {
            if header.is_none() && !message.sidechain {
                header = Some(ClaudeSessionHeader {
                    session_identity: message.session_identity.clone(),
                    cwd: message.cwd.clone(),
                });
            }
            if !message.sidechain {
                active_leaf = Some(message.uuid.clone());
                top_level_messages.push(message.uuid.clone());
            }
            if nodes
                .insert(
                    message.uuid,
                    Node {
                        parent_uuid: message.parent_uuid,
                        session_identity: Some(message.session_identity),
                        kind: message.kind,
                    },
                )
                .is_some()
            {
                return Err(ClaudeSessionError::InvalidTree);
            }
            continue;
        }

        if let Some((uuid, parent_uuid)) = parse_transparent_node(entry) {
            if nodes
                .insert(
                    uuid,
                    Node {
                        parent_uuid,
                        session_identity: entry
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        kind: NodeKind::Transparent,
                    },
                )
                .is_some()
            {
                return Err(ClaudeSessionError::InvalidTree);
            }
        }
    }

    validate_message_chains(&top_level_messages, &nodes)?;
    let header = header.ok_or(ClaudeSessionError::InvalidHeader)?;
    for entry in &entries {
        let title_session = entry.get("sessionId").and_then(Value::as_str);
        if title_session != Some(header.session_identity.as_str()) {
            continue;
        }
        match entry.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                if let Some(title) = entry
                    .get("customTitle")
                    .and_then(Value::as_str)
                    .and_then(complete_line)
                    .or_else(|| {
                        entry
                            .get("title")
                            .and_then(Value::as_str)
                            .and_then(complete_line)
                    })
                {
                    custom_title = Some(title);
                }
            }
            Some("ai-title") => {
                if let Some(title) = entry
                    .get("aiTitle")
                    .and_then(Value::as_str)
                    .and_then(complete_line)
                {
                    ai_title = Some(title);
                }
            }
            _ => {}
        }
    }

    let chain = active_chain(
        active_leaf
            .as_deref()
            .ok_or(ClaudeSessionError::InvalidHeader)?,
        &nodes,
        &header.session_identity,
    )?;
    let mut latest_user_index = None;
    for (index, node) in chain.iter().enumerate() {
        if matches!(&node.kind, NodeKind::User(Some(_))) {
            latest_user_index = Some(index);
        }
    }
    let mut last_message = None;
    if let Some(user_index) = latest_user_index {
        for node in chain.iter().skip(user_index + 1) {
            if let NodeKind::Assistant(Some(line)) = &node.kind {
                last_message = Some(line.clone());
            }
        }
    }
    let tab_name_source = custom_title.or(ai_title);
    let session_name = tab_name_source.as_deref().and_then(display_line);
    Ok(ClaudeSessionView {
        display: DisplayView {
            session_identity: header.session_identity.clone(),
            session_name,
            tab_name_source,
            last_message,
        },
        header,
    })
}

fn parse_entries(input: &str) -> Result<Vec<Value>, ClaudeSessionError> {
    let mut entries = Vec::new();
    let has_trailing_newline = input.ends_with('\n');
    let line_count = input.lines().count();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(value) => entries.push(value),
            Err(_) if index + 1 == line_count && !has_trailing_newline => {
                return Err(ClaudeSessionError::IncompleteTail);
            }
            Err(_) => return Err(ClaudeSessionError::MalformedJson),
        }
    }
    Ok(entries)
}

fn parse_message(entry: &Value) -> Result<Option<ParsedMessage>, ClaudeSessionError> {
    let Some(record_kind @ ("user" | "assistant")) = entry.get("type").and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let sidechain = required_bool(entry, "isSidechain")?;
    for key in [
        "isMeta",
        "isCompactSummary",
        "isVisibleInTranscriptOnly",
        "isApiErrorMessage",
    ] {
        validate_optional_bool(entry, key)?;
    }
    let message = entry
        .get("message")
        .and_then(Value::as_object)
        .ok_or(ClaudeSessionError::InvalidRecord)?;
    if message.get("role").and_then(Value::as_str) != Some(record_kind) {
        return Err(ClaudeSessionError::InvalidRecord);
    }
    let uuid = required_uuid(entry, "uuid")?;
    let parent_uuid = parse_parent_uuid(entry, &uuid)?;
    let session_identity = required_uuid(entry, "sessionId")?;
    let cwd = entry
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(ClaudeSessionError::InvalidRecord)?;
    let kind = if sidechain {
        NodeKind::Transparent
    } else {
        match record_kind {
            "user" => NodeKind::User(genuine_user_text(entry, message)),
            "assistant" => NodeKind::Assistant(assistant_text(entry, message)),
            _ => unreachable!(),
        }
    };
    Ok(Some(ParsedMessage {
        uuid,
        parent_uuid,
        session_identity,
        cwd,
        sidechain,
        kind,
    }))
}

fn parse_transparent_node(entry: &Value) -> Option<(String, Option<String>)> {
    let uuid = entry
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|value| is_uuid(value))?;
    let parent = entry.get("parentUuid")?;
    let parent_uuid = if parent.is_null() {
        None
    } else {
        Some(
            parent
                .as_str()
                .filter(|value| is_uuid(value) && *value != uuid)?
                .to_owned(),
        )
    };
    Some((uuid.to_owned(), parent_uuid))
}

fn validate_message_chains(
    message_ids: &[String],
    nodes: &HashMap<String, Node>,
) -> Result<(), ClaudeSessionError> {
    let mut validated = HashSet::new();
    for start in message_ids {
        let mut visiting = HashSet::new();
        let mut path = Vec::new();
        let mut cursor = Some(start.as_str());
        while let Some(uuid) = cursor {
            if validated.contains(uuid) {
                break;
            }
            if !visiting.insert(uuid.to_owned()) {
                return Err(ClaudeSessionError::InvalidTree);
            }
            let node = nodes.get(uuid).ok_or(ClaudeSessionError::InvalidTree)?;
            path.push(uuid.to_owned());
            cursor = node.parent_uuid.as_deref();
        }
        validated.extend(path);
    }
    Ok(())
}

fn active_chain<'a>(
    leaf: &str,
    nodes: &'a HashMap<String, Node>,
    session_identity: &str,
) -> Result<Vec<&'a Node>, ClaudeSessionError> {
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut cursor = Some(leaf);
    while let Some(uuid) = cursor {
        if !visited.insert(uuid.to_owned()) {
            return Err(ClaudeSessionError::InvalidTree);
        }
        let node = nodes.get(uuid).ok_or(ClaudeSessionError::InvalidTree)?;
        if node
            .session_identity
            .as_deref()
            .is_some_and(|identity| identity != session_identity)
        {
            return Err(ClaudeSessionError::IdentityMismatch);
        }
        chain.push(node);
        cursor = node.parent_uuid.as_deref();
    }
    chain.reverse();
    Ok(chain)
}

fn genuine_user_text(entry: &Value, message: &serde_json::Map<String, Value>) -> Option<String> {
    if optional_true(entry, "isMeta")
        || optional_true(entry, "isCompactSummary")
        || optional_true(entry, "isVisibleInTranscriptOnly")
        || entry.get("sourceToolAssistantUUID").is_some()
        || entry.get("sourceToolUseID").is_some()
        || entry.get("toolUseResult").is_some()
    {
        return None;
    }
    message.get("content").and_then(|content| {
        content
            .as_str()
            .and_then(display_line)
            .or_else(|| first_text(content))
    })
}

fn assistant_text(entry: &Value, message: &serde_json::Map<String, Value>) -> Option<String> {
    if optional_true(entry, "isSidechain") || optional_true(entry, "isApiErrorMessage") {
        return None;
    }
    let content = message.get("content")?.as_array()?;
    content.iter().rev().find_map(|block| {
        (block.get("type").and_then(Value::as_str) == Some("text"))
            .then(|| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .and_then(display_line)
            })
            .flatten()
    })
}

fn first_text(content: &Value) -> Option<String> {
    content.as_array()?.iter().find_map(|block| {
        (block.get("type").and_then(Value::as_str) == Some("text"))
            .then(|| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .and_then(display_line)
            })
            .flatten()
    })
}

fn required_bool(entry: &Value, key: &str) -> Result<bool, ClaudeSessionError> {
    entry
        .get(key)
        .and_then(Value::as_bool)
        .ok_or(ClaudeSessionError::InvalidRecord)
}

fn optional_true(entry: &Value, key: &str) -> bool {
    entry.get(key).and_then(Value::as_bool) == Some(true)
}

fn validate_optional_bool(entry: &Value, key: &str) -> Result<(), ClaudeSessionError> {
    if entry.get(key).is_some_and(|value| !value.is_boolean()) {
        return Err(ClaudeSessionError::InvalidRecord);
    }
    Ok(())
}

fn required_uuid(entry: &Value, key: &str) -> Result<String, ClaudeSessionError> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| is_uuid(value))
        .map(ToOwned::to_owned)
        .ok_or(ClaudeSessionError::InvalidRecord)
}

fn parse_parent_uuid(entry: &Value, uuid: &str) -> Result<Option<String>, ClaudeSessionError> {
    let parent = entry
        .get("parentUuid")
        .ok_or(ClaudeSessionError::InvalidRecord)?;
    if parent.is_null() {
        return Ok(None);
    }
    parent
        .as_str()
        .filter(|value| is_uuid(value) && *value != uuid)
        .map(|value| Some(value.to_owned()))
        .ok_or(ClaudeSessionError::InvalidRecord)
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
    use serde_json::json;

    const SESSION_ID: &str = "10000000-0000-4000-8000-000000000001";

    fn user(uuid: &str, parent_uuid: Option<&str>, session_id: &str, content: Value) -> Value {
        json!({
            "type": "user",
            "uuid": uuid,
            "parentUuid": parent_uuid,
            "sessionId": session_id,
            "cwd": "/work/project",
            "isSidechain": false,
            "message": {"role": "user", "content": content}
        })
    }

    fn assistant(uuid: &str, parent_uuid: &str, session_id: &str, text: &str) -> Value {
        json!({
            "type": "assistant",
            "uuid": uuid,
            "parentUuid": parent_uuid,
            "sessionId": session_id,
            "cwd": "/work/project",
            "isSidechain": false,
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}
        })
    }

    fn jsonl(entries: &[Value]) -> String {
        entries
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    #[test]
    fn derives_identity_and_cwd_without_inventing_a_session_title() {
        let view = parse_session_text(concat!(
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
            "\"sessionId\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/work/project\",",
            "\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"content\":\"private\"}]}}\n"
        ))
        .unwrap();

        assert_eq!(
            view.header.session_identity,
            "10000000-0000-4000-8000-000000000001"
        );
        assert_eq!(view.header.cwd, PathBuf::from("/work/project"));
        assert_eq!(view.display.session_name, None);
        assert_eq!(view.display.last_message, None);
    }

    #[test]
    fn custom_title_and_latest_assistant_text_block_win() {
        let view = parse_session_text(concat!(
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
            "\"sessionId\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/work/project\",",
            "\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Fallback task\"}}\n",
            "{\"type\":\"ai-title\",\"aiTitle\":\"Generated title\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\"}\n",
            "{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000002\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000001\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"message\":{\"role\":\"assistant\",\"content\":[",
            "{\"type\":\"text\",\"text\":\"Earlier block\"},{\"type\":\"thinking\",\"thinking\":\"private\"},",
            "{\"type\":\"text\",\"text\":\"Latest answer\"}]}}\n",
            "{\"type\":\"custom-title\",\"customTitle\":\"Explicit name\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\"}\n"
        ))
        .unwrap();

        assert_eq!(view.display.session_name.as_deref(), Some("Explicit name"));
        assert_eq!(view.display.last_message.as_deref(), Some("Latest answer"));
    }

    #[test]
    fn keeps_complete_title_source_separate_from_sidebar_bound() {
        let title = format!("a{}", "\u{301}".repeat(90));
        let entries = vec![
            user(
                "00000000-0000-4000-8000-000000000001",
                None,
                SESSION_ID,
                json!("Task"),
            ),
            json!({"type":"custom-title","customTitle":title,"sessionId":SESSION_ID}),
        ];

        let view = parse_session_text(&jsonl(&entries)).unwrap();

        assert_eq!(
            view.display.session_name.as_ref().unwrap().chars().count(),
            80
        );
        assert_eq!(
            view.display.tab_name_source.as_deref(),
            Some(title.as_str())
        );
    }

    #[test]
    fn follows_last_physical_leaf_through_transparent_nodes() {
        let view = parse_session_text(concat!(
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
            "\"sessionId\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/work/project\",",
            "\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Root task\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000002\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000001\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Abandoned\"}]}}\n",
            "{\"type\":\"system\",\"uuid\":\"00000000-0000-4000-8000-000000000003\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000001\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\"}\n",
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000004\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000003\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/other/cwd\",\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Rewound turn\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000005\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000004\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/other/cwd\",\"isSidechain\":false,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Active answer\"}]}}\n",
            "{\"type\":\"queue-operation\",\"operation\":\"dequeue\"}\n"
        ))
        .unwrap();

        assert_eq!(view.display.session_name, None);
        assert_eq!(view.header.cwd, PathBuf::from("/work/project"));
        assert_eq!(view.display.last_message.as_deref(), Some("Active answer"));
    }

    #[test]
    fn sidechain_messages_are_transparent_ancestry_only() {
        let sidechain = json!({
            "type": "user",
            "uuid": "00000000-0000-4000-8000-000000000001",
            "parentUuid": null,
            "sessionId": SESSION_ID,
            "cwd": "/work/project",
            "isSidechain": true,
            "message": {"role": "user", "content": "Private subagent task"}
        });
        let visible = assistant(
            "00000000-0000-4000-8000-000000000002",
            "00000000-0000-4000-8000-000000000001",
            SESSION_ID,
            "Visible answer",
        );

        let view = parse_session_text(&jsonl(&[sidechain, visible])).unwrap();

        assert_eq!(view.display.session_name, None);
        assert_eq!(view.display.last_message, None);
    }

    #[test]
    fn filters_tool_users_and_ineligible_assistant_content() {
        let view = parse_session_text(concat!(
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
            "\"sessionId\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/work/project\",",
            "\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Human task\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000002\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000001\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Visible answer\"}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000003\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000002\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"sourceToolAssistantUUID\":\"tool\",",
            "\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"content\":\"private\"}]}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000004\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000003\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"isApiErrorMessage\":true,",
            "\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Private error\"}]}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000005\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000004\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"message\":{\"role\":\"assistant\",\"content\":[",
            "{\"type\":\"thinking\",\"thinking\":\"private\"},{\"type\":\"tool_use\",\"name\":\"Read\"},{\"type\":\"fallback\",\"text\":\"private\"}]}}\n"
        ))
        .unwrap();

        assert_eq!(view.display.session_name, None);
        assert_eq!(view.display.last_message.as_deref(), Some("Visible answer"));
    }

    #[test]
    fn new_genuine_user_returns_no_replacement_activity() {
        let view = parse_session_text(concat!(
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
            "\"sessionId\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/work/project\",",
            "\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"First\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000002\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000001\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Old answer\"}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000003\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000002\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Next\"}}\n"
        ))
        .unwrap();

        assert_eq!(view.display.last_message, None);
    }

    #[test]
    fn rejects_non_boolean_message_markers() {
        let mut entry = assistant(
            "00000000-0000-4000-8000-000000000002",
            "00000000-0000-4000-8000-000000000001",
            SESSION_ID,
            "Answer",
        );
        entry["isApiErrorMessage"] = json!("true");
        let root = user(
            "00000000-0000-4000-8000-000000000001",
            None,
            SESSION_ID,
            json!("Task"),
        );
        assert!(matches!(
            parse_session_text(&jsonl(&[root, entry])),
            Err(ClaudeSessionError::InvalidRecord)
        ));
    }

    #[test]
    fn rejects_invalid_abandoned_message_branches() {
        let abandoned = assistant(
            "00000000-0000-4000-8000-000000000002",
            "00000000-0000-4000-8000-000000000099",
            SESSION_ID,
            "Abandoned",
        );
        let valid_leaf = user(
            "00000000-0000-4000-8000-000000000003",
            None,
            SESSION_ID,
            json!("Valid leaf"),
        );

        assert!(matches!(
            parse_session_text(&jsonl(&[abandoned, valid_leaf])),
            Err(ClaudeSessionError::InvalidTree)
        ));
    }

    #[test]
    fn rejects_partial_malformed_and_invalid_active_trees() {
        let valid = user(
            "00000000-0000-4000-8000-000000000001",
            None,
            SESSION_ID,
            json!("Task"),
        );
        let incomplete = format!("{}{{", jsonl(std::slice::from_ref(&valid)));
        assert!(matches!(
            parse_session_text(&incomplete),
            Err(ClaudeSessionError::IncompleteTail)
        ));
        assert!(matches!(
            parse_session_text(&(incomplete + "\n")),
            Err(ClaudeSessionError::MalformedJson)
        ));

        let missing_parent = assistant(
            "00000000-0000-4000-8000-000000000002",
            "00000000-0000-4000-8000-000000000099",
            SESSION_ID,
            "Answer",
        );
        assert!(matches!(
            parse_session_text(&jsonl(&[valid.clone(), missing_parent])),
            Err(ClaudeSessionError::InvalidTree)
        ));

        let duplicate = assistant(
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000099",
            SESSION_ID,
            "Answer",
        );
        assert!(matches!(
            parse_session_text(&jsonl(&[valid.clone(), duplicate])),
            Err(ClaudeSessionError::InvalidTree)
        ));

        let mismatched = assistant(
            "00000000-0000-4000-8000-000000000002",
            "00000000-0000-4000-8000-000000000001",
            "10000000-0000-4000-8000-000000000002",
            "Answer",
        );
        assert!(matches!(
            parse_session_text(&jsonl(&[valid, mismatched])),
            Err(ClaudeSessionError::IdentityMismatch)
        ));
    }

    #[test]
    fn accepts_legacy_custom_title_key() {
        let entries = vec![
            user(
                "00000000-0000-4000-8000-000000000001",
                None,
                SESSION_ID,
                json!("Task"),
            ),
            json!({
                "type":"custom-title",
                "customTitle":"   ",
                "title":"Legacy",
                "sessionId":SESSION_ID
            }),
        ];

        let view = parse_session_text(&jsonl(&entries)).unwrap();

        assert_eq!(view.display.session_name.as_deref(), Some("Legacy"));
    }

    #[test]
    fn ignores_mismatched_titles_and_uses_latest_matching_ai_title() {
        let entries = vec![
            user(
                "00000000-0000-4000-8000-000000000001",
                None,
                SESSION_ID,
                json!("Task"),
            ),
            json!({"type":"ai-title","aiTitle":"Wrong","sessionId":"10000000-0000-4000-8000-000000000099"}),
            json!({"type":"ai-title","aiTitle":"Earlier","sessionId":SESSION_ID}),
            json!({"type":"custom-title","title":"   ","sessionId":SESSION_ID}),
            json!({"type":"ai-title","aiTitle":"Latest","sessionId":SESSION_ID}),
        ];

        let view = parse_session_text(&jsonl(&entries)).unwrap();

        assert_eq!(view.display.session_name.as_deref(), Some("Latest"));
    }

    #[test]
    fn ignores_unknown_unreferenced_uuid_records() {
        let view = parse_session_text(concat!(
            "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
            "\"sessionId\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/work/project\",",
            "\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Task\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000002\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000001\",\"sessionId\":\"10000000-0000-4000-8000-000000000001\",",
            "\"cwd\":\"/work/project\",\"isSidechain\":false,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Answer\"}]}}\n",
            "{\"type\":\"future-record\",\"uuid\":\"00000000-0000-4000-8000-000000000003\"}\n"
        ))
        .unwrap();

        assert_eq!(view.display.last_message.as_deref(), Some("Answer"));
    }
}
