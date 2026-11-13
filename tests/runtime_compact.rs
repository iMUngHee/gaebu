mod common;

use gaebu::{
    Reason, authz, compact,
    model::*,
    ops::{self, DutySpec, SeedSpec},
    pager::{HostStatus, Pager, WhoAmI},
    store::{CLI_DEADLINE, Store},
};
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const DAY: u64 = 24 * 60 * 60 * 1000;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    store: Store,
    _cleanup: common::RemoveOnDrop,
    root: Actor,
    worker: Actor,
    team: String,
}

fn seed(st: &mut State, label: &str) -> (Actor, Actor, String) {
    let root_session = format!("{label}-root");
    let worker_session = format!("{label}-worker");
    ops::dev_seed(
        st,
        10,
        &SeedSpec {
            team: label.into(),
            project: label.into(),
            commander_alias: root_session.clone(),
            commander_session: Some(root_session.clone()),
            commander_tool: Some(Tool::Codex),
            members: vec![(
                worker_session.clone(),
                Some(worker_session.clone()),
                Some(Tool::Codex),
            )],
        },
    )
    .unwrap();
    let root = Actor {
        member: authz::member_for_session(st, &root_session)
            .unwrap()
            .id
            .clone(),
        session_id: root_session,
    };
    let worker = Actor {
        member: authz::member_for_session(st, &worker_session)
            .unwrap()
            .id
            .clone(),
        session_id: worker_session,
    };
    let team = st.members[&root.member].team.clone();
    (root, worker, team)
}

impl Fixture {
    fn new() -> Self {
        Self::with_limit(64 * 1024 * 1024)
    }
    fn with_limit(limit: u64) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-compact-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::open_with_archive_limit(&dir, limit).unwrap();
        let (root, worker, team) = store
            .transact(CLI_DEADLINE, |st| {
                let actors = seed(st, "one");
                st.records.clear();
                st.outbox.clear();
                Ok(actors)
            })
            .unwrap();
        Self {
            store,
            _cleanup: common::RemoveOnDrop(dir),
            root,
            worker,
            team,
        }
    }
    fn duty(&self) -> String {
        self.store
            .transact(CLI_DEADLINE, |st| {
                let id = terminal(st, &self.root, &self.worker);
                st.records.clear();
                Ok(id)
            })
            .unwrap()
    }
    fn path(&self, collection: &str) -> PathBuf {
        self.store
            .home()
            .join("archive")
            .join(&self.team)
            .join(format!("{collection}.jsonl"))
    }
    fn segment(&self, name: &str, n: u64) -> PathBuf {
        if n == 1 {
            self.path(name)
        } else {
            self.path(name).with_extension(format!("{n}.jsonl"))
        }
    }
    fn stream(&self, name: &str) -> ArchiveStream {
        let state = self.store.read_current().unwrap();
        let archives = &state.archives[&self.team];
        match name {
            "duties" => archives.duties.clone(),
            "escalations" => archives.escalations.clone(),
            "records" => archives.records.clone(),
            _ => panic!("unknown stream"),
        }
    }
    fn archive_duties(&self, count: usize) -> Vec<String> {
        (0..count)
            .map(|_| {
                let id = self.duty();
                compact::run(&self.store, &self.team, None).unwrap();
                id
            })
            .collect()
    }
    fn reopen(&mut self, limit: u64) {
        self.store = Store::open_with_archive_limit(self.store.home(), limit).unwrap();
    }
    fn archive_batches(&self, count: usize) -> Vec<[String; 3]> {
        (0..count)
            .map(|_| {
                let ids = self.batch();
                compact::run(&self.store, &self.team, None).unwrap();
                ids
            })
            .collect()
    }
    fn batch(&self) -> [String; 3] {
        self.store
            .transact(CLI_DEADLINE, |st| {
                let duty = terminal(st, &self.root, &self.worker);
                st.duties.get_mut(&duty).unwrap().note = Some("d".repeat(1024));
                let old = now_ms() - 2 * DAY;
                let escalation = ops::escalate(st, old, &self.root, None, &"e".repeat(1024))
                    .unwrap()
                    .ids[0]
                    .clone();
                ops::escalation_withdraw(st, old, &self.root, &escalation, None).unwrap();
                st.records.clear();
                st.outbox.clear();
                let record = st.mint("r");
                st.records.push(Record {
                    id: record.clone(),
                    team: self.team.clone(),
                    kind: RecordKind::Note,
                    author: self.root.member.clone(),
                    refs: vec![],
                    text: "r".repeat(1024),
                    at: old,
                });
                Ok([duty, escalation, record])
            })
            .unwrap()
    }
    fn heal(&self) -> gaebu::Result<serde_json::Value> {
        gaebu::schedule::compact(&self.store, &caller(&self.root), true)
    }
    fn mcp(&self, args: serde_json::Value) -> serde_json::Value {
        self.store
            .session(&self.root.session_id)
            .update_sighting(|s| {
                s.tool = Some(Tool::Codex);
                s.last_seen = Some(now_ms());
            })
            .unwrap();
        let request = serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
            "name":"compact", "arguments":args, "_meta":{"threadId":self.root.session_id}}});
        let mut output = Vec::new();
        gaebu::mcp::serve(
            &mut std::io::Cursor::new(format!("{request}\n")),
            &mut output,
            &self.store,
            &QuietPager,
        )
        .unwrap();
        serde_json::from_slice(&output).unwrap()
    }
    fn get(&self, id: &str) -> gaebu::Result<Duty> {
        compact::full_duty(
            &self.store,
            &self.store.read_view().unwrap(),
            &self.worker.session_id,
            id,
        )
    }
}

fn terminal(st: &mut State, root: &Actor, worker: &Actor) -> String {
    let old = now_ms() - 2 * DAY;
    let id = ops::duty_issue(
        st,
        old,
        root,
        &worker.member,
        DutySpec {
            intent: "keep the full contract".into(),
            outcome: "delivered".into(),
            scope: vec!["src/**".into()],
            done_criteria: "tested".into(),
            ..Default::default()
        },
    )
    .unwrap()
    .ids[0]
        .clone();
    ops::duty_respond(st, old, worker, &id, None, true, None).unwrap();
    ops::duty_progress(st, old, worker, &id, DutyStatus::Done, Some("evidence")).unwrap();
    id
}

fn retired() -> compact::Retired {
    compact::Retired {
        schedule_id: "schedule-one".into(),
        items: BTreeSet::new(),
        next_item: 2,
    }
}

fn link(st: &mut State, team: &str, id: &str) {
    st.schedules.insert(
        team.into(),
        ScheduleRegistration {
            schedule_id: "schedule-one".into(),
            path: "unreadable".into(),
            manager: None,
        },
    );
    st.duties.get_mut(id).unwrap().schedule_ref = Some(ScheduleRef {
        schedule_id: "schedule-one".into(),
        item_id: "w-1".into(),
        dispatch_token: "token-one".into(),
        manifest_hash: "hash".into(),
    });
}

#[test]
fn archived_duty_contract_cli_and_team_authorization() {
    let f = Fixture::new();
    let id = f.duty();
    let before = f.store.read_current().unwrap();
    let original = before.duties[&id].clone();
    let (_, other, _) = f
        .store
        .transact(CLI_DEADLINE, |st| Ok(seed(st, "two")))
        .unwrap();
    let hot = f.store.read_current().unwrap();
    assert_eq!(
        compact::full_duty(&f.store, &hot, &other.session_id, &id)
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
    let result = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(result["duties"], 1);
    let after = f.store.read_current().unwrap();
    assert!(!after.duties.contains_key(&id));
    assert_eq!(f.get(&id).unwrap(), original);
    assert_eq!(
        compact::full_duty(&f.store, &after, &other.session_id, &id)
            .unwrap_err()
            .reason,
        Reason::NotFound
    );
    assert_eq!(after.runtime_id, before.runtime_id);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gaebu"))
        .args([
            "--json",
            "duty",
            "show",
            &id,
            "--session",
            &f.worker.session_id,
        ])
        .env("GAEBU_HOME", f.store.home())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: Duty = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown, original);
}

#[test]
fn hot_snapshot_lookup_does_not_wait_for_state_lock() {
    let f = Fixture::new();
    let id = f.duty();
    let snapshot = f.store.read_current().unwrap();
    let original = snapshot.duties[&id].clone();
    compact::run(&f.store, &f.team, None).unwrap();
    let lock = fs::File::options()
        .read(true)
        .write(true)
        .open(f.store.home().join("state.lock"))
        .unwrap();
    lock.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            tx.send(compact::full_duty(
                &f.store,
                &snapshot,
                &f.worker.session_id,
                &id,
            ))
            .unwrap();
        });
        let result = rx.recv_timeout(Duration::from_millis(250));
        drop(lock);
        assert_eq!(
            result
                .expect("hot snapshot lookup waited for state.lock")
                .unwrap(),
            original
        );
    });
}

