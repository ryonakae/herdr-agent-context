use herdr_agent_context::config::Config;
use herdr_agent_context::herdr::protocol::{
    AgentInfo, AgentSessionInfo, LAST_MESSAGE_TOKEN, ProcessInfo, SESSION_NAME_TOKEN,
};
use herdr_agent_context::herdr::socket::{EventPoll, SocketTransport};
use herdr_agent_context::herdr::{HerdrApi, MetadataReport};
use herdr_agent_context::runtime::Runtime;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
struct Report {
    agent: String,
    pane_id: String,
    applies_to_source: Option<String>,
    seq: u64,
    ttl_ms: u64,
    session_name: Option<String>,
    last_message: Option<String>,
}

#[derive(Debug)]
enum FakeError {
    Transient,
}

struct FakeApi {
    agents: Vec<AgentInfo>,
    process_args: HashMap<String, Vec<String>>,
    reports: Vec<Report>,
    fail_next_clear: bool,
}

impl HerdrApi for FakeApi {
    type Error = FakeError;

    fn list_agents(&mut self) -> Result<Vec<AgentInfo>, Self::Error> {
        Ok(self.agents.clone())
    }

    fn process_info(&mut self, pane_id: &str) -> Result<ProcessInfo, Self::Error> {
        Ok(ProcessInfo {
            pane_id: pane_id.to_owned(),
            foreground_processes: vec![herdr_agent_context::herdr::protocol::Process {
                pid: 1,
                name: "pi".into(),
                argv: Some(self.process_args.get(pane_id).cloned().unwrap_or_default()),
                argv0: Some("pi".into()),
                cmdline: None,
            }],
        })
    }

    fn report_metadata(&mut self, report: MetadataReport<'_>) -> Result<(), Self::Error> {
        if self.fail_next_clear && report.session_name.is_none() && report.last_message.is_none() {
            self.fail_next_clear = false;
            return Err(FakeError::Transient);
        }
        self.reports.push(Report {
            agent: report.agent.into(),
            pane_id: report.pane_id.into(),
            applies_to_source: report.applies_to_source.map(ToOwned::to_owned),
            seq: report.seq,
            ttl_ms: report.ttl_ms,
            session_name: report.session_name.map(ToOwned::to_owned),
            last_message: report.last_message.map(ToOwned::to_owned),
        });
        Ok(())
    }
}

fn agent() -> AgentInfo {
    AgentInfo {
        terminal_id: "term-1".into(),
        agent: Some("pi".into()),
        agent_status: "working".into(),
        cwd: Some("/work/project".into()),
        foreground_cwd: Some("/work/project".into()),
        pane_id: "w1:p1".into(),
        revision: 1,
        state_change_seq: 1,
        agent_session: None,
    }
}

fn claude_agent(cwd: &str, pane_id: &str) -> AgentInfo {
    AgentInfo {
        terminal_id: format!("term-{pane_id}"),
        agent: Some("claude".into()),
        agent_status: "working".into(),
        cwd: Some(cwd.into()),
        foreground_cwd: Some(cwd.into()),
        pane_id: pane_id.into(),
        revision: 1,
        state_change_seq: 1,
        agent_session: None,
    }
}

fn fake_api() -> FakeApi {
    FakeApi {
        agents: vec![agent()],
        process_args: HashMap::from([("w1:p1".into(), vec!["pi".into()])]),
        reports: Vec::new(),
        fail_next_clear: false,
    }
}

fn session_text(last_entry: &str) -> String {
    format!(
        concat!(
            "{{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}}\n",
            "{{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{{\"role\":\"user\",\"content\":\"Build context\"}}}}\n",
            "{{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"Initial answer\"}}]}}}}\n",
            "{last_entry}"
        ),
        last_entry = last_entry
    )
}

fn pi_session_text(cwd: &str, task: &str, answer: &str) -> String {
    [
        json!({"type": "session", "id": "s1", "cwd": cwd}),
        json!({
            "type": "message",
            "id": "u1",
            "parentId": null,
            "message": {"role": "user", "content": task}
        }),
        json!({
            "type": "message",
            "id": "a1",
            "parentId": "u1",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": answer}]
            }
        }),
    ]
    .into_iter()
    .map(|entry| format!("{entry}\n"))
    .collect()
}

