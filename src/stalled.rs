//! Stall detection: work that waits on a session observed to be resting.
//!
//! Detection reads only: it never refreshes bindings or writes the state, a
//! session log or a Schedule, so a timer may run it as often as it likes.
//! `notify` adds reporting, which keeps a ledger of its own beside them.
//! A finding is an observation, not a verdict: "resting" means a `Stop` was
//! recorded and no later turn was, which a person typing to that session and
//! a turn the hook failed to record both look like.

pub mod notify;

use crate::authz;
use crate::error::Result;
use crate::model::{
    BindingState, DutyStatus, EscalationStatus, Member, MemberStatus, Ms, State, TeamStatus,
};
use crate::pager::{HostStatus, Hosts, Pager};
use crate::store::{EventKind, GLOBAL_MARKERS, Store};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A seat was designated and its session has not confirmed it.
    SeatPending,
    /// A formed team has no open duty and no live Schedule item, and its
    /// commander rests.
    TeamIdle,
    /// An accepted duty's assignee rests.
    IdleAfterStop,
    /// A duty waits on an answer, an acceptance or an unblocking, and the
    /// member who owes it rests.
    CounterpartIdle,
    /// A Schedule item can be dispatched and its task master rests.
    ReadyButIdle,
    /// An open duty's assignee is bound to a session pager reports gone.
    HostGone,
    /// Something a judgement needs could not be read.
    Unknown,
}

/// A member a finding names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Party {
    pub member: String,
    pub alias: String,
}

/// Who should hear of a finding. Computed, never sent from here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Recipient {
    Member(Party),
    Human,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub kind: Kind,
    pub team: String,
    pub team_name: String,
    /// The seat, duty, Schedule item or team the finding is about.
    pub subject: String,
    pub waiting_on: Option<Party>,
    pub report_to: Recipient,
    /// When the observed rest or the pending designation began.
    pub since: Option<Ms>,
    /// Stable while the same wait lasts and new once it recurs.
    pub key: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub rev: u64,
    pub after_minutes: u64,
    pub findings: Vec<Finding>,
}

/// What a session's log says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    /// The last recorded event is a `Stop` at least the threshold ago.
    Resting {
        session: String,
        stop_seq: u64,
        since: Ms,
    },
    Active,
    Unreadable(String),
}

struct Scan<'a> {
    store: &'a Store,
    st: &'a State,
    hosts: Hosts,
    after: Ms,
    now: Ms,
    findings: Vec<Finding>,
}

/// Every stall the state, the session logs, the Schedules and pager's roster
/// show at `now`, for waits at least `after` long.
pub fn find(store: &Store, pager: &dyn Pager, after: Ms, now: Ms) -> Result<Report> {
    let st = store.read_view()?;
    let mut scan = Scan {
        store,
        st: &st,
        hosts: Hosts::all(HostStatus::Unknown),
        after,
        now,
        findings: Vec::new(),
    };
    match pager.hosts() {
        Ok(hosts) => scan.hosts = hosts,
        Err(error) => scan.unknown("", "pager", format!("pager roster unreadable: {error}")),
    }
    let mut teams: Vec<_> = st
        .teams
        .values()
        .filter(|team| team.status == TeamStatus::Active)
        .collect();
    teams.sort_by(|a, b| a.id.cmp(&b.id));
    for team in teams {
        scan.pending_seats(&team.id);
        let open_duties = scan.duties(&team.id);
        let live_items = scan.schedule(&team.id);
        if open_duties == 0 && live_items == Some(0) {
            scan.idle_team(&team.id);
        }
    }
    Ok(Report {
        rev: st.rev,
        after_minutes: after / 60_000,
        findings: scan.findings,
    })
}

impl<'a> Scan<'a> {
    fn party(&self, member: &Member) -> Party {
        Party {
            member: member.id.clone(),
            alias: member.binding.alias.clone(),
        }
    }

