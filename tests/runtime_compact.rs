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
        let dir = std::env::temp_dir().join(format!(
            "gaebu-compact-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::open(&dir).unwrap();
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
        items: BTreeSet::from(["w-1".into()]),
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
    assert_eq!(
        compact::full_duty(&f.store, &before, &other.session_id, &id)
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
        compact::full_duty(&f.store, &before, &other.session_id, &id)
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
    r.items.clear();
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
fn archive_boundary_corrupt_missing_short_and_unterminated_preserve_hot() {
    for damage in ["missing", "short", "newline"] {
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
            st.archives.get_mut(&f.team).unwrap().duties = bytes.len() as u64;
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
            st.archives.get_mut(&f.team).unwrap().duties = bytes.len() as u64;
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
