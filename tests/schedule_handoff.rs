mod common;

use gaebu::{
    Reason, Result, authz, evidence,
    identity::{Caller, Source as CallerSource},
    model::*,
    ops::{self, SeedSpec},
    pager::{HostStatus, Pager, WhoAmI},
    schedule,
    store::{CLI_DEADLINE, EventKind, Store},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct QuietPager;
impl Pager for QuietPager {
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
        Ok(WhoAmI {
            session_id: session.unwrap_or("root-b").into(),
            via: "native".into(),
            alias: None,
            tool: None,
        })
    }
    fn hosts(&self) -> Result<gaebu::pager::Hosts> {
        Ok(gaebu::pager::Hosts::all(HostStatus::Unknown))
    }
    fn send(&self, _: &gaebu::pager::Message<'_>) -> Result<()> {
        Ok(())
    }
}

struct NoticePager(std::cell::RefCell<Vec<String>>);
impl Pager for NoticePager {
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
        QuietPager.whoami(session)
    }
    fn hosts(&self) -> Result<gaebu::pager::Hosts> {
        QuietPager.hosts()
    }
    fn send(&self, message: &gaebu::pager::Message<'_>) -> Result<()> {
        let target = message.target;
        self.0.borrow_mut().push(target.to_owned());
        Ok(())
    }
}

struct TeamFixture {
    id: String,
    root: Caller,
    bu: String,
    worker: Caller,
    worker_id: String,
}
struct Fixture {
    _cleanup: common::RemoveOnDrop,
    store: Store,
    path: PathBuf,
    teams: Vec<TeamFixture>,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-handoff-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        let store = Store::open(dir.join("runtime")).unwrap();
        for name in ["a", "b", "c"] {
            store
                .transact(CLI_DEADLINE, |st| {
                    ops::dev_seed(
                        st,
                        now_ms().saturating_sub(100),
                        &SeedSpec {
                            team: name.into(),
                            project: "project".into(),
                            commander_alias: format!("root-{name}"),
                            commander_session: Some(format!("root-{name}")),
                            commander_tool: Some(Tool::Codex),
                            members: vec![
                                (
                                    format!("worker-{name}"),
                                    Some(format!("worker-{name}")),
                                    Some(Tool::Codex),
                                ),
                                (
                                    format!("extra-{name}"),
                                    Some(format!("extra-{name}")),
                                    Some(Tool::Codex),
                                ),
                            ],
                        },
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let st = store.read_current().unwrap();
        let caller = |session: String| Caller {
            session_id: session,
            tool: Some(Tool::Codex),
            source: CallerSource::PagerNative,
            alias: None,
        };
        let teams = ["a", "b", "c"]
            .into_iter()
            .map(|name| {
                let root = authz::member_for_session(&st, &format!("root-{name}")).unwrap();
                let worker = authz::member_for_session(&st, &format!("worker-{name}")).unwrap();
                TeamFixture {
                    id: root.team.clone(),
                    root: caller(format!("root-{name}")),
                    bu: root.bu.clone(),
                    worker: caller(format!("worker-{name}")),
                    worker_id: worker.id.clone(),
                }
            })
            .collect::<Vec<_>>();
        let path = dir.join("schedule.md");
        schedule::register(&store, &teams[0].root, &path, true).unwrap();
        Self {
            _cleanup: common::RemoveOnDrop(dir),
            store,
            path,
            teams,
        }
    }
    fn call(&self, side: usize, name: &str, args: Value) -> Result<Value> {
        schedule::call(
            &self.store,
            &QuietPager,
            &self.teams[side].root,
            name,
            &args,
        )
    }
    fn add(&self, title: &str, deps: &[&str]) -> String {
        self.call(
            0,
            "schedule_add",
            json!({"title":title,"scope":["src/**"],"owner_bu":self.teams[0].bu,"depends_on":deps}),
        )
        .unwrap()["item"]["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn dispatch_args(&self, side: usize, item: &str, key: &str) -> Value {
        json!({"item":item,"key":key,"assignee":self.teams[side].worker_id,"spec":{"intent":"handoff test","outcome":"verified change","scope":["src/**"],"decision_rights":"implementation","escalate_on":[],"done_criteria":"tests"}})
    }
    fn dispatch(&self, item: &str) -> Value {
        self.call(0, "schedule_dispatch", self.dispatch_args(0, item, "work"))
            .unwrap()
    }
    fn independent_duty(&self) -> String {
        schedule::transaction(&self.store, &self.teams[0].root, |st, actor| {
            ops::duty_issue(
                st,
                now_ms(),
                actor,
                &self.teams[0].worker_id,
                ops::DutySpec {
                    intent: "independent work".into(),
                    outcome: "separate result".into(),
                    scope: vec!["src/**".into()],
                    decision_rights: "implementation".into(),
                    escalate_on: vec![],
                    done_criteria: "tests".into(),
                    plan_ref: None,
                    exec_root: None,
                    source_quote: None,
                    source: None,
                },
            )
        })
        .unwrap()
        .ids[0]
            .clone()
    }
    fn prompt(&self, side: usize, words: &str) -> Evidence {
        let session = &self.teams[side].root.session_id;
        let normalized = evidence::normalize(words);
        self.store
            .session(session)
            .append(
                now_ms(),
                EventKind::UserPrompt,
                Some(evidence::sha256_hex(&normalized)),
                Some(normalized),
                None,
            )
            .unwrap();
        let log = self.store.session(session);
        evidence::current_turn_evidence(
            session,
            &log.events().unwrap(),
            &log.composers().unwrap(),
            words,
            now_ms(),
        )
        .unwrap()
    }
    fn cancel(&self, side: usize) {
        let ev = self.prompt(side, "cancel this fixture team");
        self.store
            .transact(CLI_DEADLINE, |st| {
                ops::root_cancel_team(st, now_ms(), &ev, &self.teams[side].id)
            })
            .unwrap();
    }
    fn offer(&self, to: usize) {
        self.call(0, "schedule_offer", json!({"to":self.teams[to].id}))
            .unwrap();
    }
    fn adopt(&self, side: usize) -> Result<Value> {
        self.call(side, "schedule_adopt", json!({"team":self.teams[0].id}))
    }
    fn mcp(&self, side: usize, name: &str, args: Value) -> Value {
        let session = &self.teams[side].root.session_id;
        self.store
            .session(session)
            .update_sighting(|sighting| {
                sighting.tool = Some(Tool::Codex);
                sighting.last_seen = Some(now_ms());
            })
            .unwrap();
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args,"_meta":{"threadId":session}}});
        let mut output = vec![];
        gaebu::mcp::serve(
            &mut std::io::Cursor::new(format!("{request}\n")),
            &mut output,
            &self.store,
            &QuietPager,
        )
        .unwrap();
        let reply: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }
}

#[test]
fn adopt_from_cancelled_team() {
    let f = Fixture::new();
    let first = f.add("first", &[]);
    let second = f.add("second", &[&first]);
    let dispatched = f.dispatch(&first);
    let original = schedule::read_document(&f.path).unwrap().schedule;
    f.cancel(0);
    let result = f.adopt(1).unwrap();
    assert_eq!(result["adopted"], true);
    let st = f.store.read_current().unwrap();
    assert!(!st.schedules.contains_key(&f.teams[0].id));
    assert_eq!(st.schedules[&f.teams[1].id].schedule_id, original.id);
    let adopted = schedule::read_document(&f.path).unwrap().schedule;
    assert_eq!(adopted.items[1].depends_on, vec![first]);
    assert_eq!(adopted.items[1].id, second);
    assert!(adopted.items.iter().all(|i| i.owner_bu.is_none()));
    assert!(adopted.items[0].dispatches[0].retired.is_some());
    assert_eq!(adopted.items[0].dispatches[0].token, dispatched["token"]);
    assert_eq!(adopted.next_item, original.next_item);
    assert_eq!(adopted.archive, original.archive);
}

#[test]
fn active_source_requires_offer() {
    let f = Fixture::new();
    f.add("work", &[]);
    let before = fs::read(&f.path).unwrap();
    assert!(f.adopt(1).is_err());
    assert_eq!(fs::read(&f.path).unwrap(), before);
    f.offer(1);
    assert_eq!(f.adopt(1).unwrap()["adopted"], true);
}

#[test]
fn offer_withdraw_refuses_adopt() {
    let f = Fixture::new();
    f.add("work", &[]);
    f.offer(1);
    f.call(0, "schedule_offer", json!({"withdraw":true}))
        .unwrap();
    let before = fs::read(&f.path).unwrap();
    assert!(f.adopt(1).is_err());
    assert_eq!(fs::read(&f.path).unwrap(), before);
}

#[test]
fn adopter_redispatches_item() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    let old = f.dispatch(&item);
    f.offer(1);
    f.adopt(1).unwrap();
    f.call(
        1,
        "schedule_update",
        json!({"item":item,"owner_bu":f.teams[1].bu}),
    )
    .unwrap();
    let mut args = f.dispatch_args(1, &item, "work");
    args["predecessor"] = old["token"].clone();
    let next = f.call(1, "schedule_dispatch", args).unwrap();
    let st = f.store.read_current().unwrap();
    assert_eq!(
        st.duties[next["duty"].as_str().unwrap()].team,
        f.teams[1].id
    );
    assert_ne!(next["token"], old["token"]);
}