fn claude_session_text(session_id: &str, cwd: &str, title: &str, answer: &str) -> String {
    format!(
        concat!(
            "{{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
            "\"sessionId\":\"{session_id}\",\"cwd\":\"{cwd}\",\"isSidechain\":false,",
            "\"message\":{{\"role\":\"user\",\"content\":\"Claude task\"}}}}\n",
            "{{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-000000000002\",",
            "\"parentUuid\":\"00000000-0000-4000-8000-000000000001\",\"sessionId\":\"{session_id}\",",
            "\"cwd\":\"{cwd}\",\"isSidechain\":false,\"message\":{{\"role\":\"assistant\",",
            "\"content\":[{{\"type\":\"text\",\"text\":\"{answer}\"}}]}}}}\n",
            "{{\"type\":\"custom-title\",\"title\":\"{title}\",\"sessionId\":\"{session_id}\"}}\n"
        ),
        session_id = session_id,
        cwd = cwd,
        title = title,
        answer = answer
    )
}

fn claude_user_only_text(session_id: &str, cwd: &str, title: &str) -> String {
    format!(
        concat!(
            "{{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
            "\"sessionId\":\"{session_id}\",\"cwd\":\"{cwd}\",\"isSidechain\":false,",
            "\"message\":{{\"role\":\"user\",\"content\":\"Task\"}}}}\n",
            "{{\"type\":\"custom-title\",\"title\":\"{title}\",\"sessionId\":\"{session_id}\"}}\n"
        ),
        session_id = session_id,
        cwd = cwd,
        title = title
    )
}

fn runtime_for(root: &Path) -> Runtime {
    Runtime::new(
        Config {
            pi_session_dirs: vec![root.to_owned()],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    )
}

#[test]
fn claude_authoritative_id_blocks_fallback_until_exact_target_is_valid() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("claude");
    let project = root.join("-work-claude");
    fs::create_dir_all(&project).unwrap();
    let fallback_id = "10000000-0000-4000-8000-000000000001";
    fs::write(
        project.join(format!("{fallback_id}.jsonl")),
        claude_session_text(fallback_id, "/work/claude", "Fallback", "Fallback answer"),
    )
    .unwrap();
    let authoritative_id = "10000000-0000-4000-8000-000000000002";
    let mut claude = claude_agent("/work/claude", "w1:p2");
    claude.agent_session = Some(AgentSessionInfo {
        source: "herdr:claude".into(),
        agent: "claude".into(),
        kind: "id".into(),
        value: authoritative_id.into(),
    });
    let mut api = FakeApi {
        agents: vec![claude],
        process_args: HashMap::from([("w1:p2".into(), vec!["claude".into()])]),
        reports: Vec::new(),
        fail_next_clear: false,
    };
    let mut runtime = Runtime::new(
        Config {
            claude_session_dirs: vec![root],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    );

    runtime.reconcile(&mut api).unwrap();
    assert!(api.reports.is_empty());

    fs::write(
        project.join(format!("{authoritative_id}.jsonl")),
        claude_session_text(
            authoritative_id,
            "/work/claude",
            "Authoritative",
            "Exact answer",
        ),
    )
    .unwrap();
    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 1);
    assert_eq!(api.reports[0].agent, "claude");
    assert_eq!(
        api.reports[0].session_name.as_deref(),
        Some("Authoritative")
    );
    assert_eq!(
        api.reports[0].applies_to_source.as_deref(),
        Some("herdr:claude")
    );
}

#[test]
fn claude_exact_uuid_hint_binds_without_claiming_official_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("claude");
    let project = root.join("-work-claude");
    fs::create_dir_all(&project).unwrap();
    let session_id = "10000000-0000-4000-8000-000000000001";
    fs::write(
        project.join(format!("{session_id}.jsonl")),
        claude_session_text(session_id, "/work/claude", "Exact", "Answer"),
    )
    .unwrap();
    let mut api = FakeApi {
        agents: vec![claude_agent("/work/claude", "w1:p2")],
        process_args: HashMap::from([(
            "w1:p2".into(),
            vec!["claude".into(), "--session-id".into(), session_id.into()],
        )]),
        reports: Vec::new(),
        fail_next_clear: false,
    };
    let mut runtime = Runtime::new(
        Config {
            claude_session_dirs: vec![root],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    );

    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 1);
    assert_eq!(api.reports[0].agent, "claude");
    assert_eq!(api.reports[0].applies_to_source, None);
    assert_eq!(api.reports[0].session_name.as_deref(), Some("Exact"));

    api.process_args
        .insert("w1:p2".into(), vec!["claude".into(), "--print".into()]);
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 2);
    assert_eq!(api.reports[1].agent, "claude");
    assert_eq!(api.reports[1].session_name, None);
    assert_eq!(api.reports[1].last_message, None);
}

