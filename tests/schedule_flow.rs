mod common;

use gaebu::{
    Reason, Result, authz,
    identity::{Caller, Source as CallerSource},
    model::*,
    ops::{self, SeedSpec},
    pager::{HostStatus, Pager, WhoAmI},
    schedule,
    store::{CLI_DEADLINE, Store},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    _cleanup: common::RemoveOnDrop,
    store: Store,
    path: PathBuf,
    root: Caller,
    worker: Caller,
    bu: String,
    worker_id: String,
}
struct QuietPager;
impl Pager for QuietPager {
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
        Ok(WhoAmI {
            session_id: session.unwrap_or("root-session").into(),
            via: "native".into(),
            alias: None,
            tool: None,
        })
    }
    fn host_status(&self, _: &str) -> Result<HostStatus> {
        Ok(HostStatus::Unknown)
    }
    fn send(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-schedule-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("runtime")).unwrap();
        store
            .transact(CLI_DEADLINE, |st| {
                ops::dev_seed(
                    st,
                    now_ms(),
                    &SeedSpec {
                        team: "schedule-test".into(),
                        project: "project".into(),
                        commander_alias: "root".into(),
                        commander_session: Some("root-session".into()),
                        commander_tool: Some(Tool::Codex),
                        members: vec![
                            (
                                "worker".into(),
                                Some("worker-session".into()),
                                Some(Tool::Codex),
                            ),
                            (
                                "manager".into(),
                                Some("manager-session".into()),
                                Some(Tool::Codex),
                            ),
                        ],
                    },
                )?;
                Ok(())
            })
            .unwrap();
        let st = store.read().unwrap();
        let root_member = authz::member_for_session(&st, "root-session").unwrap();
        let bu = root_member.bu.clone();
        let worker_id = authz::member_for_session(&st, "worker-session")
            .unwrap()
            .id
            .clone();
        let caller = |session: &str| Caller {
            session_id: session.into(),
            tool: Some(Tool::Codex),
            source: CallerSource::PagerNative,
        };
        let root = caller("root-session");
        let worker = caller("worker-session");
        let path = dir.join("schedule.md");
        schedule::register(&store, &root, &path, true).unwrap();
        Self {
            _cleanup: common::RemoveOnDrop(dir),
            store,
            path,
            root,
            worker,
            bu,
            worker_id,
        }
    }
    fn call(&self, name: &str, args: Value) -> Result<Value> {
        schedule::call(&self.store, &QuietPager, &self.root, name, &args)
    }
    fn add(&self, title: &str, deps: &[&str]) -> String {
        self.call(
            "schedule_add",
            json!({"title":title,"scope":["src/**"],"owner_bu":self.bu,"depends_on":deps}),
        )
        .unwrap()["item"]["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn dispatch(&self, item: &str, key: &str) -> Result<Value> {
        self.call("schedule_dispatch", json!({"item":item,"key":key,"assignee":self.worker_id,"spec":{"intent":"test","outcome":"deliver","scope":["src/**"],"decision_rights":"internals","escalate_on":[],"done_criteria":"tests","plan_ref":null,"exec_root":null}}))
    }
}

#[test]
fn markdown_roundtrip_preserves_prose_and_rejects_unknown_data() {
    let f = Fixture::new();
    let original = fs::read_to_string(&f.path).unwrap();
    fs::write(&f.path, format!("앞말\r\n{original}\r\nAfterword\n")).unwrap();
    f.add("한글 작업", &[]);
    let changed = fs::read_to_string(&f.path).unwrap();
    assert!(changed.starts_with("앞말\r\n"));
    assert!(changed.ends_with("\r\nAfterword\n"));
    let doc = schedule::read_document(&f.path).unwrap();
    assert_eq!(doc.schedule.items[0].title, "한글 작업");
    let changed = changed.replacen(
        "\"schema_version\": 3",
        "\"unknown_field\": true, \"schema_version\": 3",
        1,
    );
    fs::write(&f.path, changed).unwrap();
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
}