#[test]
fn linked_duty_stays_hot_until_own_matching_tombstone() {
    let f = Fixture::new();
    let id = f.duty();
    f.store
        .transact(CLI_DEADLINE, |st| {
            link(st, &f.team, &id);
            Ok(())
        })
        .unwrap();
    let mut r = retired();
    r.items.insert("w-1".into());
    for supplied in [None, Some(&r)] {
        assert_eq!(
            compact::run(&f.store, &f.team, supplied).unwrap()["duties"],
            0
        );
        let st = f.store.read_current().unwrap();
        assert_eq!(st.duties.len(), 1);
        assert_eq!(
            st.duties[&id].schedule_ref.as_ref().unwrap().dispatch_token,
            "token-one"
        );
    }
    let wrong = compact::Retired {
        schedule_id: "wrong".into(),
        items: retired().items,
        next_item: 2,
    };
    assert_eq!(
        compact::run(&f.store, &f.team, Some(&wrong)).unwrap()["duties"],
        0
    );
    assert_eq!(
        compact::run(&f.store, &f.team, Some(&retired())).unwrap()["duties"],
        1
    );
    assert_eq!(
        f.get(&id).unwrap().schedule_ref.unwrap().dispatch_token,
        "token-one"
    );
}

#[test]
fn linked_duty_stays_hot_and_dispatch_retry_reuses_terminal_contract() {
    use gaebu::identity::{Caller, Source};
    use serde_json::json;
    let f = Fixture::new();
    let caller = Caller {
        session_id: f.root.session_id.clone(),
        tool: Some(Tool::Codex),
        source: Source::PagerNative,
    };
    let path = f.store.home().join("schedule.md");
    gaebu::schedule::register(&f.store, &caller, &path, true).unwrap();
    let st = f.store.read_current().unwrap();
    let bu = &st.members[&f.root.member].bu;
    let add = gaebu::schedule::call(
        &f.store,
        &QuietPager,
        &caller,
        "schedule_add",
        &json!({"title":"active", "scope":["src/**"], "owner_bu":bu}),
    )
    .unwrap();
    let args = json!({"item":add["item"]["id"], "key":"one", "assignee": f.worker.member, "spec":{"intent":"test", "outcome":"deliver", "scope":["src/**"], "done_criteria":"tested", "decision_rights":"internals", "escalate_on":[]}});
    let first =
        gaebu::schedule::call(&f.store, &QuietPager, &caller, "schedule_dispatch", &args).unwrap();
    let id = first["duty"].as_str().unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            let d = st.duties.get_mut(id).unwrap();
            d.status = DutyStatus::Done;
            d.updated_at = now_ms() - 2 * DAY;
            st.records.clear();
            Ok(())
        })
        .unwrap();
    assert_eq!(compact::run(&f.store, &f.team, None).unwrap()["duties"], 0);
    let again =
        gaebu::schedule::call(&f.store, &QuietPager, &caller, "schedule_dispatch", &args).unwrap();
    assert_eq!(again["duty"], id);
    assert_eq!(again["existing"], true);
    assert_eq!(f.store.read_current().unwrap().duties.len(), 1);
}

struct QuietPager;
impl Pager for QuietPager {
    fn whoami(&self, _: Option<&str>) -> gaebu::Result<WhoAmI> {
        unreachable!()
    }
    fn host_status(&self, _: &str) -> gaebu::Result<HostStatus> {
        Ok(HostStatus::Unknown)
    }
    fn send(&self, _: &str, _: &str) -> gaebu::Result<()> {
        Ok(())
    }
}

#[test]
fn foreign_linked_duty_stays_hot_except_cancelled_team() {
    let f = Fixture::new();
    let (other_team, id) = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let (root, worker, team) = seed(st, "two");
            let id = terminal(st, &root, &worker);
            link(st, &team, &id);
            st.records.clear();
            Ok((team, id))
        })
        .unwrap();
    assert_eq!(
        compact::run(&f.store, &f.team, Some(&retired())).unwrap()["duties"],
        0
    );
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.teams.get_mut(&other_team).unwrap().status = TeamStatus::Cancelled;
            Ok(())
        })
        .unwrap();
    assert_eq!(compact::run(&f.store, &f.team, None).unwrap()["duties"], 1);
    assert!(!f.store.read_current().unwrap().duties.contains_key(&id));
    assert!(
        f.store
            .home()
            .join("archive")
            .join(other_team)
            .join("duties.jsonl")
            .exists()
    );
}

#[test]
fn invalid_archive_team_id_preserves_hot_state() {
    for team in ["", "invalid.name"] {
        let f = Fixture::new();
        let id = f.duty();
        f.store
            .transact(CLI_DEADLINE, |st| {
                st.duties.get_mut(&id).unwrap().team = team.into();
                Ok(())
            })
            .unwrap();
        let before = f.store.read_current().unwrap();
        assert_eq!(
            compact::run(&f.store, &f.team, None).unwrap_err().reason,
            Reason::StoreCorrupt
        );
        assert_eq!(f.store.read_current().unwrap(), before);
    }
}

#[test]
fn schedule_probe_requires_eligible_own_links_and_potential_threshold() {
    let f = Fixture::new();
    let id = f.duty();
    f.store
        .transact(CLI_DEADLINE, |st| {
            link(st, &f.team, &id);
            Ok(())
        })
        .unwrap();
    let mut st = f.store.read_current().unwrap();
    assert!(!compact::needs_schedule(&st, &f.team));
    let template = st.duties[&id].clone();
    for _ in 0..63 {
        let mut d = template.clone();
        d.id = st.mint("d");
        st.duties.insert(d.id.clone(), d);
    }
    assert!(compact::needs_schedule(&st, &f.team));
    for d in st.duties.values_mut() {
        d.updated_at = now_ms();
    }
    assert!(!compact::needs_schedule(&st, &f.team));
    for d in st.duties.values_mut() {
        d.updated_at = now_ms() - 2 * DAY;
        d.schedule_ref.as_mut().unwrap().schedule_id = "wrong".into();
    }
    assert!(!compact::needs_schedule(&st, &f.team));
    for d in st.duties.values_mut() {
        d.schedule_ref.as_mut().unwrap().schedule_id = "schedule-one".into();
    }
    st.bus.get_mut(&template.bu).unwrap().hold = Some(Hold {
        evidence: evidence(),
        until: "wait".into(),
        at: 30,
    });
    assert!(!compact::needs_schedule(&st, &f.team));
    st.bus.get_mut(&template.bu).unwrap().hold = None;
    st.teams.get_mut(&f.team).unwrap().status = TeamStatus::Cancelled;
    assert!(!compact::needs_schedule(&st, &f.team));
}

#[test]
fn retention_holds_open_escalations_and_terminal_statuses() {
    let f = Fixture::new();
    let base = f.duty();
    let expected = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let template = st.duties.remove(&base).unwrap();
            let mut archived = BTreeSet::new();
            for status in [
                DutyStatus::Done,
                DutyStatus::Declined,
                DutyStatus::Cancelled,
                DutyStatus::HandedOff,
                DutyStatus::Proposed,
                DutyStatus::Accepted,
                DutyStatus::Blocked,
            ] {
                let mut d = template.clone();
                d.id = st.mint("d");
                d.status = status;
                if matches!(
                    status,
                    DutyStatus::Done
                        | DutyStatus::Declined
                        | DutyStatus::Cancelled
                        | DutyStatus::HandedOff
                ) {
                    archived.insert(d.id.clone());
                }
                st.duties.insert(d.id.clone(), d);
            }
            let mut recent = template.clone();
            recent.id = st.mint("d");
            recent.updated_at = now_ms();
            st.duties.insert(recent.id.clone(), recent);
            let mut held = template.clone();
            held.id = st.mint("d");
            held.hold = Some(Hold {
                evidence: evidence(),
                until: "human release".into(),
                at: 20,
            });
            st.duties.insert(held.id.clone(), held);
            let mut asked = template;
            asked.id = st.mint("d");
            let id = asked.id.clone();
            st.duties.insert(id.clone(), asked);
            ops::escalate(st, 30, &f.worker, Some(&id), "open question")?;
            st.records.clear();
            Ok(archived)
        })
        .unwrap();
    let n = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(n["duties"], 4);
    let st = f.store.read_current().unwrap();
    assert_eq!(st.duties.len(), 6);
    for id in expected {
        assert!(f.get(&id).is_ok());
        assert!(!st.duties.contains_key(&id));
    }
    let held_bu = st.members[&f.worker.member].bu.clone();
    let inherited = f.duty();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.bus.get_mut(&held_bu).unwrap().hold = Some(Hold {
                evidence: evidence(),
                until: "wait".into(),
                at: 40,
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(compact::run(&f.store, &f.team, None).unwrap()["duties"], 0);
    assert!(
        f.store
            .read_current()
            .unwrap()
            .duties
            .contains_key(&inherited)
    );
}

fn evidence() -> Evidence {
    Evidence {
        session_id: "root-evidence".into(),
        prompt_seq: 1,
        sha256: "hash".into(),
        attest: Attest::Fixture,
        quote: "appoint root".into(),
        at: 1,
        strength: Strength::Explicit,
    }
}

#[test]
fn escalation_closure_times_retention_and_archived_mutation_refusal() {
    let f = Fixture::new();
    let old = now_ms() - 2 * DAY;
    let (answered, withdrawn, superseded, recent, legacy) = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let answered = ops::escalate(st, old, &f.worker, None, "one")?.ids[0].clone();
            ops::escalation_answer(st, old, &f.root, &answered, "yes")?;
            let withdrawn = ops::escalate(st, old, &f.worker, None, "two")?.ids[0].clone();
            ops::escalation_withdraw(st, old, &f.worker, &withdrawn, None)?;
            let superseded = ops::escalate(st, old, &f.worker, None, "three")?.ids[0].clone();
            let recent = ops::escalate(st, now_ms(), &f.worker, None, "four")?.ids[0].clone();
            ops::escalation_answer(st, now_ms(), &f.root, &recent, "yes")?;
            let legacy = ops::escalate(st, old, &f.worker, None, "legacy")?.ids[0].clone();
            ops::escalation_withdraw(st, old, &f.worker, &legacy, None)?;
            st.escalations.get_mut(&legacy).unwrap().closed_at = None;
            st.records.clear();
            Ok((answered, withdrawn, superseded, recent, legacy))
        })
        .unwrap();
    let st = f.store.read_current().unwrap();
    assert_eq!(st.escalations[&answered].closed_at, Some(old));
    assert_eq!(st.escalations[&withdrawn].closed_at, Some(old));
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.escalations.get_mut(&answered).unwrap().closed_at = None;
            Ok(())
        })
        .unwrap();
    assert!(st.escalations[&superseded].closed_at.unwrap() >= now_ms() - 1000);
    assert_eq!(
        compact::run(&f.store, &f.team, None).unwrap()["escalations"],
        3
    );
    let mut st = f.store.read_current().unwrap();
    assert!(st.escalations.contains_key(&superseded));
    assert!(st.escalations.contains_key(&recent));
    for id in [&answered, &withdrawn, &legacy] {
        assert!(!st.escalations.contains_key(id));
        for result in [
            ops::escalation_answer(&mut st, now_ms(), &f.root, id, "late"),
            ops::escalation_withdraw(&mut st, now_ms(), &f.worker, id, None),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.reason, Reason::NotFound);
            assert!(error.message.contains("archive"));
        }
    }
    let lines = fs::read_to_string(f.path("escalations")).unwrap();
    assert_eq!(lines.lines().count(), 3);
}

