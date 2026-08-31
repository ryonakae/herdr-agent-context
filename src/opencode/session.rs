use crate::backend::DisplayView;
use crate::text::{complete_line, display_line};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ORDINARY_MAX_AGE_MILLIS: i64 = 30 * 24 * 60 * 60 * 1_000;
const ORDINARY_MAX_CANDIDATES: usize = 25;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct RowFingerprint {
    pub time_created: i64,
    pub time_updated: i64,
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFingerprint {
    pub session_time_created: i64,
    pub session_time_updated: i64,
    pub session_title: String,
    pub latest_message: Option<RowFingerprint>,
    pub latest_part: Option<RowFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeSessionView {
    pub cwd: PathBuf,
    pub display: DisplayView,
    pub fingerprint: SessionFingerprint,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum OpenCodeSessionError {
    #[error("failed to read OpenCode database")]
    Read,
    #[error("OpenCode database schema is incompatible")]
    IncompatibleSchema,
    #[error("OpenCode session is invalid")]
    InvalidSession,
    #[error("OpenCode session history is malformed")]
    MalformedHistory,
}

#[derive(Debug)]
struct SessionRow {
    id: String,
    parent_id: Option<String>,
    directory: String,
    title: String,
    time_created: i64,
    time_updated: i64,
}

#[derive(Debug)]
struct MessageRow {
    id: String,
    time_created: i64,
    time_updated: i64,
    data: Value,
}

#[derive(Debug)]
struct PartRow {
    id: String,
    time_created: i64,
    time_updated: i64,
    data: Value,
}

pub fn read_session(
    database_path: &Path,
    session_identity: &str,
    expected_cwd: &Path,
) -> Result<Option<OpenCodeSessionView>, OpenCodeSessionError> {
    with_read_transaction(database_path, |transaction| {
        validate_required_schema(transaction)?;
        read_session_from_transaction(transaction, session_identity, expected_cwd)
    })
}

pub fn scan_sessions(
    database_path: &Path,
    expected_cwd: &Path,
    now_millis: i64,
) -> Result<Vec<OpenCodeSessionView>, OpenCodeSessionError> {
    let input_cwd = expected_cwd
        .to_str()
        .ok_or(OpenCodeSessionError::InvalidSession)?
        .to_owned();
    let expected_cwd =
        normalized_absolute(expected_cwd).ok_or(OpenCodeSessionError::InvalidSession)?;
    let canonical_cwd = expected_cwd
        .to_str()
        .ok_or(OpenCodeSessionError::InvalidSession)?
        .to_owned();
    with_read_transaction(database_path, |transaction| {
        validate_required_schema(transaction)?;
        let mut statement = transaction
            .prepare(
                "SELECT id, parent_id, directory, title, time_created, time_updated, time_archived
                 FROM session
                 WHERE parent_id IS NULL
                   AND time_archived IS NULL
                   AND time_updated >= ?1
                   AND (directory = ?2 OR directory = ?3)
                 ORDER BY time_updated DESC, id ASC
                 LIMIT ?4",
            )
            .map_err(classify_sql_error)?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    now_millis.saturating_sub(ORDINARY_MAX_AGE_MILLIS),
                    input_cwd,
                    canonical_cwd,
                    ORDINARY_MAX_CANDIDATES as i64
                ],
                |row| {
                    let id: String = row.get(0)?;
                    let _: Option<String> = row.get(1)?;
                    let directory: String = row.get(2)?;
                    let _: String = row.get(3)?;
                    let _: i64 = row.get(4)?;
                    let _: i64 = row.get(5)?;
                    let _: Option<i64> = row.get(6)?;
                    Ok((id, directory))
                },
            )
            .map_err(classify_sql_error)?;
        let mut identities = Vec::new();
        for row in rows {
            let (identity, directory) = row.map_err(classify_sql_error)?;
            if identity.trim().is_empty() {
                return Err(OpenCodeSessionError::InvalidSession);
            }
            let Some(directory) = normalized_absolute(Path::new(&directory)) else {
                return Err(OpenCodeSessionError::InvalidSession);
            };
            if directory == expected_cwd {
                identities.push(identity);
            }
        }
        drop(statement);

        let mut sessions = Vec::with_capacity(identities.len());
        for identity in identities {
            let session = read_session_from_transaction(transaction, &identity, &expected_cwd)?
                .ok_or(OpenCodeSessionError::InvalidSession)?;
            sessions.push(session);
        }
        Ok(sessions)
    })
}

fn validate_required_schema(transaction: &Transaction<'_>) -> Result<(), OpenCodeSessionError> {
    for query in [
        "SELECT id, parent_id, directory, title, time_created, time_updated, time_archived FROM session LIMIT 0",
        "SELECT id, session_id, time_created, time_updated, data FROM message LIMIT 0",
        "SELECT id, message_id, session_id, time_created, time_updated, data FROM part LIMIT 0",
    ] {
        transaction.prepare(query).map_err(classify_sql_error)?;
    }
    Ok(())
}

fn with_read_transaction<T>(
    database_path: &Path,
    read: impl FnOnce(&Transaction<'_>) -> Result<T, OpenCodeSessionError>,
) -> Result<T, OpenCodeSessionError> {
    let mut connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| OpenCodeSessionError::Read)?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|_| OpenCodeSessionError::Read)?;
    let transaction = connection
        .transaction()
        .map_err(|_| OpenCodeSessionError::Read)?;
    let result = read(&transaction);
    transaction
        .commit()
        .map_err(|_| OpenCodeSessionError::Read)?;
    result
}