#[test]
fn dependency_waits_for_master_signoff_and_cancel_does_not_complete_it() {
    let f = Fixture::new();
    let first = f.add("first", &[]);
    let second = f.add("second", &[&first]);
    let view = f.call("schedule_show", json!({})).unwrap();
    let successor = view["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == second)
        .unwrap();
    assert_eq!(successor["depends_on"], json!([first]));
    assert_eq!(
        f.dispatch(&second, "work").unwrap_err().reason,
        Reason::Conflict
    );
    let dispatch = f.dispatch(&first, "work").unwrap();
    let duty = dispatch["duty"].as_str().unwrap().to_string();
    schedule::transaction(&f.store, &f.worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, &duty, None, true, None)?;
        ops::duty_progress(
            st,
            now_ms(),
            actor,
            &duty,
            DutyStatus::Done,
            Some("tests pass"),
        )
    })
    .unwrap();
    assert_eq!(
        f.dispatch(&second, "work").unwrap_err().reason,
        Reason::Conflict
    );
    assert_eq!(
        schedule::call(
            &f.store,
            &QuietPager,
            &f.worker,
            "schedule_complete",
            &json!({"item":first,"evidence":"done"})
        )
        .unwrap_err()
        .reason,
        Reason::NoGrant
    );
    f.call(
        "schedule_complete",
        json!({"item":first,"evidence":"reviewed result"}),
    )
    .unwrap();
    f.dispatch(&second, "work").unwrap();
    let cancelled = f.add("cancelled", &[]);
    let dependent = f.add("dependent", &[&cancelled]);
    f.call(
        "schedule_cancel",
        json!({"item":cancelled,"reason":"not needed"}),
    )
    .unwrap();
    assert_eq!(
        f.dispatch(&dependent, "work").unwrap_err().reason,
        Reason::Conflict
    );
}

#[test]
fn same_dispatch_retry_is_idempotent_and_rejects_semantic_amendment() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    let first = f.dispatch(&item, "api").unwrap();
    let second = f.dispatch(&item, "api").unwrap();
    assert_eq!(first["duty"], second["duty"]);
    assert_eq!(f.store.read().unwrap().duties.len(), 1);
    assert_eq!(
        f.call(
            "schedule_update",
            json!({"item":item,"title":"different work"})
        )
        .unwrap_err()
        .reason,
        Reason::Conflict
    );
    let duty = first["duty"].as_str().unwrap();
    let error = schedule::transaction(&f.store, &f.root, |st, actor| {
        let spec = ops::DutySpec {
            outcome: "different outcome".into(),
            scope: vec!["src/**".into()],
            ..Default::default()
        };
        ops::duty_amend(st, now_ms(), actor, duty, 1, spec)
    })
    .unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
    assert_eq!(f.store.read().unwrap().duties[duty].outcome, "deliver");
    let error = schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::duty_amend(
            st,
            now_ms(),
            actor,
            duty,
            1,
            ops::DutySpec {
                outcome: "changed before cleanup".into(),
                scope: vec!["src/**".into()],
                ..Default::default()
            },
        )?;
        ops::duty_cancel(
            st,
            now_ms(),
            actor,
            duty,
            "cleanup must not hide a contract rewrite",
        )
    })
    .unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
}

#[test]
fn completed_scope_and_dependencies_cannot_be_rewritten() {
    let f = Fixture::new();
    let item = f.add("direct work", &[]);
    f.call(
        "schedule_complete",
        json!({"item":item,"evidence":"direct result reviewed"}),
    )
    .unwrap();
    for patch in [
        json!({"item":item,"scope":["docs/**"]}),
        json!({"item":item,"title":"new work"}),
        json!({"item":item,"owner_bu":null}),
    ] {
        assert_eq!(
            f.call("schedule_update", patch).unwrap_err().reason,
            Reason::Conflict
        );
    }
    assert!(f.store.read().unwrap().duties.is_empty());
}

#[test]
fn stale_revision_and_cycles_do_not_publish() {
    let f = Fixture::new();
    let first = f.add("first", &[]);
    let second = f.add("second", &[&first]);
    let before = fs::read(&f.path).unwrap();
    assert_eq!(
        f.call(
            "schedule_update",
            json!({"item":first,"depends_on":[second]})
        )
        .unwrap_err()
        .reason,
        Reason::InvalidArgument
    );
    assert_eq!(fs::read(&f.path).unwrap(), before);
    assert_eq!(
        f.call(
            "schedule_update",
            json!({"item":first,"priority":0,"expected_revision":0})
        )
        .unwrap_err()
        .reason,
        Reason::Conflict
    );
}

