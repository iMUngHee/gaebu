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
        // Registration canonicalizes paths, including macOS temp-directory aliases
        // and Windows verbatim prefixes; fixtures must use the same spelling.
        let dir = fs::canonicalize(dir).unwrap();
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

impl Fixture {
    fn archive_path(&self) -> PathBuf {
        self.path.with_file_name("schedule.md.archive.jsonl")
    }
    fn child_bu(&self) -> String {
        schedule::transaction(&self.store, &self.root, |st, actor| {
            ops::bu_open(
                st,
                now_ms(),
                actor,
                &self.worker_id,
                Some("child"),
                &[Verb::Assign, Verb::Open],
                &["src/**".into()],
            )
        })
        .unwrap()
        .ids[0]
            .clone()
    }
    fn mcp(&self, name: &str, args: Value) -> Value {
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call", "params": {
            "name":name,"arguments":args,"_meta":{"threadId":"root-session"}}});
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
    fn document(&self) -> schedule::Schedule {
        schedule::read_document(&self.path).unwrap().schedule
    }
    fn write(&self, document: &schedule::Schedule) {
        fs::write(
            &self.path,
            format!(
                "# Test\n<!-- gaebu-schedule -->\n```json\n{}\n```\n",
                serde_json::to_string_pretty(document).unwrap()
            ),
        )
        .unwrap();
    }
    fn complete(&self, item: &str) {
        self.call(
            "schedule_complete",
            json!({"item":item,"evidence":"reviewed"}),
        )
        .unwrap();
    }
    fn done(&self, duty: &str) {
        schedule::transaction(&self.store, &self.worker, |st, actor| {
            ops::duty_respond(st, now_ms(), actor, duty, None, true, None)?;
            ops::duty_progress(
                st,
                now_ms(),
                actor,
                duty,
                DutyStatus::Done,
                Some("verified"),
            )
        })
        .unwrap();
    }
}

fn commit_fixture(f: &Fixture, document: &mut schedule::Schedule) {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(f.archive_path()).unwrap();
    let mut chain = Sha256::digest(document.id.as_bytes()).to_vec();
    let mut rows = 0;
    let mut last_item = 0;
    let mut last_dispatch = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let entry: Value = serde_json::from_slice(line).unwrap();
        let item: schedule::Item = serde_json::from_value(entry["item"].clone()).unwrap();
        last_item = last_item.max(item.id.strip_prefix("w-").unwrap().parse::<u64>().unwrap());
        for dispatch in item.dispatches {
            last_dispatch = last_dispatch.max(
                dispatch
                    .token
                    .strip_prefix("x-")
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
            );
        }
        chain = Sha256::digest([chain.as_slice(), line].concat()).to_vec();
        rows += 1;
    }
    document.archive = Some(schedule::ArchiveHead {
        bytes: bytes.len() as u64,
        rows,
        chain: chain.iter().map(|byte| format!("{byte:02x}")).collect(),
        last_item,
        last_dispatch,
    });
    f.write(document);
}

