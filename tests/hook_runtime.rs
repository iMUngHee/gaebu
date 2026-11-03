use gaebu::model::{BindingState, State, Tool, now_ms};
use gaebu::pager::{HostStatus, Pager, WhoAmI};
use gaebu::store::{EventKind, Store};
use gaebu::{Result, hook, identity};
use serde_json::{Value, json};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    store: Store,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gaebu-hook-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Self {
            store: Store::open(&path).unwrap(),
            path,
        }
    }
    fn seed(&self, tool: Tool) {
        let session = if tool == Tool::Claude {
            "fixture-claude"
        } else {
            "fixture-codex"
        };
        let mut value = serde_json::to_value(State::default()).unwrap();
        value["teams"]["t"] = json!({"id":"t","name":"fixture-team","project":"fixture","commander":"m","root_bu":"b","status":"active","created_at":1,"cancelled_at":null});
        value["bus"]["b"] = json!({"id":"b","team":"t","parent":null,"name":"fixture-bu","head":"m","status":"open","created_at":1});
        value["members"]["m"] = json!({"id":"m","team":"t","bu":"b","superior":null,"title":"fixture-title","status":"active","binding":{"state":"bound","alias":"bepo","tool":tool,"session_id":session,"designated_at":1,"bound_at":1,"vacated_at":null,"vacated_reason":null},"created_at":1});
        let state = serde_json::from_value(value).unwrap();
        self.store
            .transact(std::time::Duration::from_secs(1), |s| {
                *s = state;
                Ok(())
            })
            .unwrap();
    }
    fn run(&self, tool: Tool, input: &[u8], pager: &dyn Pager) -> Vec<u8> {
        let mut out = Vec::new();
        hook::run(tool, &mut Cursor::new(input), &mut out, &self.store, pager);
        out
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct FakePager(HostStatus);
impl Pager for FakePager {
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
        Ok(WhoAmI {
            session_id: session.unwrap_or("fixture-codex").into(),
            via: "native".into(),
            alias: Some("bepo".into()),
            tool: Some("codex".into()),
        })
    }
    fn host_status(&self, _: &str) -> Result<HostStatus> {
        Ok(self.0)
    }
    fn send(&self, _: &str, _: &str) -> Result<()> {
        panic!("hook must not send notices")
    }
}

fn context(bytes: &[u8]) -> String {
    let value: Value = serde_json::from_slice(bytes).unwrap();
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn hook_session_start_claude_full_card() {
    let f = Fixture::new();
    f.seed(Tool::Claude);
    let out = f.run(
        Tool::Claude,
        include_bytes!("fixtures/hooks/synthetic-claude-start.json"),
        &FakePager(HostStatus::Gone),
    );
    let text = context(&out);
    assert!(text.contains("fixture-team"));
    assert!(text.contains("fixture-title"));
    assert!(text.contains("rev"));
    assert!(text.lines().count() > 1 && text.lines().count() <= 12);
    assert_eq!(
        f.store.read().unwrap().members["m"].binding.state,
        BindingState::Bound,
        "hook must not probe gone hosts"
    );
}

#[test]
fn hook_prompt_unchanged_rev_one_line() {
    let f = Fixture::new();
    f.seed(Tool::Claude);
    let pager = FakePager(HostStatus::Live);
    f.run(
        Tool::Claude,
        include_bytes!("fixtures/hooks/synthetic-claude-start.json"),
        &pager,
    );
    let text = context(&f.run(
        Tool::Claude,
        include_bytes!("fixtures/hooks/synthetic-claude-prompt.json"),
        &pager,
    ));
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains("변경 없음"));
    let events = f.store.session("fixture-claude").events().unwrap();
    assert_eq!(events.last().unwrap().kind, EventKind::UserPrompt);
    assert_eq!(
        events.last().unwrap().text.as_deref(),
        Some("fixture user request")
    );
}