#[test]
fn incomplete_claude_tail_keeps_sticky_binding_and_does_not_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("claude");
    let project = root.join("-work-claude");
    fs::create_dir_all(&project).unwrap();
    let now = std::time::SystemTime::now();
    let older_id = "10000000-0000-4000-8000-000000000001";
    let older = project.join(format!("{older_id}.jsonl"));
    fs::write(
        &older,
        claude_session_text(older_id, "/work/claude", "Older", "Older answer"),
    )
    .unwrap();
    fs::File::options()
        .write(true)
        .open(&older)
        .unwrap()
        .set_modified(now - Duration::from_secs(20))
        .unwrap();
    let current_id = "10000000-0000-4000-8000-000000000002";
    let current = project.join(format!("{current_id}.jsonl"));
    let valid = claude_session_text(current_id, "/work/claude", "Current", "Current answer");
    fs::write(&current, &valid).unwrap();
    fs::File::options()
        .write(true)
        .open(&current)
        .unwrap()
        .set_modified(now - Duration::from_secs(10))
        .unwrap();
    let mut api = FakeApi {
        agents: vec![claude_agent("/work/claude", "w1:p2")],
        process_args: HashMap::from([("w1:p2".into(), vec!["claude".into()])]),
        reports: Vec::new(),
        fail_next_clear: false,
    };
    let mut runtime = Runtime::new(
        Config {
            claude_session_dirs: vec![root],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    );
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports[0].session_name.as_deref(), Some("Current"));

    fs::write(&current, format!("{valid}{{")).unwrap();
    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 1);
}

#[test]
fn claude_retains_activity_within_a_session_but_not_after_switching() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("claude");
    let project = root.join("-work-claude");
    fs::create_dir_all(&project).unwrap();
    let first_id = "10000000-0000-4000-8000-000000000001";
    let first = project.join(format!("{first_id}.jsonl"));
    let old_time = std::time::SystemTime::now() - Duration::from_secs(10);
    fs::write(
        &first,
        claude_session_text(first_id, "/work/claude", "First", "Old answer"),
    )
    .unwrap();
    fs::File::options()
        .write(true)
        .open(&first)
        .unwrap()
        .set_modified(old_time)
        .unwrap();
    let mut api = FakeApi {
        agents: vec![claude_agent("/work/claude", "w1:p2")],
        process_args: HashMap::from([("w1:p2".into(), vec!["claude".into()])]),
        reports: Vec::new(),
        fail_next_clear: false,
    };
    let mut runtime = Runtime::new(
        Config {
            claude_session_dirs: vec![root],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    );
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports[0].last_message.as_deref(), Some("Old answer"));

    let mut updated = claude_session_text(first_id, "/work/claude", "First", "Old answer");
    updated.push_str(concat!(
        "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000003\",",
        "\"parentUuid\":\"00000000-0000-4000-8000-000000000002\",",
        "\"sessionId\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/work/claude\",",
        "\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Next\"}}\n"
    ));
    fs::write(&first, updated).unwrap();
    fs::File::options()
        .write(true)
        .open(&first)
        .unwrap()
        .set_modified(old_time)
        .unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports[1].last_message.as_deref(), Some("Old answer"));

    let second_id = "10000000-0000-4000-8000-000000000002";
    let second = project.join(format!("{second_id}.jsonl"));
    fs::write(
        &second,
        claude_user_only_text(second_id, "/work/claude", "Second"),
    )
    .unwrap();
    fs::File::options()
        .write(true)
        .open(&second)
        .unwrap()
        .set_modified(old_time + Duration::from_secs(20))
        .unwrap();
    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports[2].session_name.as_deref(), Some("Second"));
    assert_eq!(api.reports[2].last_message, None);
}

#[test]
fn malformed_claude_pane_does_not_block_other_backends() {
    let temp = tempfile::tempdir().unwrap();
    let pi_root = temp.path().join("pi");
    let claude_root = temp.path().join("claude");
    fs::create_dir_all(&pi_root).unwrap();
    fs::write(pi_root.join("session.jsonl"), session_text("")).unwrap();
    let broken_project = claude_root.join("-work-broken");
    let healthy_project = claude_root.join("-work-healthy");
    fs::create_dir_all(&broken_project).unwrap();
    fs::create_dir_all(&healthy_project).unwrap();
    fs::write(
        broken_project.join("10000000-0000-4000-8000-000000000001.jsonl"),
        "malformed\n",
    )
    .unwrap();
    let healthy_id = "10000000-0000-4000-8000-000000000002";
    fs::write(
        healthy_project.join(format!("{healthy_id}.jsonl")),
        claude_session_text(healthy_id, "/work/healthy", "Healthy", "Answer"),
    )
    .unwrap();
    let mut api = fake_api();
    api.agents.push(claude_agent("/work/broken", "w1:p2"));
    api.agents.push(claude_agent("/work/healthy", "w1:p3"));
    api.process_args
        .insert("w1:p2".into(), vec!["claude".into()]);
    api.process_args
        .insert("w1:p3".into(), vec!["claude".into()]);
    let mut runtime = Runtime::new(
        Config {
            pi_session_dirs: vec![pi_root],
            claude_session_dirs: vec![claude_root],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    );

    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 2);
    assert!(
        api.reports
            .iter()
            .any(|report| report.pane_id == "w1:p1" && report.agent == "pi")
    );
    assert!(api.reports.iter().any(|report| {
        report.pane_id == "w1:p3"
            && report.agent == "claude"
            && report.session_name.as_deref() == Some("Healthy")
    }));
    assert!(!api.reports.iter().any(|report| report.pane_id == "w1:p2"));
}

