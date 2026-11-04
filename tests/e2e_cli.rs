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
                    text: "b".into(),
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

// ----- holds and strength (plan gaebu-v0-2) -----

/// gapu commands, nawi accepted one duty. Returns the duty id.
fn team_with_duty(store: &Store, pager: &FakePager) -> String {
    user_says(store, "s-gapu", "너한테 가절월 줄게. nawi를 속관으로 주마.");
    pager.as_session("s-gapu");
    assert_eq!(
        gaebu(
            pager,
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
            pager,
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
    session_start(store, pager, "s-gapu");
    session_start(store, pager, "s-nawi");
    let issue = [
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
    ];
    assert_eq!(gaebu(pager, &issue), 0);
    let duty = store.read().unwrap().duties.keys().next().unwrap().clone();
    pager.as_session("s-nawi");
    assert_eq!(gaebu(pager, &["duty", "accept", &duty]), 0);
    duty
}

fn strength_of(store: &Store, action: &str) -> String {
    let st = store.read().unwrap();
    let (_, ra) = st
        .root_actions
        .iter()
        .filter(|(k, _)| k.contains(&format!(":{action}:")))
        .max_by_key(|(_, ra)| ra.rev)
        .unwrap_or_else(|| panic!("no root action {action}"));
    serde_json::to_value(ra.evidence.as_ref().unwrap().strength)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn hold_requires_user_evidence() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu"), ("s-nawi", "nawi")]);
    let duty = team_with_duty(&store, &pager);
    let hold = |quote: &str| {
        gaebu(
            &pager,
            &["hold", &duty, "--until", "대협 확인", "--evidence", quote],
        )
    };
    // The words are not in the user's current prompt.
    pager.as_session("s-gapu");
    assert_eq!(hold("잠깐 멈춰, 내가 확인할 때까지"), 1);
    // A peer's message is not the user.
    peer_arrives(&store, "s-gapu", "잠깐 멈춰, 내가 확인할 때까지");
    assert_eq!(hold("잠깐 멈춰, 내가 확인할 때까지"), 1);
    assert!(store.read().unwrap().duties[&duty].hold.is_none());
    // The user says it.
    user_says(&store, "s-gapu", "잠깐 멈춰, 내가 확인할 때까지");
    assert_eq!(hold("잠깐 멈춰, 내가 확인할 때까지"), 0);
    assert!(store.read().unwrap().duties[&duty].hold.is_some());
    // nawi cannot finish it, and cannot lift it without the user.
    pager.as_session("s-nawi");
    assert_eq!(gaebu(&pager, &["duty", "done", &duty]), 1);
    assert_eq!(
        gaebu(
            &pager,
            &["release", &duty, "--evidence", "이제 진행해도 좋아"]
        ),
        1
    );
    user_says(&store, "s-nawi", "이제 진행해도 좋아");
    assert_eq!(
        gaebu(
            &pager,
            &["release", &duty, "--evidence", "이제 진행해도 좋아"]
        ),
        0
    );
    assert_eq!(gaebu(&pager, &["duty", "done", &duty]), 0);
}

#[test]
fn explicit_words_later_confirm_an_inferred_appointment() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu")]);
    let title = || {
        let st = store.read().unwrap();
        let cmdr = st.teams.values().next().unwrap().commander.clone();
        gaebu::card::root_title(&st, &cmdr)
    };
    user_says(&store, "s-gapu", "너한테 가절월 줄게");
    pager.as_session("s-gapu");
    assert_eq!(
        gaebu(&pager, &["appoint", "gapu", "--evidence", "가절월 줄게"]),
        0
    );
    assert_eq!(title(), "가절월(추론)");
    // A later prompt; evidence must be newer than the appointment's.
    std::thread::sleep(std::time::Duration::from_millis(5));
    user_says(&store, "s-gapu", "gapu에게 가절월을 준 것 맞아");
    let confirm = [
        "appoint",
        "gapu",
        "--evidence",
        "gapu에게 가절월을 준 것 맞아",
    ];
    // Without --explicit it is the old refusal.
    assert_eq!(gaebu(&pager, &confirm), 1);
    assert_eq!(gaebu(&pager, &[&confirm[..], &["--explicit"]].concat()), 0);
    assert_eq!(title(), "가절월");
    let st = store.read().unwrap();
    assert_eq!(st.teams.len(), 1);
    assert!(
        st.records
            .iter()
            .any(|r| r.text.contains("was inferred: 가절월 줄게")),
        "the inferred quote stays in the team record"
    );
}

#[test]
fn root_ops_record_strength() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu"), ("s-nawi", "nawi"), ("s-bepo", "bepo")]);
    user_says(&store, "s-gapu", "gapu에게 가절월을 준다");
    pager.as_session("s-gapu");
    let root = |args: &[&str]| assert_eq!(gaebu(&pager, args), 0, "{args:?}");
    root(&[
        "appoint",
        "gapu",
        "--team",
        "t",
        "--evidence",
        "gapu에게 가절월을 준다",
        "--explicit",
    ]);
    let st = store.read().unwrap();
    let g = st.grants.values().find(|g| g.grantor.is_none()).unwrap();
    assert_eq!(
        g.evidence.as_ref().unwrap().strength,
        gaebu::model::Strength::Explicit
    );
    drop(st);
    assert_eq!(strength_of(&store, "appoint"), "explicit");
    user_says(&store, "s-gapu", "개부해라, nawi랑 bepo 데리고");
    root(&[
        "assign",
        "nawi",
        "bepo",
        "--to",
        "gapu",
        "--evidence",
        "개부해라, nawi랑 bepo",
    ]);
    assert_eq!(strength_of(&store, "assign"), "inferred");
    session_start(&store, &pager, "s-gapu");
    session_start(&store, &pager, "s-nawi");
    session_start(&store, &pager, "s-bepo");
    let issue = [
        "duty",
        "issue",
        "--to",
        "nawi",
        "--outcome",
        "o",
        "--scope",
        "src/**",
        "--rights",
        "r",
        "--done",
        "d",
    ];
    root(&issue);
    let st = store.read().unwrap();
    let duty = st.duties.keys().next().unwrap().clone();
    let nawi = st
        .members
        .values()
        .find(|m| m.binding.alias == "nawi")
        .unwrap()
        .id
        .clone();
    drop(st);
    user_says(&store, "s-gapu", "그 소임 잠깐 멈춰");
    root(&[
        "hold",
        &duty,
        "--until",
        "확인 후",
        "--evidence",
        "그 소임 잠깐 멈춰",
        "--explicit",
    ]);
    assert_eq!(strength_of(&store, "hold"), "explicit");
    assert_eq!(
        store.read().unwrap().duties[&duty]
            .hold
            .as_ref()
            .unwrap()
            .evidence
            .strength,
        gaebu::model::Strength::Explicit
    );
    user_says(&store, "s-gapu", "이제 됐어 계속해");
    root(&["release", &duty, "--evidence", "이제 됐어 계속해"]);
    assert_eq!(strength_of(&store, "release"), "inferred");
    user_says(&store, "s-gapu", "nawi 자리는 nuzu로 바꿔");
    root(&[
        "rebind",
        &nawi,
        "--alias",
        "nuzu",
        "--evidence",
        "nawi 자리는 nuzu로 바꿔",
        "--explicit",
    ]);
    assert_eq!(strength_of(&store, "rebind"), "explicit");
    user_says(&store, "s-gapu", "bepo는 이제 빠져도 돼");
    root(&["dismiss", "bepo", "--evidence", "bepo는 이제 빠져도 돼"]);
    assert_eq!(strength_of(&store, "dismiss"), "inferred");
    user_says(&store, "s-gapu", "팀 t 해산해");
    root(&[
        "cancel-team",
        "t",
        "--evidence",
        "팀 t 해산해",
        "--explicit",
    ]);
    assert_eq!(strength_of(&store, "cancel-team"), "explicit");
}