#[test]
fn hook_codex_every_prompt_full_card() {
    let f = Fixture::new();
    f.seed(Tool::Codex);
    for _ in 0..2 {
        let text = context(&f.run(
            Tool::Codex,
            include_bytes!("fixtures/hooks/synthetic-codex-poke.json"),
            &FakePager(HostStatus::Live),
        ));
        assert!(text.lines().count() > 1);
    }
    let events = f.store.session("fixture-codex").events().unwrap();
    assert_eq!(events.last().unwrap().kind, EventKind::ExcludedPrompt);
    assert!(events.last().unwrap().sha256.is_some());
    assert!(
        events
            .last()
            .unwrap()
            .text
            .as_ref()
            .unwrap()
            .contains("[pager-poke]")
    );
}

#[test]
fn hook_non_member_empty() {
    let f = Fixture::new();
    assert!(
        f.run(
            Tool::Claude,
            include_bytes!("fixtures/hooks/synthetic-claude-start.json"),
            &FakePager(HostStatus::Gone)
        )
        .is_empty()
    );
    assert_eq!(
        f.store.session("fixture-claude").sighting().unwrap().tool,
        Some(Tool::Claude)
    );
}

#[test]
fn hook_never_fails() {
    let f = Fixture::new();
    for raw in [
        b"not json".as_slice(),
        b"{}",
        b"null",
        br#"{"session_id":"s","hook_event_name":"Unknown"}"#,
    ] {
        assert!(
            f.run(Tool::Claude, raw, &FakePager(HostStatus::Unknown))
                .is_empty()
        );
    }
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_gaebu"))
        .args(["hook", "claude"])
        .env("GAEBU_HOME", &f.path)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(result.stdout.is_empty());
}