#[test]
fn runtime_reports_pi_and_claude_with_backend_specific_agent_labels() {
    let temp = tempfile::tempdir().unwrap();
    let pi_root = temp.path().join("pi");
    let claude_root = temp.path().join("claude");
    fs::create_dir_all(&pi_root).unwrap();
    let claude_project = claude_root.join("-work-claude");
    fs::create_dir_all(&claude_project).unwrap();
    fs::write(pi_root.join("session.jsonl"), session_text("")).unwrap();
    let claude_id = "10000000-0000-4000-8000-000000000001";
    fs::write(
        claude_project.join(format!("{claude_id}.jsonl")),
        claude_session_text(claude_id, "/work/claude", "Claude name", "Claude answer"),
    )
    .unwrap();
    let mut runtime = Runtime::new(
        Config {
            pi_session_dirs: vec![pi_root],
            claude_session_dirs: vec![claude_root],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    );
    let mut api = fake_api();
    let mut claude = agent();
    claude.terminal_id = "term-2".into();
    claude.agent = Some("claude".into());
    claude.cwd = Some("/work/claude".into());
    claude.foreground_cwd = Some("/work/claude".into());
    claude.pane_id = "w1:p2".into();
    api.agents.push(claude);
    api.process_args
        .insert("w1:p2".into(), vec!["claude".into()]);

    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 2);
    let pi = api
        .reports
        .iter()
        .find(|report| report.agent == "pi")
        .unwrap();
    assert_eq!(pi.pane_id, "w1:p1");
    assert_eq!(pi.session_name.as_deref(), Some("Build context"));
    assert_eq!(pi.last_message.as_deref(), Some("Initial answer"));
    let claude = api
        .reports
        .iter()
        .find(|report| report.agent == "claude")
        .unwrap();
    assert_eq!(claude.pane_id, "w1:p2");
    assert_eq!(claude.session_name.as_deref(), Some("Claude name"));
    assert_eq!(claude.last_message.as_deref(), Some("Claude answer"));
}