#[test]
fn records_retention_and_root_idempotency_survive_compaction() {
    let f = Fixture::new();
    let ev = evidence();
    let (first, recent_id) = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let first = ops::root_appoint(st, 1, &ev, "extra", None, "project", None)?;
            let recent_id =
                ops::record_add(st, now_ms(), &f.root, RecordKind::Note, &[], "recent")?.ids[0]
                    .clone();
            Ok((first, recent_id))
        })
        .unwrap();
    let before = f.store.read_current().unwrap();
    compact::run(&f.store, &f.team, None).unwrap();
    let mut st = f.store.read_current().unwrap();
    assert_eq!(st.records.len(), 1);
    assert_eq!(st.records[0].id, recent_id);
    assert_eq!(st.root_actions, before.root_actions);
    assert_eq!(st.teams, before.teams);
    assert_eq!(st.bus, before.bus);
    assert_eq!(st.members, before.members);
    assert_eq!(st.grants, before.grants);
    assert_eq!(st.next_id, before.next_id);
    assert_eq!(st.runtime_id, before.runtime_id);
    assert_eq!(st.rev, before.rev + 1);
    let again = ops::root_appoint(&mut st, 1, &ev, "extra", None, "project", None).unwrap();
    assert_eq!(again.ids, first.ids);
    assert!(again.summary.contains("already"));
}

#[test]
fn archive_boundary_corrupt_short_and_unterminated_preserve_hot() {
    for damage in ["short", "newline"] {
        let f = Fixture::new();
        let first = f.duty();
        compact::run(&f.store, &f.team, None).unwrap();
        let second = f.duty();
        let path = f.path("duties");
        let mut bytes = fs::read(&path).unwrap();
        match damage {
            "missing" => fs::remove_file(&path).unwrap(),
            "short" => {
                bytes.truncate(bytes.len() - 2);
                fs::write(&path, bytes).unwrap();
            }
            _ => {
                *bytes.last_mut().unwrap() = b' ';
                fs::write(&path, bytes).unwrap();
            }
        }
        let before = f.store.read_current().unwrap();
        assert_eq!(
            f.get(&first).unwrap_err().reason,
            Reason::StoreCorrupt,
            "{damage}"
        );
        assert_eq!(
            compact::run(&f.store, &f.team, None).unwrap_err().reason,
            Reason::StoreCorrupt,
            "{damage}"
        );
        assert_eq!(f.store.read_current().unwrap(), before);
        assert!(before.duties.contains_key(&second));
    }
}

#[test]
fn archive_boundary_uncommitted_tail_is_ignored_and_truncated() {
    let f = Fixture::new();
    let first = f.duty();
    compact::run(&f.store, &f.team, None).unwrap();
    let original = fs::read(f.path("duties")).unwrap();
    let mut bytes = original.clone();
    let committed = bytes.len();
    bytes.extend_from_slice(b"{torn tail without newline");
    fs::write(f.path("duties"), &bytes).unwrap();
    assert_eq!(f.get(&first).unwrap().id, first);
    let second = f.duty();
    compact::run(&f.store, &f.team, None).unwrap();
    let bytes = fs::read(f.path("duties")).unwrap();
    assert_eq!(&bytes[..committed], &original[..]);
    let lines: Vec<Duty> = String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].id, first);
    assert_eq!(lines[1].id, second);
}

#[test]
fn archive_boundary_parsing_is_lookup_only_and_validates_all_lines() {
    let f = Fixture::new();
    let first = f.duty();
    compact::run(&f.store, &f.team, None).unwrap();
    let path = f.path("duties");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b'!';
    fs::write(&path, bytes).unwrap();
    assert_eq!(f.get(&first).unwrap_err().reason, Reason::StoreCorrupt);
    let second = f.duty();
    assert_eq!(compact::run(&f.store, &f.team, None).unwrap()["duties"], 1);
    assert_eq!(f.get(&second).unwrap_err().reason, Reason::StoreCorrupt);
}

#[test]
fn archive_boundary_utf8_wrong_team_and_last_matching_id() {
    let f = Fixture::new();
    let id = f.duty();
    compact::run(&f.store, &f.team, None).unwrap();
    let original = f.get(&id).unwrap();
    let mut newest = original.clone();
    newest.note = Some("latest contract".into());
    let mut bytes = fs::read(f.path("duties")).unwrap();
    bytes.extend(serde_json::to_vec(&newest).unwrap());
    bytes.push(b'\n');
    fs::write(f.path("duties"), &bytes).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.archives
                .get_mut(&f.team)
                .unwrap()
                .duties
                .segments
                .last_mut()
                .unwrap()
                .bytes = bytes.len() as u64;
            Ok(())
        })
        .unwrap();
    assert_eq!(f.get(&id).unwrap(), newest);
    bytes[0] = 255;
    fs::write(f.path("duties"), &bytes).unwrap();
    assert_eq!(f.get(&id).unwrap_err().reason, Reason::StoreCorrupt);
    let mut foreign = original;
    foreign.team = "other-team".into();
    let mut bytes = serde_json::to_vec(&foreign).unwrap();
    bytes.push(b'\n');
    fs::write(f.path("duties"), &bytes).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.archives
                .get_mut(&f.team)
                .unwrap()
                .duties
                .segments
                .last_mut()
                .unwrap()
                .bytes = bytes.len() as u64;
            Ok(())
        })
        .unwrap();
    assert_eq!(f.get(&id).unwrap_err().reason, Reason::StoreCorrupt);
}

#[test]
fn failed_state_publication_leaves_hot_and_retry_truncates_suffix() {
    let f = Fixture::new();
    let id = f.duty();
    fs::remove_file(f.store.home().join("state.prev.json")).unwrap();
    fs::create_dir(f.store.home().join("state.prev.json")).unwrap();
    let before = f.store.read_current().unwrap();
    assert_eq!(
        compact::run(&f.store, &f.team, None).unwrap_err().reason,
        Reason::StoreWriteFailed
    );
    assert_eq!(f.store.read_current().unwrap(), before);
    assert!(f.path("duties").is_file());
    fs::remove_dir(f.store.home().join("state.prev.json")).unwrap();
    compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(
        fs::read_to_string(f.path("duties"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(f.get(&id).unwrap().id, id);
}

#[test]
fn repair_reloads_boundaries_and_authority_for_stale_lookup() {
    let f = Fixture::new();
    let first = f.duty();
    compact::run(&f.store, &f.team, None).unwrap();
    let second = f.duty();
    compact::run(&f.store, &f.team, None).unwrap();
    let stale = f.store.read_current().unwrap();
    fs::write(f.store.home().join("state.json"), "{").unwrap();
    f.store.repair().unwrap();
    assert_eq!(
        compact::full_duty(&f.store, &stale, &f.worker.session_id, &second)
            .unwrap()
            .id,
        second
    );
    assert_eq!(f.get(&first).unwrap().id, first);
    // Truncation after repair must not make the older snapshot authoritative.
    compact::run(&f.store, &f.team, None).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.members
                .get_mut(&f.worker.member)
                .unwrap()
                .binding
                .session_id = Some("replacement-session".into());
            Ok(())
        })
        .unwrap();
    assert_eq!(
        compact::full_duty(&f.store, &stale, &f.worker.session_id, &first)
            .unwrap_err()
            .reason,
        Reason::NotMember
    );
}

#[test]
fn lookup_waits_for_state_lock_before_selecting_hot_or_archive() {
    let f = Fixture::new();
    let id = f.duty();
    compact::run(&f.store, &f.team, None).unwrap();
    let stale = f.store.read_current().unwrap();
    let lock = fs::File::options()
        .read(true)
        .write(true)
        .open(f.store.home().join("state.lock"))
        .unwrap();
    lock.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let result = compact::full_duty(&f.store, &stale, &f.worker.session_id, &id);
            tx.send(result).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        // This is repair's state replacement while holding the same lock.
        fs::copy(
            f.store.home().join("state.prev.json"),
            f.store.home().join("state.json"),
        )
        .unwrap();
        fs::write(f.path("duties"), b"").unwrap();
        drop(lock);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap().id,
            id
        );
    });
}