    /// A member that holds its seat with a confirmed session.
    fn seated(&self, id: &str) -> Option<&'a Member> {
        let st: &'a State = self.st;
        st.members
            .get(id)
            .filter(|m| m.status == MemberStatus::Active && m.binding.state == BindingState::Bound)
    }

    /// Whether a human hold covers `bu`, on it or on a bu above it.
    fn held(&self, bu: &str) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        let mut next = Some(bu);
        while let Some(id) = next.filter(|id| seen.insert(*id)) {
            let Some(bu) = self.st.bus.get(id) else {
                break;
            };
            if bu.hold.is_some() {
                return true;
            }
            next = bu.parent.as_deref();
        }
        false
    }

    fn observe(&self, member: &Member) -> Seen {
        let Some(session) = member.binding.session_id.as_deref() else {
            return Seen::Unreadable("bound seat has no session".into());
        };
        let log = self.store.session(session);
        let events = match log.events() {
            Ok(events) => events,
            Err(error) => return Seen::Unreadable(format!("session log unreadable: {error}")),
        };
        let Some(last) = events.iter().max_by_key(|event| event.seq) else {
            return Seen::Unreadable("session has no recorded event".into());
        };
        if last.kind != EventKind::Stop {
            return Seen::Active;
        }
        // A prompt the hook could not record may have started a turn since.
        let missed = [log, self.store.session(GLOBAL_MARKERS)]
            .iter()
            .any(|log| log.unrecorded_after(last.at).unwrap_or(true));
        if missed {
            return Seen::Unreadable("a later prompt could not be recorded".into());
        }
        if self.now.saturating_sub(last.at) < self.after {
            return Seen::Active;
        }
        Seen::Resting {
            session: session.to_owned(),
            stop_seq: last.seq,
            since: last.at,
        }
    }

    /// The team's Schedule manager, then its commander, skipping anyone who
    /// is the party waited on, holds no confirmed seat, or whose host pager
    /// reports gone; the state still shows such a seat bound, since nothing
    /// here refreshes bindings.
    fn recipient(&self, team: &str, waiting_on: Option<&str>) -> Recipient {
        let manager = self
            .st
            .schedules
            .get(team)
            .and_then(|registration| registration.manager.as_deref());
        let commander = self.st.teams.get(team).map(|team| team.commander.as_str());
        [manager, commander]
            .into_iter()
            .flatten()
            .filter(|id| Some(*id) != waiting_on)
            .filter_map(|id| self.seated(id))
            .find(|member| self.hosts.status(&member.binding.alias) != HostStatus::Gone)
            .map_or(Recipient::Human, |member| {
                Recipient::Member(self.party(member))
            })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one finding has this many independent parts"
    )]
    fn push(
        &mut self,
        kind: Kind,
        team: &str,
        subject: &str,
        waiting_on: Option<&Member>,
        since: Option<Ms>,
        anchor: &str,
        detail: String,
    ) {
        let waiting = waiting_on.map(|member| self.party(member));
        let report_to = self.recipient(team, waiting_on.map(|member| member.id.as_str()));
        let waited = waiting.as_ref().map_or("", |party| party.member.as_str());
        let key = key(&[
            kind_word(kind),
            &self.st.runtime_id,
            team,
            subject,
            waited,
            anchor,
        ]);
        let team_name = self
            .st
            .teams
            .get(team)
            .map(|team| team.name.clone())
            .unwrap_or_default();
        self.findings.push(Finding {
            kind,
            team: team.into(),
            team_name,
            subject: subject.into(),
            waiting_on: waiting,
            report_to,
            since,
            key,
            detail,
        });
    }

    fn unknown(&mut self, team: &str, subject: &str, detail: String) {
        self.push(Kind::Unknown, team, subject, None, None, "", detail);
    }

    /// A finding of `kind` when `member` rests; an `unknown` one when its
    /// log cannot say.
    fn resting(&mut self, kind: Kind, team: &str, subject: &str, member: &Member, anchor: &str) {
        match self.observe(member) {
            Seen::Active => {}
            Seen::Unreadable(why) => {
                let detail = format!("{}: {why}", member.binding.alias);
                self.unknown(team, subject, detail);
            }
            Seen::Resting {
                session,
                stop_seq,
                since,
            } => {
                let anchor = format!("{session}:{stop_seq}:{anchor}");
                let detail = format!("{} observed resting", member.binding.alias);
                self.push(
                    kind,
                    team,
                    subject,
                    Some(member),
                    Some(since),
                    &anchor,
                    detail,
                );
            }
        }
    }

    fn pending_seats(&mut self, team: &str) {
        let pending: Vec<&Member> = self
            .st
            .members
            .values()
            .filter(|m| {
                m.team == team
                    && m.status == MemberStatus::Active
                    && m.binding.state == BindingState::Pending
                    && !authz::bu_cancelled(self.st, &m.bu)
                    && !self.held(&m.bu)
                    && self.now.saturating_sub(m.binding.designated_at) >= self.after
            })
            .collect();
        for member in pending {
            let anchor = format!("{}:{}", member.binding.alias, member.binding.designated_at);
            let detail = format!("seat for {} not confirmed", member.binding.alias);
            self.push(
                Kind::SeatPending,
                team,
                &member.id,
                Some(member),
                Some(member.binding.designated_at),
                &anchor,
                detail,
            );
        }
    }

    /// Judge every open duty of the team; returns how many are open.
    fn duties(&mut self, team: &str) -> usize {
        let open: Vec<_> = self
            .st
            .duties
            .values()
            .filter(|duty| {
                duty.team == team
                    && matches!(
                        duty.status,
                        DutyStatus::Proposed | DutyStatus::Accepted | DutyStatus::Blocked
                    )
                    && !authz::bu_cancelled(self.st, &duty.bu)
            })
            .collect();
        for duty in &open {
            // Only the human lifts a hold; a held duty waits on nobody here.
            if !authz::holds(self.st, duty).is_empty() {
                continue;
            }
            if let Some(assignee) = self.seated(&duty.assignee)
                && self.hosts.status(&assignee.binding.alias) == HostStatus::Gone
            {
                let anchor = assignee.binding.session_id.clone().unwrap_or_default();
                let detail = format!("host of {} reported gone", assignee.binding.alias);
                self.push(
                    Kind::HostGone,
                    team,
                    &duty.id,
                    Some(assignee),
                    None,
                    &anchor,
                    detail,
                );
                continue;
            }
            let question = self.st.escalations.values().find(|e| {
                e.status == EscalationStatus::Open
                    && e.from == duty.assignee
                    && e.duty.as_deref() == Some(&duty.id)
            });
            let (kind, owed_by) = match (question, duty.status) {
                // The human's queue already shows a question put to them.
                (Some(question), _) => match question.to.as_deref() {
                    Some(to) => (Kind::CounterpartIdle, to),
                    None => continue,
                },
                (None, DutyStatus::Proposed) => (Kind::CounterpartIdle, duty.assignee.as_str()),
                (None, DutyStatus::Blocked) => (Kind::CounterpartIdle, duty.issuer.as_str()),
                (None, _) => (Kind::IdleAfterStop, duty.assignee.as_str()),
            };
            if let Some(member) = self.seated(owed_by) {
                let anchor = duty.version.to_string();
                self.resting(kind, team, &duty.id, member, &anchor);
            }
        }
        open.len()
    }

    /// Judge the team's ready Schedule items; returns how many items are
    /// live, or `None` when the Schedule could not be read.
    fn schedule(&mut self, team: &str) -> Option<usize> {
        if !self.st.schedules.contains_key(team) {
            return Some(0);
        }
        let view = match crate::schedule::view(self.store, self.st, team) {
            Ok(view) if view.get("error").is_none() => view,
            Ok(view) => {
                self.unknown(
                    team,
                    team,
                    format!("Schedule unreadable: {}", view["error"]),
                );
                return None;
            }
            Err(error) => {
                self.unknown(team, team, format!("Schedule unreadable: {error}"));
                return None;
            }
        };
        let items = view["items"].as_array().cloned().unwrap_or_default();
        let mut live = 0;
        for item in &items {
            let stage = item["stage"].as_str().unwrap_or_default();
            if !matches!(stage, "complete" | "cancelled") {
                live += 1;
            }
            if stage != "ready" {
                continue;
            }
            let Some(id) = item["id"].as_str() else {
                continue;
            };
            // Dispatch belongs to the head of the item's own bu.
            if let Some(master) = item["master"]["member"]
                .as_str()
                .and_then(|member| self.seated(member))
                .filter(|master| !self.held(&master.bu))
            {
                self.resting(Kind::ReadyButIdle, team, id, master, "");
            }
        }
        Some(live)
    }

    fn idle_team(&mut self, team: &str) {
        let Some(commander) = self
            .st
            .teams
            .get(team)
            .and_then(|team| self.seated(&team.commander))
        else {
            return;
        };
        if self.held(&commander.bu) {
            return;
        }
        self.resting(Kind::TeamIdle, team, team, commander, "");
    }
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::SeatPending => "seat_pending",
        Kind::TeamIdle => "team_idle",
        Kind::IdleAfterStop => "idle_after_stop",
        Kind::CounterpartIdle => "counterpart_idle",
        Kind::ReadyButIdle => "ready_but_idle",
        Kind::HostGone => "host_gone",
        Kind::Unknown => "unknown",
    }
}

/// First 16 hex digits of the SHA-256 of the parts, each length-prefixed so
/// no two lists of parts share an encoding.
fn key(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())[..16].to_owned()
}

/// One line per finding, for a person reading the terminal.
pub fn render(report: &Report) -> String {
    if report.findings.is_empty() {
        return format!(
            "[어사대] rev {} · {}분 기준 · 발견 없음\n",
            report.rev, report.after_minutes
        );
    }
    let mut out = format!(
        "[어사대] rev {} · {}분 기준 · {}건\n",
        report.rev,
        report.after_minutes,
        report.findings.len()
    );
    for finding in &report.findings {
        let to = match &finding.report_to {
            Recipient::Member(party) => party.alias.as_str(),
            Recipient::Human => "대협",
        };
        out.push_str(&format!(
            "  {} {} — {} → {} [{}]\n",
            kind_word(finding.kind),
            finding.subject,
            finding.detail,
            to,
            finding.key
        ));
    }
    out
}