#[test]
fn source_from_other_session_refused() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu"), ("s-nawi", "nawi")]);
    let duty = team_with_duty(&store, &pager);
    // The user's words were said in nawi's session, not the issuer's.
    user_says(&store, "s-nawi", "린터 규칙도 정리해 줘");
    pager.as_session("s-gapu");
    let issue = |quote: &str| {
        gaebu(
            &pager,
            &[
                "duty",
                "issue",
                "--to",
                "nawi",
                "--outcome",
                "lint rules",
                "--scope",
                "src/scope.rs",
                "--rights",
                "r",
                "--done",
                "d",
                "--source",
                quote,
            ],
        )
    };
    assert_eq!(issue("린터 규칙도 정리해 줘"), 1);
    // Through MCP the same quote is refused too.
    let call = |args: serde_json::Value| -> serde_json::Value {
        let input = format!(
            "{}\n",
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"duty_issue","arguments":args,"_meta":{"threadId":"s-gapu"}}})
        );
        let mut out = Vec::new();
        gaebu::mcp::serve(&mut std::io::Cursor::new(input), &mut out, &store, &pager).unwrap();
        serde_json::from_slice(&out).unwrap()
    };
    let spec = |quote: &str| {
        serde_json::json!({"assignee":"nawi","spec":{"intent":"i","outcome":"lint rules",
            "scope":["src/scope.rs"],"decision_rights":"r","escalate_on":[],"done_criteria":"d",
            "source_quote":quote}})
    };
    let reply = call(spec("린터 규칙도 정리해 줘"));
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let before = store.read().unwrap().duties.len();
    // Said in the issuer's own session: recorded with the duty.
    user_says(&store, "s-gapu", "린터 규칙도 정리해 줘");
    assert_eq!(issue("린터 규칙도 정리해 줘"), 0);
    let reply = call(spec("린터 규칙도 정리해"));
    assert_ne!(reply["result"]["isError"], true, "{reply}");
    let st = store.read().unwrap();
    assert_eq!(st.duties.len(), before + 2);
    let sourced: Vec<_> = st
        .duties
        .values()
        .filter_map(|d| d.source.as_ref())
        .collect();
    assert_eq!(sourced.len(), 2);
    assert!(sourced.iter().all(|s| s.session_id == "s-gapu"));
    // Amending without a source keeps the one recorded.
    let lint = st
        .duties
        .values()
        .find(|d| d.source.is_some() && d.id != duty)
        .unwrap()
        .clone();
    drop(st);
    let v = lint.version.to_string();
    let amend = [
        "duty",
        "amend",
        &lint.id,
        "--version",
        &v,
        "--outcome",
        "lint rules v2",
        "--scope",
        "src/scope.rs",
        "--done",
        "d",
    ];
    assert_eq!(gaebu(&pager, &amend), 0);
    assert_eq!(store.read().unwrap().duties[&lint.id].source, lint.source);
}