#[test]
fn handoff_closes_source_duties() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    let dispatch = f.dispatch(&item);
    let duty = dispatch["duty"].as_str().unwrap();
    schedule::transaction(&f.store, &f.teams[0].worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, duty, None, true, None)?;
        ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/file.rs",
            LeaseMode::Exclusive,
            None,
            "working",
        )
    })
    .unwrap();
    f.offer(1);
    let previous_version = f.store.read_current().unwrap().duties[duty].version;
    let result = f.adopt(1).unwrap();
    assert_eq!(result["handed_off_duties"], 1);
    assert_eq!(result["released_leases"], 1);
    let st = f.store.read_current().unwrap();
    assert_eq!(st.duties[duty].status, DutyStatus::HandedOff);
    assert_eq!(st.duties[duty].version, previous_version + 1);
    for team in &f.teams[..2] {
        assert_eq!(
            st.records
                .iter()
                .filter(|r| r.team == team.id && r.kind == RecordKind::Handoff)
                .count(),
            1
        );
    }
}

#[test]
fn holds_require_evidence() {
    let f = Fixture::new();
    let item = f.add("held work", &[]);
    let dispatch = f.dispatch(&item);
    let duty = dispatch["duty"].as_str().unwrap();
    let ev = f.prompt(0, "hold this fixture duty until approval");
    f.store
        .transact(CLI_DEADLINE, |st| {
            ops::root_hold(st, now_ms(), &ev, duty, "human approval")
        })
        .unwrap();
    f.offer(1);
    let before = fs::read(&f.path).unwrap();
    assert_eq!(f.adopt(1).unwrap_err().reason, Reason::Held);
    assert_eq!(fs::read(&f.path).unwrap(), before);
    let words = "adopt this Schedule and lift its runtime holds";
    f.prompt(1, words);
    let result = f
        .call(
            1,
            "schedule_adopt",
            json!({"team":f.teams[0].id,"evidence":words}),
        )
        .unwrap();
    assert_eq!(result["lifted_holds"], 1);
    let st = f.store.read_current().unwrap();
    assert!(st.duties[duty].hold.is_none());
    assert_eq!(st.duties[duty].status, DutyStatus::HandedOff);
    assert!(
        st.records
            .iter()
            .filter(|r| r.kind == RecordKind::Handoff)
            .all(|r| r.text.contains(words) && r.text.contains("human approval"))
    );
    for record in st.records.iter().filter(|r| r.kind == RecordKind::Handoff) {
        let details: Value = serde_json::from_str(&record.text).unwrap();
        assert_eq!(details["holds"][0]["cleared"], true);
    }
    for scenario in [
        "duty",
        "bu",
        "cancelled-duty",
        "reserved-bu",
        "unrelated-bu",
    ] {
        let f = Fixture::new();
        let item = f.add("held transfer", &[]);
        let issued = if scenario == "reserved-bu" {
            schedule::reserve(
                &f.store,
                &f.teams[0].root,
                &f.dispatch_args(0, &item, "work"),
            )
            .unwrap()
        } else {
            f.dispatch(&item)
        };
        let target = if matches!(scenario, "duty" | "cancelled-duty") {
            issued["duty"].as_str().unwrap().to_owned()
        } else if scenario == "unrelated-bu" {
            let st = f.store.read_current().unwrap();
            let actor = gaebu::identity::actor(&st, &f.teams[0].root).unwrap();
            let extra = authz::member_for_session(&st, "extra-a")
                .unwrap()
                .id
                .clone();
            f.store
                .transact(CLI_DEADLINE, |st| {
                    ops::bu_open(
                        st,
                        now_ms(),
                        &actor,
                        &extra,
                        Some("unrelated"),
                        &[Verb::Open],
                        &[],
                    )
                })
                .unwrap()
                .ids[0]
                .clone()
        } else {
            f.teams[0].bu.clone()
        };
        let words = "hold this source work pending human approval";
        let ev = f.prompt(0, words);
        f.store
            .transact(CLI_DEADLINE, |st| {
                ops::root_hold(st, now_ms(), &ev, &target, "human approval")
            })
            .unwrap();
        if scenario == "cancelled-duty" {
            f.cancel(0);
        } else {
            f.offer(1);
        }
        let original = fs::read(&f.path).unwrap();
        assert_eq!(f.adopt(1).unwrap_err().reason, Reason::Held, "{scenario}");
        assert_eq!(fs::read(&f.path).unwrap(), original);
        if scenario != "cancelled-duty" {
            f.cancel(0);
        }
        f.store
            .transact(CLI_DEADLINE, |st| {
                for member in st.members.values_mut().filter(|m| m.team == f.teams[0].id) {
                    member.binding.state = BindingState::Vacant;
                    member.binding.session_id = None;
                }
                Ok(())
            })
            .unwrap();
        let words = "adopt the cancelled team's held Schedule";
        f.prompt(1, words);
        f.prompt(1, "a newer instruction supersedes the quoted words");
        assert_eq!(
            f.call(
                1,
                "schedule_adopt",
                json!({"team":f.teams[0].id,"evidence":words})
            )
            .unwrap_err()
            .reason,
            Reason::NoUserEvidence
        );
        f.prompt(1, words);
        let result = f
            .call(
                1,
                "schedule_adopt",
                json!({"team":f.teams[0].id,"evidence":words}),
            )
            .unwrap();
        assert_eq!(result["lifted_holds"], 0);
        let st = f.store.read_current().unwrap();
        let records: Vec<_> = st
            .records
            .iter()
            .filter(|r| r.kind == RecordKind::Handoff)
            .collect();
        assert_eq!(records.len(), 2);
        for record in records {
            assert!(
                record.text.contains(&target)
                    && record.text.contains("human approval")
                    && record.text.contains(words)
            );
            let details: Value = serde_json::from_str(&record.text).unwrap();
            assert_eq!(details["holds"][0]["cleared"], false);
        }
    }
}

