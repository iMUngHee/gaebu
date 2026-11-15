mod common;

use gaebu::{
    Result, authz,
    identity::{Caller, Source as CallerSource},
    model::*,
    ops::{self, DutySpec, SeedSpec},
    pager::{HostStatus, Hosts, Message, Pager, WhoAmI},
    schedule,
    stalled::{self, Finding, Kind, Recipient},
    store::{CLI_DEADLINE, EventKind, Store},
};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const MINUTE: Ms = 60_000;
/// The threshold every test judges with.
const AFTER: Ms = 10 * MINUTE;

/// pager as a roster only: `None` makes the read fail.
struct Roster(RefCell<Option<Hosts>>);

impl Pager for Roster {
    fn whoami(&self, _: Option<&str>) -> Result<WhoAmI> {
        panic!("stall detection resolves no caller")
    }
    fn hosts(&self) -> Result<Hosts> {
        self.0.borrow().clone().ok_or_else(|| {
            gaebu::error::Error::new(gaebu::Reason::PagerUnavailable, "roster unavailable")
        })
    }
    fn send(&self, _: &Message<'_>) -> Result<()> {
        Ok(())
    }
}

/// A team "watch": commander boss, 속관 aide and clerk, each bound to
/// `s-<alias>`.
struct World {
    _cleanup: common::RemoveOnDrop,
    dir: PathBuf,
    store: Store,
    pager: Roster,
    now: Ms,
}