#[test]
fn auto_threshold_and_failure_keep_committed_operation() {
    let f = Fixture::new();
    f.store
        .transact(CLI_DEADLINE, |st| {
            for _ in 0..63 {
                terminal(st, &f.root, &f.worker);
            }
            st.records.clear();
            Ok(())
        })
        .unwrap();
    let before = f.store.read_current().unwrap();
    compact::auto(&f.store, &f.team, None);
    assert_eq!(f.store.read_current().unwrap(), before);
    f.duty();
    fs::write(f.store.home().join("archive"), b"blocked").unwrap();
    let (id, _) = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let id = ops::duty_issue(
                st,
                now_ms(),
                &f.root,
                &f.worker.member,
                DutySpec {
                    intent: "cancel".into(),
                    outcome: "stop".into(),
                    ..Default::default()
                },
            )?
            .ids[0]
                .clone();
            let result = ops::duty_cancel(st, now_ms(), &f.root, &id, "stop")?;
            Ok((id, result))
        })
        .unwrap();
    compact::auto(&f.store, &f.team, None);
    let committed = f.store.read_current().unwrap();
    assert_eq!(committed.duties[&id].status, DutyStatus::Cancelled);
    assert_eq!(committed.duties.len(), 65);
    fs::remove_file(f.store.home().join("archive")).unwrap();
    compact::auto(&f.store, &f.team, None);
    assert_eq!(f.store.read_current().unwrap().duties.len(), 1);
}

struct NoPager;
impl Pager for NoPager {
    fn whoami(&self, _: Option<&str>) -> gaebu::Result<WhoAmI> {
        panic!("bound hook needs no pager identity")
    }
    fn host_status(&self, _: &str) -> gaebu::Result<HostStatus> {
        panic!("bound hook needs no roster")
    }
    fn send(&self, _: &str, _: &str) -> gaebu::Result<()> {
        panic!("hook must not send")
    }
}

#[test]
fn hook_does_not_compact() {
    let f = Fixture::new();
    f.store
        .transact(CLI_DEADLINE, |st| {
            for _ in 0..65 {
                terminal(st, &f.root, &f.worker);
            }
            st.records.clear();
            Ok(())
        })
        .unwrap();
    let input = serde_json::json!({"session_id": f.worker.session_id, "hook_event_name": "UserPromptSubmit", "prompt": "continue"}).to_string();
    let start = Instant::now();
    let mut output = Vec::new();
    gaebu::hook::run(
        Tool::Codex,
        &mut input.as_bytes(),
        &mut output,
        &f.store,
        &NoPager,
    );
    assert!(start.elapsed() < Duration::from_secs(4));
    assert!(!output.is_empty());
    assert_eq!(f.store.read_current().unwrap().duties.len(), 65);
    assert!(!f.store.home().join("archive").exists());
}

#[test]
fn archive_large_cost() {
    let f = Fixture::new();
    let id = f.duty();
    let template = f.store.read_current().unwrap().duties[&id].clone();
    f.store
        .transact(CLI_DEADLINE, |st| {
            for _ in 0..9_999 {
                let mut d = template.clone();
                d.id = st.mint("d");
                st.duties.insert(d.id.clone(), d);
            }
            Ok(())
        })
        .unwrap();
    let start = Instant::now();
    assert_eq!(
        compact::run(&f.store, &f.team, None).unwrap()["duties"],
        10_000
    );
    let initial = start.elapsed();
    let last = f.duty();
    let start = Instant::now();
    compact::run(&f.store, &f.team, None).unwrap();
    let append = start.elapsed();
    let start = Instant::now();
    assert_eq!(f.get(&last).unwrap().id, last);
    let lookup = start.elapsed();
    assert_eq!(f.get(&id).unwrap(), template);
    assert_eq!(
        fs::read_to_string(f.path("duties"))
            .unwrap()
            .lines()
            .count(),
        10_001
    );
    println!(
        "10,000 entries: initial={initial:?}, boundary-check+append={append:?}, locked-lookup={lookup:?}"
    );
}

#[test]
fn linked_retirement_uses_live_set_and_allocator() {
    for (item, live, expected) in [
        ("w-1", false, 1),
        ("w-1", true, 0),
        ("w-0", false, 0),
        ("w-2", false, 0),
        ("bad", false, 0),
    ] {
        let f = Fixture::new();
        let id = f.duty();
        f.store
            .transact(CLI_DEADLINE, |st| {
                link(st, &f.team, &id);
                st.duties
                    .get_mut(&id)
                    .unwrap()
                    .schedule_ref
                    .as_mut()
                    .unwrap()
                    .item_id = item.into();
                Ok(())
            })
            .unwrap();
        let mut snapshot = retired();
        if live {
            snapshot.items.insert(item.into());
        }
        assert_eq!(
            compact::run(&f.store, &f.team, Some(&snapshot)).unwrap()["duties"],
            expected,
            "{item}, live={live}"
        );
    }
}

#[test]
fn runtime_archives_stay_under_limit() {
    let f = Fixture::with_limit(4096);
    let mut written = [0u64; 3];
    let mut largest = [0u64; 3];
    for _ in 0..40 {
        let sizes = f
            .store
            .transact(CLI_DEADLINE, |st| {
                let duty = terminal(st, &f.root, &f.worker);
                st.duties.get_mut(&duty).unwrap().note = Some("d".repeat(2048));
                let old = now_ms() - 2 * DAY;
                let escalation = ops::escalate(st, old, &f.root, None, &"e".repeat(2048))
                    .unwrap()
                    .ids[0]
                    .clone();
                ops::escalation_withdraw(st, old, &f.root, &escalation, None).unwrap();
                st.records.clear();
                let record = Record {
                    id: st.mint("r"),
                    team: f.team.clone(),
                    kind: RecordKind::Note,
                    author: f.root.member.clone(),
                    refs: vec![],
                    text: "r".repeat(2048),
                    at: old,
                };
                let sizes = [
                    serde_json::to_vec(&st.duties[&duty]).unwrap().len() as u64 + 1,
                    serde_json::to_vec(&st.escalations[&escalation])
                        .unwrap()
                        .len() as u64
                        + 1,
                    serde_json::to_vec(&record).unwrap().len() as u64 + 1,
                ];
                st.records.push(record);
                Ok(sizes)
            })
            .unwrap();
        let result = compact::run(&f.store, &f.team, None).unwrap();
        for (index, name) in ["duties", "escalations", "records"].iter().enumerate() {
            written[index] += sizes[index];
            largest[index] = largest[index].max(sizes[index]);
            let stream = f.stream(name);
            let physical: u64 = stream
                .segments
                .iter()
                .map(|s| fs::metadata(f.segment(name, s.n)).unwrap().len())
                .sum();
            assert!(physical <= 4096 + largest[index], "{name}: {physical}");
            assert!(stream.segments.len() <= 17);
            assert_eq!(result["retained_bytes"][name], physical);
            assert_eq!(result["limit_bytes"], 4096);
        }
    }
    assert!(written.iter().all(|bytes| *bytes > 10 * 4096));
}

#[test]
fn expired_duty_is_not_found() {
    let f = Fixture::with_limit(8);
    let ids = f.archive_duties(2);
    let error = f.get(&ids[0]).unwrap_err();
    assert_eq!(error.reason, Reason::NotFound);
    assert_eq!(
        error.message,
        "duty not found in hot state or retained team archive"
    );
    let mut latest = f.get(&ids[1]).unwrap();
    latest.note = Some("newest segment".into());
    // Recovered hot duties may be re-archived with the same id.
    let larger = Store::open_with_archive_limit(f.store.home(), 2048).unwrap();
    larger
        .transact(CLI_DEADLINE, |st| {
            st.duties.insert(latest.id.clone(), latest.clone());
            Ok(())
        })
        .unwrap();
    compact::run(&larger, &f.team, None).unwrap();
    assert_eq!(f.stream("duties").segments.len(), 2);
    assert_eq!(f.get(&ids[1]).unwrap(), latest);
}