#[test]
fn runtime_reports_unquoted_eighty_scalar_boundaries_for_pi_and_claude() {
    let temp = tempfile::tempdir().unwrap();
    let pi_root = temp.path().join("pi");
    let claude_root = temp.path().join("claude");
    let claude_project = claude_root.join("-work-claude");
    fs::create_dir_all(&pi_root).unwrap();
    fs::create_dir_all(&claude_project).unwrap();
    let pi_session = pi_root.join("session.jsonl");
    let claude_id = "10000000-0000-4000-8000-000000000001";
    let claude_session = claude_project.join(format!("{claude_id}.jsonl"));
    let exact_name = "名".repeat(80);
    let exact_message = "答".repeat(80);
    fs::write(
        &pi_session,
        pi_session_text("/work/project", &exact_name, &exact_message),
    )
    .unwrap();
    fs::write(
        &claude_session,
        claude_session_text(claude_id, "/work/claude", &exact_name, &exact_message),
    )
    .unwrap();
    let mut runtime = Runtime::new(
        Config {
            pi_session_dirs: vec![pi_root],
            claude_session_dirs: vec![claude_root],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    );
    let mut api = fake_api();
    api.agents.push(claude_agent("/work/claude", "w1:p2"));
    api.process_args
        .insert("w1:p2".into(), vec!["claude".into()]);

    runtime.reconcile(&mut api).unwrap();
    for agent in ["pi", "claude"] {
        let report = api
            .reports
            .iter()
            .find(|report| report.agent == agent)
            .unwrap();
        assert_eq!(report.session_name.as_deref(), Some(exact_name.as_str()));
        assert_eq!(report.last_message.as_deref(), Some(exact_message.as_str()));
    }

    let over_name = "名".repeat(81);
    let over_message = "答".repeat(81);
    fs::write(
        &pi_session,
        pi_session_text("/work/project", &over_name, &over_message),
    )
    .unwrap();
    fs::write(
        &claude_session,
        claude_session_text(claude_id, "/work/claude", &over_name, &over_message),
    )
    .unwrap();
    runtime.reconcile(&mut api).unwrap();

    let truncated_name = format!("{}…", "名".repeat(79));
    let truncated_message = format!("{}…", "答".repeat(79));
    for agent in ["pi", "claude"] {
        let report = api
            .reports
            .iter()
            .rev()
            .find(|report| report.agent == agent)
            .unwrap();
        assert_eq!(
            report.session_name.as_deref(),
            Some(truncated_name.as_str())
        );
        assert_eq!(
            report.last_message.as_deref(),
            Some(truncated_message.as_str())
        );
    }
}

#[test]
fn runtime_refreshes_ttl_and_retains_activity_until_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session.jsonl");
    fs::write(
        &session,
        session_text(
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"a1\",\"name\":\"Named session\"}\n",
        ),
    )
    .unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();

    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);
    assert_eq!(api.reports[0].agent, "pi");
    assert_eq!(
        api.reports[0].session_name.as_deref(),
        Some("Named session")
    );
    assert_eq!(
        api.reports[0].last_message.as_deref(),
        Some("Initial answer")
    );
    assert_eq!(api.reports[0].ttl_ms, 10_000);

    fs::write(
        &session,
        session_text(
            concat!(
                "{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"a1\",\"message\":{\"role\":\"user\",\"content\":\"Next question\"}}\n",
                "{\"type\":\"session_info\",\"id\":\"n2\",\"parentId\":\"u2\",\"name\":\"Renamed\"}\n"
            ),
        ),
    )
    .unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 2);
    assert_eq!(api.reports[1].session_name.as_deref(), Some("Renamed"));
    assert_eq!(
        api.reports[1].last_message.as_deref(),
        Some("Initial answer")
    );
    assert!(api.reports[1].seq > api.reports[0].seq);

    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 3);
    assert_eq!(
        api.reports[2].last_message.as_deref(),
        Some("Initial answer")
    );
}

#[test]
fn runtime_does_not_refresh_ttl_after_parse_failure_and_recovers() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session.jsonl");
    fs::write(&session, session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);

    fs::write(&session, format!("{}{{", session_text(""))).unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);

    fs::write(
        &session,
        session_text(
            "{\"type\":\"message\",\"id\":\"a2\",\"parentId\":\"a1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Recovered answer\"}]}}\n",
        ),
    )
    .unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 2);
    assert_eq!(
        api.reports[1].last_message.as_deref(),
        Some("Recovered answer")
    );
}

#[cfg(unix)]
#[test]
fn runtime_stops_refreshing_when_cached_session_becomes_unreadable() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session.jsonl");
    fs::write(&session, session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);

    fs::set_permissions(&session, fs::Permissions::from_mode(0o000)).unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);
    fs::set_permissions(&session, fs::Permissions::from_mode(0o600)).unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 2);
}

#[test]
fn transient_clear_failure_is_retried() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("session.jsonl"), session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    runtime.reconcile(&mut api).unwrap();
    api.agents.clear();
    api.fail_next_clear = true;
    assert!(runtime.reconcile(&mut api).is_err());
    assert_eq!(api.reports.len(), 1);

    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 2);
    assert_eq!(api.reports[1].session_name, None);
}

#[test]
fn runtime_clears_metadata_for_no_session_and_non_pi_panes() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("session.jsonl"), session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    runtime.reconcile(&mut api).unwrap();

    api.process_args.insert(
        "w1:p1".into(),
        vec![
            "safehouse".into(),
            "--".into(),
            "pi".into(),
            "--no-session".into(),
        ],
    );
    runtime.reconcile(&mut api).unwrap();
    let clear = api.reports.last().unwrap();
    assert_eq!(clear.pane_id, "w1:p1");
    assert_eq!(clear.session_name, None);
    assert_eq!(clear.last_message, None);

    api.process_args.insert("w1:p1".into(), vec!["pi".into()]);
    runtime.reconcile(&mut api).unwrap();
    assert!(api.reports.last().unwrap().session_name.is_some());
    api.agents[0].agent = Some("codex".into());
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.last().unwrap().session_name, None);
}