fn read_session_from_transaction(
    transaction: &Transaction<'_>,
    session_identity: &str,
    expected_cwd: &Path,
) -> Result<Option<OpenCodeSessionView>, OpenCodeSessionError> {
    if session_identity.trim().is_empty() {
        return Err(OpenCodeSessionError::InvalidSession);
    }
    let session = transaction
        .query_row(
            "SELECT id, parent_id, directory, title, time_created, time_updated, time_archived
             FROM session WHERE id = ?1",
            [session_identity],
            |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    parent_id: row.get(1)?,
                    directory: row.get(2)?,
                    title: row.get(3)?,
                    time_created: row.get(4)?,
                    time_updated: {
                        let _: Option<i64> = row.get(6)?;
                        row.get(5)?
                    },
                })
            },
        )
        .optional()
        .map_err(classify_sql_error)?;
    let Some(session) = session else {
        return Ok(None);
    };
    if session.id != session_identity || session.id.trim().is_empty() || session.parent_id.is_some()
    {
        return Err(OpenCodeSessionError::InvalidSession);
    }
    let cwd = normalized_absolute(Path::new(&session.directory))
        .ok_or(OpenCodeSessionError::InvalidSession)?;
    let expected_cwd =
        normalized_absolute(expected_cwd).ok_or(OpenCodeSessionError::InvalidSession)?;
    if cwd != expected_cwd {
        return Err(OpenCodeSessionError::InvalidSession);
    }

    let messages = read_messages(transaction, session_identity)?;
    let parts = read_parts(transaction, session_identity, &messages)?;
    let latest_message = messages
        .iter()
        .map(|message| RowFingerprint {
            time_created: message.time_created,
            time_updated: message.time_updated,
            id: message.id.clone(),
        })
        .max();
    let latest_part = parts
        .values()
        .flatten()
        .map(|part| RowFingerprint {
            time_created: part.time_created,
            time_updated: part.time_updated,
            id: part.id.clone(),
        })
        .max();

    let mut first_user = None;
    let mut latest_user_order = None;
    let mut last_message = None;
    for (message_order, message) in messages.iter().enumerate() {
        let object = message
            .data
            .as_object()
            .ok_or(OpenCodeSessionError::MalformedHistory)?;
        let role = object
            .get("role")
            .and_then(Value::as_str)
            .ok_or(OpenCodeSessionError::MalformedHistory)?;
        let message_parts = parts
            .get(&message.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        match role {
            "user" => {
                let mut visible = Vec::new();
                for part in message_parts {
                    if let Some(text) = visible_text(part)
                        && let Some(line) = complete_line(&text?)
                    {
                        visible.push(line);
                    }
                }
                if let Some(text) = visible.first() {
                    first_user.get_or_insert_with(|| text.clone());
                    latest_user_order = Some(message_order);
                    last_message = None;
                }
            }
            "assistant" => {
                let has_error = object.get("error").is_some_and(|value| !value.is_null());
                if has_error || latest_user_order.is_none() {
                    continue;
                }
                for text in message_parts.iter().filter_map(visible_text) {
                    if let Some(line) = display_line(&text?) {
                        last_message = Some(line);
                    }
                }
            }
            _ => {}
        }
    }

    let cwd_name = cwd
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(complete_line);
    let title = complete_line(&session.title).filter(|title| !is_default_title(title));
    let tab_name_source = title.or(first_user).or(cwd_name);
    let session_name = tab_name_source.as_deref().and_then(display_line);
    Ok(Some(OpenCodeSessionView {
        cwd,
        display: DisplayView {
            session_identity: session.id,
            session_name,
            tab_name_source,
            last_message,
        },
        fingerprint: SessionFingerprint {
            session_time_created: session.time_created,
            session_time_updated: session.time_updated,
            session_title: session.title,
            latest_message,
            latest_part,
        },
    }))
}