#[test]
fn lookup_skips_segments_by_bounds() {
    let f = Fixture::with_limit(2048);
    let ids = f.archive_duties(3);
    let newest_bytes = fs::read(f.segment("duties", 3)).unwrap();
    let mut damaged_newest = newest_bytes.clone();
    damaged_newest[0] = b'!';
    fs::write(f.segment("duties", 3), damaged_newest).unwrap();
    assert_eq!(f.get(&ids[1]).unwrap().id, ids[1]);
    assert_eq!(f.get(&ids[2]).unwrap_err().reason, Reason::StoreCorrupt);
    fs::write(f.segment("duties", 3), newest_bytes).unwrap();
    let mut corrupt = fs::read(f.segment("duties", 1)).unwrap();
    corrupt[0] = b'!';
    fs::write(f.segment("duties", 1), corrupt).unwrap();
    assert_eq!(f.get(&ids[2]).unwrap().id, ids[2]);
    assert_eq!(f.get(&ids[0]).unwrap_err().reason, Reason::StoreCorrupt);
    // Retiring out of order makes ranges overlap; the newer matching row wins.
    let mut recovered = f.get(&ids[1]).unwrap();
    recovered.note = Some("out of order".into());
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.duties.insert(recovered.id.clone(), recovered.clone());
            Ok(())
        })
        .unwrap();
    compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(f.get(&ids[1]).unwrap(), recovered);
    f.store
        .transact(CLI_DEADLINE, |st| {
            let segments = &mut st.archives.get_mut(&f.team).unwrap().duties.segments;
            for segment in segments {
                segment.lo = Some(1);
                segment.hi = Some(u64::MAX);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(f.get(&ids[1]).unwrap(), recovered);
    assert_eq!(f.get("d-999999").unwrap_err().reason, Reason::StoreCorrupt);
}

#[test]
fn missing_prefix_is_expired_but_holes_are_corrupt() {
    for missing in [vec![1], vec![1, 2, 3], vec![2], vec![3]] {
        let f = Fixture::with_limit(2048);
        let ids = f.archive_duties(3);
        assert_eq!(f.stream("duties").segments.len(), 3);
        let next = f.stream("duties").next;
        for number in &missing {
            fs::remove_file(f.segment("duties", *number)).unwrap();
        }
        let before = f.store.read_current().unwrap();
        let newest = f.get(&ids[2]);
        if missing[0] != 1 {
            assert_eq!(newest.unwrap_err().reason, Reason::StoreCorrupt);
            assert_eq!(
                compact::run(&f.store, &f.team, None).unwrap_err().reason,
                Reason::StoreCorrupt
            );
            assert_eq!(f.store.read_current().unwrap(), before);
        } else {
            if missing.len() == 3 {
                assert_eq!(newest.unwrap_err().reason, Reason::NotFound);
            } else {
                assert_eq!(newest.unwrap().id, ids[2]);
            }
            assert_eq!(f.get(&ids[0]).unwrap_err().reason, Reason::NotFound);
            let result = compact::run(&f.store, &f.team, None).unwrap();
            assert_eq!(result["expired"]["duties"]["segments"], missing.len());
            assert_eq!(
                result["expired"]["duties"]["bytes"],
                before.archives[&f.team]
                    .duties
                    .segments
                    .iter()
                    .filter(|s| missing.contains(&s.n))
                    .map(|s| s.bytes)
                    .sum::<u64>()
            );
            assert_eq!(f.stream("duties").next, next);
            let id = f.duty();
            compact::run(&f.store, &f.team, None).unwrap();
            assert_eq!(f.stream("duties").segments.last().unwrap().n, next);
            assert_eq!(f.get(&id).unwrap().id, id);
        }
    }
}

#[test]
fn segment_count_is_capped_across_limits() {
    let mut f = Fixture::with_limit(8192);
    for _ in 0..8 {
        let id = f.duty();
        f.store
            .transact(CLI_DEADLINE, |st| {
                let duty = st.duties.get_mut(&id).unwrap();
                duty.note = Some(String::new());
                let size = serde_json::to_vec(&*duty).unwrap().len() + 1;
                duty.note = Some("x".repeat(1024 - size));
                Ok(())
            })
            .unwrap();
        let result = compact::run(&f.store, &f.team, None).unwrap();
        assert_eq!(result["over_limit"], serde_json::json!([]));
    }
    assert_eq!(f.stream("duties").segments.len(), 8);
    f.reopen(64 * 1024);
    for _ in 0..20 {
        // A batch exceeds the new segment target, forcing one rollover per pass.
        f.store
            .transact(CLI_DEADLINE, |st| {
                let id = terminal(st, &f.root, &f.worker);
                st.duties.get_mut(&id).unwrap().note = Some("x".repeat(8192));
                st.records.clear();
                Ok(())
            })
            .unwrap();
        compact::run(&f.store, &f.team, None).unwrap();
        assert!(f.stream("duties").segments.len() <= 17);
    }
}

#[test]
fn explicit_maintenance_enforces_limit_without_new_rows() {
    let mut f = Fixture::with_limit(2048);
    let ids = f.archive_duties(3);
    let prior = f.stream("duties");
    f.reopen(8);
    let result = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(result["duties"], 0);
    assert_eq!(result["escalations"], 0);
    assert_eq!(result["records"], 0);
    assert_eq!(result["expired"]["duties"]["segments"], 2);
    assert_eq!(
        result["expired"]["duties"]["bytes"],
        prior.segments[..2].iter().map(|s| s.bytes).sum::<u64>()
    );
    assert_eq!(f.stream("duties").segments.len(), 1);
    assert_eq!(f.get(&ids[0]).unwrap_err().reason, Reason::NotFound);
    assert_eq!(f.get(&ids[2]).unwrap().id, ids[2]);
    assert!(result["retained_bytes"]["duties"].as_u64().unwrap() > 8);
    assert!(result.get("expiry_deferred").is_none());
    assert_eq!(
        result["over_limit"],
        serde_json::json!([{
            "team": f.team, "stream": "duties", "bytes": f.stream("duties").segments[0].bytes, "limit_bytes": 8
        }])
    );
}

#[test]
fn compact_refuses_damaged_segment() {
    for limit in [2048, 64 * 1024 * 1024] {
        for damage in ["short", "newline", "missing"] {
            let mut f = Fixture::with_limit(2048);
            f.archive_duties(2);
            f.reopen(limit);
            let new_id = f.duty();
            let path = f.segment("duties", 2);
            let mut bytes = fs::read(&path).unwrap();
            match damage {
                "short" => {
                    bytes.pop();
                    fs::write(&path, bytes).unwrap();
                }
                "newline" => {
                    *bytes.last_mut().unwrap() = b' ';
                    fs::write(&path, bytes).unwrap();
                }
                _ => fs::remove_file(&path).unwrap(),
            }
            let before = f.store.read_current().unwrap();
            let first = fs::read(f.segment("duties", 1)).unwrap();
            assert_eq!(
                compact::run(&f.store, &f.team, None).unwrap_err().reason,
                Reason::StoreCorrupt
            );
            assert_eq!(f.store.read_current().unwrap(), before);
            assert!(before.duties.contains_key(&new_id));
            assert_eq!(fs::read(f.segment("duties", 1)).unwrap(), first);
            assert!(!f.segment("duties", 3).exists());
        }
    }
}

#[test]
fn listed_segments_have_no_uncommitted_tail() {
    let mut f = Fixture::new();
    f.archive_duties(1);
    f.reopen(2048);
    for _ in 0..2 {
        let last = f.stream("duties").segments.last().unwrap().clone();
        let path = f.segment("duties", last.n);
        let prefix = fs::read(&path).unwrap();
        let mut garbage = prefix.clone();
        garbage.extend_from_slice(b"failed append tail");
        fs::write(&path, garbage).unwrap();
        f.duty();
        compact::run(&f.store, &f.team, None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), prefix);
        let segments = f.stream("duties").segments;
        for segment in &segments {
            assert_eq!(
                fs::metadata(f.segment("duties", segment.n)).unwrap().len(),
                segment.bytes
            );
        }
        let on_disk: u64 = fs::read_dir(f.path("duties").parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("duties."))
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        let largest = segments.iter().map(|s| s.bytes).max().unwrap_or(0);
        assert!(on_disk <= f.store.archive_limit() + largest);
    }
    let last = f.stream("duties").segments.last().unwrap().clone();
    let path = f.segment("duties", last.n);
    let prefix = fs::read(&path).unwrap();
    let mut garbage = prefix.clone();
    garbage.extend_from_slice(b"idle failed tail");
    fs::write(&path, garbage).unwrap();
    assert_eq!(compact::run(&f.store, &f.team, None).unwrap()["duties"], 0);
    assert_eq!(fs::read(&path).unwrap(), prefix);
}

#[test]
fn unpublished_segment_is_reused() {
    let f = Fixture::with_limit(2048);
    let ids = f.archive_duties(1);
    let prefix = fs::read(f.segment("duties", 1)).unwrap();
    let mut garbage = prefix.clone();
    garbage.extend_from_slice(b"unpublished tail");
    fs::write(f.segment("duties", 1), garbage).unwrap();
    fs::write(f.segment("duties", 2), b"unpublished segment garbage").unwrap();
    assert_eq!(f.get(&ids[0]).unwrap().id, ids[0]);
    let second = f.duty();
    compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(fs::read(f.segment("duties", 1)).unwrap(), prefix);
    let rows: Vec<Duty> = fs::read_to_string(f.segment("duties", 2))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, second);
    assert_eq!(f.get(&second).unwrap().id, second);
}

fn block_publication(f: &Fixture) -> Vec<u8> {
    let prev = f.store.home().join("state.prev.json");
    let bytes = fs::read(&prev).unwrap();
    fs::remove_file(&prev).unwrap();
    fs::create_dir(&prev).unwrap();
    bytes
}

fn unblock_publication(f: &Fixture, previous: &[u8]) {
    let prev = f.store.home().join("state.prev.json");
    fs::remove_dir(&prev).unwrap();
    fs::write(prev, previous).unwrap();
}