#[test]
fn missing_runtime_is_uncertainty_not_abandonment_proof() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    let result = f.dispatch(&item, "work").unwrap();
    let token = result["token"].as_str().unwrap();
    fs::remove_file(f.store.home().join("state.json")).unwrap();
    assert!(
        f.call(
            "schedule_retire",
            json!({"item":item,"token":token,"reason":"abandon"})
        )
        .is_err()
    );
    assert!(
        schedule::read_document(&f.path).unwrap().schedule.items[0].dispatches[0]
            .retired
            .is_none()
    );
}

#[test]
fn reserved_only_assignment_recovers_once_and_abandoned_token_never_replays() {
    let f = Fixture::new();
    let item = f.add("recoverable", &[]);
    let args = json!({"item":item,"key":"api","assignee":f.worker_id,"spec":{"intent":"why","outcome":"result","scope":["src/**"],"decision_rights":"internals","escalate_on":[],"done_criteria":"tests"}});
    let reserved = schedule::reserve(&f.store, &f.root, &args).unwrap();
    assert!(f.store.read_current().unwrap().duties.is_empty());
    let shown = f.call("schedule_show", json!({})).unwrap();
    assert_eq!(shown["items"][0]["stage"], "dispatching");
    f.call(
        "schedule_reconcile",
        json!({"item":item,"token":reserved["token"]}),
    )
    .unwrap();
    f.call(
        "schedule_reconcile",
        json!({"item":item,"token":reserved["token"]}),
    )
    .unwrap();
    assert_eq!(f.store.read_current().unwrap().duties.len(), 1);

    let other = f.add("abandoned", &[]);
    let mut args = args.clone();
    args["item"] = json!(other);
    let pending = schedule::reserve(&f.store, &f.root, &args).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.members.get_mut(&f.worker_id).unwrap().status = MemberStatus::Dismissed;
            Ok(())
        })
        .unwrap();
    f.call("schedule_abandon",json!({"item":other,"token":pending["token"],"reason":"the old worker is no longer available"})).unwrap();
    assert!(
        f.call(
            "schedule_reconcile",
            json!({"item":other,"token":pending["token"]})
        )
        .is_err()
    );
    f.call("schedule_update", json!({"item":other,"owner_bu":null}))
        .unwrap();
    assert!(
        schedule::read_document(&f.path)
            .unwrap()
            .schedule
            .items
            .iter()
            .find(|i| i.id == other)
            .unwrap()
            .dispatches[0]
            .retired
            .is_some()
    );
}

#[test]
fn corrupt_current_with_valid_previous_cannot_prove_dispatch_absent() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    let reserved=schedule::reserve(&f.store,&f.root,&json!({"item":item,"key":"api","assignee":f.worker_id,"spec":{"intent":"why","outcome":"result","scope":["src/**"],"decision_rights":"internals","escalate_on":[],"done_criteria":"tests"}})).unwrap();
    let state_path = f.store.home().join("state.json");
    fs::copy(&state_path, f.store.home().join("state.prev.json")).unwrap();
    fs::write(&state_path, b"corrupt-current").unwrap();
    assert!(
        f.store.read().is_ok(),
        "the recovery snapshot is intentionally valid"
    );
    let before = fs::read(&f.path).unwrap();
    assert_eq!(
        f.call(
            "schedule_abandon",
            json!({"item":item,"token":reserved["token"],"reason":"not found"})
        )
        .unwrap_err()
        .reason,
        Reason::StoreCorrupt
    );
    assert_eq!(fs::read(&f.path).unwrap(), before);
}