impl World {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-stalled-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("runtime")).unwrap();
        let now = now_ms();
        let world = World {
            _cleanup: common::RemoveOnDrop(dir.clone()),
            dir,
            store,
            pager: Roster(RefCell::new(Some(Hosts::all(HostStatus::Live)))),
            now,
        };
        world.tx(|st| {
            let seat = |alias: &str| (alias.into(), Some(format!("s-{alias}")), Some(Tool::Codex));
            ops::dev_seed(
                st,
                now - 600 * MINUTE,
                &SeedSpec {
                    team: "watch".into(),
                    project: "project".into(),
                    commander_alias: "boss".into(),
                    commander_session: Some("s-boss".into()),
                    commander_tool: Some(Tool::Codex),
                    members: vec![seat("aide"), seat("clerk")],
                },
            )
        });
        // Every session is mid-turn unless a test says it stopped.
        for alias in ["boss", "aide", "clerk"] {
            world.prompts(alias, 500);
        }
        world
    }
    fn tx<T>(&self, f: impl FnOnce(&mut State) -> Result<T>) -> T {
        self.store.transact(CLI_DEADLINE, f).unwrap()
    }
    fn event(&self, alias: &str, minutes_ago: u64, kind: EventKind) {
        self.store
            .session(&format!("s-{alias}"))
            .append(self.now - minutes_ago * MINUTE, kind, None, None, None)
            .unwrap();
    }
    fn prompts(&self, alias: &str, minutes_ago: u64) {
        self.event(alias, minutes_ago, EventKind::UserPrompt);
    }
    fn stops(&self, alias: &str, minutes_ago: u64) {
        self.event(alias, minutes_ago, EventKind::Stop);
    }
    fn actor(&self, alias: &str) -> Actor {
        let session = format!("s-{alias}");
        let st = self.store.read().unwrap();
        Actor {
            member: authz::member_for_session(&st, &session).unwrap().id.clone(),
            session_id: session,
        }
    }
    fn id(&self, alias: &str) -> String {
        self.actor(alias).member
    }
    fn team(&self) -> String {
        self.store
            .read()
            .unwrap()
            .teams
            .keys()
            .next()
            .unwrap()
            .clone()
    }
    /// boss proposes a duty to `alias`.
    fn proposes(&self, alias: &str) -> String {
        let (boss, to) = (self.actor("boss"), self.id(alias));
        self.tx(|st| ops::duty_issue(st, now_ms(), &boss, &to, spec()))
            .ids[0]
            .clone()
    }
    /// boss issues a duty that `alias` accepts.
    fn accepted(&self, alias: &str) -> String {
        let duty = self.proposes(alias);
        let assignee = self.actor(alias);
        self.tx(|st| ops::duty_respond(st, now_ms(), &assignee, &duty, None, true, None));
        duty
    }
    fn caller(&self) -> Caller {
        Caller {
            session_id: "s-boss".into(),
            tool: Some(Tool::Codex),
            source: CallerSource::PagerNative,
            alias: None,
        }
    }
    /// Register a Schedule for the team and return its path.
    fn schedule(&self) -> PathBuf {
        let path = self.dir.join("schedule.md");
        schedule::register(&self.store, &self.caller(), &path, true).unwrap();
        path
    }
    /// Add a Schedule item owned by the commander's bu.
    fn item(&self, title: &str) -> String {
        let bu = self.store.read().unwrap().members[&self.id("boss")]
            .bu
            .clone();
        schedule::call(
            &self.store,
            &self.pager,
            &self.caller(),
            "schedule_add",
            &json!({"title":title,"scope":["src/**"],"owner_bu":bu}),
        )
        .unwrap()["item"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn findings(&self) -> Vec<Finding> {
        stalled::find(&self.store, &self.pager, AFTER, self.now)
            .unwrap()
            .findings
    }
    fn of(&self, kind: Kind) -> Vec<Finding> {
        self.findings()
            .into_iter()
            .filter(|finding| finding.kind == kind)
            .collect()
    }
    /// pager reports `alias` gone and everyone else live.
    fn gone(&self, alias: &str) {
        let rows = ["boss", "aide", "clerk"]
            .into_iter()
            .map(|name| {
                let status = if name == alias {
                    HostStatus::Gone
                } else {
                    HostStatus::Live
                };
                (name.to_owned(), status)
            })
            .collect();
        *self.pager.0.borrow_mut() = Some(Hosts::from_rows(rows));
    }
}

fn spec() -> DutySpec {
    DutySpec {
        intent: "why".into(),
        outcome: "deliver".into(),
        scope: vec!["src/**".into()],
        done_criteria: "tests pass".into(),
        ..Default::default()
    }
}

fn alias_of(recipient: &Recipient) -> &str {
    match recipient {
        Recipient::Member(party) => &party.alias,
        Recipient::Human => "human",
    }
}

fn waited(finding: &Finding) -> &str {
    finding.waiting_on.as_ref().map_or("", |p| p.alias.as_str())
}

/// Every file below `root` with its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

#[test]
fn stalled_is_read_only() {
    let w = World::new();
    w.schedule();
    w.item("ready work");
    let duty = w.accepted("aide");
    w.stops("aide", 30);
    w.stops("boss", 30);
    w.gone("clerk");
    let clerk_duty = w.accepted("clerk");
    let before = snapshot(&w.dir);

    let found = w.findings();
    assert!(found.iter().any(|f| f.subject == duty));
    assert!(
        found
            .iter()
            .any(|f| f.subject == clerk_duty && f.kind == Kind::HostGone)
    );
    // Nothing written: no file changed, appeared or went away, and the seat
    // pager reports gone is still bound.
    assert_eq!(snapshot(&w.dir), before);
    let st = w.store.read().unwrap();
    assert_eq!(
        st.members[&w.id("clerk")].binding.state,
        BindingState::Bound
    );

    // An empty home gets no state from being looked at.
    let empty = w.dir.join("empty");
    let store = Store::open(&empty).unwrap();
    let report = stalled::find(&store, &w.pager, AFTER, w.now).unwrap();
    assert!(report.findings.is_empty());
    assert!(!empty.join("state.json").exists());
}

#[test]
fn formed_team_without_work() {
    let w = World::new();
    let boss = w.id("boss");
    // The commander was appointed and its session never confirmed the seat.
    let set_pending = |designated_at: Ms| {
        w.tx(|st| {
            let binding = &mut st.members.get_mut(&boss).unwrap().binding;
            binding.state = BindingState::Pending;
            binding.session_id = None;
            binding.designated_at = designated_at;
            Ok(())
        })
    };
    set_pending(w.now - 30 * MINUTE);
    let pending = w.of(Kind::SeatPending);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].subject, boss);
    assert_eq!(waited(&pending[0]), "boss");
    // Nobody in the team can be told about its own commander.
    assert_eq!(pending[0].report_to, Recipient::Human);
    assert_eq!(pending[0].since, Some(w.now - 30 * MINUTE));

    // A seat re-pointed a moment ago has not waited yet, however old it is.
    set_pending(w.now - MINUTE);
    assert!(w.of(Kind::SeatPending).is_empty());
    set_pending(w.now - 10 * MINUTE);
    assert_eq!(w.of(Kind::SeatPending).len(), 1);
}