#[test]
fn failed_publish_after_expiry_is_consistent() {
    for oversized in [false, true] {
        let mut f = Fixture::with_limit(2048);
        let ids = f.archive_duties(if oversized { 1 } else { 3 });
        let pending = f.duty();
        let before = f.store.read_current().unwrap();
        f.reopen(8);
        let previous = block_publication(&f);
        assert_eq!(
            compact::run(&f.store, &f.team, None).unwrap_err().reason,
            Reason::StoreWriteFailed
        );
        assert_eq!(f.store.read_current().unwrap(), before);
        unblock_publication(&f, &previous);
        for id in &ids {
            assert_eq!(f.get(id).unwrap_err().reason, Reason::NotFound);
        }
        assert_eq!(f.get(&pending).unwrap().id, pending);
        compact::run(&f.store, &f.team, None).unwrap();
        assert_eq!(f.get(&pending).unwrap().id, pending);
        assert_eq!(f.stream("duties").segments.len(), 1);
        assert_eq!(
            f.stream("duties").segments[0].n,
            before.archives[&f.team].duties.next
        );
    }
}

#[test]
fn repair_after_expiry_reads_and_reappends() {
    for oversized in [false, true] {
        let mut f = Fixture::with_limit(2048);
        let ids = f.archive_duties(if oversized { 1 } else { 3 });
        let pending = f.duty();
        f.reopen(8);
        compact::run(&f.store, &f.team, None).unwrap();
        fs::write(f.store.home().join("state.json"), b"broken").unwrap();
        f.store.repair().unwrap();
        for id in &ids {
            assert_eq!(f.get(id).unwrap_err().reason, Reason::NotFound);
        }
        assert_eq!(f.get(&pending).unwrap().id, pending);
        compact::run(&f.store, &f.team, None).unwrap();
        assert_eq!(f.get(&pending).unwrap().id, pending);
    }
    let mut f = Fixture::with_limit(2048);
    let ids = f.archive_duties(2);
    f.reopen(8);
    let previous = block_publication(&f);
    // Idle enforcement deletes the last segment named by the predecessor.
    assert_eq!(
        compact::run(&f.store, &f.team, None).unwrap_err().reason,
        Reason::StoreWriteFailed
    );
    unblock_publication(&f, &previous);
    f.store.repair().unwrap();
    assert_eq!(f.stream("duties").segments.len(), 1);
    assert_eq!(f.get(&ids[0]).unwrap_err().reason, Reason::NotFound);
    assert_eq!(f.get(&ids[1]).unwrap().id, ids[1]);
    compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(f.get(&ids[1]).unwrap().id, ids[1]);
}

#[test]
fn maintenance_removes_unlisted_files_after_repair() {
    let mut f = Fixture::with_limit(2048);
    let ids = f.archive_duties(2);
    // Keep a predecessor with no pending rows, so repair itself creates idle upkeep.
    compact::run(&f.store, &f.team, None).unwrap();
    let previous = block_publication(&f);
    // Publish fails after segment 3 is written and older files expire.
    // The pending duty is already in the current generation, but not its predecessor.
    let mut state = f.store.read_current().unwrap();
    let pending = terminal(&mut state, &f.root, &f.worker);
    state.records.clear();
    fs::write(
        f.store.home().join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    f.reopen(8);
    assert_eq!(
        compact::run(&f.store, &f.team, None).unwrap_err().reason,
        Reason::StoreWriteFailed
    );
    assert!(f.segment("duties", 3).exists());
    unblock_publication(&f, &previous);
    f.store.repair().unwrap();
    assert!(f.store.read_current().unwrap().duties.is_empty());
    assert_eq!(f.get(&pending).unwrap_err().reason, Reason::NotFound);
    assert_eq!(f.get(&ids[0]).unwrap_err().reason, Reason::NotFound);
    let unrelated = f.path("duties").with_extension("03.jsonl");
    fs::write(&unrelated, b"unrelated").unwrap();
    fs::write(f.segment("duties", 900), b"far future unlisted").unwrap();
    for _ in 0..2 {
        let result = compact::run(&f.store, &f.team, None).unwrap();
        assert_eq!(result["duties"], 0);
        assert!(f.stream("duties").segments.is_empty());
        for number in [1, 2, 3, 900] {
            assert!(!f.segment("duties", number).exists());
        }
        assert!(unrelated.exists());
    }
}

#[test]
fn bound_exceptions_are_reported() {
    let mut f = Fixture::with_limit(2048);
    let ids = f.archive_duties(2);
    // Windows can keep the segment readable while denying deletion. Unix permits
    // unlinking open files, so substitute a directory until enforcement returns.
    #[cfg(windows)]
    let blocked = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(f.segment("duties", 1))
            .unwrap()
    };
    #[cfg(not(windows))]
    let oldest = {
        let bytes = fs::read(f.segment("duties", 1)).unwrap();
        fs::remove_file(f.segment("duties", 1)).unwrap();
        fs::create_dir(f.segment("duties", 1)).unwrap();
        bytes
    };
    f.reopen(8);
    let result = compact::run(&f.store, &f.team, None).unwrap();
    assert!(result["expiry_deferred"]["duties"].as_str().is_some());
    assert_eq!(f.stream("duties").segments.len(), 2);
    #[cfg(windows)]
    drop(blocked);
    #[cfg(not(windows))]
    {
        fs::remove_dir(f.segment("duties", 1)).unwrap();
        fs::write(f.segment("duties", 1), oldest).unwrap();
    }
    assert_eq!(f.get(&ids[0]).unwrap().id, ids[0]);
    let sole = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(sole["expired"]["duties"]["segments"], 1);
    assert!(sole["retained_bytes"]["duties"].as_u64().unwrap() > 8);
    assert_eq!(f.get(&ids[1]).unwrap().id, ids[1]);
    f.archive_duties(1);
    assert_eq!(f.get(&ids[1]).unwrap_err().reason, Reason::NotFound);
    // Failed publication leaves one unlisted segment, reused by the retry.
    f.duty();
    let previous = block_publication(&f);
    let next = f.stream("duties").next;
    assert_eq!(
        compact::run(&f.store, &f.team, None).unwrap_err().reason,
        Reason::StoreWriteFailed
    );
    assert!(f.segment("duties", next).exists());
    unblock_publication(&f, &previous);
    compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(f.stream("duties").segments[0].n, next);
}

#[test]
fn automatic_compaction_accepts_more_than_the_threshold() {
    let f = Fixture::new();
    f.store
        .transact(CLI_DEADLINE, |st| {
            for _ in 0..65 {
                terminal(st, &f.root, &f.worker);
            }
            st.records.clear();
            Ok(())
        })
        .unwrap();
    compact::auto(&f.store, &f.team, None);
    let state = f.store.read_current().unwrap();
    assert!(state.duties.is_empty());
    assert_eq!(
        fs::read_to_string(f.path("duties"))
            .unwrap()
            .lines()
            .count(),
        65
    );
}

#[test]
fn maintenance_only_touches_caller_and_streams_with_new_rows() {
    let f = Fixture::with_limit(2048);
    let (other_root, other_worker, other_team) = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let actors = seed(st, "other");
            terminal(st, &actors.0, &actors.1);
            st.records.clear();
            let record_id = st.mint("r");
            st.records.push(Record {
                id: record_id,
                team: actors.2.clone(),
                kind: RecordKind::Note,
                author: actors.0.member.clone(),
                refs: vec![],
                text: "old record".into(),
                at: 1,
            });
            Ok(actors)
        })
        .unwrap();
    compact::run(&f.store, &f.team, None).unwrap();
    let base = f
        .store
        .home()
        .join("archive")
        .join(&other_team)
        .join("records.jsonl");
    fs::write(&base, b"damaged").unwrap();
    let foreign_duty_extra = base.with_file_name("duties.99.jsonl");
    fs::write(&foreign_duty_extra, b"unlisted foreign duty").unwrap();
    let extra = base.with_extension("99.jsonl");
    fs::write(&extra, b"unlisted foreign history").unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            terminal(st, &other_root, &other_worker);
            st.records.clear();
            Ok(())
        })
        .unwrap();
    let archived = f.store.read_current().unwrap().archives[&other_team]
        .records
        .clone();
    let result = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(result["duties"], 1);
    assert_eq!(result["records"], 0);
    assert_eq!(
        fs::read(&foreign_duty_extra).unwrap(),
        b"unlisted foreign duty"
    );
    assert_eq!(fs::read(&base).unwrap(), b"damaged");
    assert_eq!(fs::read(&extra).unwrap(), b"unlisted foreign history");
    assert_eq!(
        f.store.read_current().unwrap().archives[&other_team].records,
        archived
    );
    // A caller-team maintenance pass must still detect its own damaged archive.
    assert_eq!(
        compact::run(&f.store, &other_team, None)
            .unwrap_err()
            .reason,
        Reason::StoreCorrupt
    );
}

#[test]
fn damaged_foreign_archive_is_skipped_not_fatal() {
    let f = Fixture::with_limit(2048);
    let (other_root, other_worker, other_team) = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let actors = seed(st, "other");
            terminal(st, &actors.0, &actors.1);
            st.records.clear();
            Ok(actors)
        })
        .unwrap();
    compact::run(&f.store, &f.team, None).unwrap();
    let foreign = f
        .store
        .home()
        .join("archive")
        .join(&other_team)
        .join("duties.jsonl");
    fs::write(&foreign, b"damaged").unwrap();
    let waiting = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let id = terminal(st, &other_root, &other_worker);
            st.records.clear();
            Ok(id)
        })
        .unwrap();
    let own = f.duty();
    let before = f.store.read_current().unwrap().archives[&other_team].clone();
    let result = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(result["duties"], 1);
    assert_eq!(result["skipped"][0]["team"], other_team.as_str());
    assert_eq!(result["skipped"][0]["stream"], "duties");
    let after = f.store.read_current().unwrap();
    assert!(!after.duties.contains_key(&own));
    assert!(after.duties.contains_key(&waiting));
    assert_eq!(after.archives[&other_team], before);
    assert_eq!(fs::read(&foreign).unwrap(), b"damaged");
    assert_eq!(f.get(&own).unwrap().id, own);
}