#[test]
fn concurrent_adopts_single_winner() {
    let f = Fixture::new();
    f.add("work", &[]);
    f.cancel(0);
    let results = std::thread::scope(|scope| {
        let b = scope.spawn(|| f.adopt(1));
        let c = scope.spawn(|| f.adopt(2));
        [b.join().unwrap(), c.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let st = f.store.read_current().unwrap();
    assert_eq!(st.schedules.len(), 1);
}

#[test]
fn adopt_resumes_after_document_only_write() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    let issued = f.dispatch(&item);
    let duty = issued["duty"].as_str().unwrap();
    schedule::transaction(&f.store, &f.teams[0].worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, duty, None, true, None)
    })
    .unwrap();
    f.offer(1);
    let before = fs::read(f.store.home().join("state.json")).unwrap();
    f.adopt(1).unwrap();
    fs::write(f.store.home().join("state.json"), before).unwrap();
    assert!(
        f.call(
            0,
            "schedule_reconcile",
            json!({"item":item,"token":issued["token"]})
        )
        .is_err()
    );
    assert!(
        schedule::transaction(
            &f.store,
            &f.teams[0].worker,
            |st, actor| ops::duty_progress(
                st,
                now_ms(),
                actor,
                duty,
                DutyStatus::Done,
                Some("late")
            )
        )
        .is_err()
    );
    let resumed = f.adopt(1).unwrap();
    assert_eq!(resumed["adopted"], true);
    assert_eq!(
        f.store.read_current().unwrap().duties[duty].status,
        DutyStatus::HandedOff
    );
}

#[test]
fn hold_set_between_attempts_is_kept() {
    for materialized in [false, true] {
        let f = Fixture::new();
        let item = f.add("work", &[]);
        let reserved = if materialized {
            f.dispatch(&item)
        } else {
            schedule::reserve(
                &f.store,
                &f.teams[0].root,
                &f.dispatch_args(0, &item, "work"),
            )
            .unwrap()
        };
        f.offer(1);
        let before = fs::read(f.store.home().join("state.json")).unwrap();
        f.adopt(1).unwrap();
        fs::write(f.store.home().join("state.json"), before).unwrap();
        let target = if materialized {
            reserved["duty"].as_str().unwrap().to_owned()
        } else {
            f.teams[0].bu.clone()
        };
        let ev = f.prompt(0, "hold the work pending a new decision");
        f.store
            .transact(CLI_DEADLINE, |st| {
                ops::root_hold(st, now_ms(), &ev, &target, "new decision")
            })
            .unwrap();
        let document = fs::read(&f.path).unwrap();
        assert_eq!(f.adopt(1).unwrap_err().reason, Reason::Held);
        assert_eq!(fs::read(&f.path).unwrap(), document);
    }
}

