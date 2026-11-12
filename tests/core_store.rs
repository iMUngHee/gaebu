use gaebu::{
    Reason,
    model::State,
    store::{ComposerRecord, EventKind, Store},
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Deadline for the writer-serialization tests. They check that no update is
/// lost, not how fast 100 commits land: on a Windows runner each commit fsyncs
/// twice and the lock is polled, not queued, so one writer could wait out the
/// CLI's 5 s and fail `busy` (CI, 2026-11-04) though nothing was lost.
const SERIAL_DEADLINE: Duration = Duration::from_secs(60);

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "gaebu-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn store(&self) -> Store {
        Store::open(&self.0).unwrap()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn store_roundtrip_and_rejected_transaction() {
    let home = Home::new();
    let store = home.store();
    assert_eq!(store.read().unwrap(), State::default());
    store
        .transact(Duration::from_secs(1), |s| {
            s.next_id = 42;
            Ok(())
        })
        .unwrap();
    assert_eq!(store.read().unwrap().rev, 1);
    let err = store
        .transact(Duration::from_secs(1), |s| {
            s.next_id = 99;
            gaebu::error::refuse::<()>(Reason::Conflict, "rejected")
        })
        .unwrap_err();
    assert_eq!(err.reason, Reason::Conflict);
    assert_eq!(store.read().unwrap().next_id, 42);
}

#[test]
fn corrupt_state_refuses_write_reads_prev() {
    let home = Home::new();
    let store = home.store();
    store
        .transact(Duration::from_secs(1), |s| {
            s.next_id = 42;
            Ok(())
        })
        .unwrap();
    store
        .transact(Duration::from_secs(1), |s| {
            s.next_id = 43;
            Ok(())
        })
        .unwrap();
    fs::write(home.0.join("state.json"), b"broken").unwrap();
    assert_eq!(store.read().unwrap().next_id, 42);
    assert_eq!(
        store
            .transact(Duration::from_secs(1), |_| Ok(()))
            .unwrap_err()
            .reason,
        Reason::StoreCorrupt
    );
    assert_eq!(fs::read(home.0.join("state.json")).unwrap(), b"broken");
    store.repair().unwrap();
    assert_eq!(store.read().unwrap().next_id, 42);
}

#[test]
fn stale_tmp_ignored_and_cleaned() {
    let home = Home::new();
    let store = home.store();
    let stale = home.0.join("state.json.tmp-123-1");
    let fresh = home.0.join("state.json.tmp-123-2");
    fs::write(&stale, b"interrupted").unwrap();
    fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(120)),
        )
        .unwrap();
    fs::write(&fresh, b"fresh").unwrap();
    assert_eq!(store.read().unwrap(), State::default());
    store.transact(Duration::from_secs(1), |_| Ok(())).unwrap();
    assert!(!stale.exists());
    assert!(fresh.exists());
}

#[test]
fn concurrent_writers_serialize() {
    let home = Home::new();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let path = &home.0;
            scope.spawn(move || {
                let store = Store::open(path).unwrap();
                for _ in 0..25 {
                    store
                        .transact(SERIAL_DEADLINE, |s| {
                            s.next_id += 1;
                            Ok(())
                        })
                        .unwrap();
                }
            });
        }
    });
    let state = home.store().read().unwrap();
    assert_eq!(state.next_id, 101);
    assert_eq!(state.rev, 100);
}

#[test]
fn lock_timeout_is_busy() {
    let home = Home::new();
    let store = home.store();
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.0.join("state.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert_eq!(
        store
            .transact(Duration::from_millis(20), |_| Ok(()))
            .unwrap_err()
            .reason,
        Reason::Busy
    );
}