#[test]
fn installed_pi_integration_waits_for_agent_session_before_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    let previous = sessions.join("previous.jsonl");
    fs::write(
        &previous,
        session_text(
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"a1\",\"name\":\"Previous\"}\n",
        ),
    )
    .unwrap();
    let extension_dir = temp.path().join(".pi/agent/extensions");
    fs::create_dir_all(&extension_dir).unwrap();
    fs::write(extension_dir.join("herdr-agent-state.ts"), "integration").unwrap();
    let mut runtime = Runtime::new(
        Config {
            pi_session_dirs: vec![sessions],
            ..Config::default()
        },
        temp.path().to_owned(),
        HashMap::new(),
    );
    let mut api = fake_api();

    runtime.reconcile(&mut api).unwrap();
    assert!(api.reports.is_empty());

    api.agents[0].agent_session = Some(AgentSessionInfo {
        source: "herdr:pi".into(),
        agent: "pi".into(),
        kind: "path".into(),
        value: previous.display().to_string(),
    });
    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 1);
    assert_eq!(api.reports[0].session_name.as_deref(), Some("Previous"));
    assert_eq!(
        api.reports[0].applies_to_source.as_deref(),
        Some("herdr:pi")
    );
}

#[test]
fn authoritative_path_wins_over_fallback_and_preserves_source() {
    let temp = tempfile::tempdir().unwrap();
    let authoritative = temp.path().join("authoritative.jsonl");
    let fallback = temp.path().join("fallback.jsonl");
    fs::write(
        &authoritative,
        session_text(
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"a1\",\"name\":\"Authoritative\"}\n",
        ),
    )
    .unwrap();
    fs::write(
        &fallback,
        session_text(
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"a1\",\"name\":\"Fallback\"}\n",
        ),
    )
    .unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    api.agents[0].agent_session = Some(AgentSessionInfo {
        source: "native-pi".into(),
        agent: "pi".into(),
        kind: "path".into(),
        value: authoritative.display().to_string(),
    });

    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 1);
    assert_eq!(
        api.reports[0].session_name.as_deref(),
        Some("Authoritative")
    );
    assert_eq!(
        api.reports[0].applies_to_source.as_deref(),
        Some("native-pi")
    );
}

#[test]
fn authoritative_path_change_clears_previous_metadata_before_new_file_exists() {
    let temp = tempfile::tempdir().unwrap();
    let previous = temp.path().join("previous.jsonl");
    fs::write(
        &previous,
        session_text(
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"a1\",\"name\":\"Previous\"}\n",
        ),
    )
    .unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    api.agents[0].agent_session = Some(AgentSessionInfo {
        source: "herdr:pi".into(),
        agent: "pi".into(),
        kind: "path".into(),
        value: previous.display().to_string(),
    });
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);
    assert_eq!(api.reports[0].session_name.as_deref(), Some("Previous"));

    api.agents[0].agent_session.as_mut().unwrap().value =
        temp.path().join("new.jsonl").display().to_string();
    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 2);
    assert_eq!(api.reports[1].pane_id, "w1:p1");
    assert_eq!(api.reports[1].session_name, None);
    assert_eq!(api.reports[1].last_message, None);
}

#[test]
fn authoritative_path_recovers_after_same_fingerprint_parse_failure() {
    let temp = tempfile::tempdir().unwrap();
    let previous = temp.path().join("previous.jsonl");
    fs::write(&previous, session_text("")).unwrap();
    let pending = temp.path().join("pending.jsonl");
    let valid = session_text(
        "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"a1\",\"name\":\"Recovered\"}\n",
    );
    let fixed_time = std::time::SystemTime::now() - Duration::from_secs(30);
    fs::write(&pending, "x".repeat(valid.len())).unwrap();
    fs::File::options()
        .write(true)
        .open(&pending)
        .unwrap()
        .set_modified(fixed_time)
        .unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    api.agents[0].agent_session = Some(AgentSessionInfo {
        source: "herdr:pi".into(),
        agent: "pi".into(),
        kind: "path".into(),
        value: previous.display().to_string(),
    });
    runtime.reconcile(&mut api).unwrap();

    api.agents[0].agent_session.as_mut().unwrap().value = pending.display().to_string();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 2);
    assert_eq!(api.reports[1].session_name, None);

    fs::write(&pending, valid).unwrap();
    fs::File::options()
        .write(true)
        .open(&pending)
        .unwrap()
        .set_modified(fixed_time)
        .unwrap();
    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 3);
    assert_eq!(api.reports[2].session_name.as_deref(), Some("Recovered"));
}

#[test]
fn authoritative_same_path_parse_failure_does_not_clear_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session.jsonl");
    fs::write(&session, session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    api.agents[0].agent_session = Some(AgentSessionInfo {
        source: "herdr:pi".into(),
        agent: "pi".into(),
        kind: "path".into(),
        value: session.display().to_string(),
    });
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);

    fs::write(&session, format!("{}{{", session_text(""))).unwrap();
    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 1);
}