#[test]
fn kind_seat_pending_reports_to_the_commander() {
    let w = World::new();
    let aide = w.id("aide");
    w.tx(|st| {
        let binding = &mut st.members.get_mut(&aide).unwrap().binding;
        binding.state = BindingState::Pending;
        binding.designated_at = w.now - 20 * MINUTE;
        Ok(())
    });
    let pending = w.of(Kind::SeatPending);
    assert_eq!(pending.len(), 1);
    assert_eq!(alias_of(&pending[0].report_to), "boss");

    // A dismissed seat waits for nobody.
    w.tx(|st| {
        st.members.get_mut(&aide).unwrap().status = MemberStatus::Dismissed;
        Ok(())
    });
    assert!(w.of(Kind::SeatPending).is_empty());
}

#[test]
fn kind_team_idle() {
    let w = World::new();
    // The commander is mid-turn: forming the team is still under way.
    assert!(w.of(Kind::TeamIdle).is_empty());

    w.stops("boss", 30);
    let idle = w.of(Kind::TeamIdle);
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0].subject, w.team());
    assert_eq!(waited(&idle[0]), "boss");
    // The commander is the one resting, and there is no manager.
    assert_eq!(idle[0].report_to, Recipient::Human);

    // A stop a moment ago is not a rest yet.
    w.prompts("boss", 8);
    w.stops("boss", 5);
    assert!(w.of(Kind::TeamIdle).is_empty());
    w.prompts("boss", 4);
    w.stops("boss", 3);
    assert!(w.of(Kind::TeamIdle).is_empty());
}

#[test]
fn kind_team_idle_needs_no_work_at_all() {
    let w = World::new();
    w.stops("boss", 30);
    assert_eq!(w.of(Kind::TeamIdle).len(), 1);

    // An open duty is work, whoever holds it.
    let duty = w.proposes("aide");
    assert!(w.of(Kind::TeamIdle).is_empty());
    let boss = w.actor("boss");
    w.tx(|st| ops::duty_cancel(st, now_ms(), &boss, &duty, "not needed"));
    assert_eq!(w.of(Kind::TeamIdle).len(), 1);

    // So is a live Schedule item.
    w.schedule();
    assert_eq!(w.of(Kind::TeamIdle).len(), 1);
    w.item("ready work");
    assert!(w.of(Kind::TeamIdle).is_empty());
}

#[test]
fn kind_idle_after_stop() {
    let w = World::new();
    let duty = w.accepted("aide");
    // Mid-turn.
    assert!(w.of(Kind::IdleAfterStop).is_empty());

    w.stops("aide", 30);
    let idle = w.of(Kind::IdleAfterStop);
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0].subject, duty);
    assert_eq!(waited(&idle[0]), "aide");
    assert_eq!(alias_of(&idle[0].report_to), "boss");
    assert_eq!(idle[0].since, Some(w.now - 30 * MINUTE));

    // A turn that pager's poke started is a turn.
    w.event("aide", 20, EventKind::ExcludedPrompt);
    assert!(w.of(Kind::IdleAfterStop).is_empty());
    // So is one the mod saw submitted.
    w.stops("aide", 19);
    assert_eq!(w.of(Kind::IdleAfterStop).len(), 1);
    w.event("aide", 18, EventKind::ModHeartbeat);
    assert!(w.of(Kind::IdleAfterStop).is_empty());
}

#[test]
fn kind_idle_after_stop_outlasts_the_quiet_threshold() {
    let w = World::new();
    let duty = w.accepted("clerk");
    // Long enough that the card calls the seat quiet and the duty vacant.
    w.stops("clerk", 180);
    let idle = w.of(Kind::IdleAfterStop);
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0].subject, duty);
}