#[test]
fn separate_manager_can_route_work_but_cannot_command_sibling_worker() {
    let f = Fixture::new();
    schedule::set_manager(&f.store, &f.root, "manager", &["src/**".into()]).unwrap();
    let manager = Caller {
        session_id: "manager-session".into(),
        tool: Some(Tool::Codex),
        source: CallerSource::PagerNative,
    };
    assert_eq!(
        f.call(
            "schedule_add",
            json!({"title":"old manager","scope":["src/**"]})
        )
        .unwrap_err()
        .reason,
        Reason::NoGrant
    );
    let item = schedule::call(
        &f.store,
        &QuietPager,
        &manager,
        "schedule_add",
        &json!({"title":"routed","scope":["src/**"],"owner_bu":f.bu}),
    )
    .unwrap()["item"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(schedule::call(&f.store,&QuietPager,&manager,"schedule_dispatch",&json!({"item":item,"key":"api","assignee":f.worker_id,"spec":{"intent":"why","outcome":"result","scope":["src/**"],"decision_rights":"internals","escalate_on":[],"done_criteria":"tests"}})).unwrap_err().reason,Reason::NoGrant);
    assert_eq!(
        schedule::call(
            &f.store,
            &QuietPager,
            &manager,
            "schedule_add",
            &json!({"title":"outside","scope":["docs/**"]})
        )
        .unwrap_err()
        .reason,
        Reason::OutOfScope
    );
    schedule::set_manager(&f.store, &f.root, "commander", &[]).unwrap();
    f.dispatch(&item, "api").unwrap();
    let st = f.store.read_current().unwrap();
    let manager_id = authz::member_for_session(&st, "manager-session")
        .unwrap()
        .id
        .clone();
    assert!(
        authz::require(&st, &manager_id, Verb::Assign, &["src/**".into()]).is_ok(),
        "restoring commander mode preserves independent grants"
    );
}

#[test]
fn hold_blocks_scheduled_execution_but_allows_independent_lease_and_cleanup() {
    use gaebu::{evidence, store::EventKind};
    let f = Fixture::new();
    let item = f.add("held", &[]);
    let dispatch = f.dispatch(&item, "api").unwrap();
    let duty = dispatch["duty"].as_str().unwrap().to_owned();
    schedule::transaction(&f.store, &f.worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, &duty, None, true, None)
    })
    .unwrap();
    let prompt = "hold this work until I review it";
    f.store
        .session(&f.root.session_id)
        .append(
            now_ms(),
            EventKind::UserPrompt,
            Some(evidence::sha256_hex(prompt)),
            Some(prompt.into()),
            None,
        )
        .unwrap();
    schedule::human_hold(
        &f.store,
        &f.root,
        &json!({"item":item,"until":"I review it","evidence":prompt}),
        false,
    )
    .unwrap();
    assert_eq!(
        schedule::transaction(&f.store, &f.worker, |st, actor| ops::duty_progress(
            st,
            now_ms(),
            actor,
            &duty,
            DutyStatus::Done,
            None
        ))
        .unwrap_err()
        .reason,
        Reason::Held
    );
    assert_eq!(
        schedule::transaction(&f.store, &f.worker, |st, actor| ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/a.rs",
            LeaseMode::Exclusive,
            None,
            "work"
        ))
        .unwrap_err()
        .reason,
        Reason::Held
    );
    let independent = schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::duty_issue(
            st,
            now_ms(),
            actor,
            &f.worker_id,
            ops::DutySpec {
                outcome: "independent".into(),
                scope: vec!["src/**".into()],
                ..Default::default()
            },
        )
    })
    .unwrap()
    .ids[0]
        .clone();
    schedule::transaction(&f.store, &f.worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, &independent, None, true, None)
    })
    .unwrap();
    let lease = schedule::transaction(&f.store, &f.worker, |st, actor| {
        ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/a.rs",
            LeaseMode::Exclusive,
            None,
            "independent",
        )
    })
    .unwrap()
    .ids[0]
        .clone();
    schedule::transaction(&f.store, &f.worker, |st, actor| {
        ops::lease_release(st, now_ms(), actor, &lease)
    })
    .unwrap();
    schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::duty_cancel(st, now_ms(), actor, &duty, "cleanup")
    })
    .unwrap();
}

#[test]
fn linked_dispatch_rejects_foreign_origin_and_unknown_nested_fields() {
    let f = Fixture::new();
    let item = f.add("work", &[]);
    f.dispatch(&item, "api").unwrap();
    let original = fs::read_to_string(&f.path).unwrap();
    let nested = original.replacen("\"spec\": {", "\"spec\": {\"unknown_nested\": 1,", 1);
    fs::write(&f.path, nested).unwrap();
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
    fs::write(&f.path, &original).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.runtime_id = "foreign-runtime".into();
            Ok(())
        })
        .unwrap();
    assert!(
        f.call("schedule_reconcile", json!({"item":item})).unwrap()["results"][0]
            .get("error")
            .is_some()
    );
    assert!(
        f.call(
            "schedule_abandon",
            json!({"item":item,"token":"x-1","reason":"foreign"})
        )
        .is_err()
    );
}