#[test]
fn invalid_segment_bookkeeping_refuses_lookup_and_maintenance() {
    for damage in ["next-zero", "next-reused", "inverted", "lo-only", "hi-only"] {
        let f = Fixture::with_limit(2048);
        let ids = f.archive_duties(1);
        let original = fs::read(f.path("duties")).unwrap();
        f.store
            .transact(CLI_DEADLINE, |st| {
                let stream = &mut st.archives.get_mut(&f.team).unwrap().duties;
                match damage {
                    "next-zero" => stream.next = 0,
                    "next-reused" => stream.next = 1,
                    "inverted" => {
                        stream.segments[0].lo = Some(2);
                        stream.segments[0].hi = Some(1);
                    }
                    "lo-only" => stream.segments[0].hi = None,
                    _ => stream.segments[0].lo = None,
                }
                Ok(())
            })
            .unwrap();
        let before = f.store.read_current().unwrap();
        assert_eq!(
            f.get(&ids[0]).unwrap_err().reason,
            Reason::StoreCorrupt,
            "{damage}"
        );
        assert_eq!(
            compact::run(&f.store, &f.team, None).unwrap_err().reason,
            Reason::StoreCorrupt,
            "{damage}"
        );
        assert_eq!(f.store.read_current().unwrap(), before);
        assert_eq!(fs::read(f.path("duties")).unwrap(), original);
    }
}

#[test]
fn duty_bounds_cover_out_of_order_appends() {
    let f = Fixture::new();
    let ids = f.archive_duties(1);
    let original = f.get(&ids[0]).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            for id in ["d-1", "d-999999"] {
                let mut recovered = original.clone();
                recovered.id = id.into();
                st.duties.insert(id.into(), recovered);
            }
            Ok(())
        })
        .unwrap();
    compact::run(&f.store, &f.team, None).unwrap();
    let stream = f.stream("duties");
    assert_eq!(stream.segments.len(), 1);
    assert_eq!(
        (stream.segments[0].lo, stream.segments[0].hi),
        (Some(1), Some(999999))
    );
    for id in [&ids[0], "d-1", "d-999999"] {
        assert_eq!(f.get(id).unwrap().id, id);
    }
}

fn caller(actor: &Actor) -> gaebu::identity::Caller {
    gaebu::identity::Caller {
        session_id: actor.session_id.clone(),
        tool: Some(Tool::Codex),
        source: gaebu::identity::Source::PagerNative,
    }
}

#[test]
fn compact_heal_recovers_a_hole() {
    let f = Fixture::with_limit(8192);
    let ids = f.archive_batches(4);
    let before = f.store.read_current().unwrap();
    for name in ["duties", "escalations", "records"] {
        assert_eq!(f.stream(name).segments.len(), 4);
        fs::remove_file(f.segment(name, 2)).unwrap();
    }
    assert_eq!(
        gaebu::schedule::compact(&f.store, &caller(&f.root), false)
            .unwrap_err()
            .reason,
        Reason::StoreCorrupt
    );
    assert_eq!(f.store.read_current().unwrap(), before);
    let result = f.heal().unwrap();
    for name in ["duties", "escalations", "records"] {
        let archives = &before.archives[&f.team];
        let stream = match name {
            "duties" => &archives.duties,
            "escalations" => &archives.escalations,
            _ => &archives.records,
        };
        assert_eq!(result["healed"][name]["segments"], 1);
        assert_eq!(result["healed"][name]["bytes"], stream.segments[0].bytes);
        assert_eq!(result["expired"][name]["segments"], 1);
        assert_eq!(result["expired"][name]["bytes"], stream.segments[1].bytes);
        assert_eq!(
            f.stream(name)
                .segments
                .iter()
                .map(|s| s.n)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(!f.segment(name, 1).exists());
        for segment in f.stream(name).segments {
            assert!(f.segment(name, segment.n).is_file());
        }
    }
    assert_eq!(f.get(&ids[3][0]).unwrap().id, ids[3][0]);
    let expired = f.get(&ids[0][0]).unwrap_err();
    assert_eq!(expired.reason, Reason::NotFound);
    assert_eq!(
        expired.message,
        "duty not found in hot state or retained team archive"
    );
    let again = f.heal().unwrap();
    for name in ["duties", "escalations", "records"] {
        assert_eq!(again["healed"][name]["segments"], 0);
    }
}

#[test]
fn compact_heal_requires_commander() {
    for damage in ["member", "demoted", "dismissed", "moved-team"] {
        let f = Fixture::with_limit(8192);
        f.archive_batches(4);
        fs::remove_file(f.segment("duties", 2)).unwrap();
        if damage == "demoted" {
            f.store
                .transact(CLI_DEADLINE, |st| {
                    st.teams.get_mut(&f.team).unwrap().commander = f.worker.member.clone();
                    Ok(())
                })
                .unwrap();
        } else if damage == "dismissed" {
            f.store
                .transact(CLI_DEADLINE, |st| {
                    st.members.get_mut(&f.root.member).unwrap().status = MemberStatus::Dismissed;
                    Ok(())
                })
                .unwrap();
        } else if damage == "moved-team" {
            f.store
                .transact(CLI_DEADLINE, |st| {
                    let (_, _, team) = seed(st, "other");
                    st.members.get_mut(&f.root.member).unwrap().team = team;
                    Ok(())
                })
                .unwrap();
        }
        let before = f.store.read_current().unwrap();
        let bytes = fs::read(f.segment("duties", 1)).unwrap();
        let error = if damage == "member" {
            gaebu::schedule::compact(&f.store, &caller(&f.worker), true).unwrap_err()
        } else {
            compact::run_heal(&f.store, &caller(&f.root), &f.team, None).unwrap_err()
        };
        assert!(
            matches!(error.reason, Reason::NoGrant | Reason::NotMember),
            "{damage}: {error}"
        );
        assert_eq!(f.store.read_current().unwrap(), before);
        assert_eq!(fs::read(f.segment("duties", 1)).unwrap(), bytes);
    }
    let f = Fixture::new();
    f.duty();
    assert_eq!(
        gaebu::schedule::compact(&f.store, &caller(&f.worker), false).unwrap()["duties"],
        1
    );
}

#[test]
fn heal_does_not_mask_other_corruption() {
    for damage in ["next", "bounds", "short", "newline"] {
        let f = Fixture::with_limit(8192);
        f.archive_batches(4);
        fs::remove_file(f.segment("duties", 2)).unwrap();
        match damage {
            "next" | "bounds" => {
                f.store
                    .transact(CLI_DEADLINE, |st| {
                        let stream = &mut st.archives.get_mut(&f.team).unwrap().records;
                        if damage == "next" {
                            stream.next = 4;
                        } else {
                            stream.segments[0].lo = Some(2);
                            stream.segments[0].hi = Some(1);
                        }
                        Ok(())
                    })
                    .unwrap();
            }
            _ => {
                let path = f.segment("records", 4);
                let mut bytes = fs::read(&path).unwrap();
                if damage == "short" {
                    bytes.pop();
                } else {
                    *bytes.last_mut().unwrap() = b' ';
                }
                fs::write(path, bytes).unwrap();
            }
        }
        let before = f.store.read_current().unwrap();
        let original = fs::read(f.segment("duties", 1)).unwrap();
        assert_eq!(
            f.heal().unwrap_err().reason,
            Reason::StoreCorrupt,
            "{damage}"
        );
        assert_eq!(f.store.read_current().unwrap(), before);
        assert_eq!(fs::read(f.segment("duties", 1)).unwrap(), original);
        for name in ["duties", "escalations", "records"] {
            for n in 1..=4 {
                if name != "duties" || n != 2 {
                    assert!(f.segment(name, n).is_file());
                }
            }
        }
    }
}

#[test]
fn heal_handles_missing_last_segment() {
    let f = Fixture::with_limit(8192);
    f.archive_batches(4);
    for name in ["duties", "escalations", "records"] {
        fs::remove_file(f.segment(name, 4)).unwrap();
    }
    let result = f.heal().unwrap();
    for name in ["duties", "escalations", "records"] {
        assert_eq!(result["healed"][name]["segments"], 3);
        assert_eq!(result["expired"][name]["segments"], 1);
        assert!(f.stream(name).segments.is_empty());
        assert_eq!(f.stream(name).next, 5);
    }
    f.batch();
    compact::run(&f.store, &f.team, None).unwrap();
    for name in ["duties", "escalations", "records"] {
        assert_eq!(f.stream(name).segments[0].n, 5);
    }
}

#[test]
fn heal_reports_listed_and_unlisted_separately() {
    let f = Fixture::with_limit(8192);
    f.archive_batches(4);
    let original = f.stream("duties");
    let unlisted_bytes = fs::metadata(f.segment("duties", 2)).unwrap().len();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.archives
                .get_mut(&f.team)
                .unwrap()
                .duties
                .segments
                .retain(|s| s.n >= 3);
            Ok(())
        })
        .unwrap();
    fs::remove_file(f.segment("duties", 1)).unwrap();
    fs::remove_file(f.segment("duties", 4)).unwrap();
    let result = f.heal().unwrap();
    assert_eq!(result["healed"]["duties"]["segments"], 1);
    assert_eq!(
        result["healed"]["duties"]["bytes"],
        original.segments[2].bytes
    );
    assert_eq!(result["healed"]["duties"]["unlisted_files"], 1);
    assert_eq!(result["healed"]["duties"]["unlisted_bytes"], unlisted_bytes);
    assert_eq!(result["expired"]["duties"]["segments"], 1);
    assert!(f.stream("duties").segments.is_empty());
}

