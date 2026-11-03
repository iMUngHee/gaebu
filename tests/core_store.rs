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
                        .transact(Duration::from_secs(5), |s| {
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
            .transact(Duration::from_secs(5), |s| {
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