#[test]
#[ignore = "child process helper"]
fn lock_child() {
    let Some(path) = std::env::var_os("GAEBU_TEST_LOCK_HOME") else {
        return;
    };
    let path = PathBuf::from(path);
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.join("state.lock"))
        .unwrap();
    lock.lock().unwrap();
    fs::write(path.join("ready"), b"ready").unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn lock_released_when_holder_killed() {
    let home = Home::new();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_child", "--ignored"])
        .env("GAEBU_TEST_LOCK_HOME", &home.0)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while !home.0.join("ready").exists() {
        if std::time::Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not lock");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let blocked = home.store().transact(Duration::from_millis(20), |_| Ok(()));
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(blocked.unwrap_err().reason, Reason::Busy);
    home.store()
        .transact(Duration::from_secs(1), |_| Ok(()))
        .unwrap();
}

#[test]
fn session_rotation_sequence_and_sighting() {
    let home = Home::new();
    let store = home.store();
    let log = store.session("session");
    for n in 1..=60 {
        assert_eq!(
            log.append(
                n,
                EventKind::UserPrompt,
                Some(n.to_string()),
                Some("가".repeat(2000)),
                None
            )
            .unwrap(),
            n
        );
    }
    let events = log.events().unwrap();
    assert_eq!(events.len(), 50);
    assert_eq!(events[0].seq, 11);
    assert!(
        events
            .iter()
            .all(|e| e.text.as_ref().unwrap().len() <= 4096)
    );
    log.append_composer(&ComposerRecord {
        at: 60,
        sha256: "hash".into(),
    })
    .unwrap();
    assert_eq!(log.composers().unwrap().len(), 1);
    log.update_sighting(|s| s.last_seen = Some(100)).unwrap();
    log.update_sighting(|s| s.last_card_hash = Some("card".into()))
        .unwrap();
    assert_eq!(log.sighting().unwrap().last_seen, Some(100));
}

#[cfg(windows)]
#[test]
fn rename_retry_then_store_write_failed() {
    use std::os::windows::fs::OpenOptionsExt;
    let home = Home::new();
    let store = home.store();
    store.transact(Duration::from_secs(1), |_| Ok(())).unwrap();
    let before = fs::read(home.0.join("state.json")).unwrap();
    let held = fs::File::options()
        .read(true)
        .share_mode(1)
        .open(home.0.join("state.json"))
        .unwrap();
    let start = std::time::Instant::now();
    assert_eq!(
        store
            .transact(Duration::from_secs(1), |_| Ok(()))
            .unwrap_err()
            .reason,
        Reason::StoreWriteFailed
    );
    assert!(start.elapsed() >= Duration::from_millis(950));
    drop(held);
    assert_eq!(fs::read(home.0.join("state.json")).unwrap(), before);
}

#[test]
fn unrecorded_marker_blocks_older_evidence() {
    let home = Home::new();
    let store = home.store();
    let log = store.session("marker-test");
    log.append(
        10,
        EventKind::UserPrompt,
        Some("old".into()),
        Some("old prompt".into()),
        None,
    )
    .unwrap();
    log.mark_unrecorded(20).unwrap();
    assert!(log.unrecorded_after(10).unwrap());
    assert!(log.unrecorded_after(20).unwrap());
    assert!(!log.unrecorded_after(21).unwrap());
    log.append(15, EventKind::Stop, None, None, None).unwrap();
    assert!(log.unrecorded_after(10).unwrap());
    log.append(
        30,
        EventKind::UserPrompt,
        Some("new".into()),
        Some("new prompt".into()),
        None,
    )
    .unwrap();
    assert!(!log.unrecorded_after(10).unwrap());
}

#[test]
#[ignore = "child process helper"]
fn writer_child() {
    let Some(path) = std::env::var_os("GAEBU_TEST_WRITER_HOME") else {
        return;
    };
    let store = Store::open(PathBuf::from(path)).unwrap();
    for _ in 0..25 {
        store
            .transact(SERIAL_DEADLINE, |s| {
                s.next_id += 1;
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn multiprocess_writers_serialize() {
    let home = Home::new();
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "writer_child", "--ignored"])
                .env("GAEBU_TEST_WRITER_HOME", &home.0)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    let statuses: Vec<_> = children.iter_mut().map(|c| c.wait().unwrap()).collect();
    assert!(statuses.iter().all(|s| s.success()));
    let state = home.store().read().unwrap();
    assert_eq!(state.next_id, 101);
    assert_eq!(state.rev, 100);
}

#[test]
fn transact_respects_total_deadline() {
    let home = Home::new();
    let store = home.store();
    store.transact(Duration::from_secs(1), |_| Ok(())).unwrap();
    let before = fs::read(home.0.join("state.json")).unwrap();
    let result = store.transact(Duration::from_millis(10), |s| {
        s.next_id = 999;
        std::thread::sleep(Duration::from_millis(30));
        Ok(())
    });
    assert_eq!(result.unwrap_err().reason, Reason::StoreWriteFailed);
    assert_eq!(fs::read(home.0.join("state.json")).unwrap(), before);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let held = fs::File::options()
            .read(true)
            .share_mode(1)
            .open(home.0.join("state.json"))
            .unwrap();
        let start = std::time::Instant::now();
        let result = store.transact(Duration::from_millis(140), |_| {
            std::thread::sleep(Duration::from_millis(60));
            Ok(())
        });
        let elapsed = start.elapsed();
        drop(held);
        assert_eq!(result.unwrap_err().reason, Reason::StoreWriteFailed);
        assert!(elapsed < Duration::from_millis(230), "elapsed={elapsed:?}");
        assert_eq!(fs::read(home.0.join("state.json")).unwrap(), before);
    }
}

#[test]
fn legacy_sighting_defaults_pending_throttle_fields() {
    let sighting: gaebu::store::Sighting = serde_json::from_str(r#"{"last_seen":10}"#).unwrap();
    assert_eq!(sighting.pending_checked_at, None);
    assert_eq!(sighting.pending_checked_rev, None);
    let home = Home::new();
    let store = home.store();
    let log = store.session("pending");
    log.update_sighting(|s| {
        s.pending_checked_at = Some(10);
        s.pending_checked_rev = Some(2);
    })
    .unwrap();
    let stored = log.sighting().unwrap();
    assert_eq!(stored.pending_checked_at, Some(10));
    assert_eq!(stored.pending_checked_rev, Some(2));
}

// ----- schema v2 migration (plan gaebu-v0-2, Decisions 12–12d) -----

const V1: &str = include_str!("fixtures/state-v1.json");
const FUTURE: &str = r#"{"schema_version":5,"teams":"a newer shape this model cannot parse"}"#;

fn json(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn schema_two_migrates_with_original_backup_and_current_previous_generation() {
    let home = Home::new();
    let mut legacy = serde_json::to_value(State::default()).unwrap();
    legacy["schema_version"] = 2.into();
    legacy.as_object_mut().unwrap().remove("runtime_id");
    legacy.as_object_mut().unwrap().remove("schedules");
    let original = serde_json::to_vec(&legacy).unwrap();
    fs::write(home.0.join("state.json"), &original).unwrap();
    home.store()
        .transact(Duration::from_secs(5), |_| Ok(()))
        .unwrap();
    let current = json(&home.0.join("state.json"));
    assert_eq!(current["schema_version"], gaebu::model::SCHEMA_VERSION);
    assert!(!current["runtime_id"].as_str().unwrap().is_empty());
    assert_eq!(fs::read(home.0.join("state.v2.json")).unwrap(), original);
    assert_eq!(
        json(&home.0.join("state.prev.json"))["schema_version"],
        gaebu::model::SCHEMA_VERSION
    );
    assert_eq!(current["schedules"], serde_json::json!({}));
}

#[test]
fn schema_3_migrates_with_exact_backup_and_archive_defaults() {
    let home = Home::new();
    let mut legacy = serde_json::to_value(State::default()).unwrap();
    legacy["schema_version"] = 3.into();
    legacy["runtime_id"] = "original-runtime".into();
    legacy["rev"] = 8.into();
    legacy["next_id"] = 50.into();
    legacy.as_object_mut().unwrap().remove("archives");
    let original = serde_json::to_vec(&legacy).unwrap();
    fs::write(home.0.join("state.json"), &original).unwrap();
    let store = home.store();
    assert_eq!(store.read_view().unwrap().schema_version, 4);
    store.transact(Duration::from_secs(5), |_| Ok(())).unwrap();
    let state = store.read_current().unwrap();
    assert_eq!(state.schema_version, 4);
    assert_eq!(state.runtime_id, "original-runtime");
    assert_eq!(state.next_id, 50);
    assert_eq!(state.rev, 10);
    assert!(state.archives.is_empty());
    assert_eq!(fs::read(home.0.join("state.v3.json")).unwrap(), original);
    assert_eq!(json(&home.0.join("state.prev.json"))["schema_version"], 4);
}

#[test]
fn runtime_origin_is_stable_after_commit_and_distinct_between_stores() {
    let one = Home::new();
    let two = Home::new();
    for home in [&one, &two] {
        home.store()
            .transact(Duration::from_secs(5), |_| Ok(()))
            .unwrap();
    }
    let origin = json(&one.0.join("state.json"))["runtime_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!origin.is_empty());
    one.store()
        .transact(Duration::from_secs(5), |_| Ok(()))
        .unwrap();
    assert_eq!(json(&one.0.join("state.json"))["runtime_id"], origin);
    assert_ne!(json(&two.0.join("state.json"))["runtime_id"], origin);
}

#[test]
fn authoritative_read_never_uses_previous_generation_to_prove_absence() {
    let home = Home::new();
    assert_eq!(
        home.store().read_current().unwrap_err().reason,
        Reason::NotFound
    );
    for _ in 0..2 {
        home.store()
            .transact(Duration::from_secs(5), |_| Ok(()))
            .unwrap();
    }
    let previous = home.store().read().unwrap();
    fs::write(home.0.join("state.json"), "{").unwrap();
    assert_eq!(home.store().read().unwrap().runtime_id, previous.runtime_id);
    assert_eq!(
        home.store().read_current().unwrap_err().reason,
        Reason::StoreCorrupt
    );
}

#[test]
fn view_reads_distinguish_first_use_from_corrupt_or_missing_runtime() {
    let home = Home::new();
    assert_eq!(home.store().read_view().unwrap(), State::default());
    for _ in 0..2 {
        home.store()
            .transact(Duration::from_secs(5), |_| Ok(()))
            .unwrap();
    }
    fs::write(home.0.join("state.json"), "{").unwrap();
    assert_eq!(
        home.store().read_view().unwrap_err().reason,
        Reason::StoreCorrupt
    );
    fs::remove_file(home.0.join("state.json")).unwrap();
    assert_eq!(
        home.store().read_view().unwrap_err().reason,
        Reason::NotFound
    );
}

/// The fixture migrated by a real transaction, as current-schema bytes.
fn current_bytes() -> Vec<u8> {
    let home = Home::new();
    fs::write(home.0.join("state.json"), V1).unwrap();
    home.store()
        .transact(Duration::from_secs(5), |_| Ok(()))
        .unwrap();
    fs::read(home.0.join("state.json")).unwrap()
}

#[test]
fn v1_fixture_migrates_to_current() {
    let home = Home::new();
    fs::write(home.0.join("state.json"), V1).unwrap();
    home.store()
        .transact(Duration::from_secs(5), |_| Ok(()))
        .unwrap();
    let st = json(&home.0.join("state.json"));
    assert_eq!(st["schema_version"], gaebu::model::SCHEMA_VERSION);
    assert_eq!(st["grants"]["g-4"]["evidence"]["strength"], "inferred");
    assert_eq!(st["escalations"]["e-21"]["status"], "superseded");
    assert_eq!(st["escalations"]["e-21"]["superseded_by"], "e-22");
    assert_eq!(st["escalations"]["e-22"]["status"], "open");
    assert_eq!(st["escalations"]["e-23"]["status"], "answered");
    let v1: serde_json::Value = serde_json::from_str(V1).unwrap();
    let original = v1["outbox"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "n-24")
        .unwrap();
    let migrated = st["outbox"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "n-24")
        .unwrap();
    let body = original["body"].as_str().unwrap();
    assert_eq!(migrated["text"], gaebu::model::notice_key(body).as_str());
    assert!(!migrated["text"].as_str().unwrap().contains("r-25"));
    assert_eq!(migrated["body"], original["body"]);
    assert_eq!(migrated["attempts"], original["attempts"]);
    assert_eq!(migrated["claimed_at"], original["claimed_at"]);
}

#[test]
fn migration_keeps_v1_backup_and_current_prev() {
    let home = Home::new();
    let store = home.store();
    fs::write(home.0.join("state.json"), V1).unwrap();
    store.transact(Duration::from_secs(5), |_| Ok(())).unwrap();
    assert_eq!(
        fs::read(home.0.join("state.v1.json")).unwrap(),
        V1.as_bytes()
    );
    assert_eq!(
        json(&home.0.join("state.prev.json"))["schema_version"],
        gaebu::model::SCHEMA_VERSION
    );
    // A second schema-1 file (restored by hand) migrates again but never
    // replaces the first original.
    let other = V1.replacen("\"rev\": ", "\"rev\": 9", 1);
    fs::write(home.0.join("state.json"), &other).unwrap();
    store.transact(Duration::from_secs(5), |_| Ok(())).unwrap();
    assert_eq!(
        fs::read(home.0.join("state.v1.json")).unwrap(),
        V1.as_bytes()
    );
    assert_eq!(
        json(&home.0.join("state.json"))["schema_version"],
        gaebu::model::SCHEMA_VERSION
    );
}

#[test]
fn future_schema_refused_without_fallback() {
    let home = Home::new();
    let store = home.store();
    fs::write(home.0.join("state.prev.json"), current_bytes()).unwrap();
    fs::write(home.0.join("state.json"), FUTURE).unwrap();
    assert_eq!(store.read().unwrap_err().reason, Reason::StoreUnsupported);
    let err = store
        .transact(Duration::from_secs(5), |_| Ok(()))
        .unwrap_err();
    assert_eq!(err.reason, Reason::StoreUnsupported);
    assert_eq!(
        fs::read(home.0.join("state.json")).unwrap(),
        FUTURE.as_bytes()
    );
}

#[test]
fn migration_resumes_after_partial_write() {
    // Interrupted after the backup and the current-schema prev, before state.json.
    let home = Home::new();
    let store = home.store();
    fs::write(home.0.join("state.v1.json"), V1).unwrap();
    fs::write(home.0.join("state.prev.json"), current_bytes()).unwrap();
    fs::write(home.0.join("state.json"), V1).unwrap();
    let v1_rev = serde_json::from_str::<serde_json::Value>(V1).unwrap()["rev"]
        .as_u64()
        .unwrap();
    let calls = std::cell::Cell::new(0);
    store
        .transact(Duration::from_secs(5), |st| {
            calls.set(calls.get() + 1);
            st.next_id += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(calls.get(), 1);
    let st = json(&home.0.join("state.json"));
    assert_eq!(st["schema_version"], gaebu::model::SCHEMA_VERSION);
    assert_eq!(
        st["rev"],
        v1_rev + 2,
        "one migration commit, one user commit"
    );
    assert_eq!(
        json(&home.0.join("state.prev.json"))["schema_version"],
        gaebu::model::SCHEMA_VERSION
    );
    assert_eq!(
        fs::read(home.0.join("state.v1.json")).unwrap(),
        V1.as_bytes()
    );
}

#[test]
fn failed_migration_skips_user_change() {
    let home = Home::new();
    let store = home.store();
    fs::write(home.0.join("state.json"), V1).unwrap();
    // A directory where state.prev.json goes makes the migration's write fail.
    fs::create_dir_all(home.0.join("state.prev.json")).unwrap();
    let ran = std::cell::Cell::new(false);
    let err = store
        .transact(Duration::from_secs(5), |_| {
            ran.set(true);
            Ok(())
        })
        .unwrap_err();
    assert_eq!(err.reason, Reason::StoreWriteFailed);
    assert!(!ran.get(), "the user change must not run");
    assert_eq!(fs::read(home.0.join("state.json")).unwrap(), V1.as_bytes());
}

#[test]
fn repair_refuses_unsupported_state() {
    let home = Home::new();
    let prev = current_bytes();
    fs::write(home.0.join("state.prev.json"), &prev).unwrap();
    fs::write(home.0.join("state.json"), FUTURE).unwrap();
    let err = home.store().repair().unwrap_err();
    assert_eq!(err.reason, Reason::StoreUnsupported);
    assert_eq!(
        fs::read(home.0.join("state.json")).unwrap(),
        FUTURE.as_bytes()
    );
    assert_eq!(fs::read(home.0.join("state.prev.json")).unwrap(), prev);
}

#[test]
fn repair_from_v1_prev_migrates_consistently() {
    let home = Home::new();
    fs::write(home.0.join("state.json"), "{").unwrap();
    fs::write(home.0.join("state.prev.json"), V1).unwrap();
    home.store().repair().unwrap();
    assert_eq!(
        json(&home.0.join("state.json"))["schema_version"],
        gaebu::model::SCHEMA_VERSION
    );
    assert_eq!(
        json(&home.0.join("state.prev.json"))["schema_version"],
        gaebu::model::SCHEMA_VERSION
    );
    assert_eq!(
        fs::read(home.0.join("state.v1.json")).unwrap(),
        V1.as_bytes()
    );
    // Interrupted after prev ← current schema: the next repair finishes from the current-schema prev.
    let home = Home::new();
    let prev = current_bytes();
    fs::write(home.0.join("state.json"), "{").unwrap();
    fs::write(home.0.join("state.prev.json"), &prev).unwrap();
    fs::write(home.0.join("state.v1.json"), V1).unwrap();
    home.store().repair().unwrap();
    assert_eq!(
        json(&home.0.join("state.json"))["schema_version"],
        gaebu::model::SCHEMA_VERSION
    );
    assert_eq!(fs::read(home.0.join("state.prev.json")).unwrap(), prev);
    assert_eq!(
        fs::read(home.0.join("state.v1.json")).unwrap(),
        V1.as_bytes()
    );
}

#[test]
fn notice_serializes_target_as_to_session() {
    // The stored key is `to_session`, the name 0.3.0 binaries read and write,
    // whatever the field is called in code.
    let home = Home::new();
    home.store()
        .transact(Duration::from_secs(5), |st| {
            st.outbox.push(gaebu::model::Notice {
                id: "n-1".into(),
                to_member: "m-1".into(),
                target: "fasa".into(),
                body: "b".into(),
                text: "b".into(),
                created_at: 1,
                attempts: 0,
                claimed_at: None,
            });
            Ok(())
        })
        .unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&fs::read(home.0.join("state.json")).unwrap()).unwrap();
    let stored = &raw["outbox"][0];
    assert_eq!(stored["to_session"], "fasa");
    assert!(stored.get("target").is_none(), "{stored}");
    assert_eq!(home.store().read().unwrap().outbox[0].target, "fasa");
}