fn read_messages(
    transaction: &Transaction<'_>,
    session_identity: &str,
) -> Result<Vec<MessageRow>, OpenCodeSessionError> {
    let mut statement = transaction
        .prepare(
            "SELECT id, session_id, time_created, time_updated, data
             FROM message WHERE session_id = ?1 ORDER BY time_created ASC, id ASC",
        )
        .map_err(classify_sql_error)?;
    let rows = statement
        .query_map([session_identity], |row| {
            let id: String = row.get(0)?;
            let row_session: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let time_updated: i64 = row.get(3)?;
            let data: String = row.get(4)?;
            Ok((id, row_session, time_created, time_updated, data))
        })
        .map_err(classify_sql_error)?;
    let mut messages = Vec::new();
    for row in rows {
        let (id, row_session, time_created, time_updated, data) =
            row.map_err(classify_sql_error)?;
        if id.trim().is_empty() || row_session != session_identity {
            return Err(OpenCodeSessionError::MalformedHistory);
        }
        let data =
            serde_json::from_str(&data).map_err(|_| OpenCodeSessionError::MalformedHistory)?;
        messages.push(MessageRow {
            id,
            time_created,
            time_updated,
            data,
        });
    }
    Ok(messages)
}

fn read_parts(
    transaction: &Transaction<'_>,
    session_identity: &str,
    messages: &[MessageRow],
) -> Result<HashMap<String, Vec<PartRow>>, OpenCodeSessionError> {
    let message_ids: HashSet<&str> = messages.iter().map(|message| message.id.as_str()).collect();
    let mut statement = transaction
        .prepare(
            "SELECT id, message_id, session_id, time_created, time_updated, data
             FROM part
             WHERE session_id = ?1
                OR message_id IN (SELECT id FROM message WHERE session_id = ?1)
             ORDER BY time_created ASC, id ASC",
        )
        .map_err(classify_sql_error)?;
    let rows = statement
        .query_map([session_identity], |row| {
            let id: String = row.get(0)?;
            let message_id: String = row.get(1)?;
            let row_session: String = row.get(2)?;
            let time_created: i64 = row.get(3)?;
            let time_updated: i64 = row.get(4)?;
            let data: String = row.get(5)?;
            Ok((
                id,
                message_id,
                row_session,
                time_created,
                time_updated,
                data,
            ))
        })
        .map_err(classify_sql_error)?;
    let mut parts: HashMap<String, Vec<PartRow>> = HashMap::new();
    for row in rows {
        let (id, message_id, row_session, time_created, time_updated, data) =
            row.map_err(classify_sql_error)?;
        if id.trim().is_empty()
            || row_session != session_identity
            || !message_ids.contains(message_id.as_str())
        {
            return Err(OpenCodeSessionError::MalformedHistory);
        }
        let data =
            serde_json::from_str(&data).map_err(|_| OpenCodeSessionError::MalformedHistory)?;
        parts.entry(message_id).or_default().push(PartRow {
            id,
            time_created,
            time_updated,
            data,
        });
    }
    Ok(parts)
}

fn visible_text(part: &PartRow) -> Option<Result<String, OpenCodeSessionError>> {
    let object = match part.data.as_object() {
        Some(object) => object,
        None => return Some(Err(OpenCodeSessionError::MalformedHistory)),
    };
    let part_type = match object.get("type").and_then(Value::as_str) {
        Some(part_type) => part_type,
        None => return Some(Err(OpenCodeSessionError::MalformedHistory)),
    };
    if part_type != "text" {
        return None;
    }
    for key in ["synthetic", "ignored"] {
        if let Some(value) = object.get(key)
            && !value.is_boolean()
        {
            return Some(Err(OpenCodeSessionError::MalformedHistory));
        }
    }
    if object
        .get("synthetic")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || object
            .get("ignored")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }
    Some(
        object
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or(OpenCodeSessionError::MalformedHistory),
    )
}