#[test]
fn kind_counterpart_idle_and_hold() {
    let w = World::new();
    // Proposed and not answered: the assignee owes the answer.
    let duty = w.proposes("aide");
    w.stops("aide", 30);
    let waiting = w.of(Kind::CounterpartIdle);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].subject, duty);
    assert_eq!(waited(&waiting[0]), "aide");
    assert!(w.of(Kind::IdleAfterStop).is_empty());

    // A human hold: nobody in the team is waited on.
    let said = Evidence {
        session_id: "s-boss".into(),
        prompt_seq: 1,
        sha256: "hash".into(),
        attest: Attest::Fixture,
        quote: "wait".into(),
        at: now_ms(),
        strength: Strength::Explicit,
    };
    w.tx(|st| ops::root_hold(st, now_ms(), &said, &duty, "until I look"));
    assert!(w.findings().is_empty());
}

#[test]
fn kind_counterpart_idle_follows_the_question() {
    let w = World::new();
    let duty = w.accepted("aide");
    w.stops("aide", 30);
    w.stops("boss", 30);
    // The assignee asked its superior: the assignee is no longer the one
    // resting on the job.
    let aide = w.actor("aide");
    w.tx(|st| ops::escalate(st, now_ms(), &aide, Some(&duty), "which way?"));
    let waiting = w.of(Kind::CounterpartIdle);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waited(&waiting[0]), "boss");
    assert_eq!(waiting[0].report_to, Recipient::Human);
    assert!(w.of(Kind::IdleAfterStop).is_empty());

    // A question in the human's own queue is the human's to answer, and the
    // board already shows it.
    let queued = w.accepted("clerk");
    w.stops("clerk", 30);
    let clerk = w.actor("clerk");
    w.tx(|st| ops::escalate(st, now_ms(), &clerk, Some(&queued), "for the human"));
    w.tx(|st| {
        let asked = st
            .escalations
            .values_mut()
            .find(|e| e.duty.as_deref() == Some(&queued))
            .unwrap();
        asked.to = None;
        Ok(())
    });
    assert!(w.findings().iter().all(|f| f.subject != queued));
}

#[test]
fn kind_counterpart_idle_for_a_blocked_duty() {
    let w = World::new();
    let duty = w.accepted("aide");
    let aide = w.actor("aide");
    w.tx(|st| {
        ops::duty_progress(
            st,
            now_ms(),
            &aide,
            &duty,
            DutyStatus::Blocked,
            Some("need a key"),
        )
    });
    w.stops("aide", 30);
    // The issuer has to unblock it, and is mid-turn.
    assert!(w.findings().is_empty());
    w.stops("boss", 30);
    let waiting = w.of(Kind::CounterpartIdle);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waited(&waiting[0]), "boss");
}

#[test]
fn kind_ready_but_idle() {
    let w = World::new();
    w.schedule();
    let item = w.item("ready work");
    assert!(w.of(Kind::ReadyButIdle).is_empty());

    w.stops("boss", 30);
    let ready = w.of(Kind::ReadyButIdle);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].subject, item);
    // The head of the item's bu dispatches it.
    assert_eq!(waited(&ready[0]), "boss");
}

#[test]
fn kind_host_gone() {
    let w = World::new();
    let duty = w.accepted("aide");
    w.stops("aide", 30);
    assert!(w.of(Kind::HostGone).is_empty());

    w.gone("aide");
    let gone = w.of(Kind::HostGone);
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].subject, duty);
    // The finding names the member whose session is gone.
    assert_eq!(waited(&gone[0]), "aide");
    assert_eq!(alias_of(&gone[0].report_to), "boss");
    // Said once: a session that is gone is not also reported as resting.
    assert!(w.of(Kind::IdleAfterStop).is_empty());
}