#[test]
fn complete_retires() {
    let f = Fixture::new();
    let item = f.add("completed", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    f.done(dispatch["duty"].as_str().unwrap());
    let before = f.document().revision;
    f.complete(&item);
    let document = f.document();
    assert!(document.items.is_empty());
    assert_eq!(document.retired[0].id, item);
    assert_eq!(document.retired[0].last_dispatch, 1);
    assert_eq!(document.revision, before + 1);
    assert_eq!(
        fs::read_to_string(f.archive_path())
            .unwrap()
            .lines()
            .count(),
        1
    );
    let shown = f.call("schedule_show", json!({"item":item})).unwrap();
    assert_eq!(shown["stage"], "complete");
    assert_eq!(shown["dispatches"][0]["spec"]["outcome"], "deliver");
    assert!(shown["dispatches"][0].get("source").is_some());
    let shown = f.call("schedule_show", json!({})).unwrap();
    assert_eq!(shown["items"], json!([]));
    assert_eq!(shown["counts"]["retired"], 1);
    assert_eq!(
        f.call("schedule_show", json!({"all":true})).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

// Unix supports unprivileged directory symlinks; child processes isolate TMPDIR
// from the other tests running concurrently in this process.
#[cfg(unix)]
#[test]
fn aliased_temp_directory_preserves_read_observation_and_move_checks() {
    let dir = std::env::temp_dir().join(format!(
        "gaebu-aliased-temp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _cleanup = common::RemoveOnDrop(dir.clone());
    let real = dir.join("real");
    fs::create_dir_all(&real).unwrap();
    let alias = dir.join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    for name in [
        "unrelated_op_document_reads",
        "automatic_compaction_is_post_commit_and_does_not_parse_below_threshold",
        "move_registration_checks_foreign_paths_and_old_path_errors",
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name])
            .env("TMPDIR", &alias)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name} with an aliased TMPDIR failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn live_lease_defers() {
    let f = Fixture::new();
    let item = f.add("leased", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    let duty = dispatch["duty"].as_str().unwrap();
    let lease = schedule::transaction(&f.store, &f.worker, |st, actor| {
        ops::duty_respond(st, now_ms(), actor, duty, None, true, None)?;
        let lease = ops::lease_acquire(
            st,
            now_ms(),
            actor,
            "src/file.rs",
            LeaseMode::Exclusive,
            None,
            "test",
        )?;
        ops::duty_progress(
            st,
            now_ms(),
            actor,
            duty,
            DutyStatus::Done,
            Some("verified"),
        )?;
        Ok(lease.ids[0].clone())
    })
    .unwrap();
    f.complete(&item);
    assert_eq!(f.document().items.len(), 1);
    let result = f.call("schedule_archive", json!({})).unwrap();
    assert_eq!(result["deferred"].as_array().unwrap().len(), 1);
    schedule::transaction(&f.store, &f.worker, |st, actor| {
        ops::lease_release(st, now_ms(), actor, &lease)
    })
    .unwrap();
    f.add("trigger", &[]);
    assert_eq!(f.document().retired.len(), 1);
}

#[test]
fn unmaterialized_token_blocks() {
    let f = Fixture::new();
    let item = f.add("reserved", &[]);
    let result = schedule::reserve(&f.store, &f.root, &json!({"item":item,"key":"reserved","assignee":f.worker_id,
        "spec":{"intent":"test","outcome":"deliver","scope":["src/**"],"decision_rights":"internals","escalate_on":[],"done_criteria":"tests"}})).unwrap();
    f.call("schedule_cancel", json!({"item":item,"reason":"cancel"}))
        .unwrap();
    assert_eq!(f.document().items.len(), 1);
    assert!(
        f.call("schedule_archive", json!({})).unwrap()["retired"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    f.call(
        "schedule_abandon",
        json!({"item":item,"token":result["token"],"reason":"never materialized"}),
    )
    .unwrap();
    assert!(f.document().items.is_empty());
    assert!(f.store.read().unwrap().duties.is_empty());
}

#[test]
fn dependencies_retired_completion_and_cancelled_cycle() {
    let f = Fixture::new();
    let first = f.add("first", &[]);
    let second = f.add("second", &[&first]);
    f.complete(&first);
    assert_eq!(
        f.call("schedule_show", json!({"item":second})).unwrap()["stage"],
        "ready"
    );
    f.dispatch(&second, "work").unwrap();
    let cancelled = f.add("cancelled", &[]);
    let blocked = f.add("blocked", &[&cancelled]);
    f.call(
        "schedule_cancel",
        json!({"item":cancelled,"reason":"cancel"}),
    )
    .unwrap();
    let shown = f.call("schedule_show", json!({"item":blocked})).unwrap();
    assert_eq!(shown["stage"], "dependency_wait");
    assert_eq!(
        shown["reasons"],
        json!([format!("waiting for {cancelled} acceptance")])
    );
    let mut document = f.document();
    document
        .retired
        .iter_mut()
        .find(|r| r.id == cancelled)
        .unwrap()
        .depends_on = vec![blocked];
    f.write(&document);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
}

#[test]
fn allocators_preserve_highest_retired_ids_and_limit_active_only() {
    let f = Fixture::new();
    let item = f.add("retire high token", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    f.done(dispatch["duty"].as_str().unwrap());
    f.complete(&item);
    let next = f.add("next", &[]);
    assert_eq!(next, "w-2");
    let second = f.dispatch(&next, "work").unwrap();
    assert_eq!(second["token"], "x-2");
    let original = f.document();
    let mut invalid = original.clone();
    invalid.next_dispatch = 1;
    f.write(&invalid);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
    invalid = original.clone();
    invalid.next_item = 1;
    f.write(&invalid);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
    invalid = original.clone();
    invalid.retired[0].id = next.clone();
    f.write(&invalid);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
    let mut many = original;
    let template = many.items[0].clone();
    for n in 3..=2001 {
        let mut item = template.clone();
        item.id = format!("w-{n}");
        item.dispatches.clear();
        many.items.push(item);
    }
    many.next_item = 2002;
    f.write(&many);
    assert_eq!(
        schedule::read_document(&f.path)
            .unwrap()
            .schedule
            .items
            .len(),
        2000
    );
    assert_eq!(
        f.call("schedule_show", json!({"item":item})).unwrap()["stage"],
        "complete"
    );
}

#[test]
fn retired_token_replay_never_issues_duty() {
    let f = Fixture::new();
    let item = f.add("done", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    f.done(dispatch["duty"].as_str().unwrap());
    f.complete(&item);
    let count = f.store.read().unwrap().duties.len();
    let error = f.dispatch(&item, "work").unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
    assert!(error.message.contains("retired"));
    let error = f
        .call(
            "schedule_reconcile",
            json!({"item":item,"token":dispatch["token"]}),
        )
        .unwrap_err();
    assert!(error.message.contains("retired"));
    assert_eq!(f.store.read().unwrap().duties.len(), count);
}

#[test]
fn crash_between_writes() {
    let f = Fixture::new();
    let item = f.add("crash", &[]);

    fs::create_dir(f.archive_path()).unwrap();
    f.complete(&item);
    let committed_item = fs::read(&f.path).unwrap();
    fs::remove_dir(f.archive_path()).unwrap();
    f.call("schedule_archive", json!({})).unwrap();
    fs::write(&f.path, committed_item).unwrap();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(f.archive_path())
        .unwrap();
    use std::io::Write;
    file.write_all(b"{torn tail").unwrap();
    drop(file);
    f.call("schedule_archive", json!({})).unwrap();
    assert!(f.document().items.is_empty());
    let lines = fs::read_to_string(f.archive_path()).unwrap();
    assert!(
        lines
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).is_ok())
            .count()
            >= 1
    );
    assert_eq!(
        f.call("schedule_show", json!({"item":item})).unwrap()["title"],
        "crash"
    );
}

#[test]
fn corrupt_archive_hash_and_missing_line_are_rejected() {
    let f = Fixture::new();
    let item = f.add("original", &[]);
    f.complete(&item);
    let original = fs::read_to_string(f.archive_path()).unwrap();
    fs::write(f.archive_path(), original.replace("original", "modified")).unwrap();
    assert_eq!(
        f.call("schedule_show", json!({"item":item}))
            .unwrap_err()
            .reason,
        Reason::StoreCorrupt
    );
    assert_eq!(
        f.call("schedule_show", json!({"all":true}))
            .unwrap_err()
            .reason,
        Reason::StoreCorrupt
    );
    fs::remove_file(f.archive_path()).unwrap();
    assert_eq!(
        f.call("schedule_show", json!({"item":item}))
            .unwrap_err()
            .reason,
        Reason::StoreCorrupt
    );
    f.add("active remains usable", &[]);
}

#[test]
fn revision_and_io_failure() {
    let f = Fixture::new();
    let item = f.add("done", &[]);

    fs::create_dir(f.archive_path()).unwrap();
    let revision = f.document().revision;
    f.complete(&item);
    assert_eq!(f.document().revision, revision + 1);
    assert_eq!(f.document().items.len(), 1);
    assert_eq!(
        f.call("schedule_show", json!({})).unwrap()["items"],
        json!([])
    );
    assert!(f.call("schedule_archive", json!({})).is_err());
    fs::remove_dir(f.archive_path()).unwrap();
    assert_eq!(
        f.call("schedule_archive", json!({"expected_revision":revision}))
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
    f.call("schedule_archive", json!({"expected_revision":revision+1}))
        .unwrap();
    assert_eq!(f.document().revision, revision + 2);
}

#[test]
fn compact_output_keeps_cancelled_live_duties() {
    let f = Fixture::new();
    let complete = f.add("complete", &[]);
    f.complete(&complete);
    let cancelled = f.add("cancelled without duty", &[]);
    f.call(
        "schedule_cancel",
        json!({"item":cancelled,"reason":"cancel"}),
    )
    .unwrap();
    let live = f.add("cancelled with live duty", &[]);
    let dispatch = f.dispatch(&live, "work").unwrap();
    f.call("schedule_cancel", json!({"item":live,"reason":"cancel"}))
        .unwrap();
    let shown = f.call("schedule_show", json!({})).unwrap();
    assert_eq!(shown["items"].as_array().unwrap().len(), 1);
    assert_eq!(shown["items"][0]["id"], live);
    assert_eq!(shown["items"][0]["stage"], "cancelled");
    assert!(shown["items"][0]["dispatches"][0].get("spec").is_none());
    assert!(shown["items"][0]["dispatches"][0].get("source").is_none());
    let detail = f.call("schedule_show", json!({"item":live})).unwrap();
    assert_eq!(detail["dispatches"][0]["spec"]["outcome"], "deliver");
    assert_eq!(
        f.call("schedule_show", json!({"all":true})).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::duty_cancel(
            st,
            now_ms(),
            actor,
            dispatch["duty"].as_str().unwrap(),
            "cleanup",
        )
    })
    .unwrap();
    assert!(
        f.call("schedule_show", json!({})).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unrelated_op_document_reads() {
    let f = Fixture::new();
    schedule::document_read_count(&f.path, true);
    fs::write(&f.path, "broken").unwrap();
    schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::record_add(st, now_ms(), actor, RecordKind::Note, &[], "unrelated")
    })
    .unwrap();
    assert_eq!(schedule::document_read_count(&f.path, false), 0);
    // A linked guard is the control: the counter observes a real document read.
    let f = Fixture::new();
    let item = f.add("linked", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    schedule::document_read_count(&f.path, true);
    schedule::transaction(&f.store, &f.worker, |st, actor| {
        ops::duty_respond(
            st,
            now_ms(),
            actor,
            dispatch["duty"].as_str().unwrap(),
            None,
            true,
            None,
        )
    })
    .unwrap();
    assert_eq!(schedule::document_read_count(&f.path, false), 1);
}

#[test]
fn many_tombstones_validate() {
    let f = Fixture::new();
    let item = f.add("active", &[]);
    let mut document = f.document();
    for n in 2..=5001 {
        document.retired.push(schedule::Retired {
            id: format!("w-{n}"),
            state: schedule::RetiredState::Complete,
            digest: "0123456789abcdef".into(),
            last_dispatch: 0,
            depends_on: vec![],
        });
    }
    document.next_item = 5002;
    document.items[0].depends_on = vec!["w-5001".into()];
    f.write(&document);
    let start = std::time::Instant::now();
    let parsed = schedule::read_document(&f.path).unwrap();
    eprintln!("5000 tombstones validation: {:?}", start.elapsed());
    assert_eq!(parsed.schedule.retired.len(), 5000);
    assert_eq!(
        f.call("schedule_show", json!({"item":item})).unwrap()["stage"],
        "ready"
    );
    document.retired[0].state = schedule::RetiredState::Cancelled;
    document.retired[0].depends_on = vec![item.clone()];
    document.items[0].depends_on = vec!["w-2".into()];
    f.write(&document);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
}

#[test]
fn move_registration_requires_missing_old_path_and_copied_archive() {
    let f = Fixture::new();
    let item = f.add("retired", &[]);
    f.complete(&item);
    let destination = f.path.with_file_name("moved.md");
    fs::copy(&f.path, &destination).unwrap();
    assert!(
        f.call("schedule_move", json!({"path":destination}))
            .is_err()
    );
    fs::remove_file(&f.path).unwrap();
    assert_eq!(
        f.call("schedule_move", json!({"path":destination}))
            .unwrap_err()
            .reason,
        Reason::StoreCorrupt
    );
    fs::rename(
        f.archive_path(),
        destination.with_file_name("moved.md.archive.jsonl"),
    )
    .unwrap();
    assert_eq!(
        schedule::call(
            &f.store,
            &QuietPager,
            &f.worker,
            "schedule_move",
            &json!({"path":destination})
        )
        .unwrap_err()
        .reason,
        Reason::NoGrant
    );
    f.call("schedule_move", json!({"path":destination}))
        .unwrap();
    assert_eq!(
        f.call("schedule_show", json!({"item":item})).unwrap()["title"],
        "retired"
    );
    f.add("after move", &[]);
    assert!(!f.path.exists());
}

#[test]
fn bu_close_lists_open_items_and_allows_explicit_transfer() {
    let f = Fixture::new();
    let child = f.child_bu();
    let item = f.add("child work", &[]);
    f.call("schedule_update", json!({"item":item,"owner_bu":child}))
        .unwrap();
    let error = schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::bu_close(st, now_ms(), actor, &child)
    })
    .unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
    assert!(error.message.contains(&item));
    assert!(error.message.contains("--owner-bu"));
    assert_eq!(f.store.read().unwrap().bus[&child].status, BuStatus::Open);
    f.call("schedule_update", json!({"item":item,"owner_bu":f.bu}))
        .unwrap();
    schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::bu_close(st, now_ms(), actor, &child)
    })
    .unwrap();
    assert_eq!(f.store.read().unwrap().bus[&child].status, BuStatus::Closed);
}

#[test]
fn bu_close_refuses_unreadable_schedule_but_cancel_does_not_read_it() {
    let f = Fixture::new();
    let child = f.child_bu();
    let before = fs::read(f.store.home().join("state.json")).unwrap();
    fs::write(&f.path, "broken").unwrap();
    assert!(
        schedule::transaction(&f.store, &f.root, |st, actor| {
            ops::bu_close(st, now_ms(), actor, &child)
        })
        .is_err()
    );
    assert_eq!(fs::read(f.store.home().join("state.json")).unwrap(), before);
    schedule::document_read_count(&f.path, true);
    schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::bu_cancel(st, now_ms(), actor, &child, "cancel")
    })
    .unwrap();
    assert_eq!(schedule::document_read_count(&f.path, false), 0);
    assert_eq!(
        f.store.read().unwrap().bus[&child].status,
        BuStatus::Cancelled
    );
}

#[test]
fn bu_close_refuses_a_replaced_schedule_identity() {
    let f = Fixture::new();
    let child = f.child_bu();
    let mut document = f.document();
    document.id = "replaced-schedule".into();
    document.archive.as_mut().unwrap().chain = gaebu::evidence::sha256_hex(&document.id);
    f.write(&document);
    let error = schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::bu_close(st, now_ms(), actor, &child)
    })
    .unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
    assert!(error.message.contains("identity changed"));
    assert_eq!(f.store.read().unwrap().bus[&child].status, BuStatus::Open);
}

