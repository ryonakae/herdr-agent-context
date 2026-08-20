use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub const METADATA_SOURCE: &str = "ryonakae.agent-context";
pub const SESSION_NAME_TOKEN: &str = "agent_context_session_name";
pub const LAST_MESSAGE_TOKEN: &str = "agent_context_last_message";

pub const SUBSCRIPTIONS: &[&str] = &[
    "pane.created",
    "pane.updated",
    "pane.closed",
    "pane.exited",
    "pane.agent_detected",
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AgentInfo {
    pub terminal_id: String,
    pub agent: Option<String>,
    pub agent_status: String,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    pub pane_id: String,
    pub revision: u64,
    #[serde(default)]
    pub state_change_seq: u64,
    pub agent_session: Option<AgentSessionInfo>,
}

#[derive(Debug, Deserialize)]
pub struct AgentListResult {
    pub agents: Vec<AgentInfo>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ProcessInfo {
    pub pane_id: String,
    #[serde(default)]
    pub foreground_processes: Vec<Process>,
}

impl ProcessInfo {
    pub fn args(&self) -> Vec<String> {
        self.foreground_processes
            .iter()
            .flat_map(|process| {
                process
                    .argv
                    .iter()
                    .flatten()
                    .cloned()
                    .chain(process.argv0.iter().cloned())
                    .chain(process.cmdline.iter().cloned())
            })
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    pub argv: Option<Vec<String>>,
    pub argv0: Option<String>,
    pub cmdline: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneEvent {
    pub kind: String,
    pub pane_id: Option<String>,
}

pub fn request(id: &str, method: &str, params: Value) -> Value {
    json!({"id": id, "method": method, "params": params})
}

pub fn subscription_params() -> Value {
    json!({
        "subscriptions": SUBSCRIPTIONS
            .iter()
            .map(|kind| json!({"type": kind}))
            .collect::<Vec<_>>()
    })
}

pub fn process_info_params(pane_id: &str) -> Value {
    json!({"pane_id": pane_id})
}

pub fn metadata_params(
    agent: &str,
    pane_id: &str,
    applies_to_source: Option<&str>,
    seq: u64,
    ttl_ms: u64,
    session_name: Option<&str>,
    last_message: Option<&str>,
) -> Value {
    let mut tokens = BTreeMap::new();
    tokens.insert(SESSION_NAME_TOKEN, session_name);
    tokens.insert(LAST_MESSAGE_TOKEN, last_message);
    json!({
        "pane_id": pane_id,
        "source": METADATA_SOURCE,
        "agent": agent,
        "applies_to_source": applies_to_source,
        "tokens": tokens,
        "seq": seq,
        "ttl_ms": ttl_ms
    })
}

pub fn parse_result(value: Value) -> Result<Value, String> {
    if let Some(error) = value.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(code.to_owned());
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| "missing_result".to_owned())
}

pub fn parse_agents(result: Value) -> Result<Vec<AgentInfo>, serde_json::Error> {
    serde_json::from_value::<AgentListResult>(result).map(|result| result.agents)
}

pub fn parse_process_info(result: Value) -> Result<ProcessInfo, serde_json::Error> {
    let value = result.get("process_info").cloned().unwrap_or(result);
    serde_json::from_value(value)
}

pub fn parse_event(value: &Value) -> Option<PaneEvent> {
    let event = value.get("event")?.as_str()?.to_owned();
    let data = value.get("data")?.as_object()?;
    let data_kind = data.get("type").and_then(Value::as_str);
    let expected = event.replace('.', "_");
    if data_kind != Some(expected.as_str()) && data_kind != Some(event.as_str()) {
        return None;
    }
    Some(PaneEvent {
        kind: event,
        pane_id: pane_id(data),
    })
}

fn pane_id(data: &Map<String, Value>) -> Option<String> {
    data.get("pane_id")
        .and_then(Value::as_str)
        .or_else(|| {
            data.get("pane")
                .and_then(Value::as_object)
                .and_then(|pane| pane.get("pane_id"))
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_uses_dotted_types_in_subscription_objects() {
        let value = subscription_params();
        assert_eq!(value["subscriptions"].as_array().unwrap().len(), 5);
        assert_eq!(value["subscriptions"][0]["type"], "pane.created");
    }

    #[test]
    fn event_accepts_snake_case_envelope_from_dotted_subscription() {
        let event = parse_event(&json!({
            "event": "pane_agent_detected",
            "data": {"type": "pane_agent_detected", "pane_id": "w1:p1", "released": false}
        }))
        .unwrap();
        assert_eq!(event.kind, "pane_agent_detected");
        assert_eq!(event.pane_id.as_deref(), Some("w1:p1"));
    }

    #[test]
    fn metadata_uses_the_selected_backend_agent_label() {
        let value = metadata_params(
            "claude",
            "w1:p1",
            Some("herdr:claude"),
            8,
            10_000,
            Some("name"),
            Some("activity"),
        );
        assert_eq!(
            value,
            json!({
                "pane_id": "w1:p1",
                "source": METADATA_SOURCE,
                "agent": "claude",
                "applies_to_source": "herdr:claude",
                "tokens": {
                    (SESSION_NAME_TOKEN): "name",
                    (LAST_MESSAGE_TOKEN): "activity"
                },
                "seq": 8,
                "ttl_ms": 10_000
            })
        );
    }

    #[test]
    fn metadata_contains_only_owned_fields_and_nullable_tokens() {
        let value = metadata_params("pi", "w1:p1", Some("native"), 7, 10_000, Some("name"), None);
        assert_eq!(value["source"], METADATA_SOURCE);
        assert_eq!(value["agent"], "pi");
        assert_eq!(value["tokens"][SESSION_NAME_TOKEN], "name");
        assert!(value["tokens"][LAST_MESSAGE_TOKEN].is_null());
        assert!(value.get("title").is_none());
        assert!(value.get("display_agent").is_none());
        assert!(value.get("state_labels").is_none());
        assert_eq!(value.as_object().unwrap().len(), 7);
    }

    #[test]
    fn process_info_flattens_only_observable_argv() {
        let process: ProcessInfo = serde_json::from_value(json!({
            "pane_id": "w1:p1",
            "foreground_processes": [
                {"pid": 1, "name": "node", "argv": null, "argv0": "pi", "cmdline": "pi --no-session"},
                {"pid": 2, "name": "bash", "argv": ["safehouse", "--", "pi", "--no-session"], "argv0": "bash", "cmdline": null}
            ],
            "future_field": true
        }))
        .unwrap();
        assert_eq!(
            process.args(),
            vec![
                "pi",
                "pi --no-session",
                "safehouse",
                "--",
                "pi",
                "--no-session",
                "bash"
            ]
        );
    }
}