#[test]
fn kind_unknown_when_a_log_cannot_say() {
    let w = World::new();
    let duty = w.accepted("aide");
    w.stops("aide", 30);
    assert_eq!(w.of(Kind::IdleAfterStop).len(), 1);

    // A prompt the hook failed to record may have started a turn.
    w.store
        .session("s-aide")
        .mark_unrecorded(w.now - 20 * MINUTE)
        .unwrap();
    assert!(w.of(Kind::IdleAfterStop).is_empty());
    let unknown = w.of(Kind::Unknown);
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].subject, duty);

    // A session that never recorded anything.
    let clerk_duty = w.accepted("clerk");
    fs::remove_dir_all(w.dir.join("runtime/sessions/s-clerk")).unwrap();
    let unknown = w.of(Kind::Unknown);
    assert!(unknown.iter().any(|f| f.subject == clerk_duty));
    assert!(w.of(Kind::IdleAfterStop).is_empty());
}

#[test]
fn kind_unknown_when_the_schedule_or_roster_cannot_be_read() {
    let w = World::new();
    let path = w.schedule();
    w.stops("boss", 30);
    assert_eq!(w.of(Kind::TeamIdle).len(), 1);

    // An unreadable Schedule is not an empty one.
    fs::write(&path, "not a schedule").unwrap();
    assert!(w.of(Kind::TeamIdle).is_empty());
    let unknown = w.of(Kind::Unknown);
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].subject, w.team());

    *w.pager.0.borrow_mut() = None;
    let unknown = w.of(Kind::Unknown);
    assert!(unknown.iter().any(|f| f.subject == "pager"));
}

#[test]
fn routing_and_key() {
    let w = World::new();
    let duty = w.accepted("aide");
    w.stops("aide", 30);
    let first = w.of(Kind::IdleAfterStop).remove(0);
    assert_eq!(alias_of(&first.report_to), "boss");
    assert_eq!(first.key.len(), 16);
    assert_eq!(w.of(Kind::IdleAfterStop)[0].key, first.key);

    // A seated manager hears it before the commander does.
    let clerk = w.id("clerk");
    w.schedule();
    w.tx(|st| {
        st.schedules.values_mut().next().unwrap().manager = Some(clerk.clone());
        Ok(())
    });
    assert_eq!(alias_of(&w.of(Kind::IdleAfterStop)[0].report_to), "clerk");
    // Whoever hears it stopping or starting does not make it a new finding.
    w.stops("clerk", 40);
    w.stops("boss", 40);
    assert_eq!(w.of(Kind::IdleAfterStop)[0].key, first.key);

    // A manager whose host is gone cannot hear it; the commander can.
    w.gone("clerk");
    assert_eq!(alias_of(&w.of(Kind::IdleAfterStop)[0].report_to), "boss");
    // With both gone, only the human is left.
    *w.pager.0.borrow_mut() = Some(Hosts::from_rows(vec![
        ("boss".into(), HostStatus::Gone),
        ("clerk".into(), HostStatus::Gone),
    ]));
    assert_eq!(w.of(Kind::IdleAfterStop)[0].report_to, Recipient::Human);
    *w.pager.0.borrow_mut() = Some(Hosts::all(HostStatus::Live));

    // The manager is never told about its own rest.
    let clerk_duty = w.accepted("clerk");
    let about_clerk = w
        .of(Kind::IdleAfterStop)
        .into_iter()
        .find(|f| f.subject == clerk_duty)
        .unwrap();
    assert_eq!(alias_of(&about_clerk.report_to), "boss");

    // The assignee works again and stops again: a new finding.
    w.prompts("aide", 25);
    w.stops("aide", 20);
    let again = w
        .of(Kind::IdleAfterStop)
        .into_iter()
        .find(|f| f.subject == duty)
        .unwrap();
    assert_ne!(again.key, first.key);

    // Another session in the same seat, stopped at the same place in its
    // own log, is not the same finding either.
    let aide = w.id("aide");
    let seq = w.store.session("s-aide").events().unwrap().len();
    w.tx(|st| {
        st.members.get_mut(&aide).unwrap().binding.session_id = Some("s-other".into());
        Ok(())
    });
    let other = w.store.session("s-other");
    for _ in 0..seq - 1 {
        other
            .append(w.now - 60 * MINUTE, EventKind::UserPrompt, None, None, None)
            .unwrap();
    }
    other
        .append(w.now - 20 * MINUTE, EventKind::Stop, None, None, None)
        .unwrap();
    let rebound = w
        .of(Kind::IdleAfterStop)
        .into_iter()
        .find(|f| f.subject == duty)
        .unwrap();
    assert_eq!(rebound.since, again.since);
    assert_ne!(rebound.key, again.key);
}