#[test]
fn adopter_archives_inherited_terminal_items() {
    let f = Fixture::new();
    let item = f.add("complete inline", &[]);
    let issued = f.dispatch(&item);
    let duty = issued["duty"].as_str().unwrap();
    schedule::transaction(&f.store, &f.teams[0].worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, duty, None, true, None)?;
        ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/file.rs",
            LeaseMode::Exclusive,
            None,
            "pending cleanup",
        )?;
        ops::duty_progress(st, now_ms(), actor, duty, DutyStatus::Done, Some("tested"))
    })
    .unwrap();
    f.call(
        0,
        "schedule_complete",
        json!({"item":item,"evidence":"accepted"}),
    )
    .unwrap();
    f.offer(1);
    assert_eq!(f.adopt(1).unwrap()["retired_dispatches"], 1);
    assert_eq!(
        schedule::read_document(&f.path).unwrap().schedule.items[0]
            .owner_bu
            .as_ref()
            .map(|owner| owner.id.as_str()),
        Some(f.teams[0].bu.as_str())
    );
    f.call(1, "schedule_archive", json!({})).unwrap();
    assert!(
        schedule::read_document(&f.path)
            .unwrap()
            .schedule
            .items
            .is_empty()
    );
    assert!(f.call(1, "schedule_show", json!({"item":item})).is_ok());
}

#[test]
fn adopter_reads_predecessor_duty() {
    let f = Fixture::new();
    let unrelated = f.independent_duty();
    schedule::transaction(&f.store, &f.teams[0].worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, &unrelated, None, true, None)?;
        ops::duty_progress(
            st,
            now_ms(),
            actor,
            &unrelated,
            DutyStatus::Done,
            Some("independent result"),
        )
    })
    .unwrap();
    let item = f.add("work", &[]);
    let issued = f.dispatch(&item);
    let duty = issued["duty"].as_str().unwrap();
    f.offer(1);
    f.adopt(1).unwrap();
    let st = f.store.read_current().unwrap();
    assert_eq!(
        gaebu::compact::full_duty(&f.store, &st, &f.teams[1].worker.session_id, &unrelated)
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
    assert_eq!(
        gaebu::card::full_duty(&st, &f.teams[1].worker.session_id, duty)
            .unwrap()
            .id,
        duty
    );
    assert_eq!(
        gaebu::compact::full_duty(&f.store, &st, &f.teams[1].worker.session_id, duty)
            .unwrap()
            .id,
        duty
    );
    assert_eq!(
        gaebu::compact::full_duty(&f.store, &st, &f.teams[2].worker.session_id, duty)
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
    f.store
        .transact(CLI_DEADLINE, |st| {
            for id in [duty, unrelated.as_str()] {
                st.duties.get_mut(id).unwrap().updated_at =
                    now_ms().saturating_sub(2 * 24 * 60 * 60 * 1000);
            }
            Ok(())
        })
        .unwrap();
    f.cancel(0);
    gaebu::compact::run(&f.store, &f.teams[0].id, None).unwrap();
    let st = f.store.read_current().unwrap();
    assert!(!st.duties.contains_key(duty));
    assert_eq!(
        gaebu::compact::full_duty(&f.store, &st, &f.teams[1].worker.session_id, &unrelated)
            .unwrap_err()
            .reason,
        Reason::NotFound
    );
    assert_eq!(
        gaebu::compact::full_duty(&f.store, &st, &f.teams[1].worker.session_id, duty)
            .unwrap()
            .id,
        duty
    );
    assert_eq!(
        gaebu::compact::full_duty(&f.store, &st, &f.teams[2].worker.session_id, duty)
            .unwrap_err()
            .reason,
        Reason::NotFound
    );
    let archived = f
        .store
        .home()
        .join("archive")
        .join(&f.teams[0].id)
        .join("duties.jsonl");
    let mut bytes = fs::read(&archived).unwrap();
    bytes[0] = b'!';
    fs::write(&archived, bytes).unwrap();
    let unreadable =
        gaebu::compact::full_duty(&f.store, &st, &f.teams[1].worker.session_id, duty).unwrap_err();
    assert_eq!(unreadable.reason, Reason::NotFound);
    assert_eq!(unreadable.message, "predecessor archive unavailable");
    fs::remove_file(archived).unwrap();
    let expired =
        gaebu::compact::full_duty(&f.store, &st, &f.teams[1].worker.session_id, duty).unwrap_err();
    assert_eq!(expired.reason, Reason::NotFound);
    assert_eq!(
        expired.message,
        "duty not found in hot state or retained team archive"
    );
}

#[test]
fn handoff_keeps_unlinked_accepted_work_and_its_lease() {
    let f = Fixture::new();
    let item = f.add("linked work", &[]);
    let linked = f.dispatch(&item);
    let independent = f.independent_duty();
    schedule::transaction(&f.store, &f.teams[0].worker, |st, actor| {
        ops::duty_respond(
            st,
            now_ms(),
            actor,
            linked["duty"].as_str().unwrap(),
            None,
            true,
            None,
        )?;
        ops::duty_respond(st, now_ms(), actor, &independent, None, true, None)?;
        ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/file.rs",
            LeaseMode::Exclusive,
            None,
            "independent work",
        )
    })
    .unwrap();
    let before = f.store.read_current().unwrap().duties[&independent].clone();
    f.offer(1);
    let result = f.adopt(1).unwrap();
    assert_eq!(result["handed_off_duties"], 1);
    assert_eq!(result["released_leases"], 0);
    let state = f.store.read_current().unwrap();
    assert_eq!(state.duties[&independent], before);
    assert_eq!(state.leases.len(), 1);
}

#[test]
fn handoff_records_the_adopter_and_notifies_the_source() {
    let f = Fixture::new();
    let item = f.add("transfer", &[]);
    f.dispatch(&item);
    f.offer(1);
    let pager = NoticePager(std::cell::RefCell::new(vec![]));
    schedule::call(
        &f.store,
        &pager,
        &f.teams[1].root,
        "schedule_adopt",
        &json!({"team":f.teams[0].id}),
    )
    .unwrap();
    let mut targets = pager.0.into_inner();
    targets.sort();
    assert_eq!(targets, vec!["root-a", "worker-a"]);
    let state = f.store.read_current().unwrap();
    let adopter = authz::member_for_session(&state, &f.teams[1].root.session_id).unwrap();
    let records: Vec<_> = state
        .records
        .iter()
        .filter(|record| record.kind == RecordKind::Handoff)
        .collect();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| record.author == adopter.id));
}