#[test]
fn recovery_preserves_verified_source_after_prompt_log_rolls_away() {
    use gaebu::{evidence, store::EventKind};
    let f = Fixture::new();
    let item = f.add("source recovery", &[]);
    let prompt = "please deliver this worker result";
    f.store
        .session(&f.root.session_id)
        .append(
            now_ms(),
            EventKind::UserPrompt,
            Some(evidence::sha256_hex(prompt)),
            Some(prompt.into()),
            None,
        )
        .unwrap();
    let reserved=schedule::reserve(&f.store,&f.root,&json!({"item":item,"key":"source","assignee":f.worker_id,"spec":{"intent":"why","outcome":"result","scope":["src/**"],"decision_rights":"internals","escalate_on":[],"done_criteria":"tests","source_quote":prompt}})).unwrap();
    let source = schedule::read_document(&f.path).unwrap().schedule.items[0].dispatches[0]
        .source
        .clone()
        .unwrap();
    let original = fs::read_to_string(&f.path).unwrap();
    fs::write(
        &f.path,
        original.replacen("\"source\": {", "\"source\": {\"unknown_source\": true,", 1),
    )
    .unwrap();
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
    fs::write(&f.path, original).unwrap();
    fs::remove_file(
        f.store
            .session(&f.root.session_id)
            .dir()
            .join("events.jsonl"),
    )
    .unwrap();
    f.call(
        "schedule_reconcile",
        json!({"item":item,"token":reserved["token"]}),
    )
    .unwrap();
    let st = f.store.read_current().unwrap();
    assert_eq!(
        st.duties.values().next().unwrap().source.as_ref(),
        Some(&source)
    );
}