#[test]
fn ready_item_key_names_its_master() {
    let w = World::new();
    w.schedule();
    let item = w.item("ready work");
    w.stops("boss", 30);
    w.stops("aide", 30);
    let by_boss = w.of(Kind::ReadyButIdle).remove(0);
    assert_eq!(by_boss.subject, item);

    // The item's bu gets another head, stopped at the same place in its log.
    let (boss, aide) = (w.id("boss"), w.id("aide"));
    w.tx(|st| {
        let bu = st.members[&boss].bu.clone();
        st.bus.get_mut(&bu).unwrap().head = aide.clone();
        Ok(())
    });
    let by_aide = w.of(Kind::ReadyButIdle).remove(0);
    assert_eq!(waited(&by_aide), "aide");
    assert_ne!(by_aide.key, by_boss.key);
}

#[test]
fn report_renders_one_line_per_finding() {
    let w = World::new();
    let report = stalled::find(&w.store, &w.pager, AFTER, w.now).unwrap();
    assert!(stalled::render(&report).contains("발견 없음"));

    let duty = w.accepted("aide");
    w.stops("aide", 30);
    let report = stalled::find(&w.store, &w.pager, AFTER, w.now).unwrap();
    assert_eq!(report.after_minutes, 10);
    let text = stalled::render(&report);
    assert!(text.contains("1건"), "{text}");
    let line = text.lines().nth(1).unwrap();
    assert!(line.contains("idle_after_stop"), "{line}");
    assert!(line.contains(&duty), "{line}");
    assert!(line.contains("→ boss"), "{line}");
    assert!(line.contains(&report.findings[0].key), "{line}");
}

#[test]
fn rest_counts_from_the_threshold_itself() {
    let w = World::new();
    w.accepted("aide");
    w.stops("aide", 9);
    assert!(w.of(Kind::IdleAfterStop).is_empty());
    let w = World::new();
    w.accepted("aide");
    w.stops("aide", 10);
    assert_eq!(w.of(Kind::IdleAfterStop).len(), 1);
}

#[test]
fn recipient_must_hold_a_confirmed_seat() {
    let w = World::new();
    w.accepted("aide");
    w.stops("aide", 30);
    let clerk = w.id("clerk");
    w.schedule();
    let set = |f: &dyn Fn(&mut Member)| {
        w.tx(|st| {
            st.schedules.values_mut().next().unwrap().manager = Some(clerk.clone());
            f(st.members.get_mut(&clerk).unwrap());
            Ok(())
        })
    };
    set(&|_| {});
    assert_eq!(alias_of(&w.of(Kind::IdleAfterStop)[0].report_to), "clerk");
    // A manager whose seat awaits its session cannot be told.
    set(&|m| m.binding.state = BindingState::Pending);
    assert_eq!(alias_of(&w.of(Kind::IdleAfterStop)[0].report_to), "boss");
    // Nor one that was dismissed while its binding still reads bound.
    set(&|m| {
        m.binding.state = BindingState::Bound;
        m.status = MemberStatus::Dismissed;
    });
    assert_eq!(alias_of(&w.of(Kind::IdleAfterStop)[0].report_to), "boss");
}