#[test]
fn heal_keeps_recovery_generation_consistent() {
    for failure in ["append", "publish"] {
        let f = Fixture::with_limit(8192);
        let ids = f.archive_batches(4);
        let pending = f.batch();
        let mut current = f.store.read_current().unwrap();
        let all = current.archives[&f.team].duties.clone();
        current
            .archives
            .get_mut(&f.team)
            .unwrap()
            .duties
            .segments
            .retain(|s| s.n >= 3);
        let mut previous = current.clone();
        previous.archives.get_mut(&f.team).unwrap().duties.segments = all
            .segments
            .into_iter()
            .filter(|s| [2, 3].contains(&s.n))
            .collect();
        previous.archives.get_mut(&f.team).unwrap().duties.next = 4;
        fs::write(
            f.store.home().join("state.json"),
            serde_json::to_vec(&current).unwrap(),
        )
        .unwrap();
        fs::write(
            f.store.home().join("state.prev.json"),
            serde_json::to_vec(&previous).unwrap(),
        )
        .unwrap();
        for n in [1, 4] {
            fs::remove_file(f.segment("duties", n)).unwrap();
        }
        let backup = if failure == "append" {
            fs::create_dir(f.segment("duties", 5)).unwrap();
            None
        } else {
            Some(block_publication(&f))
        };
        let error = f.heal().unwrap_err();
        assert_eq!(error.reason, Reason::StoreWriteFailed);
        assert!(
            error.message.contains(&format!(
                "heal had removed 2 segment(s) of {}/duties",
                f.team
            )),
            "{error}"
        );
        assert_eq!(f.store.read_current().unwrap(), current);
        for n in [1, 2, 3] {
            assert!(!f.segment("duties", n).exists());
        }
        if let Some(bytes) = backup {
            unblock_publication(&f, &bytes);
        } else {
            fs::remove_dir(f.segment("duties", 5)).unwrap();
        }
        f.store.repair().unwrap();
        assert_eq!(f.get(&ids[1][0]).unwrap_err().reason, Reason::NotFound);
        assert_eq!(f.get(&pending[0]).unwrap().id, pending[0]);
        let retry = compact::run(&f.store, &f.team, None).unwrap();
        assert_eq!(retry["duties"], 1);
        assert_eq!(f.get(&pending[0]).unwrap().id, pending[0]);
    }
}

#[test]
fn heal_error_after_unlink_names_removals() {
    for escalation_hole in [false, true] {
        let f = Fixture::with_limit(8192);
        f.archive_batches(4);
        f.batch();
        fs::remove_file(f.segment("duties", 2)).unwrap();
        if escalation_hole {
            fs::remove_file(f.segment("escalations", 2)).unwrap();
        }
        fs::create_dir(f.segment("escalations", 5)).unwrap();
        let before = f.store.read_current().unwrap();
        let error = f.heal().unwrap_err();
        assert_eq!(error.reason, Reason::StoreWriteFailed);
        assert!(
            error.message.contains(&format!(
                "heal had removed 1 segment(s) of {}/duties",
                f.team
            )),
            "{error}"
        );
        if escalation_hole {
            assert!(
                error.message.contains(&format!(
                    "heal had removed 1 segment(s) of {}/escalations",
                    f.team
                )),
                "{error}"
            );
        } else {
            assert!(
                !error
                    .message
                    .contains(&format!("segment(s) of {}/escalations", f.team)),
                "{error}"
            );
        }
        assert_eq!(f.store.read_current().unwrap(), before);
        assert!(!f.segment("duties", 1).exists());
        assert_eq!(!f.segment("escalations", 1).exists(), escalation_hole);
    }
}

#[test]
fn heal_then_failed_publish_is_consistent() {
    let f = Fixture::with_limit(8192);
    f.archive_batches(4);
    let waiting = f.batch();
    f.store.transact(CLI_DEADLINE, |_| Ok(())).unwrap();
    fs::remove_file(f.segment("duties", 2)).unwrap();
    let previous = block_publication(&f);
    let error = f.heal().unwrap_err();
    assert!(error.message.contains(&format!(
        "heal had removed 1 segment(s) of {}/duties",
        f.team
    )));
    unblock_publication(&f, &previous);
    f.store.repair().unwrap();
    assert_eq!(f.get(&waiting[0]).unwrap().id, waiting[0]);
    let retry = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(retry["duties"], 1);
    assert_eq!(retry["expired"]["duties"]["segments"], 2);
    assert_eq!(f.get(&waiting[0]).unwrap().id, waiting[0]);
}

#[test]
fn heal_on_adopted_archives_is_a_noop() {
    let f = Fixture::new();
    let ids = f.archive_duties(1);
    let mut value = serde_json::to_value(f.store.read_current().unwrap()).unwrap();
    value["schema_version"] = 4.into();
    value["archives"] = serde_json::json!({f.team.clone(): {"duties": fs::metadata(f.path("duties")).unwrap().len(), "escalations": 0, "records": 0}});
    let original = serde_json::to_vec(&value).unwrap();
    fs::write(f.store.home().join("state.json"), &original).unwrap();
    let result = f.heal().unwrap();
    for name in ["duties", "escalations", "records"] {
        assert_eq!(result["healed"][name]["segments"], 0);
    }
    assert_eq!(
        fs::read(f.store.home().join("state.v4.json")).unwrap(),
        original
    );
    assert_eq!(f.get(&ids[0]).unwrap().id, ids[0]);
}

#[test]
fn automatic_paths_never_heal() {
    let f = Fixture::with_limit(8192);
    f.archive_batches(4);
    fs::remove_file(f.segment("duties", 2)).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            for _ in 0..65 {
                terminal(st, &f.root, &f.worker);
            }
            st.records.clear();
            Ok(())
        })
        .unwrap();
    let before = f.store.read_current().unwrap();
    let oldest = fs::read(f.segment("duties", 1)).unwrap();
    compact::auto(&f.store, &f.team, None);
    assert_eq!(f.store.read_current().unwrap(), before);
    assert_eq!(fs::read(f.segment("duties", 1)).unwrap(), oldest);
}

struct CliPager;
impl Pager for CliPager {
    fn whoami(&self, session: Option<&str>) -> gaebu::Result<WhoAmI> {
        let session = session.unwrap_or("one-root");
        Ok(WhoAmI {
            session_id: session.into(),
            via: "native".into(),
            alias: Some(session.into()),
            tool: Some("codex".into()),
        })
    }
    fn host_status(&self, _: &str) -> gaebu::Result<HostStatus> {
        Ok(HostStatus::Unknown)
    }
    fn send(&self, _: &str, _: &str) -> gaebu::Result<()> {
        Ok(())
    }
}

#[test]
#[ignore = "child process helper"]
fn heal_cli_child() {
    if let Ok(args) = std::env::var("GAEBU_TEST_HEAL_CLI_ARGS") {
        let args: Vec<String> = serde_json::from_str(&args).unwrap();
        std::process::exit(gaebu::cli::run_with(
            args.into_iter().map(std::ffi::OsString::from),
            &CliPager,
        ));
    }
}

#[test]
fn heal_flag_reaches_both_commands() {
    for transport in ["cli", "mcp"] {
        let f = Fixture::with_limit(8192);
        f.archive_batches(4);
        fs::remove_file(f.segment("duties", 2)).unwrap();
        if transport == "cli" {
            for heal in [false, true] {
                let mut args = vec!["gaebu", "compact"];
                if heal {
                    args.push("--heal");
                }
                let result = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "heal_cli_child", "--ignored", "--nocapture"])
                    .env("GAEBU_HOME", f.store.home())
                    .env(
                        "GAEBU_TEST_HEAL_CLI_ARGS",
                        serde_json::to_string(&args).unwrap(),
                    )
                    .output()
                    .unwrap();
                assert_eq!(
                    result.status.success(),
                    heal,
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                if heal {
                    assert!(String::from_utf8_lossy(&result.stdout).contains("healed:"));
                }
            }
        } else {
            let invalid = f.mcp(serde_json::json!({"heal":"true"}));
            assert_eq!(invalid["result"]["isError"], true);
            assert!(
                invalid["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("invalid_argument"),
                "{invalid}"
            );
            let plain = f.mcp(serde_json::json!({}));
            assert_eq!(plain["result"]["isError"], true);
            let healed = f.mcp(serde_json::json!({"heal":true}));
            assert_eq!(healed["result"]["isError"], false, "{healed}");
            let result: serde_json::Value =
                serde_json::from_str(healed["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(result["healed"]["duties"]["segments"], 1);
        }
        assert!(!f.segment("duties", 1).exists());
    }
}