#[test]
fn concurrent_same_key_dispatch_creates_one_runtime_duty() {
    let f = Fixture::new();
    let item = f.add("concurrent", &[]);
    let args = json!({"item":item,"key":"api","assignee":f.worker_id,"spec":{"intent":"why","outcome":"result","scope":["src/**"],"decision_rights":"internals","escalate_on":[],"done_criteria":"tests"}});
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let home = f.store.home().to_path_buf();
    let results = std::thread::scope(|threads| {
        let joins: Vec<_> = (0..2)
            .map(|_| {
                let barrier = barrier.clone();
                let home = home.clone();
                let args = args.clone();
                let caller = f.root.clone();
                threads.spawn(move || {
                    let store = Store::open(home).unwrap();
                    barrier.wait();
                    schedule::call(&store, &QuietPager, &caller, "schedule_dispatch", &args)
                })
            })
            .collect();
        joins
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results[0]["duty"], results[1]["duty"]);
    assert_eq!(f.store.read_current().unwrap().duties.len(), 1);
    assert_eq!(
        schedule::read_document(&f.path).unwrap().schedule.items[0]
            .dispatches
            .len(),
        1
    );
}

#[test]
fn legacy_unknown_schedule_is_not_overwritten_or_claimed() {
    let f = Fixture::new();
    let legacy = f.path.parent().unwrap().join("legacy.md");
    let prose = "# Schedule\n- waiting for the API\n";
    fs::write(&legacy, prose).unwrap();
    assert!(schedule::register(&f.store, &f.root, &legacy, true).is_err());
    assert_eq!(fs::read_to_string(&legacy).unwrap(), prose);
}

#[test]
fn summary_counts_are_derived_and_error_views_do_not_claim_healthy_counts() {
    let f = Fixture::new();
    let first = f.add("ready", &[]);
    f.add("waiting", &[&first]);
    let shown = f.call("schedule_show", json!({})).unwrap();
    assert_eq!(shown["counts"]["ready"], 1);
    assert_eq!(shown["counts"]["dependency_wait"], 1);
    assert_eq!(shown["counts"]["queued"], 1);
    assert!(!shown["next_action"].as_str().unwrap().is_empty());
    fs::write(&f.path, "malformed").unwrap();
    let shown = f.call("schedule_show", json!({})).unwrap();
    assert!(shown.get("error").is_some());
    assert!(shown.get("counts").is_none());
}

#[test]
fn each_runtime_contract_corruption_refuses_worker_acceptance_without_commit() {
    for field in [
        "reference_hash",
        "assignee",
        "issuer",
        "intent",
        "outcome",
        "scope",
        "decision_rights",
        "escalate_on",
        "done_criteria",
        "plan_ref",
        "exec_root",
        "source",
    ] {
        let f = Fixture::new();
        let item = f.add("immutable contract", &[]);
        let dispatch = f.dispatch(&item, "api").unwrap();
        let duty_id = dispatch["duty"].as_str().unwrap().to_owned();
        f.store
            .transact(CLI_DEADLINE, |st| {
                let other = authz::member_for_session(st, "manager-session")
                    .unwrap()
                    .id
                    .clone();
                let duty = st.duties.get_mut(&duty_id).unwrap();
                match field {
                    "reference_hash" => {
                        duty.schedule_ref.as_mut().unwrap().manifest_hash =
                            "wrong-reference-hash".into()
                    }
                    "assignee" => duty.assignee = other,
                    "issuer" => duty.issuer = other,
                    "intent" => duty.intent = "different reason".into(),
                    "outcome" => duty.outcome = "different result".into(),
                    "scope" => duty.scope = vec!["docs/**".into()],
                    "decision_rights" => duty.decision_rights = "different rights".into(),
                    "escalate_on" => duty.escalate_on = vec!["different escalation".into()],
                    "done_criteria" => duty.done_criteria = "different acceptance".into(),
                    "plan_ref" => duty.plan_ref = Some(".agents/plans/other.md".into()),
                    "exec_root" => duty.exec_root = Some("C:/different-checkout".into()),
                    "source" => {
                        duty.source = Some(gaebu::model::Source {
                            session_id: "unrelated-session".into(),
                            prompt_seq: 1,
                            sha256: "unrelated-hash".into(),
                            quote: "unrelated provenance".into(),
                            at: now_ms(),
                        })
                    }
                    _ => unreachable!(),
                }
                Ok(())
            })
            .unwrap();
        let caller = if field == "assignee" {
            Caller {
                session_id: "manager-session".into(),
                tool: Some(Tool::Codex),
                source: CallerSource::PagerNative,
            }
        } else {
            f.worker.clone()
        };
        let before = fs::read(f.store.home().join("state.json")).unwrap();
        let error = schedule::transaction(&f.store, &caller, |st, actor| {
            ops::duty_respond(st, now_ms(), actor, &duty_id, None, true, None)
        })
        .unwrap_err();
        assert_eq!(error.reason, Reason::Conflict, "field {field}: {error}");
        assert_eq!(
            fs::read(f.store.home().join("state.json")).unwrap(),
            before,
            "field {field} must not publish acceptance or a revision"
        );
    }
}

#[test]
fn independently_foreign_dispatch_origins_refuse_worker_execution() {
    for foreign in ["assignee", "issuer"] {
        let f = Fixture::new();
        let item = f.add("origin-qualified contract", &[]);
        let result = f.dispatch(&item, "api").unwrap();
        let duty_id = result["duty"].as_str().unwrap().to_owned();
        let mut document = schedule::read_document(&f.path).unwrap();
        let dispatch = &mut document.schedule.items[0].dispatches[0];
        match foreign {
            "assignee" => dispatch.assignee.runtime_id = "foreign-runtime".into(),
            "issuer" => dispatch.issuer.runtime_id = "foreign-runtime".into(),
            _ => unreachable!(),
        }
        // Keep the serialized envelope valid so this tests origin checks rather
        // than the independent parser integrity check.
        let payload = serde_json::to_string(&(
            &dispatch.key,
            &dispatch.token,
            &dispatch.assignee,
            &dispatch.issuer,
            &dispatch.spec,
            &dispatch.source,
            &dispatch.predecessor,
        ))
        .unwrap();
        dispatch.manifest_hash = gaebu::evidence::sha256_hex(&payload);
        let manifest_hash = dispatch.manifest_hash.clone();
        fs::write(
            &f.path,
            format!(
                "# Gaebu Schedule\n\n<!-- gaebu-schedule -->\n```json\n{}\n```\n",
                serde_json::to_string_pretty(&document.schedule).unwrap()
            ),
        )
        .unwrap();
        schedule::read_document(&f.path).unwrap();
        f.store
            .transact(CLI_DEADLINE, |st| {
                st.duties
                    .get_mut(&duty_id)
                    .unwrap()
                    .schedule_ref
                    .as_mut()
                    .unwrap()
                    .manifest_hash = manifest_hash;
                Ok(())
            })
            .unwrap();
        let before = fs::read(f.store.home().join("state.json")).unwrap();
        let error = schedule::transaction(&f.store, &f.worker, |st, actor| {
            ops::duty_respond(st, now_ms(), actor, &duty_id, None, true, None)
        })
        .unwrap_err();
        assert_eq!(error.reason, Reason::Conflict, "foreign {foreign}");
        assert_eq!(
            fs::read(f.store.home().join("state.json")).unwrap(),
            before,
            "foreign {foreign} must not publish acceptance"
        );
    }
}

#[test]
fn corrupted_canonical_manifest_blocks_execution_before_runtime_commit() {
    let f = Fixture::new();
    let item = f.add("canonical integrity", &[]);
    let result = f.dispatch(&item, "api").unwrap();
    let duty = result["duty"].as_str().unwrap().to_owned();
    let text = fs::read_to_string(&f.path).unwrap();
    fs::write(
        &f.path,
        text.replacen(
            "\"outcome\": \"deliver\"",
            "\"outcome\": \"tampered result\"",
            1,
        ),
    )
    .unwrap();
    let before = fs::read(f.store.home().join("state.json")).unwrap();
    assert_eq!(
        schedule::transaction(&f.store, &f.worker, |st, actor| ops::duty_respond(
            st,
            now_ms(),
            actor,
            &duty,
            None,
            true,
            None
        ))
        .unwrap_err()
        .reason,
        Reason::Conflict
    );
    assert_eq!(fs::read(f.store.home().join("state.json")).unwrap(), before);
}

#[test]
fn task_master_ownership_needs_a_real_accepted_superior_duty_for_leases() {
    let f = Fixture::new();
    let (master_bu, master_id) = f
        .store
        .transact(CLI_DEADLINE, |st| {
            let root = gaebu::identity::actor(st, &f.root)?;
            let master = authz::member_for_session(st, "manager-session")
                .unwrap()
                .id
                .clone();
            let bu = ops::bu_open(
                st,
                now_ms(),
                &root,
                &master,
                None,
                &[Verb::Assign],
                &["src/**".into()],
            )?
            .ids[0]
                .clone();
            Ok((bu, master))
        })
        .unwrap();
    f.call(
        "schedule_add",
        json!({"title":"direct task-master work","scope":["src/**"],"owner_bu":master_bu}),
    )
    .unwrap();
    let master = Caller {
        session_id: "manager-session".into(),
        tool: Some(Tool::Codex),
        source: CallerSource::PagerNative,
    };
    assert_eq!(
        schedule::transaction(&f.store, &master, |st, actor| ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/direct.rs",
            LeaseMode::Exclusive,
            None,
            "direct work"
        ))
        .unwrap_err()
        .reason,
        Reason::OutOfScope
    );
    let duty = schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::duty_issue(
            st,
            now_ms(),
            actor,
            &master_id,
            ops::DutySpec {
                intent: "real delegation".into(),
                outcome: "direct work".into(),
                scope: vec!["src/**".into()],
                ..Default::default()
            },
        )
    })
    .unwrap()
    .ids[0]
        .clone();
    assert_eq!(
        schedule::transaction(&f.store, &master, |st, actor| ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/direct.rs",
            LeaseMode::Exclusive,
            None,
            "not yet accepted"
        ))
        .unwrap_err()
        .reason,
        Reason::OutOfScope
    );
    schedule::transaction(&f.store, &master, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, &duty, None, true, None)
    })
    .unwrap();
    let lease = schedule::transaction(&f.store, &master, |st, actor| {
        ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/direct.rs",
            LeaseMode::Exclusive,
            None,
            "accepted delegation",
        )
    })
    .unwrap()
    .ids[0]
        .clone();
    f.store
        .transact(CLI_DEADLINE, |st| {
            let evidence = Evidence {
                session_id: f.root.session_id.clone(),
                prompt_seq: 1,
                sha256: "fixture-human-stop".into(),
                attest: Attest::Fixture,
                quote: "hold the direct work until I review it".into(),
                at: now_ms(),
                strength: Strength::Explicit,
            };
            ops::root_hold(st, now_ms(), &evidence, &duty, "I review it")
        })
        .unwrap();
    assert_eq!(
        schedule::transaction(&f.store, &master, |st, actor| ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/other.rs",
            LeaseMode::Exclusive,
            None,
            "held"
        ))
        .unwrap_err()
        .reason,
        Reason::Held
    );
    assert_eq!(
        schedule::transaction(&f.store, &master, |st, actor| ops::lease_renew(
            st,
            now_ms(),
            actor,
            &lease,
            None
        ))
        .unwrap_err()
        .reason,
        Reason::Held
    );
    schedule::transaction(&f.store, &master, |st, actor| {
        ops::lease_release(st, now_ms(), actor, &lease)
    })
    .unwrap();
}