#[test]
fn only_open_duties_of_the_team_are_judged() {
    let w = World::new();
    w.stops("boss", 30);
    w.stops("aide", 30);
    w.stops("clerk", 30);

    // A finished duty waits on nobody, and leaves the team without work.
    let done = w.accepted("aide");
    let aide = w.actor("aide");
    w.tx(|st| {
        ops::duty_progress(
            st,
            now_ms(),
            &aide,
            &done,
            DutyStatus::Done,
            Some("shipped"),
        )
    });
    assert!(w.findings().iter().all(|f| f.subject != done));
    assert_eq!(w.of(Kind::TeamIdle).len(), 1);

    // Another team's open duty is not this team's work.
    let other = w.tx(|st| {
        ops::dev_seed(
            st,
            w.now - 600 * MINUTE,
            &SeedSpec {
                team: "elsewhere".into(),
                project: "project".into(),
                commander_alias: "chief".into(),
                commander_session: Some("s-chief".into()),
                commander_tool: Some(Tool::Codex),
                members: vec![("hand".into(), Some("s-hand".into()), Some(Tool::Codex))],
            },
        )?;
        let chief = Actor {
            member: authz::member_for_session(st, "s-chief").unwrap().id.clone(),
            session_id: "s-chief".into(),
        };
        let hand = authz::member_for_session(st, "s-hand").unwrap().id.clone();
        Ok(ops::duty_issue(st, now_ms(), &chief, &hand, spec())?.ids[0].clone())
    });
    w.prompts("chief", 500);
    w.stops("hand", 30);
    let watch = w.team();
    let idle = w.of(Kind::TeamIdle);
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0].team, watch);
    let about_other: Vec<_> = w
        .findings()
        .into_iter()
        .filter(|f| f.subject == other)
        .collect();
    assert_eq!(about_other.len(), 1);
    assert_ne!(about_other[0].team, watch);
    assert_eq!(alias_of(&about_other[0].report_to), "chief");

    // A duty in a cancelled bu has ended with it.
    let boss = w.actor("boss");
    let clerk = w.id("clerk");
    let unit = w
        .tx(|st| ops::bu_open(st, now_ms(), &boss, &clerk, None, &[], &[]))
        .ids[0]
        .clone();
    let in_unit = w.proposes("clerk");
    assert!(w.findings().iter().any(|f| f.subject == in_unit));
    w.tx(|st| ops::bu_cancel(st, now_ms(), &boss, &unit, "stop"));
    assert!(w.findings().iter().all(|f| f.subject != in_unit));
}

#[test]
fn an_answered_question_returns_the_wait_to_the_assignee() {
    let w = World::new();
    let duty = w.accepted("aide");
    let (aide, boss) = (w.actor("aide"), w.actor("boss"));
    let asked = w
        .tx(|st| ops::escalate(st, now_ms(), &aide, Some(&duty), "which way?"))
        .ids[0]
        .clone();
    w.stops("aide", 30);
    // The superior is mid-turn with the question: nothing rests.
    assert!(w.findings().is_empty());

    w.tx(|st| ops::escalation_answer(st, now_ms(), &boss, &asked, "left"));
    let idle = w.of(Kind::IdleAfterStop);
    assert_eq!(idle.len(), 1);
    assert_eq!(waited(&idle[0]), "aide");
    assert!(w.of(Kind::CounterpartIdle).is_empty());
}

#[test]
fn a_held_unit_waits_on_nobody() {
    let w = World::new();
    w.schedule();
    w.item("ready work");
    w.stops("boss", 30);
    let aide = w.id("aide");
    w.tx(|st| {
        let binding = &mut st.members.get_mut(&aide).unwrap().binding;
        binding.state = BindingState::Pending;
        binding.designated_at = w.now - 20 * MINUTE;
        Ok(())
    });
    assert_eq!(w.of(Kind::SeatPending).len(), 1);
    assert_eq!(w.of(Kind::ReadyButIdle).len(), 1);

    // The human stops the commander's whole unit.
    let root = w.store.read().unwrap().members[&w.id("boss")].bu.clone();
    let said = Evidence {
        session_id: "s-boss".into(),
        prompt_seq: 1,
        sha256: "hash".into(),
        attest: Attest::Fixture,
        quote: "everyone wait".into(),
        at: now_ms(),
        strength: Strength::Explicit,
    };
    w.tx(|st| ops::root_hold(st, now_ms(), &said, &root, "until I say"));
    assert!(w.findings().is_empty());

    // With the item gone the team has no work, and is still held.
    w.tx(|st| {
        st.schedules.clear();
        Ok(())
    });
    assert!(w.findings().is_empty());
    w.tx(|st| {
        ops::root_release(
            st,
            now_ms(),
            &Evidence {
                prompt_seq: 2,
                ..said.clone()
            },
            &root,
        )
    });
    assert_eq!(w.of(Kind::TeamIdle).len(), 1);
    assert_eq!(w.of(Kind::SeatPending).len(), 1);
}