#[test]
fn adopt_path_unregistered_document() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    f.dispatch(&item);
    let copy = f.path.with_file_name("copy.md");
    fs::copy(&f.path, &copy).unwrap();
    let words = "adopt this unregistered Schedule and resume work";
    f.prompt(1, words);
    assert_eq!(
        f.call(1, "schedule_adopt", json!({"path":copy,"evidence":words}))
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
    assert_eq!(
        f.call(1, "schedule_adopt", json!({"path":f.path,"evidence":words}))
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.schedules.remove(&f.teams[0].id);
            Ok(())
        })
        .unwrap();
    let before = fs::read(&f.path).unwrap();
    assert_eq!(
        f.call(1, "schedule_adopt", json!({"path":f.path}))
            .unwrap_err()
            .reason,
        Reason::NoUserEvidence
    );
    assert_eq!(fs::read(&f.path).unwrap(), before);
    // Words not later than the newest purged cancellation cannot resume a Schedule.
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.root_evidence_floor = u64::MAX;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.call(1, "schedule_adopt", json!({"path":f.path,"evidence":words}))
            .unwrap_err()
            .reason,
        Reason::NoUserEvidence
    );
    assert_eq!(fs::read(&f.path).unwrap(), before);
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.root_evidence_floor = 0;
            Ok(())
        })
        .unwrap();
    f.call(1, "schedule_adopt", json!({"path":f.path,"evidence":words}))
        .unwrap();
    let doc = schedule::read_document(&f.path).unwrap().schedule;
    assert!(doc.items[0].owner_bu.is_none());
    assert!(doc.items[0].dispatches[0].retired.is_some());
    let st = f.store.read_current().unwrap();
    assert!(st.schedules[&f.teams[1].id].predecessors.is_empty());
    assert!(
        st.records
            .iter()
            .filter(|r| r.kind == RecordKind::Handoff)
            .all(|r| r.text.contains("not recoverable"))
    );
}

#[test]
fn attach_refuses_nonempty_schedule() {
    nonempty_registration_is_refused(false);
}

#[test]
fn init_refuses_nonempty_schedule() {
    nonempty_registration_is_refused(true);
}

fn nonempty_registration_is_refused(create: bool) {
    for content in [
        "inline",
        "cleared-owner",
        "archive",
        "expired",
        "tombstone",
        "document-only",
    ] {
        let f = Fixture::new();
        let item = f.add("nonempty", &[]);
        if content == "cleared-owner" {
            f.call(0, "schedule_update", json!({"item":item,"owner_bu":null}))
                .unwrap();
        }
        if matches!(content, "archive" | "expired" | "tombstone") {
            f.call(
                0,
                "schedule_complete",
                json!({"item":item,"evidence":"reviewed"}),
            )
            .unwrap();
            if content == "expired" {
                fs::remove_file(f.path.with_file_name("schedule.md.archive.jsonl")).unwrap();
                f.call(0, "schedule_archive", json!({})).unwrap();
            } else if content == "tombstone" {
                let row: Value = serde_json::from_slice(
                    &fs::read(f.path.with_file_name("schedule.md.archive.jsonl")).unwrap(),
                )
                .unwrap();
                let mut document = schedule::read_document(&f.path).unwrap().schedule;
                document.schema_version = 1;
                document.archive = None;
                document.retired = vec![schedule::Retired {
                    id: item.clone(),
                    state: schedule::RetiredState::Complete,
                    digest: row["digest"].as_str().unwrap().into(),
                    last_dispatch: 0,
                    depends_on: vec![],
                }];
                fs::write(
                    &f.path,
                    format!(
                        "<!-- gaebu-schedule -->\n```json\n{}\n```\n",
                        serde_json::to_string(&document).unwrap()
                    ),
                )
                .unwrap();
            }
        } else if content == "document-only" {
            f.offer(1);
            let state = fs::read(f.store.home().join("state.json")).unwrap();
            f.adopt(1).unwrap();
            fs::write(f.store.home().join("state.json"), state).unwrap();
        }
        f.store
            .transact(CLI_DEADLINE, |st| {
                st.schedules.remove(&f.teams[0].id);
                Ok(())
            })
            .unwrap();
        let original = fs::read(&f.path).unwrap();
        let error = schedule::register(&f.store, &f.teams[1].root, &f.path, create).unwrap_err();
        assert!(error.message.contains("adopt --path"), "{content}: {error}");
        assert_eq!(fs::read(&f.path).unwrap(), original, "{content}");
    }
}

#[test]
fn empty_document_and_own_registration_remain_usable() {
    let f = Fixture::new();
    let original = fs::read(&f.path).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.schedules.remove(&f.teams[0].id);
            Ok(())
        })
        .unwrap();
    schedule::register(&f.store, &f.teams[1].root, &f.path, false).unwrap();
    f.call(1, "schedule_add", json!({"title":"own content","scope":[]}))
        .unwrap();
    let nonempty = fs::read(&f.path).unwrap();
    schedule::register(&f.store, &f.teams[1].root, &f.path, true).unwrap();
    assert_eq!(fs::read(&f.path).unwrap(), nonempty);
    assert_ne!(nonempty, original);
}