#[test]
fn two_processes_dispatch_one_token_without_losing_canonical_writes() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let item = f.add("multiprocess", &[]);
    let sync = f.path.parent().unwrap().join("sync");
    fs::create_dir(&sync).unwrap();
    let mut children = Vec::new();
    for child in ["one", "two"] {
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "schedule_dispatch_child",
                    "--nocapture",
                ])
                .env("GAEBU_TEST_SCHEDULE_HOME", f.store.home())
                .env("GAEBU_TEST_SCHEDULE_ITEM", &item)
                .env("GAEBU_TEST_SCHEDULE_WORKER", &f.worker_id)
                .env("GAEBU_TEST_SCHEDULE_SYNC", &sync)
                .env("GAEBU_TEST_SCHEDULE_CHILD", child)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(sync.join("one").exists() && sync.join("two").exists()) {
        if Instant::now() > deadline {
            for child in &mut children {
                let _ = child.kill();
                let _ = child.wait();
            }
            panic!("child dispatchers did not reach the barrier");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(sync.join("go"), b"go").unwrap();
    for child in children {
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "child failed: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let st = f.store.read_current().unwrap();
    let document = schedule::read_document(&f.path).unwrap();
    assert_eq!(st.duties.len(), 1);
    assert_eq!(document.schedule.items[0].dispatches.len(), 1);
    assert_eq!(
        document.schedule.revision, 3,
        "init, add and one canonical reservation"
    );
    let duty = st.duties.values().next().unwrap();
    assert_eq!(
        duty.schedule_ref.as_ref().unwrap().dispatch_token,
        document.schedule.items[0].dispatches[0].token
    );
}

#[test]
#[ignore = "subprocess helper for two-process schedule locking test"]
fn schedule_dispatch_child() {
    use std::time::{Duration, Instant};
    let home = std::env::var_os("GAEBU_TEST_SCHEDULE_HOME").unwrap();
    let sync = PathBuf::from(std::env::var_os("GAEBU_TEST_SCHEDULE_SYNC").unwrap());
    let name = std::env::var("GAEBU_TEST_SCHEDULE_CHILD").unwrap();
    let item = std::env::var("GAEBU_TEST_SCHEDULE_ITEM").unwrap();
    let worker = std::env::var("GAEBU_TEST_SCHEDULE_WORKER").unwrap();
    let store = Store::open(PathBuf::from(home)).unwrap();
    fs::write(sync.join(name), b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sync.join("go").exists() {
        assert!(
            Instant::now() < deadline,
            "parent did not release dispatcher barrier"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let caller = Caller {
        session_id: "root-session".into(),
        tool: Some(Tool::Codex),
        source: CallerSource::PagerNative,
    };
    schedule::call(&store,&QuietPager,&caller,"schedule_dispatch",&json!({"item":item,"key":"api","assignee":worker,"spec":{"intent":"why","outcome":"result","scope":["src/**"],"decision_rights":"internals","escalate_on":[],"done_criteria":"tests"}})).unwrap();
}
