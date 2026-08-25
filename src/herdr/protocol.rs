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
    "pane.focused",
    "pane.moved",
    "tab.created",
    "tab.closed",
    "tab.renamed",
    "tab.moved",
    "layout.updated",
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
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub tab_id: Option<String>,
    pub agent: Option<String>,
    pub agent_status: String,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TabLayout {
    pub workspace_id: String,
    pub tab_id: String,
    pub focused_pane_id: String,
    #[serde(default)]
    pub panes: Vec<LayoutPane>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct LayoutPane {
    pub pane_id: String,
    pub rect: PaneRect,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub struct PaneRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct SnapshotPane {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PaneLabelInfo {
    pub pane_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct SessionSnapshot {
    pub tabs: Vec<TabInfo>,
    pub layouts: Vec<TabLayout>,
    pub panes: Vec<SnapshotPane>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HerdrEvent {
    pub kind: String,
    pub pane_id: Option<String>,
    pub tab_id: Option<String>,
    pub workspace_id: Option<String>,
    pub label: Option<String>,
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

pub fn tab_rename_params(tab_id: &str, label: &str) -> Value {
    json!({"tab_id": tab_id, "label": label})
}

pub fn pane_rename_params(pane_id: &str, label: Option<&str>) -> Value {
    let mut params = Map::new();
    params.insert("pane_id".into(), Value::String(pane_id.to_owned()));
    if let Some(label) = label {
        params.insert("label".into(), Value::String(label.to_owned()));
    }
    Value::Object(params)
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

pub fn parse_session_snapshot(result: Value) -> Result<SessionSnapshot, serde_json::Error> {
    let value = result.get("snapshot").cloned().unwrap_or(result);
    serde_json::from_value(value)
}

pub fn parse_tab_info(result: Value) -> Result<TabInfo, serde_json::Error> {
    let value = result.get("tab").cloned().unwrap_or(result);
    serde_json::from_value(value)
}

pub fn parse_pane_label_info(result: Value) -> Result<PaneLabelInfo, serde_json::Error> {
    let value = result.get("pane").cloned().unwrap_or(result);
    serde_json::from_value(value)
}

pub fn parse_event(value: &Value) -> Option<HerdrEvent> {
    let event = value.get("event")?.as_str()?.to_owned();
    let data = value.get("data")?.as_object()?;
    let data_kind = data.get("type").and_then(Value::as_str);
    let expected = event.replace('.', "_");
    if data_kind != Some(expected.as_str()) && data_kind != Some(event.as_str()) {
        return None;
    }
    Some(HerdrEvent {
        kind: event,
        pane_id: event_field(data, "pane_id"),
        tab_id: event_field(data, "tab_id"),
        workspace_id: event_field(data, "workspace_id"),
        label: event_field(data, "label"),
    })
}

fn event_field(data: &Map<String, Value>, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .or_else(|| {
            ["pane", "tab", "layout"]
                .iter()
                .find_map(|object| data.get(*object)?.as_object()?.get(key)?.as_str())
        })
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_uses_dotted_types_in_subscription_objects() {
        let value = subscription_params();
        let kinds: Vec<_> = value["subscriptions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "pane.created",
                "pane.updated",
                "pane.closed",
                "pane.exited",
                "pane.agent_detected",
                "pane.focused",
                "pane.moved",
                "tab.created",
                "tab.closed",
                "tab.renamed",
                "tab.moved",
                "layout.updated",
            ]
        );
    }

    #[test]
    fn event_accepts_snake_case_envelope_from_dotted_subscription() {
        let event = parse_event(&json!({
            "event": "pane_agent_detected",
            "data": {"type": "pane_agent_detected", "pane_id": "w1:p1", "workspace_id": "w1", "released": false}
        }))
        .unwrap();
        assert_eq!(event.kind, "pane_agent_detected");
        assert_eq!(event.pane_id.as_deref(), Some("w1:p1"));
        assert_eq!(event.workspace_id.as_deref(), Some("w1"));

        let renamed = parse_event(&json!({
            "event": "tab_renamed",
            "data": {"type": "tab_renamed", "tab_id": "w1:t1", "workspace_id": "w1", "label": "manual"}
        }))
        .unwrap();
        assert_eq!(renamed.tab_id.as_deref(), Some("w1:t1"));
        assert_eq!(renamed.label.as_deref(), Some("manual"));

        let moved = parse_event(&json!({
            "event": "pane_moved",
            "data": {"type": "pane_moved", "pane": {"pane_id": "w2:p2", "tab_id": "w2:t1", "workspace_id": "w2"}}
        }))
        .unwrap();
        assert_eq!(moved.pane_id.as_deref(), Some("w2:p2"));
        assert_eq!(moved.tab_id.as_deref(), Some("w2:t1"));
        assert_eq!(moved.workspace_id.as_deref(), Some("w2"));
    }

    #[test]
    fn parses_minimum_session_snapshot_and_tab_rename_contracts() {
        let snapshot = parse_session_snapshot(json!({
            "type": "session_snapshot",
            "snapshot": {
                "version": "0.8.0",
                "protocol": 19,
                "tabs": [
                    {"tab_id":"w1:t1","workspace_id":"w1","number":7,"label":"one","focused":true,"pane_count":2,"agent_status":"working"},
                    {"tab_id":"w1:t2","workspace_id":"w1","number":9,"label":"two","focused":false,"pane_count":1,"agent_status":"idle"}
                ],
                "layouts": [
                    {
                        "workspace_id":"w1","tab_id":"w1:t1","focused_pane_id":"w1:p2",
                        "panes":[
                            {"pane_id":"w1:p2","focused":true,"rect":{"x":40,"y":1,"width":40,"height":20}},
                            {"pane_id":"w1:p1","focused":false,"rect":{"x":0,"y":1,"width":40,"height":20}}
                        ],
                        "future":true
                    },
                    {"workspace_id":"w1","tab_id":"w1:t2","focused_pane_id":"w1:p3","panes":[]}
                ],
                "workspaces": [],
                "panes": [{"pane_id":"w1:p2","workspace_id":"w1","tab_id":"w1:t1","label":null}],
                "agents": [], "future": true
            }
        }))
        .unwrap();
        assert_eq!(snapshot.tabs[0].tab_id, "w1:t1");
        assert_eq!(snapshot.tabs[0].number, 7);
        assert_eq!(snapshot.layouts[0].focused_pane_id, "w1:p2");
        assert_eq!(snapshot.layouts[0].panes[0].rect.x, 40);
        assert_eq!(snapshot.layouts[0].panes[1].pane_id, "w1:p1");
        assert_eq!(snapshot.layouts[1].tab_id, "w1:t2");
        assert_eq!(snapshot.panes[0].tab_id, "w1:t1");
        assert_eq!(snapshot.panes[0].label, None);

        assert_eq!(
            tab_rename_params("w1:t1", "title"),
            json!({"tab_id":"w1:t1","label":"title"})
        );
        let tab = parse_tab_info(json!({
            "type":"tab_info",
            "tab":{"tab_id":"w1:t1","workspace_id":"w1","number":7,"label":"title","focused":true,"pane_count":2,"agent_status":"working"}
        }))
        .unwrap();
        assert_eq!(tab.label, "title");

        assert_eq!(
            pane_rename_params("w1:p1", Some("pane title")),
            json!({"pane_id":"w1:p1","label":"pane title"})
        );
        assert_eq!(
            pane_rename_params("w1:p1", None),
            json!({"pane_id":"w1:p1"})
        );
        let pane = parse_pane_label_info(json!({
            "type":"pane_info",
            "pane":{
                "pane_id":"w1:p1","terminal_id":"term-1","workspace_id":"w1",
                "tab_id":"w1:t1","focused":true,"label":"pane title",
                "agent_status":"working","revision":1
            }
        }))
        .unwrap();
        assert_eq!(pane.pane_id, "w1:p1");
        assert_eq!(pane.label.as_deref(), Some("pane title"));
        let cleared = parse_pane_label_info(json!({
            "type":"pane_info",
            "pane":{
                "pane_id":"w1:p1","terminal_id":"term-1","workspace_id":"w1",
                "tab_id":"w1:t1","focused":true,
                "agent_status":"working","revision":2
            }
        }))
        .unwrap();
        assert_eq!(cleared.label, None);

        assert!(
            parse_session_snapshot(json!({
                "type":"session_snapshot",
                "snapshot":{"version":"0.8.0","protocol":19,"tabs":[]}
            }))
            .is_err()
        );
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