#[test]
fn measured_shape() {
    let f = Fixture::new();
    let first = f.add(&"measured work ".repeat(30), &[]);
    let mut document = f.document();
    let template = document.items[0].clone();
    for n in 2..=76 {
        let mut item = template.clone();
        item.id = format!("w-{n}");
        document.items.push(item);
    }
    document.next_item = 77;
    let by = document.items[0].owner_bu.clone().unwrap();
    for item in document.items.iter_mut().take(43) {
        item.completion = Some(schedule::Completion {
            by: by.clone(),
            at: now_ms(),
            evidence: "reviewed".repeat(100),
        });
    }
    document.items[43].cancellation = Some(schedule::Decision {
        by,
        at: now_ms(),
        reason: "cancelled".into(),
    });
    f.write(&document);
    f.call("schedule_archive", json!({})).unwrap();
    let actual = fs::read(&f.path).unwrap().len();
    let mut open_only = f.document();
    assert_eq!(open_only.items.len(), 32);
    assert!(open_only.retired.is_empty());
    assert_eq!(open_only.archive.as_ref().unwrap().rows, 44);
    open_only.retired.clear();
    let baseline = format!(
        "# Test\n<!-- gaebu-schedule -->\n```json\n{}\n```\n",
        serde_json::to_string_pretty(&open_only).unwrap()
    )
    .len();
    eprintln!(
        "76-item fixture: active={actual} open-only={baseline} overhead={}",
        actual - baseline
    );
    assert!(actual - baseline <= 6144);
    assert_eq!(
        f.call("schedule_show", json!({"item":first})).unwrap()["stage"],
        "complete"
    );
}

#[test]
fn tombstone_validation_rejects_malformed_metadata_and_repeated_dependencies() {
    let f = Fixture::new();
    let first = f.add("first", &[]);
    let cancelled = f.add("cancelled", &[&first]);
    f.add("keep tombstones", &[&first, &cancelled]);
    f.complete(&first);
    f.call(
        "schedule_cancel",
        json!({"item":cancelled,"reason":"cancel"}),
    )
    .unwrap();
    let original = f.document();
    let encoded = serde_json::to_value(&original).unwrap();
    assert!(encoded["retired"][0].get("last_dispatch").is_none());
    for (id, digest, dependencies) in [
        ("w-0", "0123456789abcdef", vec![]),
        ("bad", "0123456789abcdef", vec![]),
        ("w-1", "nothex0123456789", vec![]),
        ("w-1", "short", vec![]),
        ("w-1", "0123456789abcdef", vec![cancelled.as_str()]),
    ] {
        let mut document = original.clone();
        document.retired[0].id = id.into();
        document.retired[0].digest = digest.into();
        document.retired[0].depends_on = dependencies.into_iter().map(str::to_owned).collect();
        f.write(&document);
        assert_eq!(
            schedule::read_document(&f.path).unwrap_err().reason,
            Reason::InvalidArgument
        );
    }
    let mut document = original;
    document.retired[1].depends_on = vec![first.clone(), first];
    f.write(&document);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
}