#[test]
fn handoff_keeps_source_bu_hold() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    let issued = f.dispatch(&item);
    let duty = issued["duty"].as_str().unwrap();
    let evidence = f.prompt(0, "hold the source unit until its owner approves");
    f.store
        .transact(CLI_DEADLINE, |st| {
            ops::root_hold(st, now_ms(), &evidence, &f.teams[0].bu, "owner approval")
        })
        .unwrap();
    f.offer(1);
    let words = "handoff this work while retaining the source unit stop";
    f.prompt(1, words);
    let result = f
        .call(
            1,
            "schedule_adopt",
            json!({"team":f.teams[0].id,"evidence":words}),
        )
        .unwrap();
    assert_eq!(result["lifted_holds"], 0);
    f.store
        .transact(CLI_DEADLINE, |st| {
            assert!(st.bus[&f.teams[0].bu].hold.is_some());
            assert_eq!(st.duties[duty].status, DutyStatus::HandedOff);
            st.duties.get_mut(duty).unwrap().updated_at =
                now_ms().saturating_sub(2 * 24 * 60 * 60 * 1000);
            Ok(())
        })
        .unwrap();
    gaebu::compact::run(&f.store, &f.teams[0].id, None).unwrap();
    assert!(f.store.read_current().unwrap().duties.contains_key(duty));
}

#[test]
fn handoff_preserves_item_hold_until_human_release() {
    let f = Fixture::new();
    let item = f.add("held item", &[]);
    let old = f.dispatch(&item);
    let words = "hold this item until the next review";
    f.prompt(0, words);
    schedule::human_hold(
        &f.store,
        &f.teams[0].root,
        &json!({"item":item,"until":"next review","evidence":words}),
        false,
    )
    .unwrap();
    let before = schedule::read_document(&f.path).unwrap().schedule.items[0]
        .hold
        .clone();
    f.offer(1);
    f.adopt(1).unwrap();
    assert_eq!(
        schedule::read_document(&f.path).unwrap().schedule.items[0].hold,
        before
    );
    f.call(
        1,
        "schedule_update",
        json!({"item":item,"owner_bu":f.teams[1].bu}),
    )
    .unwrap();
    let mut args = f.dispatch_args(1, &item, "work");
    args["predecessor"] = old["token"].clone();
    assert_eq!(
        schedule::reserve(&f.store, &f.teams[1].root, &args)
            .unwrap_err()
            .reason,
        Reason::Held
    );
    let words = "release the inherited item after its review";
    f.prompt(1, words);
    schedule::human_hold(
        &f.store,
        &f.teams[1].root,
        &json!({"item":item,"evidence":words}),
        true,
    )
    .unwrap();
    schedule::reserve(&f.store, &f.teams[1].root, &args).unwrap();
}

#[test]
#[ignore = "subprocess entry point with isolated environment"]
fn handoff_cli_child() {
    let args: Vec<String> =
        serde_json::from_str(&std::env::var("GAEBU_HANDOFF_CLI_ARGS").unwrap()).unwrap();
    assert_eq!(
        gaebu::cli::run_with(args.iter().map(std::ffi::OsString::from), &QuietPager),
        0
    );
}

#[test]
fn adopt_twice_is_existing() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    f.dispatch(&item);
    f.offer(1);
    f.adopt(1).unwrap();
    let document = fs::read(&f.path).unwrap();
    let before = f.store.read_current().unwrap();
    let args = vec![
        "gaebu",
        "--json",
        "schedule",
        "adopt",
        f.teams[0].id.as_str(),
    ];
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "handoff_cli_child", "--nocapture"])
        .env("GAEBU_HOME", f.store.home())
        .env(
            "GAEBU_HANDOFF_CLI_ARGS",
            serde_json::to_string(&args).unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"existing\":true"));
    let after = f.store.read_current().unwrap();
    assert_eq!(fs::read(&f.path).unwrap(), document);
    assert_eq!(before.schedules, after.schedules);
    assert_eq!(before.duties, after.duties);
    assert_eq!(
        before
            .records
            .iter()
            .filter(|r| r.kind == RecordKind::Handoff)
            .collect::<Vec<_>>(),
        after
            .records
            .iter()
            .filter(|r| r.kind == RecordKind::Handoff)
            .collect::<Vec<_>>()
    );
}