#[test]
fn hook_open_stdin_does_not_exceed_budget() {
    let f = Fixture::new();
    let started = std::time::Instant::now();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_gaebu"))
        .args(["hook", "codex"])
        .env("GAEBU_HOME", &f.path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pipe = child.stdin.take().unwrap();
    assert!(child.wait().unwrap().success());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    drop(pipe);
}

#[test]
fn hook_stop_is_silent_and_records_turn_end() {
    let f = Fixture::new();
    f.seed(Tool::Claude);
    assert!(
        f.run(
            Tool::Claude,
            br#"{"session_id":"fixture-claude","hook_event_name":"Stop"}"#,
            &FakePager(HostStatus::Live)
        )
        .is_empty()
    );
    assert_eq!(
        f.store
            .session("fixture-claude")
            .events()
            .unwrap()
            .last()
            .unwrap()
            .kind,
        EventKind::Stop
    );
}

#[test]
fn hook_changed_card_and_compaction_restore_full_context() {
    let f = Fixture::new();
    f.seed(Tool::Claude);
    let pager = FakePager(HostStatus::Live);
    f.run(
        Tool::Claude,
        include_bytes!("fixtures/hooks/synthetic-claude-start.json"),
        &pager,
    );
    f.store
        .transact(std::time::Duration::from_secs(1), |st| {
            st.members.get_mut("m").unwrap().title = "changed-title".into();
            Ok(())
        })
        .unwrap();
    let changed = context(&f.run(
        Tool::Claude,
        include_bytes!("fixtures/hooks/synthetic-claude-prompt.json"),
        &pager,
    ));
    assert!(changed.contains("changed-title") && changed.lines().count() > 1);
    assert_eq!(
        context(&f.run(
            Tool::Claude,
            include_bytes!("fixtures/hooks/synthetic-claude-prompt.json"),
            &pager
        ))
        .lines()
        .count(),
        1
    );
    let compact = context(&f.run(
        Tool::Claude,
        br#"{"session_id":"fixture-claude","hook_event_name":"SessionStart","source":"compact"}"#,
        &pager,
    ));
    assert!(compact.contains("changed-title") && compact.lines().count() > 1);
}

#[test]
fn hook_mod_heartbeat_shortens_card_then_expires() {
    let f = Fixture::new();
    f.seed(Tool::Claude);
    let pager = FakePager(HostStatus::Live);
    f.store
        .session("fixture-claude")
        .update_sighting(|s| s.mod_heartbeat = Some(now_ms()))
        .unwrap();
    assert_eq!(
        context(&f.run(
            Tool::Claude,
            include_bytes!("fixtures/hooks/synthetic-claude-start.json"),
            &pager
        ))
        .lines()
        .count(),
        1
    );
    f.store
        .session("fixture-claude")
        .update_sighting(|s| s.mod_heartbeat = Some(now_ms() - 3_600_001))
        .unwrap();
    assert!(
        context(&f.run(
            Tool::Claude,
            include_bytes!("fixtures/hooks/synthetic-claude-prompt.json"),
            &pager
        ))
        .lines()
        .count()
            > 1
    );
}

#[test]
fn hook_capture_flag_saves_raw_payload_and_does_not_notify() {
    let f = Fixture::new();
    std::fs::write(f.path.join("capture.on"), b"").unwrap();
    let raw = include_bytes!("fixtures/hooks/synthetic-claude-start.json");
    f.run(Tool::Claude, raw, &FakePager(HostStatus::Unknown));
    let paths: Vec<_> = std::fs::read_dir(f.store.capture_dir()).unwrap().collect();
    assert_eq!(paths.len(), 1);
    assert_eq!(
        std::fs::read(paths[0].as_ref().unwrap().path()).unwrap(),
        raw
    );
}

#[test]
fn hook_store_open_error_still_exits_zero() {
    let f = Fixture::new();
    let file = f.path.join("file");
    std::fs::write(&file, b"not a directory").unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_gaebu"))
        .args(["hook", "claude"])
        .env("GAEBU_HOME", file.join("child"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!result.stderr.is_empty());
}

#[test]
fn hook_io_errors_do_not_panic() {
    struct BrokenIo;
    impl std::io::Read for BrokenIo {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("broken input"))
        }
    }
    impl std::io::Write for BrokenIo {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("broken output"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("broken flush"))
        }
    }
    let f = Fixture::new();
    f.seed(Tool::Claude);
    let mut out = Vec::new();
    hook::run(
        Tool::Claude,
        &mut BrokenIo,
        &mut out,
        &f.store,
        &FakePager(HostStatus::Unknown),
    );
    assert!(out.is_empty());
    hook::run(
        Tool::Claude,
        &mut Cursor::new(include_bytes!("fixtures/hooks/synthetic-claude-start.json")),
        &mut BrokenIo,
        &f.store,
        &FakePager(HostStatus::Unknown),
    );
    assert!(
        f.store
            .session("fixture-claude")
            .sighting()
            .unwrap()
            .last_card_hash
            .is_none()
    );
}

#[test]
fn hook_failed_prompt_or_stop_marks_old_evidence_unusable() {
    for event in ["UserPromptSubmit", "Stop"] {
        let f = Fixture::new();
        f.seed(Tool::Claude);
        let session = f.store.session("fixture-claude");
        session
            .append(
                1,
                EventKind::UserPrompt,
                Some("old-hash".into()),
                Some("old appointment".into()),
                None,
            )
            .unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(session.dir().join("lock"))
            .unwrap();
        lock.lock().unwrap();
        let input = json!({"session_id":"fixture-claude","hook_event_name":event,"prompt":"new appointment"}).to_string();
        f.run(
            Tool::Claude,
            input.as_bytes(),
            &FakePager(HostStatus::Unknown),
        );
        lock.unlock().unwrap();
        assert!(session.unrecorded_after(1).unwrap());
        assert_eq!(session.events().unwrap().len(), 1);
    }
}

#[test]
fn mcp_codex_requires_thread_meta() {
    let f = Fixture::new();
    assert_eq!(
        identity::for_mcp(&FakePager(HostStatus::Live), &f.store, None)
            .unwrap_err()
            .reason,
        gaebu::Reason::UnresolvedCaller
    );
    assert!(identity::for_mcp(&FakePager(HostStatus::Live), &f.store, Some("unseen")).is_err());
    f.store
        .session("fixture-codex")
        .update_sighting(|s| {
            s.tool = Some(Tool::Codex);
            s.last_seen = Some(now_ms());
        })
        .unwrap();
    let caller = identity::for_mcp(
        &FakePager(HostStatus::Live),
        &f.store,
        Some("fixture-codex"),
    )
    .unwrap();
    assert_eq!(caller.source, identity::Source::McpThreadMeta);
    f.store
        .session("fixture-codex")
        .update_sighting(|s| s.last_seen = Some(now_ms() - 7_200_001))
        .unwrap();
    assert!(
        identity::for_mcp(
            &FakePager(HostStatus::Live),
            &f.store,
            Some("fixture-codex")
        )
        .is_err()
    );
}

