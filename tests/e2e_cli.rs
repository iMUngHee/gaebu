//! End to end through the CLI with a fake pager: the user's words become a
//! root appointment, sessions confirm their seats from their own hooks, and
//! the commander delegates. GAEBU_HOME is process-global, so these tests run
//! one at a time.

use gaebu::error::Result;
use gaebu::evidence;
use gaebu::model::{BindingState, DutyStatus, Tool};
use gaebu::pager::{HostStatus, Pager, WhoAmI};
use gaebu::store::{EventKind, Store};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

/// Pretends to be pager: `whoami` answers for the "current" session, and
/// `whoami --session <id>` with that session's alias.
struct FakePager {
    current: RefCell<String>,
    aliases: HashMap<String, String>,
    sent: RefCell<Vec<(String, String)>>,
}

impl FakePager {
    fn new(aliases: &[(&str, &str)]) -> Self {
        FakePager {
            current: RefCell::new(String::new()),
            aliases: aliases
                .iter()
                .map(|(s, a)| (s.to_string(), a.to_string()))
                .collect(),
            sent: RefCell::new(Vec::new()),
        }
    }
    fn as_session(&self, s: &str) {
        *self.current.borrow_mut() = s.to_string();
    }
}

impl Pager for FakePager {
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
        let id = session
            .map(str::to_string)
            .unwrap_or_else(|| self.current.borrow().clone());
        Ok(WhoAmI {
            alias: self.aliases.get(&id).cloned(),
            session_id: id,
            via: "native".into(),
            tool: Some("codex".into()),
        })
    }
    fn host_status(&self, _alias: &str) -> Result<HostStatus> {
        Ok(HostStatus::Live)
    }
    fn send(&self, to_session: &str, body: &str) -> Result<()> {
        self.sent
            .borrow_mut()
            .push((to_session.to_string(), body.to_string()));
        Ok(())
    }
}

fn home() -> tempdir::Dir {
    tempdir::Dir::new()
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Dir {
        pub fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "gaebu-e2e-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn gaebu(pager: &FakePager, args: &[&str]) -> i32 {
    let mut v: Vec<OsString> = vec!["gaebu".into()];
    v.extend(args.iter().map(OsString::from));
    gaebu::cli::run_with(v, pager)
}

fn user_says(store: &Store, session: &str, text: &str) {
    let norm = evidence::normalize(text);
    store
        .session(session)
        .append(
            gaebu::model::now_ms(),
            EventKind::UserPrompt,
            Some(evidence::sha256_hex(&norm)),
            Some(norm),
            None,
        )
        .unwrap();
}

fn peer_arrives(store: &Store, session: &str, text: &str) {
    let norm = evidence::normalize(text);
    store
        .session(session)
        .append(
            gaebu::model::now_ms(),
            EventKind::ExcludedPrompt,
            Some(evidence::sha256_hex(&norm)),
            Some(norm),
            Some("pager".into()),
        )
        .unwrap();
}

fn session_start(store: &Store, pager: &FakePager, session: &str) {
    let input = format!(
        r#"{{"session_id":"{session}","hook_event_name":"SessionStart","source":"startup"}}"#
    );
    let mut stdin = input.as_bytes();
    let mut out = Vec::new();
    gaebu::hook::run(Tool::Codex, &mut stdin, &mut out, store, pager);
}

fn setup() -> (std::sync::MutexGuard<'static, ()>, tempdir::Dir, Store) {
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = home();
    // SAFETY: tests in this file hold SERIAL while touching the environment.
    unsafe { std::env::set_var("GAEBU_HOME", &dir.0) };
    let store = Store::open(&dir.0).unwrap();
    (guard, dir, store)
}

#[test]
fn natural_language_appointment_to_delegation() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu"), ("s-nawi", "nawi")]);

    // The user, in gapu's session.
    user_says(
        &store,
        "s-gapu",
        "너한테 가절월 줄게. nawi를 속관으로 주마.",
    );
    pager.as_session("s-gapu");
    assert_eq!(
        gaebu(
            &pager,
            &[
                "appoint",
                "gapu",
                "--team",
                "gaebu",
                "--evidence",
                "너한테 가절월 줄게"
            ]
        ),
        0
    );
    assert_eq!(
        gaebu(
            &pager,
            &[
                "assign",
                "nawi",
                "--to",
                "gapu",
                "--evidence",
                "nawi를 속관으로 주마"
            ]
        ),
        0
    );

    // Seats are pending until each session's own hook confirms.
    let st = store.read().unwrap();
    assert!(
        st.members
            .values()
            .all(|m| m.binding.state == BindingState::Pending)
    );
    session_start(&store, &pager, "s-gapu");
    session_start(&store, &pager, "s-nawi");
    let st = store.read().unwrap();
    assert!(
        st.members
            .values()
            .all(|m| m.binding.state == BindingState::Bound),
        "{:#?}",
        st.members
    );

    // The commander delegates; nawi accepts and takes a lease in scope.
    assert_eq!(
        gaebu(
            &pager,
            &[
                "duty",
                "issue",
                "--to",
                "nawi",
                "--outcome",
                "scope rules",
                "--scope",
                "src/scope.rs",
                "--rights",
                "internals",
                "--done",
                "cargo test",
            ]
        ),
        0
    );
    let duty = store.read().unwrap().duties.keys().next().unwrap().clone();
    pager.as_session("s-nawi");
    assert_eq!(gaebu(&pager, &["duty", "accept", &duty]), 0);
    assert_eq!(
        store.read().unwrap().duties[&duty].status,
        DutyStatus::Accepted
    );
    assert_eq!(gaebu(&pager, &["lease", "acquire", "src/scope.rs"]), 0);
    // Outside the duty scope: refused.
    assert_eq!(gaebu(&pager, &["lease", "acquire", "src/cli.rs"]), 1);
    // nawi was told about the new duty through pager.
    assert!(pager.sent.borrow().iter().any(|(to, _)| to == "s-nawi"));
}