#[test]
fn foreign_agent_path_reference_does_not_override_pi_fallback() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("fallback.jsonl"),
        session_text(
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"a1\",\"name\":\"Fallback\"}\n",
        ),
    )
    .unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    api.agents[0].agent_session = Some(AgentSessionInfo {
        source: "herdr:claude".into(),
        agent: "claude".into(),
        kind: "path".into(),
        value: temp.path().join("foreign.jsonl").display().to_string(),
    });

    runtime.reconcile(&mut api).unwrap();

    assert_eq!(api.reports.len(), 1);
    assert_eq!(api.reports[0].session_name.as_deref(), Some("Fallback"));
    assert_eq!(api.reports[0].applies_to_source, None);
}

#[test]
fn authoritative_unreadable_path_never_uses_valid_fallback() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("fallback.jsonl"), session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    api.agents[0].agent_session = Some(AgentSessionInfo {
        source: "native-pi".into(),
        agent: "pi".into(),
        kind: "path".into(),
        value: temp.path().join("missing.jsonl").display().to_string(),
    });
    runtime.reconcile(&mut api).unwrap();
    assert!(api.reports.is_empty());

    api.agents[0].agent_session = None;
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);
    assert_eq!(api.reports[0].applies_to_source, None);
}

#[test]
fn listener_restart_uses_a_fresh_monotonic_sequence_epoch() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("session.jsonl"), session_text("")).unwrap();
    let mut api = fake_api();
    runtime_for(temp.path()).reconcile(&mut api).unwrap();
    let first = api.reports.last().unwrap().seq;
    runtime_for(temp.path()).reconcile(&mut api).unwrap();
    let restarted = api.reports.last().unwrap().seq;
    assert!(restarted > first);
}

#[test]
fn listener_binary_reconnects_full_syncs_and_rejects_duplicate_owner() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("herdr.sock");
    let sessions = temp.path().join("sessions");
    let config = temp.path().join("config");
    fs::create_dir(&sessions).unwrap();
    fs::create_dir(&config).unwrap();
    fs::write(sessions.join("session.jsonl"), session_text("")).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let (report_tx, report_rx) = mpsc::channel();

    let server = thread::spawn(move || {
        for cycle in 0..2 {
            let (event_stream, _) = listener.accept().unwrap();
            let mut event_reader = BufReader::new(event_stream.try_clone().unwrap());
            let mut event_writer = event_stream;
            let mut line = String::new();
            event_reader.read_line(&mut line).unwrap();
            let subscribe: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(subscribe["method"], "events.subscribe");
            writeln!(
                event_writer,
                "{}",
                json!({
                    "event": "pane_updated",
                    "data": {"type": "pane_updated", "pane": {"pane_id": "w1:p1"}}
                })
            )
            .unwrap();
            writeln!(
                event_writer,
                "{}",
                json!({"id": subscribe["id"], "result": {"type": "subscription_started"}})
            )
            .unwrap();
            event_writer.flush().unwrap();

            for method in ["agent.list", "pane.process_info", "pane.report_metadata"] {
                let (rpc_stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(rpc_stream.try_clone().unwrap());
                let mut writer = rpc_stream;
                line.clear();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], method);
                let result = match method {
                    "agent.list" => json!({
                        "type": "agent_list",
                        "agents": [{
                            "terminal_id": "term-1", "agent": "pi", "agent_status": "working",
                            "cwd": "/work/project", "foreground_cwd": "/work/project",
                            "pane_id": "w1:p1", "revision": cycle + 1
                        }]
                    }),
                    "pane.process_info" => json!({
                        "type": "pane_process_info",
                        "process_info": {"pane_id": "w1:p1", "foreground_processes": [
                            {"pid": 1, "name": "pi", "argv": ["pi"]}
                        ]}
                    }),
                    "pane.report_metadata" => {
                        assert!(request["params"]["tokens"][SESSION_NAME_TOKEN].is_string());
                        assert!(request["params"]["tokens"][LAST_MESSAGE_TOKEN].is_string());
                        report_tx
                            .send(request["params"]["seq"].as_u64().unwrap())
                            .unwrap();
                        json!({"type": "ok"})
                    }
                    _ => unreachable!(),
                };
                writeln!(writer, "{}", json!({"id": request["id"], "result": result})).unwrap();
                writer.flush().unwrap();
            }
            drop(event_writer);
        }
    });

    let configure = |command: &mut Command| {
        command
            .arg("listen")
            .env("HERDR_ENV", "1")
            .env("HERDR_SOCKET_PATH", &socket)
            .env("HERDR_PLUGIN_CONFIG_DIR", &config)
            .env("PI_CODING_AGENT_SESSION_DIR", &sessions)
            .env("HOME", temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    };
    let binary = env!("CARGO_BIN_EXE_herdr-agent-context");
    let mut command = Command::new(binary);
    configure(&mut command);
    let mut child = command.spawn().unwrap();
    let first_seq = report_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    let mut duplicate_command = Command::new(binary);
    configure(&mut duplicate_command);
    let mut duplicate = duplicate_command.spawn().unwrap();
    let duplicate_status = (0..20)
        .find_map(|_| {
            let status = duplicate.try_wait().unwrap();
            if status.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            status
        })
        .expect("duplicate listener did not exit");
    assert!(duplicate_status.success());

    let second_seq = report_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(second_seq > first_seq);
    child.kill().unwrap();
    child.wait().unwrap();
    server.join().unwrap();
}