#[test]
fn append_truncates_uncommitted_archive_rows() {
    for wrong_schedule in [true, false] {
        let f = Fixture::new();
        let item = f.add("complete", &[]);

        fs::create_dir(f.archive_path()).unwrap();
        f.complete(&item);
        let schedule = f.document();
        let item = &schedule.items[0];
        let digest =
            gaebu::evidence::sha256_hex(&serde_json::to_string(item).unwrap())[..16].to_owned();
        fs::remove_dir(f.archive_path()).unwrap();
        let wrong = json!({"schedule_id":if wrong_schedule {"foreign"} else {&schedule.id},
            "digest":if wrong_schedule {&digest} else {"0000000000000000"},"item":item});
        fs::write(f.archive_path(), format!("{wrong}\n")).unwrap();
        f.call("schedule_archive", json!({})).unwrap();
        assert_eq!(
            fs::read_to_string(f.archive_path())
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(
            f.call("schedule_show", json!({"item":item.id})).unwrap()["title"],
            "complete"
        );
    }
}

#[test]
fn move_registration_rejects_a_writer_with_an_old_registration() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let destination = f.path.with_file_name("moved.md");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.store.home().join("state.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let store = Store::open(f.store.home()).unwrap();
    let caller = f.root.clone();
    let worker = std::thread::spawn(move || {
        schedule::call(
            &store,
            &QuietPager,
            &caller,
            "schedule_add",
            &json!({"title":"stale write","scope":["src/**"]}),
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
            Err(error) => panic!("unexpected lock failure: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "writer did not acquire its Schedule lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    fs::rename(&f.path, &destination).unwrap();
    let mut st = f.store.read_current().unwrap();
    let team = authz::member_for_session(&st, "root-session")
        .unwrap()
        .team
        .clone();
    st.schedules.get_mut(&team).unwrap().path = destination.to_string_lossy().into_owned();
    st.rev += 1;
    fs::write(
        f.store.home().join("state.json"),
        serde_json::to_vec(&st).unwrap(),
    )
    .unwrap();
    drop(lock);
    let error = worker.join().unwrap().unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
    assert!(error.message.contains("schedule registration changed"));
    assert!(!f.path.exists());
    assert!(
        schedule::read_document(&destination)
            .unwrap()
            .schedule
            .items
            .is_empty()
    );
}

#[test]
fn corrupt_archive_tombstone_disagreement_is_rejected() {
    let f = Fixture::new();
    let first = f.add("first", &[]);
    let second = f.add("second", &[]);
    let cancelled = f.add("cancelled", &[&first]);
    f.add("keep tombstones", &[&first, &second, &cancelled]);
    f.complete(&first);
    f.complete(&second);
    f.call(
        "schedule_cancel",
        json!({"item":cancelled,"reason":"cancel"}),
    )
    .unwrap();
    let original = f.document();
    for (index, state, dependencies, token) in [
        (0, schedule::RetiredState::Cancelled, vec![], 0),
        (2, schedule::RetiredState::Complete, vec![], 0),
        (2, schedule::RetiredState::Cancelled, vec![second], 0),
        (0, schedule::RetiredState::Complete, vec![], 1),
    ] {
        let mut document = original.clone();
        document.retired[index].state = state;
        document.retired[index].depends_on = dependencies;
        document.retired[index].last_dispatch = token;
        document.next_dispatch = 2;
        f.write(&document);
        assert_eq!(
            f.call("schedule_show", json!({"item":document.retired[index].id}))
                .unwrap_err()
                .reason,
            Reason::StoreCorrupt
        );
    }
}

#[test]
fn dependencies_accept_only_the_named_predecessor() {
    let f = Fixture::new();
    let complete = f.add("complete but different", &[]);
    let pending = f.add("pending", &[]);
    let dependent = f.add("dependent", &[&pending]);
    // Keep the completed item active to exercise the active completion join.

    fs::create_dir(f.archive_path()).unwrap();
    f.complete(&complete);
    assert_eq!(
        f.call("schedule_show", json!({"item":dependent})).unwrap()["stage"],
        "dependency_wait"
    );
    assert_eq!(
        f.dispatch(&dependent, "work").unwrap_err().reason,
        Reason::Conflict
    );
}

#[test]
fn retired_linked_duty_keeps_the_completed_guard_policy() {
    let f = Fixture::new();
    let item = f.add("done", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    let duty = dispatch["duty"].as_str().unwrap();
    f.done(duty);
    f.complete(&item);
    let before = fs::read(f.store.home().join("state.json")).unwrap();
    let error = schedule::transaction(&f.store, &f.worker, |st, _| {
        st.duties.get_mut(duty).unwrap().note = Some("amended after acceptance".into());
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
    assert!(error.message.contains("already complete"));
    assert_eq!(fs::read(f.store.home().join("state.json")).unwrap(), before);
}

#[test]
fn move_registration_checks_foreign_paths_and_old_path_errors() {
    let f = Fixture::new();
    let destination = f.path.with_file_name("moved.md");
    fs::copy(&f.path, &destination).unwrap();
    let st = f.store.read_current().unwrap();
    let team = authz::member_for_session(&st, "root-session")
        .unwrap()
        .team
        .clone();
    let registration = st.schedules[&team].clone();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.schedules.get_mut(&team).unwrap().path = f
                .path
                .with_file_name("invalid\0path")
                .to_string_lossy()
                .into_owned();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.call("schedule_move", json!({"path":destination}))
            .unwrap_err()
            .reason,
        Reason::StoreWriteFailed
    );
    fs::remove_file(&f.path).unwrap();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.schedules.insert(team.clone(), registration.clone());
            let mut foreign = registration.clone();
            foreign.schedule_id = "another-schedule".into();
            foreign.path = destination.to_string_lossy().into_owned();
            st.schedules.insert("foreign-team".into(), foreign);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.call("schedule_move", json!({"path":destination}))
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.schedules.get_mut("foreign-team").unwrap().path = f
                .path
                .with_file_name("other.md")
                .to_string_lossy()
                .into_owned();
            Ok(())
        })
        .unwrap();
    f.call("schedule_move", json!({"path":destination}))
        .unwrap();
}

#[test]
fn tombstone_ids_reject_zero_and_non_item_ids_without_dependency_errors() {
    let f = Fixture::new();
    let item = f.add("only item", &[]);
    f.add("keep tombstone", &[&item]);
    f.complete(&item);
    let original = f.document();
    for id in ["w-0", "bad", "w-18446744073709551616"] {
        let mut document = original.clone();
        document.retired[0].id = id.into();
        f.write(&document);
        let error = schedule::read_document(&f.path).unwrap_err();
        assert_eq!(error.reason, Reason::InvalidArgument);
        assert!(error.message.contains("invalid item id"));
    }
}

#[test]
fn corrupt_archive_completion_marker_cannot_disagree_with_complete_tombstone() {
    let f = Fixture::new();
    let item = f.add("complete", &[]);
    f.add("keep tombstone", &[&item]);
    f.complete(&item);
    let original: Value =
        serde_json::from_str(&fs::read_to_string(f.archive_path()).unwrap()).unwrap();
    let document = f.document();
    for cancelled in [false, true] {
        let mut entry = original.clone();
        if cancelled {
            entry["item"]["cancellation"] = json!({"by":entry["item"]["completion"]["by"],"at":now_ms(),"reason":"contradictory cancellation"});
        } else {
            entry["item"]["completion"] = Value::Null;
        }
        let item: schedule::Item = serde_json::from_value(entry["item"].clone()).unwrap();
        let digest =
            gaebu::evidence::sha256_hex(&serde_json::to_string(&item).unwrap())[..16].to_owned();
        entry["digest"] = json!(digest);
        let mut changed = document.clone();
        changed.retired[0].digest = digest;
        f.write(&changed);
        fs::write(f.archive_path(), format!("{entry}\n")).unwrap();
        commit_fixture(&f, &mut changed);
        assert_eq!(
            f.call("schedule_show", json!({"item":item.id}))
                .unwrap_err()
                .reason,
            Reason::StoreCorrupt
        );
    }
}

#[test]
fn archived_history_and_explicit_compact_flow_through_mcp() {
    let f = Fixture::new();
    let item = f.add("MCP archived contract", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    let duty = dispatch["duty"].as_str().unwrap();
    f.done(duty);
    f.complete(&item);
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.duties.get_mut(duty).unwrap().updated_at = now_ms() - 25 * 60 * 60 * 1000;
            Ok(())
        })
        .unwrap();
    f.store
        .session("root-session")
        .update_sighting(|sighting| {
            sighting.tool = Some(Tool::Codex);
            sighting.last_seen = Some(now_ms());
        })
        .unwrap();
    assert_eq!(f.mcp("duty_get", json!({"duty":duty}))["id"], duty);
    assert_eq!(f.mcp("compact", json!({}))["duties"], 1);
    let archived = f.mcp("duty_get", json!({"duty":duty}));
    assert_eq!(archived["outcome"], "deliver");
    assert_eq!(archived["note"], "verified");
    assert_eq!(archived["schedule_ref"]["item_id"], item);
    assert!(!f.store.read().unwrap().duties.contains_key(duty));
    let detail = f.mcp("schedule_show", json!({"item":item}));
    assert_eq!(detail["dispatches"][0]["status"], "archived");
    assert_eq!(detail["dispatches"][0]["spec"]["outcome"], "deliver");
    assert_eq!(
        f.mcp("schedule_show", json!({"all":true}))["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn automatic_compaction_is_post_commit_and_does_not_parse_below_threshold() {
    let f = Fixture::new();
    let item = f.add("auto archived", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    let duty = dispatch["duty"].as_str().unwrap();
    f.done(duty);
    f.complete(&item);
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.duties.get_mut(duty).unwrap().updated_at = now_ms() - 25 * 60 * 60 * 1000;
            let record = st.records[0].clone();
            st.records.clear();
            for n in 0..63 {
                let mut record = record.clone();
                record.id = format!("r-archive-{n}");
                record.at = now_ms() - 2 * 60 * 60 * 1000;
                st.records.push(record);
            }
            Ok(())
        })
        .unwrap();
    schedule::document_read_count(&f.path, true);
    schedule::transaction(&f.store, &f.root, |st, actor| {
        ops::record_add(
            st,
            now_ms(),
            actor,
            RecordKind::Note,
            &[],
            "committed before maintenance",
        )
    })
    .unwrap();
    assert_eq!(schedule::document_read_count(&f.path, false), 1);
    let st = f.store.read().unwrap();
    assert!(!st.duties.contains_key(duty));
    assert!(
        st.records
            .iter()
            .any(|record| record.text == "committed before maintenance")
    );
}

#[test]
fn show_all_large_archive_uses_a_bounded_scan() {
    let f = Fixture::new();
    f.add("history", &[]);
    let mut document = f.document();
    let mut template = document.items[0].clone();
    template.completion = Some(schedule::Completion {
        by: template.owner_bu.clone().unwrap(),
        at: now_ms(),
        evidence: "reviewed".into(),
    });
    document.items = (1..=2000)
        .map(|n| {
            let mut item = template.clone();
            item.id = format!("w-{n}");
            item
        })
        .collect();
    document.next_item = 2001;
    f.write(&document);
    f.call("schedule_archive", json!({})).unwrap();
    let start = std::time::Instant::now();
    let all = f.call("schedule_show", json!({"all":true})).unwrap();
    let elapsed = start.elapsed();
    eprintln!("2000 archived items show --all: {elapsed:?}");
    assert_eq!(all["items"].as_array().unwrap().len(), 2000);
    assert!(elapsed < std::time::Duration::from_secs(5));
}

#[test]
fn auto_failure_keeps_operation_committed() {
    for cancel_bu in [true, false] {
        let f = Fixture::new();
        let target = if cancel_bu {
            f.child_bu()
        } else {
            let item = f.add("cancel duty", &[]);
            f.dispatch(&item, "work").unwrap()["duty"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        f.store
            .transact(CLI_DEADLINE, |st| {
                let record = st.records[0].clone();
                st.records.clear();
                for n in 0..64 {
                    let mut record = record.clone();
                    record.id = format!("r-history-{n}");
                    record.at = now_ms() - 2 * 60 * 60 * 1000;
                    st.records.push(record);
                }
                Ok(())
            })
            .unwrap();
        fs::write(f.store.home().join("archive"), "I/O failure").unwrap();
        schedule::transaction(&f.store, &f.root, |st, actor| {
            if cancel_bu {
                ops::bu_cancel(st, now_ms(), actor, &target, "stop")
            } else {
                ops::duty_cancel(st, now_ms(), actor, &target, "stop")
            }
        })
        .unwrap();
        let st = f.store.read().unwrap();
        if cancel_bu {
            assert_eq!(st.bus[&target].status, BuStatus::Cancelled);
        } else {
            assert_eq!(st.duties[&target].status, DutyStatus::Cancelled);
        }
        assert!(st.records.iter().any(|record| record.id == "r-history-0"));
    }
}

#[test]
fn intrinsic_item_validation_rejects_invalid_pm_paths_and_dispatch_history() {
    let f = Fixture::new();
    let item = f.add("history", &[]);
    let first = f.dispatch(&item, "work").unwrap();
    f.done(first["duty"].as_str().unwrap());
    f.call(
        "schedule_retire",
        json!({"item":item,"token":first["token"],"reason":"known done"}),
    )
    .unwrap();
    let spec = f.document().items[0].dispatches[0].spec.clone();
    f.call(
        "schedule_dispatch",
        json!({"item":item,"key":"work","assignee":f.worker_id,
        "predecessor":first["token"],"spec":spec}),
    )
    .unwrap();
    let original = f.document();
    for path in ["/absolute", "a\\b", "a:b", "a*b", "a//b", "a/./b", "a/../b"] {
        let mut document = original.clone();
        document.items[0].pm_ref = Some(schedule::PmRef {
            task: "KEY".into(),
            item: "work".into(),
            plan: Some(path.into()),
        });
        f.write(&document);
        assert_eq!(
            schedule::read_document(&f.path).unwrap_err().reason,
            Reason::InvalidArgument,
            "{path}"
        );
    }
    for case in [
        "zero token",
        "missing predecessor",
        "wrong predecessor key",
        "unretired predecessor",
        "self predecessor",
    ] {
        let mut document = original.clone();
        let dispatches = &mut document.items[0].dispatches;
        match case {
            "zero token" => dispatches[1].token = "x-0".into(),
            "missing predecessor" => dispatches[1].predecessor = None,
            "wrong predecessor key" => dispatches[1].key = "different".into(),
            "unretired predecessor" => dispatches[0].retired = None,
            "self predecessor" => {
                dispatches[1].predecessor = Some(dispatches[1].token.clone());
                dispatches[1].retired = dispatches[0].retired.clone();
            }
            _ => unreachable!(),
        }
        // Recompute the public, unsigned manifest digest so validation must
        // reject the malformed history itself, rather than a stale checksum.
        for dispatch in dispatches {
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
        }
        f.write(&document);
        assert_eq!(
            schedule::read_document(&f.path).unwrap_err().reason,
            Reason::InvalidArgument,
            "{case}"
        );
    }
}

#[test]
fn corrupt_archive_dependencies_are_validated_per_item() {
    let f = Fixture::new();
    let item = f.add("archived", &[]);
    let other = f.add("other", &[]);
    f.add("keep tombstones", &[&item, &other]);
    f.complete(&item);
    f.complete(&other);
    let original = f.document();
    let archive = fs::read_to_string(f.archive_path()).unwrap();
    let entries: Vec<Value> = archive
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for deps in [
        vec!["missing".to_owned()],
        vec![item.clone()],
        vec![other.clone(), other.clone()],
    ] {
        let mut entry = entries[0].clone();
        entry["item"]["depends_on"] = json!(deps);
        let stored: schedule::Item = serde_json::from_value(entry["item"].clone()).unwrap();
        let digest =
            gaebu::evidence::sha256_hex(&serde_json::to_string(&stored).unwrap())[..16].to_owned();
        entry["digest"] = json!(digest);
        let mut document = original.clone();
        document.retired[0].digest = digest;
        f.write(&document);
        fs::write(f.archive_path(), format!("{entry}\n{}\n", entries[1])).unwrap();
        commit_fixture(&f, &mut document);
        assert_eq!(
            f.call("schedule_show", json!({"item":item}))
                .unwrap_err()
                .reason,
            Reason::StoreCorrupt
        );
    }
}

#[test]
fn runtime_join_rejects_independently_corrupt_item_or_manifest_links() {
    for field in ["item", "manifest"] {
        let f = Fixture::new();
        let item = f.add("join", &[]);
        let dispatch = f.dispatch(&item, "work").unwrap();
        let duty = dispatch["duty"].as_str().unwrap();
        f.store
            .transact(CLI_DEADLINE, |st| {
                let link = st
                    .duties
                    .get_mut(duty)
                    .unwrap()
                    .schedule_ref
                    .as_mut()
                    .unwrap();
                if field == "item" {
                    link.item_id = "w-999".into();
                } else {
                    link.manifest_hash = "corrupt".into();
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            f.call("schedule_show", json!({"item":item})).unwrap()["dispatches"][0]["status"],
            "unknown"
        );
        assert_eq!(
            f.dispatch(&item, "work").unwrap_err().reason,
            Reason::Conflict
        );
        assert_eq!(f.store.read().unwrap().duties.len(), 1);
    }
}

#[test]
fn automatic_compaction_skips_a_replaced_registration() {
    let f = Fixture::new();
    f.store
        .transact(CLI_DEADLINE, |st| {
            let record = st.records[0].clone();
            st.records.clear();
            for n in 0..64 {
                let mut record = record.clone();
                record.id = format!("r-old-{n}");
                record.at = now_ms() - 2 * 60 * 60 * 1000;
                st.records.push(record);
            }
            Ok(())
        })
        .unwrap();
    let new_path = f.path.with_file_name("new-registration.md");
    schedule::transaction(&f.store, &f.root, |st, actor| {
        let team = st.members[&actor.member].team.clone();
        st.schedules.get_mut(&team).unwrap().path = new_path.to_string_lossy().into_owned();
        ops::record_add(
            st,
            now_ms(),
            actor,
            RecordKind::Note,
            &[],
            "registration changed",
        )
    })
    .unwrap();
    let st = f.store.read_current().unwrap();
    assert_eq!(st.records.len(), 65);
    assert!(st.archives.is_empty());
}

#[test]
fn prune_drops_unreferenced_tombstones() {
    let f = Fixture::new();
    let item = f.add("cancel", &[]);
    let next = f.document().next_item;
    f.call("schedule_cancel", json!({"item":item,"reason":"stop"}))
        .unwrap();
    let document = f.document();
    assert!(document.retired.is_empty());
    assert_eq!(document.archive.unwrap().rows, 1);
    assert_eq!(document.next_item, next);
}

fn age_duty(f: &Fixture, duty: &str) {
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.duties.get_mut(duty).unwrap().updated_at = now_ms() - 25 * 60 * 60 * 1000;
            Ok(())
        })
        .unwrap();
}

#[test]
fn prune_waits_for_hot_linked_duty() {
    let f = Fixture::new();
    let item = f.add("done", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    let duty = dispatch["duty"].as_str().unwrap();
    f.done(duty);
    f.complete(&item);
    assert_eq!(f.document().retired[0].id, item);
    // Retention considers every team, even if the linked duty has been moved.
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.duties.get_mut(duty).unwrap().team = "another-team".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.call("schedule_archive", json!({})).unwrap()["pruned"],
        json!([])
    );
    let st = f.store.read_current().unwrap();
    let team = st.members[&f.worker_id].team.clone();
    f.store
        .transact(CLI_DEADLINE, |st| {
            st.duties.get_mut(duty).unwrap().team = team;
            Ok(())
        })
        .unwrap();
    age_duty(&f, duty);
    assert_eq!(schedule::compact(&f.store, &f.root).unwrap()["duties"], 1);
    assert_eq!(
        f.call("schedule_archive", json!({})).unwrap()["pruned"],
        json!([item])
    );
    assert!(f.document().retired.is_empty());
}

#[test]
fn recovery_after_prune_keeps_guard_and_compaction() {
    for cancelled in [false, true] {
        let f = Fixture::new();
        let item = f.add("restore duty", &[]);
        let dispatch = f.dispatch(&item, "work").unwrap();
        let duty = dispatch["duty"].as_str().unwrap();
        f.done(duty);
        if cancelled {
            f.call("schedule_cancel", json!({"item":item,"reason":"stop"}))
                .unwrap();
        } else {
            f.complete(&item);
        }
        let guard = || {
            schedule::transaction(&f.store, &f.worker, |st, _| {
                st.duties.get_mut(duty).unwrap().note = Some("change after retirement".into());
                Ok(())
            })
            .unwrap_err()
        };
        let before = guard();
        assert_eq!(
            before.reason,
            if cancelled {
                Reason::Cancelled
            } else {
                Reason::Conflict
            }
        );
        age_duty(&f, duty);
        assert_eq!(schedule::compact(&f.store, &f.root).unwrap()["duties"], 1);
        let previous = fs::read(f.store.home().join("state.prev.json")).unwrap();
        f.call("schedule_archive", json!({})).unwrap();
        assert!(f.document().retired.is_empty());
        // Restore the actual previous generation saved by compaction.
        fs::write(f.store.home().join("state.prev.json"), previous).unwrap();
        fs::write(f.store.home().join("state.json"), b"{").unwrap();
        f.store.repair().unwrap();
        assert!(f.store.read_current().unwrap().duties.contains_key(duty));
        let after = guard();
        assert_eq!(after.reason, before.reason);
        assert_eq!(after.message, before.message);
        assert_eq!(schedule::compact(&f.store, &f.root).unwrap()["duties"], 1);
        assert!(!f.store.read_current().unwrap().duties.contains_key(duty));
    }
}

#[test]
fn prune_keeps_only_live_referenced() {
    let f = Fixture::new();
    let a = f.add("A", &[]);
    let b = f.add("B", &[&a]);
    f.call("schedule_cancel", json!({"item":a,"reason":"stop"}))
        .unwrap();
    let c = f.add("C", &[&b]);
    f.call("schedule_cancel", json!({"item":b,"reason":"stop"}))
        .unwrap();
    let document = f.document();
    assert_eq!(document.retired.len(), 1);
    assert_eq!(document.retired[0].id, b);
    assert_eq!(document.retired[0].depends_on, vec![a]);
    assert_eq!(document.items[0].id, c);
    assert_eq!(
        f.call("schedule_show", json!({"all":true})).unwrap()["counts"]["retired"],
        2
    );
}

#[test]
fn pruned_item_reads_from_archive() {
    let f = Fixture::new();
    let item = f.add("full contract", &[]);
    f.complete(&item);
    assert!(f.document().retired.is_empty());
    let shown = f.call("schedule_show", json!({"item":item})).unwrap();
    assert_eq!(shown["title"], "full contract");
    assert_eq!(shown["completion"]["evidence"], "reviewed");
    let all = f.call("schedule_show", json!({"all":true})).unwrap();
    assert_eq!(all["items"][0], shown);
    assert_eq!(all["counts"]["retired"], 1);
    assert_eq!(
        f.call("schedule_archive", json!({})).unwrap()["retired_total"],
        1
    );
}

#[test]
fn committed_prefix_detects_damage() {
    for damage in [
        "stale",
        "delete row",
        "missing",
        "duplicate",
        "schedule",
        "rows",
        "maxima",
        "dependency",
    ] {
        let f = Fixture::new();
        let item = f.add("cancelled committed", &[]);
        let mut terminal = f.document().items[0].clone();
        terminal.completion = Some(schedule::Completion {
            by: terminal.owner_bu.clone().unwrap(),
            at: now_ms(),
            evidence: "stale".into(),
        });
        let stale = json!({"schedule_id":f.document().id,"digest":gaebu::evidence::sha256_hex(&serde_json::to_string(&terminal).unwrap())[..16],"item":terminal});
        fs::write(f.archive_path(), format!("{stale}\n")).unwrap();
        f.call("schedule_cancel", json!({"item":item,"reason":"stop"}))
            .unwrap();
        let other = f.add("other", &[]);
        f.complete(&other);
        let bytes = fs::read(f.archive_path()).unwrap();
        let mut document = f.document();
        match damage {
            "stale" => {
                let mut corrupted = bytes.clone();
                corrupted[0] = b'!';
                fs::write(f.archive_path(), corrupted).unwrap();
                // A stale complete row beyond the commitment cannot rescue corruption.
                use std::io::Write;
                writeln!(
                    fs::OpenOptions::new()
                        .append(true)
                        .open(f.archive_path())
                        .unwrap(),
                    "{stale}"
                )
                .unwrap();
            }
            "delete row" => {
                let first = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
                fs::write(f.archive_path(), &bytes[first..]).unwrap();
            }
            "missing" => fs::remove_file(f.archive_path()).unwrap(),
            "duplicate" => {
                let first = bytes.split_inclusive(|byte| *byte == b'\n').next().unwrap();
                fs::write(f.archive_path(), [first, first].concat()).unwrap();
                commit_fixture(&f, &mut document);
            }
            "schedule" => {
                let text = String::from_utf8(bytes)
                    .unwrap()
                    .replace(&document.id, "foreign-schedule");
                fs::write(f.archive_path(), text).unwrap();
                commit_fixture(&f, &mut document);
            }
            "dependency" => {
                let mut entries: Vec<Value> = String::from_utf8(bytes)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                entries[0]["item"]["depends_on"] = json!([format!("w-{}", document.next_item)]);
                let item: schedule::Item =
                    serde_json::from_value(entries[0]["item"].clone()).unwrap();
                entries[0]["digest"] = json!(
                    &gaebu::evidence::sha256_hex(&serde_json::to_string(&item).unwrap())[..16]
                );
                fs::write(
                    f.archive_path(),
                    format!("{}\n{}\n", entries[0], entries[1]),
                )
                .unwrap();
                commit_fixture(&f, &mut document);
            }
            "rows" => {
                document.archive.as_mut().unwrap().rows += 1;
                f.write(&document);
            }
            _ => {
                document.archive.as_mut().unwrap().last_dispatch += 1;
                document.next_dispatch += 1;
                f.write(&document);
            }
        }
        assert_eq!(
            f.call("schedule_show", json!({"item":item}))
                .unwrap_err()
                .reason,
            Reason::StoreCorrupt,
            "{damage}"
        );
        assert_eq!(
            f.call("schedule_show", json!({"all":true}))
                .unwrap_err()
                .reason,
            Reason::StoreCorrupt,
            "{damage}"
        );
        let revision = f.document().revision;
        assert_eq!(
            f.call(
                "schedule_add",
                json!({"title":"duplicate","scope":[],"depends_on":[item,item]})
            )
            .unwrap_err()
            .reason,
            Reason::InvalidArgument
        );
        assert_eq!(
            f.call(
                "schedule_add",
                json!({"title":"dependent","scope":[],"depends_on":[item]})
            )
            .unwrap_err()
            .reason,
            Reason::StoreCorrupt,
            "{damage}"
        );
        assert_eq!(f.document().revision, revision);
    }
}

#[test]
fn sweep_refuses_damaged_committed_prefix() {
    for damage in ["missing", "short", "newline"] {
        let f = Fixture::new();
        let first = f.add("first", &[]);
        f.complete(&first);
        let item = f.add("defer", &[]);
        let head = f.document().archive;
        let mut bytes = fs::read(f.archive_path()).unwrap();
        match damage {
            "missing" => fs::remove_file(f.archive_path()).unwrap(),
            "short" => {
                bytes.truncate(bytes.len() - 2);
                fs::write(f.archive_path(), &bytes).unwrap();
            }
            _ => {
                *bytes.last_mut().unwrap() = b' ';
                fs::write(f.archive_path(), &bytes).unwrap();
            }
        }
        let damaged = fs::read(f.archive_path()).ok();
        f.complete(&item);
        assert_eq!(f.document().items[0].id, item);
        assert!(f.document().items[0].completion.is_some());
        assert_eq!(f.document().archive, head);
        assert_eq!(
            f.call("schedule_archive", json!({})).unwrap_err().reason,
            Reason::StoreCorrupt
        );
        assert_eq!(fs::read(f.archive_path()).ok(), damaged);
        assert_eq!(f.document().archive, head);
    }
}

#[test]
fn allocators_reject_regression_after_prune() {
    let f = Fixture::new();
    let item = f.add("allocated", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    let duty = dispatch["duty"].as_str().unwrap();
    f.done(duty);
    f.complete(&item);
    age_duty(&f, duty);
    schedule::compact(&f.store, &f.root).unwrap();
    f.call("schedule_archive", json!({})).unwrap();
    let original = f.document();
    assert!(original.retired.is_empty());
    let head = original.archive.as_ref().unwrap();
    assert_eq!(head.last_item, 1);
    assert_eq!(head.last_dispatch, 1);
    for dispatch in [false, true] {
        for n in [0, 1] {
            let mut document = original.clone();
            if dispatch {
                document.next_dispatch = n;
            } else {
                document.next_item = n;
            }
            f.write(&document);
            assert_eq!(
                schedule::read_document(&f.path).unwrap_err().reason,
                Reason::InvalidArgument
            );
            assert!(f.call("schedule_add", json!({"title":"refuse"})).is_err());
            assert!(f.dispatch(&item, "work").is_err());
        }
    }
}

#[test]
fn uncommitted_tail_is_ignored_and_truncated() {
    let f = Fixture::new();
    let item = f.add("committed", &[]);
    f.complete(&item);
    let original = fs::read(f.archive_path()).unwrap();
    let mut row: Value = serde_json::from_slice(&original).unwrap();
    row["item"]["id"] = json!("w-999");
    let stored: schedule::Item = serde_json::from_value(row["item"].clone()).unwrap();
    row["digest"] =
        json!(&gaebu::evidence::sha256_hex(&serde_json::to_string(&stored).unwrap())[..16]);
    use std::io::Write;
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(f.archive_path())
            .unwrap(),
        "{row}"
    )
    .unwrap();
    assert_eq!(
        f.call("schedule_show", json!({"all":true})).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.call("schedule_show", json!({"item":"w-999"}))
            .unwrap_err()
            .reason,
        Reason::NotFound
    );
    f.call("schedule_archive", json!({})).unwrap();
    assert_eq!(fs::read(f.archive_path()).unwrap(), original);
    assert_eq!(
        fs::metadata(f.archive_path()).unwrap().len(),
        f.document().archive.unwrap().bytes
    );
}

#[test]
fn pruned_dependency_resolves_at_boundary() {
    for cancelled in [false, true] {
        let f = Fixture::new();
        let predecessor = f.add("predecessor", &[]);
        if cancelled {
            f.call(
                "schedule_cancel",
                json!({"item":predecessor,"reason":"stop"}),
            )
            .unwrap();
        } else {
            f.complete(&predecessor);
        }
        let update = f.add("update target", &[]);
        for operation in ["schedule_add", "schedule_update"] {
            let before = f.document().revision;
            let mut args =
                json!({"title":"dependent","scope":["src/**"],"depends_on":[predecessor]});
            if operation == "schedule_update" {
                args["item"] = json!(update);
            }
            let result = f.call(operation, args);
            if cancelled {
                let error = result.unwrap_err();
                assert_eq!(error.reason, Reason::Conflict);
                assert!(error.message.contains(&predecessor));
                assert_eq!(f.document().revision, before);
            } else {
                let value = result.unwrap();
                assert_eq!(value["satisfied"], json!([predecessor]));
                assert_eq!(value["item"]["depends_on"], json!([]));
                let id = value["item"]["id"].as_str().unwrap();
                assert!(
                    f.document()
                        .items
                        .iter()
                        .find(|item| item.id == id)
                        .unwrap()
                        .depends_on
                        .is_empty()
                );
            }
            for deps in [
                json!([predecessor, predecessor]),
                json!(["w-999"]),
                json!(["invalid"]),
                json!([update]),
            ] {
                // Self references apply to update; add's next id differs.
                if operation == "schedule_add" && deps == json!([update]) {
                    continue;
                }
                let before = f.document().revision;
                let mut args = json!({"title":"refuse","scope":["src/**"],"depends_on":deps});
                if operation == "schedule_update" {
                    args["item"] = json!(update);
                }
                assert_eq!(
                    f.call(operation, args).unwrap_err().reason,
                    Reason::InvalidArgument
                );
                assert_eq!(f.document().revision, before);
            }
        }
    }
    for tombstone in [false, true] {
        let f = Fixture::new();
        let predecessor = f.add("cancelled", &[]);
        if tombstone {
            f.add("retainer", &[&predecessor]);
        } else {
            fs::create_dir(f.archive_path()).unwrap();
        }
        f.call(
            "schedule_cancel",
            json!({"item":predecessor,"reason":"stop"}),
        )
        .unwrap();
        let target = f.add("target", &[]);
        for operation in ["schedule_add", "schedule_update"] {
            let revision = f.document().revision;
            let mut args = json!({"title":"refuse","scope":["src/**"],"depends_on":[predecessor]});
            if operation == "schedule_update" {
                args["item"] = json!(target);
            }
            let error = f.call(operation, args).unwrap_err();
            assert_eq!(error.reason, Reason::Conflict);
            assert!(error.message.contains(&predecessor));
            assert_eq!(f.document().revision, revision);
        }
    }
    let f = Fixture::new();
    let predecessor = f.add("complete", &[]);
    f.complete(&predecessor);
    let item = f.add("immutable", &[]);
    f.dispatch(&item, "work").unwrap();
    fs::remove_file(f.archive_path()).unwrap();
    let revision = f.document().revision;
    let error = f
        .call(
            "schedule_update",
            json!({"item":item,"depends_on":[predecessor]}),
        )
        .unwrap_err();
    assert_eq!(error.reason, Reason::Conflict);
    assert!(error.message.contains("immutable"));
    assert_eq!(f.document().revision, revision);
}

#[test]
fn pruned_item_refuses_mutation() {
    let f = Fixture::new();
    let item = f.add("pruned", &[]);
    let dispatch = f.dispatch(&item, "work").unwrap();
    let duty = dispatch["duty"].as_str().unwrap();
    f.done(duty);
    f.complete(&item);
    age_duty(&f, duty);
    schedule::compact(&f.store, &f.root).unwrap();
    f.call("schedule_archive", json!({})).unwrap();
    assert!(f.document().retired.is_empty());
    let before = f.store.read_current().unwrap();
    fs::remove_file(f.archive_path()).unwrap();
    for operation in ["schedule_update", "schedule_cancel", "schedule_reconcile"] {
        let args = match operation {
            "schedule_update" => json!({"item":item,"priority":5}),
            "schedule_cancel" => json!({"item":item,"reason":"stop"}),
            _ => json!({"item":item,"token":dispatch["token"]}),
        };
        let error = f.call(operation, args).unwrap_err();
        assert_eq!(error.reason, Reason::Conflict);
        assert_eq!(
            error.message,
            format!("schedule item {item} is retired; read it with gaebu schedule show {item}")
        );
    }
    assert_eq!(
        f.store.read_current().unwrap().duties.len(),
        before.duties.len()
    );
    for id in ["w-2", "w-999", "bad", "w-0"] {
        assert_eq!(
            f.call("schedule_update", json!({"item":id,"priority":5}))
                .unwrap_err()
                .reason,
            Reason::NotFound
        );
    }
}

#[test]
fn move_requires_committed_archive_after_prune() {
    let f = Fixture::new();
    let item = f.add("history", &[]);
    f.complete(&item);
    assert!(f.document().retired.is_empty());
    let destination = f.path.with_file_name("moved.md");
    fs::rename(&f.path, &destination).unwrap();
    assert_eq!(
        f.call("schedule_move", json!({"path":destination}))
            .unwrap_err()
            .reason,
        Reason::StoreCorrupt
    );
    fs::rename(
        f.archive_path(),
        destination.with_file_name("moved.md.archive.jsonl"),
    )
    .unwrap();
    f.call("schedule_move", json!({"path":destination}))
        .unwrap();
    assert_eq!(
        f.call("schedule_show", json!({"item":item})).unwrap()["title"],
        "history"
    );
}

#[test]
fn schema1_document_upgrades_on_first_write() {
    let f = Fixture::new();
    let item = f.add("legacy", &[]);
    let live = f.add("retain", &[&item]);
    f.complete(&item);
    let mut legacy = f.document();
    legacy.schema_version = 1;
    legacy.archive = None;
    let clean = fs::read(f.archive_path()).unwrap();
    let mut stray: Value = serde_json::from_slice(&clean).unwrap();
    stray["item"]["id"] = json!("w-999");
    let stored: schedule::Item = serde_json::from_value(stray["item"].clone()).unwrap();
    stray["digest"] =
        json!(&gaebu::evidence::sha256_hex(&serde_json::to_string(&stored).unwrap())[..16]);
    let mut foreign = stray.clone();
    foreign["schedule_id"] = json!("foreign");
    let dirty = [
        clean.clone(),
        format!("{stray}\n{foreign}\n").into_bytes(),
        clean.clone(),
        b"{torn tail".to_vec(),
    ]
    .concat();
    fs::write(f.archive_path(), dirty).unwrap();
    f.write(&legacy);
    assert_eq!(
        f.call("schedule_show", json!({"all":true})).unwrap()["counts"]["retired"],
        1
    );
    assert_eq!(
        f.call("schedule_show", json!({"item":item})).unwrap()["title"],
        "legacy"
    );
    assert_eq!(
        f.call("schedule_show", json!({"item":"w-999"}))
            .unwrap_err()
            .reason,
        Reason::NotFound
    );
    f.call(
        "schedule_update",
        json!({"item":live,"priority":7,"depends_on":[]}),
    )
    .unwrap();
    let upgraded = f.document();
    assert_eq!(upgraded.schema_version, 2);
    assert_eq!(upgraded.retired.len(), 1);
    assert!(upgraded.items[0].depends_on.is_empty());
    assert_eq!(fs::read(f.archive_path()).unwrap(), clean);
    assert_eq!(upgraded.archive.as_ref().unwrap().rows, 1);
    let mut verified = upgraded.clone();
    commit_fixture(&f, &mut verified);
    assert_eq!(upgraded.archive, verified.archive);
    // The sidecar rename persisted but publication of the schema-2 document did not.
    f.write(&legacy);
    f.call(
        "schedule_update",
        json!({"item":live,"priority":7,"depends_on":[]}),
    )
    .unwrap();
    assert_eq!(f.document().archive, upgraded.archive);
    assert_eq!(fs::read(f.archive_path()).unwrap(), clean);
    assert_eq!(
        f.call("schedule_show", json!({"item":item})).unwrap()["title"],
        "legacy"
    );
}

#[test]
fn tombstones_stay_bounded() {
    let f = Fixture::new();
    let mut tenth = 0;
    for n in 1..=300 {
        let item = f.add("bounded history", &[]);
        f.call("schedule_cancel", json!({"item":item,"reason":"stop"}))
            .unwrap();
        assert!(f.document().retired.is_empty());
        if n == 10 {
            tenth = fs::metadata(&f.path).unwrap().len();
        }
    }
    let last = fs::metadata(&f.path).unwrap().len();
    eprintln!("300 cycles: cycle10={tenth} bytes, cycle300={last} bytes");
    assert!(
        last * 100 <= tenth * 105,
        "cycle10={tenth}, cycle300={last}"
    );
    assert_eq!(f.document().archive.unwrap().rows, 300);
}

#[test]
fn schema2_requires_coherent_head() {
    let f = Fixture::new();
    let original = f.document();
    let mut missing = original.clone();
    missing.archive = None;
    f.write(&missing);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
    for chain in ["short".to_owned(), "z".repeat(64), "0".repeat(64)] {
        let mut document = original.clone();
        document.archive.as_mut().unwrap().chain = chain;
        f.write(&document);
        assert_eq!(
            schedule::read_document(&f.path).unwrap_err().reason,
            Reason::InvalidArgument
        );
    }
    for field in ["bytes", "rows", "last_item", "last_dispatch"] {
        let mut document = original.clone();
        let mut value = serde_json::to_value(document.archive.as_ref().unwrap()).unwrap();
        value[field] = json!(1);
        document.archive = Some(serde_json::from_value(value).unwrap());
        document.next_item = 2;
        document.next_dispatch = 2;
        f.write(&document);
        assert_eq!(
            schedule::read_document(&f.path).unwrap_err().reason,
            Reason::InvalidArgument,
            "empty {field}"
        );
    }
    f.write(&original);
    let item = f.add("retire", &[]);
    f.complete(&item);
    let original = f.document();
    for chain in ["short".to_owned(), "z".repeat(64)] {
        let mut document = original.clone();
        document.archive.as_mut().unwrap().chain = chain;
        f.write(&document);
        assert_eq!(
            schedule::read_document(&f.path).unwrap_err().reason,
            Reason::InvalidArgument
        );
    }
    for field in ["bytes", "last_item"] {
        let mut document = original.clone();
        let mut value = serde_json::to_value(document.archive.as_ref().unwrap()).unwrap();
        value[field] = json!(0);
        document.archive = Some(serde_json::from_value(value).unwrap());
        f.write(&document);
        assert_eq!(
            schedule::read_document(&f.path).unwrap_err().reason,
            Reason::InvalidArgument,
            "nonempty {field}"
        );
    }
    let mut document = original.clone();
    document.schema_version = 1;
    f.write(&document);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
    document.schema_version = 3;
    f.write(&document);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::StoreUnsupported
    );
}

#[test]
fn live_dependencies_must_have_document_nodes() {
    let f = Fixture::new();
    let a = f.add("A", &[]);
    let b = f.add("B", &[&a]);
    let c = f.add("C", &[&b]);
    f.call("schedule_cancel", json!({"item":a,"reason":"stop"}))
        .unwrap();
    f.call("schedule_cancel", json!({"item":b,"reason":"stop"}))
        .unwrap();
    let original = f.document();
    assert_eq!(original.retired[0].depends_on, vec![a.clone()]);
    let mut document = original.clone();
    document
        .items
        .iter_mut()
        .find(|item| item.id == c)
        .unwrap()
        .depends_on = vec![a];
    f.write(&document);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
    let mut document = original;
    document.retired[0].depends_on = vec![format!("w-{}", document.next_item)];
    f.write(&document);
    assert_eq!(
        schedule::read_document(&f.path).unwrap_err().reason,
        Reason::InvalidArgument
    );
}

#[test]
fn retained_dependencies_do_not_scan_archive() {
    let f = Fixture::new();
    let predecessor = f.add("complete", &[]);
    let dependent = f.add("retainer", &[&predecessor]);
    f.complete(&predecessor);
    fs::remove_file(f.archive_path()).unwrap();
    for operation in ["schedule_add", "schedule_update"] {
        let mut args = json!({"title":"dependent","scope":["src/**"],"depends_on":[predecessor]});
        if operation == "schedule_update" {
            args["item"] = json!(dependent);
        }
        let result = f.call(operation, args).unwrap();
        assert_eq!(result["satisfied"], json!([]));
        assert_eq!(result["item"]["depends_on"], json!([predecessor]));
    }
}

#[test]
fn allocated_but_missing_dependency_is_refused() {
    let f = Fixture::new();
    let predecessor = f.add("complete", &[]);
    f.complete(&predecessor);
    let target = f.add("target", &[]);
    let mut document = f.document();
    let missing = format!("w-{}", document.next_item);
    document.next_item += 1;
    f.write(&document);
    for operation in ["schedule_add", "schedule_update"] {
        let mut args = json!({"title":"refuse","scope":["src/**"],"depends_on":[missing]});
        if operation == "schedule_update" {
            args["item"] = json!(target);
        }
        assert_eq!(
            f.call(operation, args).unwrap_err().reason,
            Reason::InvalidArgument
        );
        assert_eq!(f.document().revision, document.revision);
    }
}