fn mcp_board(store: &Store, pager: &FakePager, thread: &str) -> gaebu::card::BoardView {
    let input = format!(
        "{}\n",
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"board","arguments":{},"_meta":{"threadId":thread}}})
    );
    let mut out = Vec::new();
    gaebu::mcp::serve(&mut std::io::Cursor::new(input), &mut out, store, pager).unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_ne!(reply["result"]["isError"], true, "{reply}");
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// Stage of every duty in a board view.
fn board_stages(v: &gaebu::card::BoardView) -> Vec<(String, gaebu::card::Stage)> {
    let mut out: Vec<_> = v
        .teams
        .iter()
        .flat_map(|t| &t.bus)
        .flat_map(|b| &b.members)
        .flat_map(|m| &m.duties)
        .map(|d| (d.id.clone(), d.stage))
        .collect();
    out.sort();
    out
}

#[test]
fn board_and_card_agree_on_stage() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu"), ("s-nawi", "nawi")]);
    let held = team_with_duty(&store, &pager);
    pager.as_session("s-gapu");
    let issue = [
        "duty",
        "issue",
        "--to",
        "nawi",
        "--outcome",
        "o",
        "--scope",
        "src/scope.rs",
        "--rights",
        "r",
        "--done",
        "d",
    ];
    assert_eq!(gaebu(&pager, &issue), 0);
    assert_eq!(gaebu(&pager, &issue), 0);
    assert_eq!(gaebu(&pager, &issue), 0);
    let st = store.read().unwrap();
    let mut ids: Vec<_> = st.duties.keys().filter(|d| **d != held).cloned().collect();
    ids.sort();
    drop(st);
    user_says(&store, "s-gapu", "첫 소임은 멈춰 둬");
    assert_eq!(
        gaebu(
            &pager,
            &[
                "hold",
                &held,
                "--until",
                "확인 후",
                "--evidence",
                "첫 소임은 멈춰 둬"
            ]
        ),
        0
    );
    pager.as_session("s-nawi");
    assert_eq!(gaebu(&pager, &["duty", "accept", &ids[0]]), 0);
    assert_eq!(
        gaebu(&pager, &["escalate", "which file?", "--duty", &ids[0]]),
        0
    );
    assert_eq!(gaebu(&pager, &["duty", "accept", &ids[2]]), 0);
    for quiet in [false, true] {
        if quiet {
            // nawi's session goes quiet: accepted duties turn into 자리 빔.
            store
                .session("s-nawi")
                .update_sighting(|s| s.last_seen = Some(gaebu::model::now_ms() - 3 * 3_600_000))
                .unwrap();
        }
        let st = store.read().unwrap();
        let now = gaebu::model::now_ms();
        let nawi = st
            .members
            .values()
            .find(|m| m.binding.alias == "nawi")
            .unwrap();
        let card = gaebu::card::build(&st, &nawi.id, now, store.last_seen("s-nawi")).unwrap();
        let mut from_card: Vec<_> = card
            .duties
            .iter()
            .map(|d| (d.id.clone(), d.stage))
            .collect();
        from_card.sort();
        let cli = gaebu::card::board(&st, now, None, &|sid| store.last_seen(sid));
        let mcp = mcp_board(&store, &pager, "s-gapu");
        assert_eq!(board_stages(&cli), from_card, "quiet={quiet}");
        assert_eq!(board_stages(&mcp), from_card, "quiet={quiet}");
        use gaebu::card::Stage;
        let worked = if quiet { Stage::Vacant } else { Stage::Working };
        assert!(
            from_card.contains(&(ids[2].clone(), worked)),
            "{from_card:?}"
        );
        assert!(from_card.contains(&(held.clone(), Stage::Held)));
        assert!(from_card.contains(&(ids[0].clone(), Stage::AwaitingAnswer)));
        assert!(from_card.contains(&(ids[1].clone(), Stage::AwaitingAcceptance)));
    }
}