#[test]
fn socket_transport_subscribes_first_buffers_events_and_uses_exact_metadata_contract() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_seen = Arc::clone(&seen);

    let server = thread::spawn(move || {
        let (event_stream, _) = listener.accept().unwrap();
        let mut event_reader = BufReader::new(event_stream.try_clone().unwrap());
        let mut line = String::new();
        event_reader.read_line(&mut line).unwrap();
        let subscribe: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(subscribe["method"], "events.subscribe");
        assert_eq!(
            subscribe["params"]["subscriptions"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        server_seen.lock().unwrap().push(subscribe.clone());
        let mut event_writer = event_stream;
        writeln!(
            event_writer,
            "{}",
            json!({
                "event": "pane_created",
                "data": {"type": "pane_created", "pane": {"pane_id": "w1:p2"}}
            })
        )
        .unwrap();
        writeln!(
            event_writer,
            "{}",
            json!({"id": subscribe["id"], "result": {"type": "events_subscribed"}})
        )
        .unwrap();
        writeln!(event_writer, "not-json").unwrap();
        event_writer.flush().unwrap();

        for _ in 0..3 {
            let (rpc_stream, _) = listener.accept().unwrap();
            let mut rpc_reader = BufReader::new(rpc_stream.try_clone().unwrap());
            let mut rpc_writer = rpc_stream;
            line.clear();
            rpc_reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            server_seen.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "agent.list" => json!({
                    "type": "agent_list",
                    "agents": [{
                        "terminal_id": "term-1", "agent": "pi", "agent_status": "working",
                        "cwd": "/work", "foreground_cwd": "/work", "pane_id": "w1:p1",
                        "revision": 1, "state_change_seq": 2, "future": true
                    }]
                }),
                "pane.process_info" => json!({
                    "type": "pane_process_info",
                    "process_info": {"pane_id": "w1:p1", "foreground_processes": [
                        {"pid": 1, "name": "bash", "argv": null, "argv0": "pi", "cmdline": "pi --no-session"}
                    ]}
                }),
                "pane.report_metadata" => json!({"type": "pane_metadata_reported"}),
                method => panic!("unexpected method {method}"),
            };
            writeln!(
                rpc_writer,
                "{}",
                json!({"id": request["id"], "result": result})
            )
            .unwrap();
            rpc_writer.flush().unwrap();
        }
    });

    let mut transport = SocketTransport::connect(&socket).unwrap();
    match transport.poll_event(Duration::from_secs(1)) {
        EventPoll::Event(event) => assert_eq!(event.pane_id.as_deref(), Some("w1:p2")),
        _ => panic!("expected buffered event"),
    }
    assert!(matches!(
        transport.poll_event(Duration::from_secs(1)),
        EventPoll::Malformed
    ));
    let agents = transport.list_agents().unwrap();
    assert_eq!(agents[0].agent.as_deref(), Some("pi"));
    assert_eq!(
        transport.process_info("w1:p1").unwrap().args(),
        vec!["pi", "pi --no-session"]
    );
    transport
        .report_metadata(MetadataReport {
            agent: "claude",
            pane_id: "w1:p1",
            applies_to_source: Some("native"),
            seq: 9,
            ttl_ms: 10_000,
            session_name: Some("name"),
            last_message: None,
        })
        .unwrap();
    drop(transport);
    server.join().unwrap();

    let requests = seen.lock().unwrap();
    let report = requests
        .iter()
        .find(|request| request["method"] == "pane.report_metadata")
        .unwrap();
    let params = &report["params"];
    assert_eq!(params["source"], "ryonakae.agent-context");
    assert_eq!(params["agent"], "claude");
    assert_eq!(params["applies_to_source"], "native");
    assert_eq!(params["tokens"][SESSION_NAME_TOKEN], "name");
    assert!(params["tokens"][LAST_MESSAGE_TOKEN].is_null());
    assert_eq!(params["seq"], 9);
    assert_eq!(params["ttl_ms"], 10_000);
    assert!(params.get("title").is_none());
}