#[test]
fn registration_inside_runtime_home_is_refused() {
    for spelling in ["absolute", "relative", "symlink"] {
        let mut f = Fixture::new();
        let relative = PathBuf::from("target")
            .join("handoff-test-tmp")
            .join(format!(
                "relative-home-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _relative_cleanup = common::RemoveOnDrop(relative.clone());
        if spelling == "relative" {
            let snapshot = f.store.read_current().unwrap();
            let store = Store::open(&relative).unwrap();
            store
                .transact(CLI_DEADLINE, |state| {
                    *state = snapshot;
                    Ok(())
                })
                .unwrap();
            f.store = store;
        } else if spelling == "symlink" {
            #[cfg(unix)]
            {
                let alias = f.path.parent().unwrap().join("runtime-alias");
                std::os::unix::fs::symlink(f.store.home(), &alias).unwrap();
                f.store = Store::open(alias).unwrap();
            }
            #[cfg(not(unix))]
            {
                continue;
            }
        }
        let target = fs::canonicalize(f.store.home())
            .unwrap()
            .join("forbidden.md");
        for create in [false, true] {
            assert_eq!(
                schedule::register(&f.store, &f.teams[1].root, &target, create)
                    .unwrap_err()
                    .reason,
                Reason::InvalidArgument,
                "{spelling}"
            );
        }
        assert_eq!(
            f.call(0, "schedule_move", json!({"path":target}))
                .unwrap_err()
                .reason,
            Reason::InvalidArgument,
            "{spelling}"
        );
        assert_eq!(
            f.call(1, "schedule_adopt", json!({"path":target}))
                .unwrap_err()
                .reason,
            Reason::InvalidArgument,
            "{spelling}"
        );
        assert!(!target.exists());
        assert!(!target.with_file_name("forbidden.md.lock").exists());
    }
}

#[test]
fn legacy_registration_inside_runtime_home_remains_idempotent() {
    let f = Fixture::new();
    let path = fs::canonicalize(f.store.home()).unwrap().join("legacy.md");
    fs::copy(&f.path, &path).unwrap();
    let mut old = serde_json::to_value(f.store.read_current().unwrap()).unwrap();
    old["schema_version"] = json!(5);
    old["schedules"][&f.teams[0].id]["path"] = json!(path);
    for field in ["root_evidence_floor", "purge_pending", "home_schedules"] {
        old.as_object_mut().unwrap().remove(field);
    }
    fs::write(
        f.store.home().join("state.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    schedule::register(&f.store, &f.teams[0].root, &path, true).unwrap();
    assert_eq!(
        f.store.read_current().unwrap().schedules[&f.teams[0].id].path,
        path.to_string_lossy()
    );
}

#[test]
fn offer_and_adopt_reach_mcp() {
    let f = Fixture::new();
    f.add("MCP transfer", &[]);
    assert_eq!(
        f.mcp(0, "schedule_offer", json!({"to":"b"}))["to"],
        f.teams[1].id
    );
    assert_eq!(
        f.mcp(1, "schedule_adopt", json!({"team":"a"}))["adopted"],
        true
    );
    assert_eq!(
        f.mcp(1, "schedule_adopt", json!({"team":"a"}))["existing"],
        true
    );
    let f = Fixture::new();
    f.add("MCP path", &[]);
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.schedules.remove(&f.teams[0].id);
            Ok(())
        })
        .unwrap();
    let words = "adopt this orphaned Schedule by its project path";
    f.prompt(1, words);
    assert_eq!(
        f.mcp(1, "schedule_adopt", json!({"path":f.path,"evidence":words}))["adopted"],
        true
    );
}

#[test]
fn offer_and_adopt_reject_invalid_targets_and_noncommanders() {
    let f = Fixture::new();
    for target in [&f.teams[0].id, "a"] {
        assert_eq!(
            f.call(0, "schedule_offer", json!({"to":target}))
                .unwrap_err()
                .reason,
            Reason::Conflict
        );
    }
    f.cancel(2);
    assert_eq!(
        f.call(0, "schedule_offer", json!({"to":"c"}))
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
    for args in [json!({}), json!({"to":"b","withdraw":true})] {
        assert_eq!(
            f.call(0, "schedule_offer", args).unwrap_err().reason,
            Reason::InvalidArgument
        );
    }
    for name in ["schedule_offer", "schedule_adopt"] {
        let args = if name == "schedule_offer" {
            json!({"to":"b"})
        } else {
            json!({"team":"a"})
        };
        assert_eq!(
            schedule::call(&f.store, &QuietPager, &f.teams[1].worker, name, &args)
                .unwrap_err()
                .reason,
            Reason::NoGrant
        );
    }
    f.offer(1);
    assert_eq!(f.adopt(2).unwrap_err().reason, Reason::Cancelled);
}

#[test]
fn handoff_evidence_fails_closed_for_each_unrecorded_marker() {
    for global in [false, true] {
        let f = Fixture::new();
        f.add("orphan", &[]);
        f.store
            .transact(CLI_DEADLINE, |st| {
                st.schedules.remove(&f.teams[0].id);
                Ok(())
            })
            .unwrap();
        let words = "adopt this orphan with the current instruction";
        let evidence = f.prompt(1, words);
        let session = if global {
            gaebu::store::GLOBAL_MARKERS
        } else {
            &f.teams[1].root.session_id
        };
        f.store
            .session(session)
            .mark_unrecorded(evidence.at)
            .unwrap();
        let before = fs::read(&f.path).unwrap();
        assert_eq!(
            f.call(1, "schedule_adopt", json!({"path":f.path,"evidence":words}))
                .unwrap_err()
                .reason,
            Reason::NoUserEvidence
        );
        assert_eq!(fs::read(&f.path).unwrap(), before);
    }
}

#[test]
fn handoff_rejects_changed_document_identity() {
    let f = Fixture::new();
    f.offer(1);
    let mut doc = schedule::read_document(&f.path).unwrap().schedule;
    doc.id = "different-schedule".into();
    doc.archive.as_mut().unwrap().base = evidence::sha256_hex(&doc.id);
    fs::write(
        &f.path,
        format!(
            "<!-- gaebu-schedule -->\n```json\n{}\n```\n",
            serde_json::to_string(&doc).unwrap()
        ),
    )
    .unwrap();
    let before = fs::read(&f.path).unwrap();
    let error = f.adopt(1).unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
    assert!(error.message.contains("schedule identity changed"));
    assert_eq!(fs::read(&f.path).unwrap(), before);
}

#[test]
fn handoff_keeps_other_teams_lease() {
    let f = Fixture::new();
    let duty = schedule::transaction(&f.store, &f.teams[2].root, |st, actor| {
        ops::duty_issue(
            st,
            now_ms(),
            actor,
            &f.teams[2].worker_id,
            ops::DutySpec {
                intent: "separate team work".into(),
                outcome: "independent result".into(),
                scope: vec!["other/**".into()],
                decision_rights: "implementation".into(),
                escalate_on: vec![],
                done_criteria: "tests".into(),
                plan_ref: None,
                exec_root: None,
                source_quote: None,
                source: None,
            },
        )
    })
    .unwrap()
    .ids[0]
        .clone();
    schedule::transaction(&f.store, &f.teams[2].worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, &duty, None, true, None)?;
        ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "other/file.rs",
            LeaseMode::Exclusive,
            None,
            "other team work",
        )
    })
    .unwrap();
    let before = f.store.read_current().unwrap().leases;
    f.offer(1);
    assert_eq!(f.adopt(1).unwrap()["released_leases"], 0);
    assert_eq!(f.store.read_current().unwrap().leases, before);
}

#[test]
fn existing_adoption_does_not_revalidate_a_stale_quote() {
    let f = Fixture::new();
    f.offer(1);
    f.adopt(1).unwrap();
    assert_eq!(
        f.call(
            1,
            "schedule_adopt",
            json!({"team":"a","evidence":"a quote from an older instruction"})
        )
        .unwrap()["existing"],
        true
    );
}

