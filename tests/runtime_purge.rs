mod common;

use gaebu::{
    Reason, authz, compact, evidence,
    identity::{Caller, Source},
    model::*,
    ops::{self, DutySpec, SeedSpec},
    pager::{HostStatus, Pager, WhoAmI},
    store::{CLI_DEADLINE, EventKind, Store},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const DAY: u64 = 86_400_000;
static NEXT: AtomicU64 = AtomicU64::new(0);
static CLI: Mutex<()> = Mutex::new(());

struct Fixture {
    store: Store,
    cleanup: common::RemoveOnDrop,
    team: String,
    actor: Actor,
    worker: Actor,
}

fn seed(st: &mut State, label: &str) -> (String, Actor) {
    let session = format!("{label}-root");
    ops::dev_seed(
        st,
        10,
        &SeedSpec {
            team: label.into(),
            project: label.into(),
            commander_alias: session.clone(),
            commander_session: Some(session.clone()),
            commander_tool: Some(Tool::Codex),
            members: vec![(
                format!("{label}-worker"),
                Some(format!("{label}-worker")),
                Some(Tool::Codex),
            )],
        },
    )
    .unwrap();
    let member = authz::member_for_session(st, &session).unwrap();
    (
        member.team.clone(),
        Actor {
            member: member.id.clone(),
            session_id: session,
        },
    )
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gaebu-purge-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::open(root.join("runtime")).unwrap();
        let (team, actor) = store
            .transact(CLI_DEADLINE, |st| Ok(seed(st, "one")))
            .unwrap();
        let st = store.read_current().unwrap();
        let worker = Actor {
            member: authz::member_for_session(&st, "one-worker")
                .unwrap()
                .id
                .clone(),
            session_id: "one-worker".into(),
        };
        Self {
            store,
            cleanup: common::RemoveOnDrop(root),
            team,
            actor,
            worker,
        }
    }
    fn edit<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        self.store.transact(CLI_DEADLINE, |st| Ok(f(st))).unwrap()
    }
    fn state(&self) -> State {
        self.store.read_current().unwrap()
    }
    fn caller(&self) -> Caller {
        caller(&self.actor.session_id)
    }
    fn cancel(&self, days: u64) {
        self.edit(|st| {
            let at = now_ms() - days * DAY;
            ops::root_cancel_team(st, at, &words("human", at, "cancel one"), &self.team).unwrap();
        });
    }
    fn duty(&self) -> String {
        self.edit(|st| {
            let at = now_ms() - 2 * DAY;
            let id = ops::duty_issue(
                st,
                at,
                &self.actor,
                &self.worker.member,
                DutySpec {
                    intent: "retain contract".into(),
                    outcome: "delivered".into(),
                    scope: vec!["src/**".into()],
                    done_criteria: "verified".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .ids[0]
                .clone();
            ops::duty_respond(st, at, &self.worker, &id, None, true, None).unwrap();
            ops::duty_progress(
                st,
                at,
                &self.worker,
                &id,
                DutyStatus::Done,
                Some("evidence"),
            )
            .unwrap();
            id
        })
    }
    fn archived(&self) -> (String, PathBuf) {
        let id = self.duty();
        compact::run(&self.store, &self.team, None).unwrap();
        (id, self.path("duties", 1))
    }
    fn path(&self, name: &str, number: u64) -> PathBuf {
        let base = self
            .store
            .home()
            .join("archive")
            .join(&self.team)
            .join(format!("{name}.jsonl"));
        if number == 1 {
            base
        } else {
            base.with_extension(format!("{number}.jsonl"))
        }
    }
    fn purge(&self) -> Value {
        gaebu::schedule::compact(&self.store, &caller("unseated"), false).unwrap()
    }
    fn block(&self) -> Vec<u8> {
        let path = self.store.home().join("state.prev.json");
        let bytes = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(path).unwrap();
        bytes
    }
    fn unblock(&self, bytes: &[u8]) {
        let path = self.store.home().join("state.prev.json");
        fs::remove_dir(&path).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

fn caller(session: &str) -> Caller {
    Caller {
        session_id: session.into(),
        tool: Some(Tool::Codex),
        source: Source::PagerNative,
    }
}
fn words(session: &str, at: u64, quote: &str) -> Evidence {
    Evidence {
        session_id: session.into(),
        prompt_seq: 1,
        sha256: evidence::sha256_hex(quote),
        attest: Attest::HookFiltered,
        quote: quote.into(),
        at,
        strength: Strength::Explicit,
    }
}
fn registration(
    path: PathBuf,
    predecessors: impl IntoIterator<Item = String>,
) -> ScheduleRegistration {
    ScheduleRegistration {
        schedule_id: "schedule-one".into(),
        path: path.to_string_lossy().into(),
        manager: None,
        offer: None,
        predecessors: predecessors.into_iter().collect(),
    }
}

#[test]
fn handed_off_linked_duty_is_compacted() {
    let f = Fixture::new();
    let id = f.duty();
    f.edit(|st| {
        let d = st.duties.get_mut(&id).unwrap();
        d.status = DutyStatus::HandedOff;
        d.schedule_ref = Some(ScheduleRef {
            schedule_id: "former-schedule".into(),
            item_id: "w-1".into(),
            dispatch_token: "old".into(),
            manifest_hash: "hash".into(),
        });
        st.records.clear();
    });
    let result = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(result["duties"], 1);
    assert!(!f.state().duties.contains_key(&id));
    assert_eq!(
        compact::full_duty(&f.store, &f.state(), &f.actor.session_id, &id)
            .unwrap()
            .status,
        DutyStatus::HandedOff
    );
}

#[test]
fn cancelled_team_is_purged_after_retention() {
    for age in [30 * DAY - 3_600_000, 30 * DAY + 3_600_000] {
        let f = Fixture::new();
        let (_, path) = f.archived();
        f.cancel(29);
        let at = now_ms() - age;
        f.edit(|st| st.teams.get_mut(&f.team).unwrap().cancelled_at = Some(at));
        let result = f.purge();
        let st = f.state();
        if age < 30 * DAY {
            assert!(st.teams.contains_key(&f.team));
            assert!(path.exists());
            assert_eq!(result["purged"], json!([]));
        } else {
            assert_eq!(result["purged"], json!([{"team":f.team,"name":"one"}]));
            assert!(
                st.teams.is_empty()
                    && st.bus.is_empty()
                    && st.members.is_empty()
                    && st.grants.is_empty()
                    && st.duties.is_empty()
                    && st.leases.is_empty()
                    && st.escalations.is_empty()
                    && st.records.is_empty()
                    && st.root_actions.is_empty()
                    && st.schedules.is_empty()
                    && st.archives.is_empty()
                    && st.outbox.is_empty()
            );
            assert!(st.purge_pending.is_empty());
            assert!(!path.parent().unwrap().exists());
            assert_eq!(st.root_evidence_floor, at);
        }
    }
}

#[test]
fn predecessors_defer_purge() {
    let f = Fixture::new();
    let (_, path) = f.archived();
    f.cancel(31);
    let keeper = f.edit(|st| {
        let (team, _) = seed(st, "keeper");
        st.schedules.insert(
            team.clone(),
            registration(
                f.cleanup.0.join("schedule.md"),
                [f.team.clone(), team.clone(), "missing".into()],
            ),
        );
        team
    });
    assert_eq!(f.purge()["purged"], json!([]));
    assert!(path.exists());
    assert_eq!(
        f.state().schedules[&keeper].predecessors,
        [f.team.clone()].into_iter().collect()
    );
    f.edit(|st| {
        let t = st.teams.get_mut(&keeper).unwrap();
        t.status = TeamStatus::Cancelled;
        t.cancelled_at = Some(now_ms() - 31 * DAY);
    });
    assert_eq!(f.purge()["purged"].as_array().unwrap().len(), 2);
    assert!(!path.exists());
}

#[test]
fn expired_cycle_is_purged() {
    let f = Fixture::new();
    f.cancel(31);
    f.edit(|st| {
        let (other, _) = seed(st, "other");
        let t = st.teams.get_mut(&other).unwrap();
        t.status = TeamStatus::Cancelled;
        t.cancelled_at = Some(now_ms() - 31 * DAY);
        st.schedules.insert(
            f.team.clone(),
            registration(f.cleanup.0.join("a.md"), [other.clone()]),
        );
        st.schedules.insert(
            other,
            registration(f.cleanup.0.join("b.md"), [f.team.clone()]),
        );
    });
    assert_eq!(f.purge()["purged"].as_array().unwrap().len(), 2);
    assert!(f.state().teams.is_empty());
}

#[test]
fn purge_publication_failure_changes_nothing() {
    let f = Fixture::new();
    let (_, path) = f.archived();
    f.cancel(31);
    let before = fs::read(f.store.home().join("state.json")).unwrap();
    let archive = fs::read(&path).unwrap();
    let prev = f.block();
    assert!(gaebu::schedule::compact(&f.store, &caller("unseated"), false).is_err());
    assert_eq!(fs::read(f.store.home().join("state.json")).unwrap(), before);
    assert_eq!(fs::read(&path).unwrap(), archive);
    f.unblock(&prev);
    assert_eq!(f.purge()["purged"].as_array().unwrap().len(), 1);
}

#[test]
fn adopt_after_failed_purge_keeps_archive() {
    let f = Fixture::new();
    let project = f.cleanup.0.join("schedule.md");
    gaebu::schedule::register(&f.store, &f.caller(), &project, true).unwrap();
    let call = |name: &str, args: Value| {
        gaebu::schedule::call(&f.store, &FakePager, &f.caller(), name, &args).unwrap()
    };
    let item = call(
        "schedule_add",
        json!({"title":"work","scope":["src/**"],"owner_bu":f.state().members[&f.actor.member].bu}),
    );
    let dispatched = call(
        "schedule_dispatch",
        json!({"item":item["item"]["id"],"key":"work","assignee":f.worker.member,
        "spec":{"intent":"keep source contract","outcome":"delivered","scope":["src/**"],"done_criteria":"verified",
            "decision_rights":"implementation","escalate_on":[]}}),
    );
    let id = dispatched["duty"].as_str().unwrap();
    f.cancel(2);
    compact::run(&f.store, &f.team, None).unwrap();
    assert!(!f.state().duties.contains_key(id));
    let (target, adopter) = f.edit(|st| {
        st.teams.get_mut(&f.team).unwrap().cancelled_at = Some(now_ms() - 31 * DAY);
        seed(st, "adopter")
    });
    let bytes = fs::read(f.path("duties", 1)).unwrap();
    let previous = f.block();
    assert!(gaebu::schedule::compact(&f.store, &caller("unseated"), false).is_err());
    f.unblock(&previous);
    let adopted = gaebu::schedule::call(
        &f.store,
        &FakePager,
        &caller(&adopter.session_id),
        "schedule_adopt",
        &json!({"team":f.team}),
    )
    .unwrap();
    assert_eq!(adopted["adopted"], true);
    assert_eq!(
        compact::full_duty(&f.store, &f.state(), &adopter.session_id, id)
            .unwrap()
            .intent,
        "keep source contract"
    );
    assert!(f.state().schedules[&target].predecessors.contains(&f.team));
    assert_eq!(fs::read(f.path("duties", 1)).unwrap(), bytes);
    assert_eq!(f.purge()["purged"], json!([]));
}

#[test]
fn deferred_cleanup_keeps_legacy_schedule_after_its_owner_is_purged() {
    for broken in [false, true] {
        let f = Fixture::new();
        let (_, blocked) = f.archived();
        let protected = f.path("records", 9);
        let (owner, actor) = f.edit(|st| seed(st, "legacy-owner"));
        let project = f.cleanup.0.join("legacy-schedule.md");
        gaebu::schedule::register(&f.store, &caller(&actor.session_id), &project, true).unwrap();
        fs::rename(&project, &protected).unwrap();
        if broken {
            fs::write(&protected, b"broken title: gaebu Schedule marker").unwrap();
        }
        let bytes = fs::read(&protected).unwrap();
        let sidecar = protected.with_extension("archive.jsonl");
        fs::write(&sidecar, b"sidecar bytes").unwrap();
        f.edit(|st| {
            st.schedules.get_mut(&owner).unwrap().path = protected.to_string_lossy().into();
            st.home_schedules.insert(
                fs::canonicalize(&protected)
                    .unwrap()
                    .to_string_lossy()
                    .into(),
            );
            st.archives
                .get_mut(&f.team)
                .unwrap()
                .records
                .segments
                .push(ArchiveSegment {
                    n: 9,
                    bytes: bytes.len() as u64,
                    lo: None,
                    hi: None,
                });
        });
        fs::remove_file(&blocked).unwrap();
        fs::create_dir(&blocked).unwrap();
        f.cancel(31);
        assert_eq!(f.purge()["purge_deferred"][0]["team"], f.team);
        f.edit(|st| {
            let t = st.teams.get_mut(&owner).unwrap();
            t.status = TeamStatus::Cancelled;
            t.cancelled_at = Some(now_ms() - 31 * DAY);
        });
        assert_eq!(f.purge()["purged"][0]["team"], owner);
        fs::remove_dir(&blocked).unwrap();
        f.purge();
        assert!(f.state().purge_pending.is_empty());
        assert_eq!(fs::read(protected).unwrap(), bytes);
        assert_eq!(fs::read(sidecar).unwrap(), b"sidecar bytes");
    }
}

#[test]
fn pending_removal_report_survives_a_team_archive_pass() {
    let f = Fixture::new();
    f.archived();
    f.edit(|st| {
        st.purge_pending
            .insert("pending".into(), TeamArchives::default());
    });
    let bad = f.store.home().join("archive/pending");
    fs::write(&bad, b"not a directory").unwrap();
    let result = compact::run(&f.store, &f.team, None).unwrap();
    assert_eq!(result["purge_deferred"][0]["team"], "pending");
    assert!(f.state().purge_pending.contains_key("pending"));
    fs::remove_file(bad).unwrap();
    let result = compact::run(&f.store, &f.team, None).unwrap();
    assert!(result.get("purge_deferred").is_none());
    assert!(f.state().purge_pending.is_empty());
}

#[test]
fn removal_failures_are_deferred() {
    let f = Fixture::new();
    let (_, path) = f.archived();
    f.cancel(31);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let result = f.purge();
    assert_eq!(result["purged"].as_array().unwrap().len(), 1);
    assert_eq!(result["purge_deferred"][0]["team"], f.team);
    assert!(f.state().purge_pending.contains_key(&f.team));
    fs::remove_dir(&path).unwrap();
    let result = f.purge();
    assert!(result.get("purge_deferred").is_none());
    assert!(f.state().purge_pending.is_empty());
}

#[test]
fn removal_touches_only_listed_segments() {
    let f = Fixture::new();
    let (_, path) = f.archived();
    let legacy = f.path("duties", 999);
    let bytes = b"unparseable Schedule: GAEBU archive marker";
    let unlisted = f.path("records", 7);
    let sidecar = legacy.with_extension("sidecar");
    fs::write(&legacy, bytes).unwrap();
    fs::write(&unlisted, b"unpublished runtime tail").unwrap();
    fs::write(&sidecar, b"sidecar").unwrap();
    let project = f.store.home().join("archive/project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("duties.jsonl"), bytes).unwrap();
    let protected = f.path("records", 999);
    fs::write(&protected, bytes).unwrap();
    f.edit(|st| {
        st.home_schedules.insert(
            fs::canonicalize(&protected)
                .unwrap()
                .to_string_lossy()
                .into(),
        );
        st.archives
            .get_mut(&f.team)
            .unwrap()
            .records
            .segments
            .push(ArchiveSegment {
                n: 999,
                bytes: bytes.len() as u64,
                lo: None,
                hi: None,
            });
    });
    f.cancel(31);
    f.purge();
    assert!(!path.exists());
    assert_eq!(fs::read(legacy).unwrap(), bytes);
    assert_eq!(fs::read(protected).unwrap(), bytes);
    assert_eq!(fs::read(unlisted).unwrap(), b"unpublished runtime tail");
    assert_eq!(fs::read(sidecar).unwrap(), b"sidecar");
    assert_eq!(fs::read(project.join("duties.jsonl")).unwrap(), bytes);
    assert!(f.state().purge_pending.is_empty());
}

#[test]
fn missing_directory_completes() {
    let f = Fixture::new();
    f.edit(|st| {
        st.archives.insert(f.team.clone(), TeamArchives::default());
    });
    f.cancel(31);
    assert_eq!(f.purge()["purged"].as_array().unwrap().len(), 1);
    assert!(f.state().purge_pending.is_empty());
}

#[test]
fn repair_after_purge_repurges() {
    let f = Fixture::new();
    let (_, path) = f.archived();
    f.cancel(31);
    f.purge();
    assert!(!path.exists());
    fs::write(f.store.home().join("state.json"), b"broken").unwrap();
    f.store.repair().unwrap();
    // The removal transaction's predecessor already has pending ownership.
    assert!(f.state().purge_pending.contains_key(&f.team));
    f.purge();
    assert!(f.state().purge_pending.is_empty());
    // Repair can also restore the generation from before the purge itself.
    let g = Fixture::new();
    let (_, p) = g.archived();
    g.cancel(31);
    let before = fs::read(g.store.home().join("state.json")).unwrap();
    g.purge();
    fs::write(g.store.home().join("state.prev.json"), before).unwrap();
    fs::write(g.store.home().join("state.json"), b"broken").unwrap();
    g.store.repair().unwrap();
    assert!(g.state().teams.contains_key(&g.team));
    assert!(!p.exists());
    assert_eq!(g.purge()["purged"].as_array().unwrap().len(), 1);
}

#[test]
fn seatless_compact_purges() {
    for bound in [false, true] {
        let f = Fixture::new();
        f.archived();
        f.cancel(31);
        let who = if bound {
            f.caller()
        } else {
            caller("unseated")
        };
        assert!(gaebu::schedule::compact(&f.store, &who, true).is_err());
        assert_eq!(
            gaebu::schedule::compact(&f.store, &who, false).unwrap()["purged"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn home_schedules_survive_compact_and_heal() {
    for access in ["absolute", "relative", "symlink"] {
        if access == "symlink" && !cfg!(unix) {
            continue;
        }
        for listed in [false, true] {
            let mut f = Fixture::new();
            let relative = PathBuf::from("target").join(format!(
                "gaebu-purge-relative-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _relative_cleanup = if access == "relative" {
                let home = relative.join("runtime");
                fs::create_dir_all(&home).unwrap();
                fs::copy(f.store.home().join("state.json"), home.join("state.json")).unwrap();
                f.store = Store::open(&home).unwrap();
                Some(common::RemoveOnDrop(relative))
            } else {
                None
            };
            if access == "symlink" {
                let alias = f.cleanup.0.join("home-link");
                #[cfg(unix)]
                std::os::unix::fs::symlink(fs::canonicalize(f.store.home()).unwrap(), &alias)
                    .unwrap();
                f.store = Store::open(alias).unwrap();
            }
            let (_, path) = f.archived();
            let protected = if listed { path } else { f.path("duties", 999) };
            let bytes = b"unparseable legacy Schedule";
            fs::write(&protected, bytes).unwrap();
            f.edit(|st| {
                st.home_schedules.insert(
                    fs::canonicalize(&protected)
                        .unwrap()
                        .to_string_lossy()
                        .into(),
                );
            });
            let id = f.duty();
            for heal in [false, true] {
                let result = gaebu::schedule::compact(&f.store, &f.caller(), heal);
                if listed {
                    assert_eq!(result.unwrap_err().reason, Reason::Conflict);
                    assert!(f.state().duties.contains_key(&id));
                } else {
                    result.unwrap();
                }
                assert_eq!(fs::read(&protected).unwrap(), bytes);
            }
            // A foreign protected stream is skipped, retaining its hot rows.
            if listed {
                let (other, _) = f.edit(|st| seed(st, "other"));
                let result = compact::run(&f.store, &other, None).unwrap();
                assert_eq!(result["skipped"][0]["team"], f.team);
                assert!(f.state().duties.contains_key(&id));
                assert_eq!(fs::read(protected).unwrap(), bytes);
            }
        }
    }
}

struct FakePager;
impl Pager for FakePager {
    fn whoami(&self, session: Option<&str>) -> gaebu::Result<WhoAmI> {
        let id = session.unwrap_or("human");
        Ok(WhoAmI {
            session_id: id.into(),
            alias: Some(id.into()),
            via: "native".into(),
            tool: Some("codex".into()),
        })
    }
    fn host_status(&self, _: &str) -> gaebu::Result<HostStatus> {
        Ok(HostStatus::Live)
    }
    fn send(&self, _: &str, _: &str) -> gaebu::Result<()> {
        Ok(())
    }
}
fn prompt(store: &Store, session: &str, text: &str, at: u64) {
    let text = evidence::normalize(text);
    store
        .session(session)
        .append(
            at,
            EventKind::UserPrompt,
            Some(evidence::sha256_hex(&text)),
            Some(text),
            None,
        )
        .unwrap();
}
fn cli(f: &Fixture, args: &[&str]) -> i32 {
    let _guard = CLI.lock().unwrap();
    let old = std::env::var_os("GAEBU_HOME");
    unsafe {
        std::env::set_var("GAEBU_HOME", f.store.home());
    }
    let result = gaebu::cli::run_with(
        std::iter::once("gaebu")
            .chain(args.iter().copied())
            .map(std::ffi::OsString::from),
        &FakePager,
    );
    unsafe {
        if let Some(old) = old {
            std::env::set_var("GAEBU_HOME", old);
        } else {
            std::env::remove_var("GAEBU_HOME");
        }
    }
    result
}

#[test]
fn root_cancel_triggers_purge_check() {
    let f = Fixture::new();
    f.archived();
    f.cancel(31);
    let (other, _) = f.edit(|st| seed(st, "other"));
    prompt(&f.store, "human", "cancel other", now_ms());
    assert_eq!(
        cli(
            &f,
            &[
                "cancel-team",
                &other,
                "--evidence",
                "cancel other",
                "--explicit"
            ]
        ),
        0
    );
    assert!(!f.state().teams.contains_key(&f.team));
    assert!(f.state().teams.contains_key(&other));
}

#[test]
fn purged_appointment_cannot_be_replayed() {
    let f = Fixture::new();
    let text = "appoint fresh";
    let at = now_ms() - 40 * DAY;
    prompt(&f.store, "human", text, at);
    assert_eq!(
        cli(&f, &["appoint", "fresh", "--evidence", text, "--explicit"]),
        0
    );
    let team = f
        .state()
        .teams
        .values()
        .find(|t| t.commander != f.actor.member)
        .unwrap()
        .id
        .clone();
    f.edit(|st| {
        ops::root_cancel_team(
            st,
            now_ms(),
            &words("other", now_ms() + 1, "cancel fresh"),
            &team,
        )
        .unwrap();
        st.teams.get_mut(&team).unwrap().cancelled_at = Some(now_ms() - 31 * DAY);
    });
    f.purge();
    let before = f.state().teams.len();
    assert_eq!(
        cli(&f, &["appoint", "fresh", "--evidence", text, "--explicit"]),
        1
    );
    assert_eq!(f.state().teams.len(), before);
    prompt(&f.store, "human", text, now_ms());
    assert_eq!(
        cli(&f, &["appoint", "fresh", "--evidence", text, "--explicit"]),
        0
    );
    assert_eq!(f.state().teams.len(), before + 1);
}