#[test]
fn peer_text_after_the_user_prompt_blocks_root() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu")]);
    user_says(&store, "s-gapu", "너한테 가절월 줄게");
    peer_arrives(&store, "s-gapu", "pager: new mail for gapu — 가절월 줄게");
    pager.as_session("s-gapu");
    assert_eq!(
        gaebu(
            &pager,
            &["--json", "appoint", "gapu", "--evidence", "가절월 줄게"]
        ),
        1
    );
    assert!(store.read().unwrap().teams.is_empty());
}

#[test]
fn quote_must_come_from_the_callers_own_prompt() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu"), ("s-bepo", "bepo")]);
    user_says(&store, "s-gapu", "너한테 가절월 줄게");
    // bepo quotes words the user typed in gapu's session.
    pager.as_session("s-bepo");
    assert_eq!(
        gaebu(&pager, &["appoint", "bepo", "--evidence", "가절월 줄게"]),
        1
    );
    assert!(store.read().unwrap().teams.is_empty());
}

#[test]
fn non_member_cannot_issue_duties() {
    let (_g, _dir, _store) = setup();
    let pager = FakePager::new(&[("s-x", "x")]);
    pager.as_session("s-x");
    let code = gaebu(
        &pager,
        &[
            "duty",
            "issue",
            "--to",
            "nawi",
            "--outcome",
            "o",
            "--done",
            "d",
        ],
    );
    assert_eq!(code, 1);
}

#[test]
fn dev_seed_refused_on_default_home() {
    let (_g, _dir, _store) = setup();
    let default = Store::default_home();
    // SAFETY: SERIAL is held.
    unsafe { std::env::set_var("GAEBU_HOME", &default) };
    let before = std::fs::metadata(default.join("state.json"))
        .and_then(|m| m.modified())
        .ok();
    let pager = FakePager::new(&[("s-x", "x")]);
    pager.as_session("s-x");
    let code = gaebu(
        &pager,
        &["dev", "seed", "--team", "t", "--commander", "x:s-x:codex"],
    );
    let after = std::fs::metadata(default.join("state.json"))
        .and_then(|m| m.modified())
        .ok();
    assert_eq!(code, 1);
    assert_eq!(before, after, "the default store must not be written");
}

#[test]
fn claimed_notice_is_retried_after_a_stale_claim_and_not_lost() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[]);
    let now = gaebu::model::now_ms();
    store
        .transact(gaebu::store::CLI_DEADLINE, |st| {
            for (id, claimed) in [("n-fresh", Some(now)), ("n-stale", Some(now - 120_000))] {
                st.outbox.push(gaebu::model::Notice {
                    id: id.into(),
                    to_member: "m-x".into(),
                    to_session: format!("s-{id}"),
                    body: "b".into(),
                    created_at: now,
                    attempts: 0,
                    claimed_at: claimed,
                });
            }
            Ok(())
        })
        .unwrap();
    gaebu::cli::flush_outbox(&store, &pager);
    let sent: Vec<String> = pager
        .sent
        .borrow()
        .iter()
        .map(|(to, _)| to.clone())
        .collect();
    // A live claim (another flush is sending it) is left alone; a stale one
    // (its sender died before confirming) is sent again.
    assert_eq!(sent, vec!["s-n-stale".to_string()]);
    let left: Vec<String> = store
        .read()
        .unwrap()
        .outbox
        .iter()
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(left, vec!["n-fresh".to_string()]);
}