#[test]
fn board_since_detects_quiet_transition() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[("s-gapu", "gapu"), ("s-nawi", "nawi")]);
    team_with_duty(&store, &pager);
    store
        .session("s-nawi")
        .update_sighting(|s| s.last_seen = Some(gaebu::model::now_ms()))
        .unwrap();
    let st = store.read().unwrap();
    let before = gaebu::card::board(&st, gaebu::model::now_ms(), None, &|sid| {
        store.last_seen(sid)
    });
    let again = gaebu::card::board(&st, gaebu::model::now_ms(), None, &|sid| {
        store.last_seen(sid)
    });
    assert_eq!(before.token, again.token, "nothing changed: same token");
    // A commit that changes nothing on the board keeps the token.
    store
        .transact(gaebu::store::CLI_DEADLINE, |st| {
            st.next_id += 1;
            Ok(())
        })
        .unwrap();
    let st = store.read().unwrap();
    let committed = gaebu::card::board(&st, gaebu::model::now_ms(), None, &|sid| {
        store.last_seen(sid)
    });
    assert_ne!(committed.rev, before.rev);
    assert_eq!(
        committed.token, before.token,
        "rev alone must not change the token"
    );
    let before = committed;
    // No commit at all, only time: nawi falls quiet.
    store
        .session("s-nawi")
        .update_sighting(|s| s.last_seen = Some(gaebu::model::now_ms() - 3 * 3_600_000))
        .unwrap();
    let st = store.read().unwrap();
    let after = gaebu::card::board(&st, gaebu::model::now_ms(), None, &|sid| {
        store.last_seen(sid)
    });
    assert_eq!(after.rev, before.rev);
    assert_ne!(
        after.token, before.token,
        "a quiet transition must not read as unchanged"
    );
    let stages = board_stages(&after);
    assert!(
        stages.iter().all(|(_, s)| *s == gaebu::card::Stage::Vacant),
        "{stages:?}"
    );
    // The CLI answers `unchanged` only for the current token.
    assert_eq!(
        gaebu(&pager, &["--json", "board", "--since", &before.token]),
        0
    );
    assert_eq!(
        gaebu(&pager, &["--json", "board", "--since", &after.token]),
        0
    );
}

#[test]
fn compose_heartbeat_marks_only_the_sighting() {
    let (_g, _dir, store) = setup();
    let pager = FakePager::new(&[]);
    let beat = ["session", "heartbeat", "--session", "s-x", "--compose"];
    assert_eq!(gaebu(&pager, &beat), 0);
    let log = store.session("s-x");
    let seen = log.sighting().unwrap();
    assert!(seen.compose_heartbeat.is_some());
    assert!(seen.mod_heartbeat.is_none());
    // Evidence reads mod heartbeats from the event log; a compose stamp adds none.
    assert!(
        log.events()
            .unwrap()
            .iter()
            .all(|e| e.kind != EventKind::ModHeartbeat)
    );
}
