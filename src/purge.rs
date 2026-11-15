//! State retirement precedes removal of positively owned runtime files.

use crate::{
    Error, Reason, Result, archive,
    model::*,
    store::{self, Store},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

const RETENTION: Ms = 30 * 24 * 60 * 60 * 1000;

pub(crate) fn selection(st: &State, now: Ms) -> BTreeSet<String> {
    let expired: BTreeSet<_> = st
        .teams
        .values()
        .filter(|t| {
            t.status == TeamStatus::Cancelled
                && t.cancelled_at
                    .is_some_and(|at| now.saturating_sub(at) >= RETENTION)
        })
        .map(|t| t.id.clone())
        .collect();
    let protected: BTreeSet<_> = st
        .schedules
        .iter()
        .filter(|(team, _)| !expired.contains(*team))
        .flat_map(|(_, reg)| reg.predecessors.iter())
        .collect();
    expired
        .into_iter()
        .filter(|team| !protected.contains(team))
        .collect()
}

pub(crate) fn needed(st: &State, now: Ms) -> bool {
    !st.purge_pending.is_empty() || !selection(st, now).is_empty()
}

pub(crate) fn retire(st: &mut State, selected: &BTreeSet<String>) -> Vec<Value> {
    let mut purged = Vec::new();
    for id in selected {
        if let Some(team) = st.teams.remove(id) {
            st.root_evidence_floor = st.root_evidence_floor.max(team.cancelled_at.unwrap_or(0));
            purged.push(json!({"team": id, "name": team.name}));
        }
        if let Some(archives) = st.archives.remove(id) {
            st.purge_pending.insert(id.clone(), archives);
        }
    }
    let removed_members: BTreeSet<_> = st
        .members
        .values()
        .filter(|m| selected.contains(&m.team))
        .map(|m| m.id.clone())
        .collect();
    st.bus.retain(|_, b| !selected.contains(&b.team));
    st.members.retain(|_, m| !selected.contains(&m.team));
    st.grants.retain(|_, g| !selected.contains(&g.team));
    st.duties.retain(|_, d| !selected.contains(&d.team));
    st.leases.retain(|_, l| !selected.contains(&l.team));
    st.escalations.retain(|_, e| !selected.contains(&e.team));
    st.records.retain(|r| !selected.contains(&r.team));
    st.root_actions.retain(|_, a| !selected.contains(&a.team));
    st.schedules.retain(|team, _| !selected.contains(team));
    st.outbox
        .retain(|n| !removed_members.contains(&n.to_member));
    for (owner, reg) in &mut st.schedules {
        reg.predecessors
            .retain(|id| id != owner && st.teams.contains_key(id));
    }
    purged
}

/// Call only under state.lock, against the pending map of that transaction.
pub(crate) fn remove_pending(store: &Store, st: &mut State) -> Vec<Value> {
    let mut deferred = Vec::new();
    st.purge_pending.retain(|team, archives| {
        let result = (|| {
            for (name, stream) in [
                ("duties", &archives.duties),
                ("escalations", &archives.escalations),
                ("records", &archives.records),
            ] {
                let base = archive::RuntimePath::new(store, &st.home_schedules, team, name)?;
                for segment in &stream.segments {
                    base.remove_committed(segment.n)?;
                }
            }
            let parent = store.home().join("archive");
            let dir = parent.join(team);
            match fs::symlink_metadata(&dir) {
                Ok(_) => {
                    sync(&dir)?;
                    match fs::remove_dir(&dir) {
                        Ok(()) => (),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::NotFound
                                    | std::io::ErrorKind::DirectoryNotEmpty
                            ) => {}
                        Err(e) => return Err(write_error(e)),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(write_error(e)),
            }
            match sync(&parent) {
                Ok(()) => Ok(()),
                Err(_) if !parent.try_exists().map_err(write_error)? => sync(store.home()),
                Err(e) => Err(e),
            }
        })();
        if let Err(error) = result {
            deferred.push(json!({"team": team, "reason": error.message}));
            true
        } else {
            false
        }
    });
    deferred
}

fn write_error(error: std::io::Error) -> Error {
    Error::new(Reason::StoreWriteFailed, error.to_string())
}

fn sync(path: &Path) -> Result<()> {
    #[cfg(test)]
    if FAILED_SYNC.with_borrow(|p| p.as_deref() == Some(path)) {
        return Err(write_error(std::io::Error::other(
            "injected purge directory sync failure",
        )));
    }
    store::sync_dir(path)
}

#[cfg(test)]
std::thread_local! {
    static FAILED_SYNC: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{self, SeedSpec};
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Home(PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (Home, Store, String) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let home = Home(std::env::temp_dir().join(format!(
            "gaebu-purge-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        let store = Store::open(&home.0).unwrap();
        let team = store
            .transact(store::CLI_DEADLINE, |st| {
                ops::dev_seed(
                    st,
                    1,
                    &SeedSpec {
                        team: "one".into(),
                        project: "one".into(),
                        commander_alias: "root".into(),
                        commander_session: Some("root".into()),
                        commander_tool: Some(Tool::Codex),
                        members: vec![],
                    },
                )?;
                Ok(st.teams.keys().next().unwrap().clone())
            })
            .unwrap();
        (home, store, team)
    }

    #[test]
    fn selection_uses_exact_retention_and_known_cancellation() {
        let (_home, store, team) = fixture();
        let mut st = store.read_current().unwrap();
        let cancelled = 100;
        assert!(selection(&st, cancelled + RETENTION).is_empty());
        st.teams.get_mut(&team).unwrap().status = TeamStatus::Cancelled;
        assert!(selection(&st, cancelled + RETENTION).is_empty());
        st.teams.get_mut(&team).unwrap().cancelled_at = Some(cancelled);
        assert!(selection(&st, cancelled + RETENTION - 1).is_empty());
        assert_eq!(
            selection(&st, cancelled + RETENTION),
            [team.clone()].into_iter().collect()
        );
        assert!(selection(&st, cancelled - 1).is_empty());
        assert!(!needed(&st, cancelled + RETENTION - 1));
        assert!(needed(&st, cancelled + RETENTION));
        st.purge_pending.insert(team, TeamArchives::default());
        assert!(needed(&st, cancelled - 1));
    }

    #[test]
    fn removal_retries_unlink_and_each_directory_sync() {
        for failure in ["unlink", "team sync", "archive sync"] {
            let (_home, store, team) = fixture();
            let parent = store.home().join("archive");
            let dir = parent.join(&team);
            fs::create_dir_all(&dir).unwrap();
            let file = dir.join("duties.jsonl");
            fs::write(&file, b"runtime bytes\n").unwrap();
            store
                .transact(store::CLI_DEADLINE, |st| {
                    let mut book = TeamArchives::default();
                    book.duties.segments.push(ArchiveSegment {
                        n: 1,
                        bytes: 14,
                        lo: None,
                        hi: None,
                    });
                    st.purge_pending.insert(team.clone(), book);
                    Ok(())
                })
                .unwrap();
            match failure {
                "unlink" => archive::FAILED_UNLINK.with_borrow_mut(|p| *p = Some(file.clone())),
                "team sync" => FAILED_SYNC.with_borrow_mut(|p| *p = Some(dir.clone())),
                "archive sync" => FAILED_SYNC.with_borrow_mut(|p| *p = Some(parent.clone())),
                _ => unreachable!(),
            }
            let deferred = store
                .transact(store::CLI_DEADLINE, |st| Ok(remove_pending(&store, st)))
                .unwrap();
            archive::FAILED_UNLINK.with_borrow_mut(|p| *p = None);
            FAILED_SYNC.with_borrow_mut(|p| *p = None);
            assert_eq!(deferred.len(), 1, "{failure}");
            assert_eq!(deferred[0]["team"], team);
            assert!(
                store
                    .read_current()
                    .unwrap()
                    .purge_pending
                    .contains_key(&team)
            );
            assert_eq!(file.exists(), failure == "unlink");
            assert_eq!(dir.exists(), failure != "archive sync");
            let deferred = store
                .transact(store::CLI_DEADLINE, |st| Ok(remove_pending(&store, st)))
                .unwrap();
            assert!(deferred.is_empty());
            assert!(store.read_current().unwrap().purge_pending.is_empty());
            assert!(!dir.exists());
        }
    }

    #[test]
    fn root_floor_precedes_target_resolution_and_idempotency() {
        let mut st = State {
            root_evidence_floor: 50,
            ..State::default()
        };
        for at in [49, 50] {
            let ev = Evidence {
                session_id: "root".into(),
                prompt_seq: 1,
                sha256: "hash".into(),
                attest: Attest::HookFiltered,
                quote: "instruction".into(),
                at,
                strength: Strength::Explicit,
            };
            for result in [
                ops::root_appoint(&mut st, 51, &ev, "new", None, "project", None),
                ops::root_assign(&mut st, 51, &ev, &["new".into()], "missing"),
                ops::root_dismiss(&mut st, 51, &ev, "missing"),
                ops::root_cancel_team(&mut st, 51, &ev, "missing"),
                ops::root_rebind(&mut st, 51, &ev, "missing", "new"),
                ops::root_hold(&mut st, 51, &ev, "missing", "human releases"),
                ops::root_release(&mut st, 51, &ev, "missing"),
            ] {
                let error = result.unwrap_err();
                assert_eq!(error.reason, Reason::NoUserEvidence);
                assert!(error.message.contains("restate the instruction"));
            }
        }
        assert!(st.teams.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn pending_directory_errors_are_not_treated_as_missing_or_nonempty() {
        use std::os::unix::fs::PermissionsExt;
        for failure in ["not a directory", "cannot remove"] {
            let (_home, store, team) = fixture();
            store
                .transact(store::CLI_DEADLINE, |st| {
                    st.purge_pending
                        .insert(team.clone(), TeamArchives::default());
                    Ok(())
                })
                .unwrap();
            let parent = store.home().join("archive");
            if failure == "not a directory" {
                fs::write(&parent, b"foreign file").unwrap();
            } else {
                fs::create_dir_all(parent.join(&team)).unwrap();
                fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
            }
            let result = store.transact(store::CLI_DEADLINE, |st| Ok(remove_pending(&store, st)));
            if failure == "cannot remove" {
                fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let deferred = result.unwrap();
            assert_eq!(deferred.len(), 1, "{failure}");
            assert!(
                store
                    .read_current()
                    .unwrap()
                    .purge_pending
                    .contains_key(&team)
            );
            if failure == "not a directory" {
                assert_eq!(fs::read(&parent).unwrap(), b"foreign file");
                fs::remove_file(&parent).unwrap();
            }
            let deferred = store
                .transact(store::CLI_DEADLINE, |st| Ok(remove_pending(&store, st)))
                .unwrap();
            assert!(deferred.is_empty());
            assert!(store.read_current().unwrap().purge_pending.is_empty());
        }
    }

    #[test]
    fn retirement_removes_only_the_selected_team_rows() {
        let (_home, store, team) = fixture();
        let mut st = store.read_current().unwrap();
        st.leases.insert(
            "old-lease".into(),
            Lease {
                id: "old-lease".into(),
                team: team.clone(),
                key: "src/**".into(),
                mode: LeaseMode::Exclusive,
                holder: st.teams[&team].commander.clone(),
                holder_session: "root".into(),
                reason: "work".into(),
                acquired_at: 1,
                expires_at: u64::MAX,
            },
        );
        let keeper = Lease {
            id: "keeper-lease".into(),
            team: "keeper".into(),
            ..st.leases["old-lease"].clone()
        };
        st.leases.insert(keeper.id.clone(), keeper.clone());
        retire(&mut st, &[team.clone()].into_iter().collect());
        assert_eq!(
            st.leases.values().cloned().collect::<Vec<_>>(),
            vec![keeper]
        );
        assert!(!st.teams.contains_key(&team));
    }
}