#[test]
fn adopt_path_refuses_archive_names_before_locking() {
    let f = Fixture::new();
    let path = f.path.with_file_name("orphan.archive.3.jsonl");
    fs::copy(&f.path, &path).unwrap();
    let before = fs::read(&path).unwrap();
    assert_eq!(
        f.call(1, "schedule_adopt", json!({"path":path,"evidence":"adopt"}))
            .unwrap_err()
            .reason,
        Reason::InvalidArgument
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!path.with_file_name("orphan.archive.3.jsonl.lock").exists());
}

#[test]
fn path_adoption_can_coexist_with_other_registered_schedules() {
    let f = Fixture::new();
    let mut doc = schedule::read_document(&f.path).unwrap().schedule;
    doc.id = "independent-orphan".into();
    doc.archive.as_mut().unwrap().base = evidence::sha256_hex(&doc.id);
    let path = f.path.with_file_name("independent.md");
    fs::write(
        &path,
        format!(
            "<!-- gaebu-schedule -->\n```json\n{}\n```\n",
            serde_json::to_string(&doc).unwrap()
        ),
    )
    .unwrap();
    let words = "adopt the independent orphan while keeping the original team";
    f.prompt(1, words);
    assert_eq!(
        f.call(1, "schedule_adopt", json!({"path":path,"evidence":words}))
            .unwrap()["adopted"],
        true
    );
    let state = f.store.read_current().unwrap();
    assert_eq!(state.schedules.len(), 2);
    assert_ne!(
        state.schedules[&f.teams[0].id].schedule_id,
        state.schedules[&f.teams[1].id].schedule_id
    );
}

#[test]
fn adoption_rechecks_same_words_from_a_new_prompt_under_the_state_lock() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    f.add("transfer", &[]);
    f.offer(1);
    let words = "adopt this Schedule under this instruction";
    f.prompt(1, words);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.store.home().join("state.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let store = Store::open(f.store.home()).unwrap();
    let caller = f.teams[1].root.clone();
    let source = f.teams[0].id.clone();
    let worker = std::thread::spawn(move || {
        schedule::call(
            &store,
            &QuietPager,
            &caller,
            "schedule_adopt",
            &json!({"team":source,"evidence":words}),
        )
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let probe = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(f.path.with_file_name("schedule.md.lock"))
            .unwrap();
        match probe.try_lock() {
            Err(fs::TryLockError::WouldBlock) => break,
            Ok(()) => drop(probe),
            Err(error) => panic!("unexpected lock error: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "adopter did not take the Schedule lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // Let the first evidence check finish before changing its sequence behind state.lock.
    std::thread::sleep(Duration::from_millis(50));
    f.prompt(1, words);
    let before = fs::read(&f.path).unwrap();
    drop(lock);
    let error = worker.join().unwrap().unwrap_err();
    assert_eq!(error.reason, Reason::NoUserEvidence);
    assert!(error.message.contains("current prompt changed"));
    assert_eq!(fs::read(&f.path).unwrap(), before);
}

#[test]
fn held_team_purged_then_path_adopt_needs_evidence() {
    let f = Fixture::new();
    let item = f.add("continue after purge", &[]);
    let evidence = f.prompt(0, "hold this source team until a new human decision");
    f.store
        .transact(CLI_DEADLINE, |st| {
            ops::root_hold(
                st,
                now_ms(),
                &evidence,
                &f.teams[0].bu,
                "new human decision",
            )
        })
        .unwrap();
    f.call(0, "schedule_update", json!({"item":item,"owner_bu":null}))
        .unwrap();
    f.cancel(0);
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.teams.get_mut(&f.teams[0].id).unwrap().cancelled_at =
                Some(now_ms() - 31 * 24 * 60 * 60 * 1000);
            Ok(())
        })
        .unwrap();
    let result = gaebu::compact::run(&f.store, &f.teams[1].id, None).unwrap();
    assert_eq!(result["purged"][0]["team"], f.teams[0].id);
    assert!(
        !f.store
            .read_current()
            .unwrap()
            .teams
            .contains_key(&f.teams[0].id)
    );
    let before = fs::read(&f.path).unwrap();
    for create in [false, true] {
        let error = schedule::register(&f.store, &f.teams[1].root, &f.path, create).unwrap_err();
        assert_eq!(error.reason, Reason::Conflict);
        assert!(error.message.contains("adopt --path"));
        assert_eq!(fs::read(&f.path).unwrap(), before);
    }
    assert_eq!(
        f.call(1, "schedule_adopt", json!({"path":f.path}))
            .unwrap_err()
            .reason,
        Reason::NoUserEvidence
    );
    assert_eq!(fs::read(&f.path).unwrap(), before);
    let words = "adopt the purged team's Schedule and resume its work";
    f.prompt(1, words);
    assert_eq!(
        f.call(1, "schedule_adopt", json!({"path":f.path,"evidence":words}))
            .unwrap()["adopted"],
        true
    );
    let state = f.store.read_current().unwrap();
    assert!(state.schedules[&f.teams[1].id].predecessors.is_empty());
    assert!(
        state
            .records
            .iter()
            .filter(|r| r.kind == RecordKind::Handoff)
            .all(|r| r.text.contains("not recoverable") && r.text.contains(words))
    );
}

#[test]
fn repeated_handoff_accumulates_predecessors_and_retains_contract_reads() {
    let f = Fixture::new();
    let item = f.add("multi-team continuation", &[]);
    let issued = f.dispatch(&item);
    f.offer(1);
    f.adopt(1).unwrap();
    f.call(1, "schedule_offer", json!({"to":"c"})).unwrap();
    f.call(2, "schedule_adopt", json!({"team":"b"})).unwrap();
    let state = f.store.read_current().unwrap();
    let registration = &state.schedules[&f.teams[2].id];
    assert_eq!(
        registration.predecessors,
        std::collections::BTreeSet::from([f.teams[0].id.clone(), f.teams[1].id.clone()])
    );
    assert!(registration.manager.is_none());
    assert!(registration.offer.is_none());
    assert!(!state.schedules.contains_key(&f.teams[1].id));
    let duty = gaebu::compact::full_duty(
        &f.store,
        &state,
        &f.teams[2].worker.session_id,
        issued["duty"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(duty.team, f.teams[0].id);
}