fn classify_sql_error(error: rusqlite::Error) -> OpenCodeSessionError {
    match error {
        rusqlite::Error::SqliteFailure(_, _) => OpenCodeSessionError::Read,
        _ => OpenCodeSessionError::IncompatibleSchema,
    }
}

fn normalized_absolute(path: &Path) -> Option<PathBuf> {
    path.is_absolute()
        .then(|| std::fs::canonicalize(path).ok())
        .flatten()
}

fn is_default_title(value: &str) -> bool {
    let Some(timestamp) = value.strip_prefix("New session - ") else {
        return false;
    };
    let bytes = timestamp.as_bytes();
    if timestamp.len() != 24
        || !matches!(bytes.get(4), Some(b'-'))
        || !matches!(bytes.get(7), Some(b'-'))
        || !matches!(bytes.get(10), Some(b'T'))
        || !matches!(bytes.get(13), Some(b':'))
        || !matches!(bytes.get(16), Some(b':'))
        || !matches!(bytes.get(19), Some(b'.'))
        || !matches!(bytes.get(23), Some(b'Z'))
        || !bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19 | 23) || byte.is_ascii_digit()
        })
    {
        return false;
    }
    let Some(year) = timestamp
        .get(..4)
        .and_then(|value| value.parse::<u16>().ok())
    else {
        return false;
    };
    let Some(month) = timestamp
        .get(5..7)
        .and_then(|value| value.parse::<u8>().ok())
    else {
        return false;
    };
    let Some(day) = timestamp
        .get(8..10)
        .and_then(|value| value.parse::<u8>().ok())
    else {
        return false;
    };
    let Some(hour) = timestamp
        .get(11..13)
        .and_then(|value| value.parse::<u8>().ok())
    else {
        return false;
    };
    let Some(minute) = timestamp
        .get(14..16)
        .and_then(|value| value.parse::<u8>().ok())
    else {
        return false;
    };
    let Some(second) = timestamp
        .get(17..19)
        .and_then(|value| value.parse::<u8>().ok())
    else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params};

    fn create_database(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE session (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    directory TEXT NOT NULL,
                    title TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    time_archived INTEGER
                );
                CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                );
                CREATE TABLE part (
                    id TEXT PRIMARY KEY,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                );
                ",
            )
            .unwrap();
        connection
    }

    fn insert_session(
        connection: &Connection,
        id: &str,
        directory: &Path,
        title: &str,
        updated: i64,
    ) {
        connection
            .execute(
                "INSERT INTO session VALUES (?1, NULL, ?2, ?3, 1, ?4, NULL)",
                params![id, directory.to_str().unwrap(), title, updated],
            )
            .unwrap();
    }

    fn insert_message(
        connection: &Connection,
        id: &str,
        session_id: &str,
        created: i64,
        updated: i64,
        data: &str,
    ) {
        connection
            .execute(
                "INSERT INTO message VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, session_id, created, updated, data],
            )
            .unwrap();
    }

    fn insert_part(
        connection: &Connection,
        id: &str,
        message_id: &str,
        session_id: &str,
        created: i64,
        updated: i64,
        data: &str,
    ) {
        connection
            .execute(
                "INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, message_id, session_id, created, updated, data],
            )
            .unwrap();
    }

    #[test]
    fn reads_meaningful_title_and_latest_assistant_text() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        connection
            .execute(
                "INSERT INTO session VALUES (?1, NULL, ?2, ?3, 1, 5, NULL)",
                params![
                    "ses_synthetic",
                    project.to_str().unwrap(),
                    "Synthetic title"
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message VALUES (?1, ?2, 2, 2, ?3)",
                params!["msg_user", "ses_synthetic", r#"{"role":"user"}"#],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO part VALUES (?1, ?2, ?3, 2, 2, ?4)",
                params![
                    "part_user",
                    "msg_user",
                    "ses_synthetic",
                    r#"{"type":"text","text":"Synthetic prompt"}"#
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message VALUES (?1, ?2, 3, 4, ?3)",
                params!["msg_assistant", "ses_synthetic", r#"{"role":"assistant"}"#],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO part VALUES (?1, ?2, ?3, 3, 4, ?4)",
                params![
                    "part_assistant",
                    "msg_assistant",
                    "ses_synthetic",
                    r#"{"type":"text","text":"Synthetic response"}"#
                ],
            )
            .unwrap();
        drop(connection);

        let view = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        assert_eq!(view.cwd, project.canonicalize().unwrap());
        assert_eq!(view.display.session_identity, "ses_synthetic");
        assert_eq!(
            view.display.session_name.as_deref(),
            Some("Synthetic title")
        );
        assert_eq!(
            view.display.tab_name_source.as_deref(),
            Some("Synthetic title")
        );
        assert_eq!(
            view.display.last_message.as_deref(),
            Some("Synthetic response")
        );
    }

    #[test]
    fn rejects_required_schema_and_relationship_failures() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();

        let missing_column = temp.path().join("missing-column.db");
        let connection = Connection::open(&missing_column).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE session (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    directory TEXT NOT NULL,
                    title TEXT NOT NULL,
                    time_updated INTEGER NOT NULL
                );
                CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                );
                CREATE TABLE part (
                    id TEXT PRIMARY KEY,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                );
                ",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO session VALUES (?1, NULL, ?2, ?3, 1)",
                params!["ses_synthetic", project.to_str().unwrap(), "Title"],
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            read_session(&missing_column, "ses_synthetic", &project),
            Err(OpenCodeSessionError::IncompatibleSchema)
        );

        let mismatched_part = temp.path().join("mismatched-part.db");
        let connection = create_database(&mismatched_part);
        connection
            .execute(
                "INSERT INTO session VALUES (?1, NULL, ?2, ?3, 1, 1, NULL)",
                params!["ses_synthetic", project.to_str().unwrap(), "Title"],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message VALUES (?1, ?2, 1, 1, ?3)",
                params!["msg_user", "ses_synthetic", r#"{"role":"user"}"#],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO part VALUES (?1, ?2, ?3, 1, 1, ?4)",
                params![
                    "part_wrong_session",
                    "msg_user",
                    "ses_other",
                    r#"{"type":"text","text":"Prompt"}"#
                ],
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            read_session(&mismatched_part, "ses_synthetic", &project),
            Err(OpenCodeSessionError::MalformedHistory)
        );
    }

    #[test]
    fn treats_only_a_valid_default_timestamp_as_a_default_title() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        let title = "New session - 2026-02-30T00:00:00.000Z";
        connection
            .execute(
                "INSERT INTO session VALUES (?1, NULL, ?2, ?3, 1, 1, NULL)",
                params!["ses_synthetic", project.to_str().unwrap(), title],
            )
            .unwrap();
        drop(connection);

        let view = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();

        assert_eq!(view.display.tab_name_source.as_deref(), Some(title));
    }

    #[test]
    fn requires_canonical_existing_cwds() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-project");
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        insert_session(&connection, "ses_synthetic", &missing, "Title", 1);
        drop(connection);

        assert_eq!(
            read_session(&database, "ses_synthetic", &missing),
            Err(OpenCodeSessionError::InvalidSession)
        );
    }

    #[test]
    fn defaults_to_first_genuine_user_then_canonical_cwd_and_observes_title_updates() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        insert_session(
            &connection,
            "ses_user",
            &project,
            "New session - 2026-08-31T12:34:56.789Z",
            1,
        );
        insert_message(
            &connection,
            "msg_user",
            "ses_user",
            1,
            1,
            r#"{"role":"user"}"#,
        );
        insert_part(
            &connection,
            "part_user",
            "msg_user",
            "ses_user",
            1,
            1,
            r#"{"type":"text","text":"  First genuine prompt\nignored"}"#,
        );
        insert_session(&connection, "ses_cwd", &project, "   ", 1);

        let user_view = read_session(&database, "ses_user", &project)
            .unwrap()
            .unwrap();
        assert_eq!(
            user_view.display.tab_name_source.as_deref(),
            Some("First genuine prompt")
        );
        let cwd_view = read_session(&database, "ses_cwd", &project)
            .unwrap()
            .unwrap();
        assert_eq!(cwd_view.display.tab_name_source.as_deref(), Some("project"));

        connection
            .execute(
                "UPDATE session SET title = ?1, time_updated = ?2 WHERE id = ?3",
                params!["Updated title", 2, "ses_user"],
            )
            .unwrap();
        drop(connection);
        let updated_view = read_session(&database, "ses_user", &project)
            .unwrap()
            .unwrap();
        assert_eq!(
            updated_view.display.tab_name_source.as_deref(),
            Some("Updated title")
        );
        assert_eq!(updated_view.fingerprint.session_time_updated, 2);
    }

    #[test]
    fn orders_messages_and_parts_by_created_time_and_id_and_tracks_streaming_updates() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        insert_session(&connection, "ses_synthetic", &project, "Title", 1);
        insert_message(
            &connection,
            "msg_user",
            "ses_synthetic",
            2,
            9,
            r#"{"role":"user"}"#,
        );
        insert_part(
            &connection,
            "part_user",
            "msg_user",
            "ses_synthetic",
            2,
            9,
            r#"{"type":"text","text":"Prompt"}"#,
        );
        insert_message(
            &connection,
            "msg_assistant",
            "ses_synthetic",
            3,
            1,
            r#"{"role":"assistant"}"#,
        );
        insert_part(
            &connection,
            "part_z",
            "msg_assistant",
            "ses_synthetic",
            4,
            1,
            r#"{"type":"text","text":"Later by id"}"#,
        );
        insert_part(
            &connection,
            "part_a",
            "msg_assistant",
            "ses_synthetic",
            4,
            99,
            r#"{"type":"text","text":"Earlier by id"}"#,
        );
        insert_message(
            &connection,
            "msg_pre_user",
            "ses_synthetic",
            1,
            100,
            r#"{"role":"assistant"}"#,
        );
        insert_part(
            &connection,
            "part_pre_user",
            "msg_pre_user",
            "ses_synthetic",
            1,
            100,
            r#"{"type":"text","text":"Must not be selected"}"#,
        );

        let initial = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        assert_eq!(initial.display.last_message.as_deref(), Some("Later by id"));
        assert_eq!(
            initial.fingerprint.latest_part,
            Some(RowFingerprint {
                time_created: 4,
                time_updated: 99,
                id: "part_a".into(),
            })
        );

        connection
            .execute(
                "UPDATE part SET data = ?1, time_updated = ?2 WHERE id = ?3",
                params![
                    r#"{"type":"text","text":"Streaming replacement"}"#,
                    100,
                    "part_z"
                ],
            )
            .unwrap();
        drop(connection);

        let streamed = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        assert_eq!(
            streamed.display.last_message.as_deref(),
            Some("Streaming replacement")
        );
        assert_ne!(streamed.fingerprint, initial.fingerprint);
        assert_eq!(streamed.fingerprint.session_time_updated, 1);
        assert_eq!(
            streamed.fingerprint.latest_part,
            Some(RowFingerprint {
                time_created: 4,
                time_updated: 100,
                id: "part_z".into(),
            })
        );
    }

    #[test]
    fn rejects_nonroot_mismatched_or_malformed_sessions_without_misreading_session_message() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let other_project = temp.path().join("other-project");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&other_project).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        insert_session(&connection, "ses_synthetic", &project, "Title", 1);
        insert_message(
            &connection,
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"role":"user"}"#,
        );
        insert_part(
            &connection,
            "part_user",
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"type":"text","text":"Prompt"}"#,
        );

        assert_eq!(read_session(&database, "missing", &project), Ok(None));
        assert_eq!(
            read_session(&database, "ses_synthetic", &other_project),
            Err(OpenCodeSessionError::InvalidSession)
        );
        connection
            .execute(
                "UPDATE session SET parent_id = ?1 WHERE id = ?2",
                params!["ses_parent", "ses_synthetic"],
            )
            .unwrap();
        assert_eq!(
            read_session(&database, "ses_synthetic", &project),
            Err(OpenCodeSessionError::InvalidSession)
        );
        connection
            .execute(
                "UPDATE session SET parent_id = NULL WHERE id = ?1",
                ["ses_synthetic"],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE message SET data = ?1 WHERE id = ?2",
                params![r#"{"role":false}"#, "msg_user"],
            )
            .unwrap();
        assert_eq!(
            read_session(&database, "ses_synthetic", &project),
            Err(OpenCodeSessionError::MalformedHistory)
        );
        connection
            .execute(
                "UPDATE message SET data = ?1 WHERE id = ?2",
                params![r#"{"role":"user"}"#, "msg_user"],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE part SET data = ?1 WHERE id = ?2",
                params![r#"{"type":"text","text":false}"#, "part_user"],
            )
            .unwrap();
        assert_eq!(
            read_session(&database, "ses_synthetic", &project),
            Err(OpenCodeSessionError::MalformedHistory)
        );
        connection
            .execute(
                "UPDATE part SET data = ?1 WHERE id = ?2",
                params![
                    r#"{"type":"text","text":"Prompt","synthetic":"true"}"#,
                    "part_user"
                ],
            )
            .unwrap();
        assert_eq!(
            read_session(&database, "ses_synthetic", &project),
            Err(OpenCodeSessionError::MalformedHistory)
        );
        drop(connection);

        let unsupported = temp.path().join("session-message-only.db");
        let connection = Connection::open(&unsupported).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session_message (id TEXT PRIMARY KEY, data TEXT NOT NULL);",
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            read_session(&unsupported, "ses_synthetic", &project),
            Err(OpenCodeSessionError::Read)
        );
    }

    #[test]
    fn filters_non_display_parts_and_returns_no_replacement_after_the_latest_user() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        insert_session(&connection, "ses_synthetic", &project, "   ", 1);
        insert_message(
            &connection,
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"role":"user"}"#,
        );
        insert_part(
            &connection,
            "part_synthetic_user",
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"type":"text","text":"Private","synthetic":true}"#,
        );
        insert_part(
            &connection,
            "part_real_user",
            "msg_user",
            "ses_synthetic",
            2,
            1,
            r#"{"type":"text","text":"Real prompt"}"#,
        );
        insert_message(
            &connection,
            "msg_assistant",
            "ses_synthetic",
            2,
            1,
            r#"{"role":"assistant"}"#,
        );
        for (id, data) in [
            ("part_reasoning", r#"{"type":"reasoning","text":"Private"}"#),
            ("part_tool", r#"{"type":"tool","text":"Private"}"#),
            (
                "part_tool_result",
                r#"{"type":"tool-result","text":"Private"}"#,
            ),
            ("part_file", r#"{"type":"file","text":"Private"}"#),
            ("part_patch", r#"{"type":"patch","text":"Private"}"#),
            ("part_step", r#"{"type":"step-start","text":"Private"}"#),
            (
                "part_ignored",
                r#"{"type":"text","text":"Private","ignored":true}"#,
            ),
            (
                "part_visible",
                r#"{"type":"text","text":"Visible response"}"#,
            ),
        ] {
            insert_part(
                &connection,
                id,
                "msg_assistant",
                "ses_synthetic",
                3,
                1,
                data,
            );
        }
        insert_message(
            &connection,
            "msg_latest",
            "ses_synthetic",
            4,
            1,
            r#"{"role":"user"}"#,
        );
        insert_part(
            &connection,
            "part_latest",
            "msg_latest",
            "ses_synthetic",
            4,
            1,
            r#"{"type":"text","text":"Next prompt"}"#,
        );
        insert_message(
            &connection,
            "msg_error",
            "ses_synthetic",
            5,
            1,
            r#"{"role":"assistant","error":{"name":"failure"}}"#,
        );
        insert_part(
            &connection,
            "part_error",
            "msg_error",
            "ses_synthetic",
            5,
            1,
            r#"{"type":"text","text":"Error response"}"#,
        );
        drop(connection);

        let view = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        assert_eq!(view.display.tab_name_source.as_deref(), Some("Real prompt"));
        assert_eq!(view.display.last_message, None);
    }

    #[test]
    fn preserves_unbounded_sources_but_bounds_unicode_sidebar_values_to_one_line() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        insert_session(&connection, "ses_synthetic", &project, "   ", 1);
        insert_message(
            &connection,
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"role":"user"}"#,
        );
        let prompt = "界".repeat(81);
        insert_part(
            &connection,
            "part_user",
            "msg_user",
            "ses_synthetic",
            1,
            1,
            &format!(r#"{{"type":"text","text":"  {prompt}  \nignored"}}"#),
        );
        insert_message(
            &connection,
            "msg_assistant",
            "ses_synthetic",
            2,
            1,
            r#"{"role":"assistant"}"#,
        );
        let response = "語".repeat(81);
        insert_part(
            &connection,
            "part_assistant",
            "msg_assistant",
            "ses_synthetic",
            2,
            1,
            &format!(r#"{{"type":"text","text":"  {response}  \nignored"}}"#),
        );
        drop(connection);

        let view = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        assert_eq!(
            view.display.tab_name_source.as_deref(),
            Some(prompt.as_str())
        );
        assert_eq!(
            view.display.session_name.as_ref().unwrap().chars().count(),
            80
        );
        assert!(view.display.session_name.as_ref().unwrap().ends_with('…'));
        assert_eq!(
            view.display.last_message.as_ref().unwrap().chars().count(),
            80
        );
        assert!(view.display.last_message.as_ref().unwrap().ends_with('…'));
    }

    #[test]
    fn reads_all_display_rows_from_one_transaction_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let database = temp.path().join("opencode.db");
        let writer = create_database(&database);
        writer.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
        insert_session(&writer, "ses_synthetic", &project, "Before", 1);
        insert_message(
            &writer,
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"role":"user"}"#,
        );
        insert_part(
            &writer,
            "part_user",
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"type":"text","text":"Prompt"}"#,
        );
        insert_message(
            &writer,
            "msg_assistant",
            "ses_synthetic",
            2,
            1,
            r#"{"role":"assistant"}"#,
        );
        insert_part(
            &writer,
            "part_assistant",
            "msg_assistant",
            "ses_synthetic",
            2,
            1,
            r#"{"type":"text","text":"Before"}"#,
        );

        let mut reader = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        let transaction = reader.transaction().unwrap();
        let _: String = transaction
            .query_row(
                "SELECT title FROM session WHERE id = ?1",
                ["ses_synthetic"],
                |row| row.get(0),
            )
            .unwrap();
        writer
            .execute(
                "UPDATE session SET title = ?1, time_updated = ?2 WHERE id = ?3",
                params!["After", 2, "ses_synthetic"],
            )
            .unwrap();
        writer
            .execute(
                "UPDATE part SET data = ?1, time_updated = ?2 WHERE id = ?3",
                params![r#"{"type":"text","text":"After"}"#, 2, "part_assistant"],
            )
            .unwrap();

        let snapshot = read_session_from_transaction(&transaction, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.display.tab_name_source.as_deref(), Some("Before"));
        assert_eq!(snapshot.display.last_message.as_deref(), Some("Before"));
        transaction.commit().unwrap();

        let current = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        assert_eq!(current.display.tab_name_source.as_deref(), Some("After"));
        assert_eq!(current.display.last_message.as_deref(), Some("After"));
    }

    #[test]
    fn reads_wal_updates_from_fresh_snapshots_and_never_waits_for_a_busy_database() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let database = temp.path().join("opencode.db");
        let writer = create_database(&database);
        writer.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
        insert_session(&writer, "ses_synthetic", &project, "Title", 1);
        insert_message(
            &writer,
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"role":"user"}"#,
        );
        insert_part(
            &writer,
            "part_user",
            "msg_user",
            "ses_synthetic",
            1,
            1,
            r#"{"type":"text","text":"Prompt"}"#,
        );
        insert_message(
            &writer,
            "msg_assistant",
            "ses_synthetic",
            2,
            1,
            r#"{"role":"assistant"}"#,
        );
        insert_part(
            &writer,
            "part_assistant",
            "msg_assistant",
            "ses_synthetic",
            2,
            1,
            r#"{"type":"text","text":"Initial stream"}"#,
        );

        let initial = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        writer
            .execute(
                "UPDATE part SET data = ?1, time_updated = ?2 WHERE id = ?3",
                params![
                    r#"{"type":"text","text":"Current stream"}"#,
                    2,
                    "part_assistant"
                ],
            )
            .unwrap();
        let current = read_session(&database, "ses_synthetic", &project)
            .unwrap()
            .unwrap();
        assert_eq!(
            initial.display.last_message.as_deref(),
            Some("Initial stream")
        );
        assert_eq!(
            current.display.last_message.as_deref(),
            Some("Current stream")
        );
        assert_ne!(initial.fingerprint, current.fingerprint);

        let busy_database = temp.path().join("busy.db");
        let locker = create_database(&busy_database);
        insert_session(&locker, "ses_busy", &project, "Title", 1);
        locker.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            read_session(&busy_database, "ses_busy", &project),
            Err(OpenCodeSessionError::Read)
        );
        assert!(started.elapsed() < Duration::from_millis(250));
        locker.execute_batch("ROLLBACK;").unwrap();
    }
}