#[test]
fn unknown_host_stays_quiet() {
    let f = Fixture::new();
    f.seed(Tool::Codex);
    assert!(
        identity::refresh_bindings(&f.store, &FakePager(HostStatus::Unknown))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store.read().unwrap().members["m"].binding.state,
        BindingState::Bound
    );
}

#[test]
fn vacant_when_pager_reports_gone() {
    let f = Fixture::new();
    f.seed(Tool::Codex);
    assert_eq!(
        identity::refresh_bindings(&f.store, &FakePager(HostStatus::Gone)).unwrap(),
        vec!["m"]
    );
    assert_eq!(
        f.store.read().unwrap().members["m"].binding.state,
        BindingState::Vacant
    );
}

#[test]
fn hook_pending_binding_confirms_on_target_hook() {
    let f = Fixture::new();
    f.seed(Tool::Codex);
    f.store
        .transact(std::time::Duration::from_secs(1), |st| {
            let b = &mut st.members.get_mut("m").unwrap().binding;
            b.state = BindingState::Pending;
            b.session_id = None;
            b.tool = None;
            b.bound_at = None;
            Ok(())
        })
        .unwrap();
    let text = context(&f.run(
        Tool::Codex,
        br#"{"session_id":"fixture-codex","hook_event_name":"SessionStart"}"#,
        &FakePager(HostStatus::Live),
    ));
    assert!(text.contains("fixture-title"));
    let binding = f.store.read().unwrap().members["m"].binding.clone();
    assert_eq!(binding.state, BindingState::Bound);
    assert_eq!(binding.session_id.as_deref(), Some("fixture-codex"));
    assert_eq!(binding.tool, Some(Tool::Codex));
    assert!(binding.bound_at.is_some());
}

#[test]
fn hook_non_matching_alias_cannot_confirm_pending_binding() {
    let f = Fixture::new();
    f.seed(Tool::Codex);
    f.store
        .transact(std::time::Duration::from_secs(1), |st| {
            let b = &mut st.members.get_mut("m").unwrap().binding;
            b.state = BindingState::Pending;
            b.session_id = None;
            b.alias = "someone-else".into();
            Ok(())
        })
        .unwrap();
    assert!(
        f.run(
            Tool::Codex,
            br#"{"session_id":"fixture-codex","hook_event_name":"SessionStart"}"#,
            &FakePager(HostStatus::Live)
        )
        .is_empty()
    );
    assert_eq!(
        f.store.read().unwrap().members["m"].binding.state,
        BindingState::Pending
    );
}

#[test]
fn live_host_alias_movement_vacates_old_binding() {
    struct Moved;
    impl Pager for Moved {
        fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
            Ok(WhoAmI {
                session_id: session.unwrap().into(),
                via: "flag".into(),
                alias: Some("new-alias".into()),
                tool: None,
            })
        }
        fn host_status(&self, _: &str) -> Result<HostStatus> {
            Ok(HostStatus::Live)
        }
        fn send(&self, _: &str, _: &str) -> Result<()> {
            panic!("observation must not send")
        }
    }
    let f = Fixture::new();
    f.seed(Tool::Codex);
    assert_eq!(
        identity::refresh_bindings(&f.store, &Moved).unwrap(),
        vec!["m"]
    );
    let st = f.store.read().unwrap();
    assert_eq!(st.members["m"].binding.state, BindingState::Vacant);
    assert_eq!(
        st.members["m"].binding.vacated_reason.as_deref(),
        Some("alias_moved")
    );
}
