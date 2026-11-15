//! Report stall findings: to a member through pager's sessionless sender,
//! to the human through a command they configure.
//!
//! A timer runs this, so it has to stop by itself. Every finding gets at
//! most three rounds at growing intervals, then one line to the human, and
//! what was sent is kept in a ledger beside the state. The state, the
//! session logs and the Schedules are still only read.

use super::{Finding, Kind, Recipient};
use crate::error::{Error, Reason, Result};
use crate::model::{Ms, NoticeKind};
use crate::pager::{Automation, Message, Pager};
use crate::store::{self, Store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// How pager names the sender: `automation:gaebu-stalled`.
pub const AUTOMATION: &str = "gaebu-stalled";
const LEDGER: &str = "stalled-notify.json";
const LOCK: &str = "stalled-notify.lock";
/// Rounds sent to a member before the human hears of the finding instead.
const MAX_ROUNDS: u32 = 3;
/// Failed sends of one round, or failed runs of the human's command, before
/// giving up on it.
const MAX_FAILURES: u32 = 3;
/// Messages one member gets from one run; the rest wait for the next run.
const PER_RECIPIENT: usize = 5;
/// How long a finding no longer seen keeps its rounds. A hold or one failed
/// read hides a finding without ending the wait behind it.
const RETENTION: Ms = 7 * 24 * 60 * 60 * 1000;
const IO_DEADLINE: Duration = Duration::from_secs(5);
/// The human's command may hang; the ledger lock must not hang with it.
pub const HUMAN_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a line for the human goes.
pub trait Human {
    fn tell(&self, body: &str) -> Result<()>;
}

/// The human's own notifier: `command` run by `sh -c` with the line in
/// `GAEBU_STALLED_BODY`, or standard error when there is none. Standard
/// output is left to the run's own summary, which may be JSON.
pub struct Shell {
    pub command: Option<String>,
    /// How long the command may run before it is killed.
    pub timeout: Duration,
}

impl Human for Shell {
    fn tell(&self, body: &str) -> Result<()> {
        let Some(command) = &self.command else {
            eprintln!("{body}");
            return Ok(());
        };
        let fail = |message: String| Error::new(Reason::PagerUnavailable, message);
        // The line travels in the environment, never in the command text, so
        // nothing in it is read as shell.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("GAEBU_STALLED_BODY", body)
            .stdin(std::process::Stdio::null())
            .stdout(io::stderr())
            .spawn()
            .map_err(|e| fail(e.to_string()))?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => return Err(fail(format!("notifier exited {status}"))),
                Ok(None) if started.elapsed() < self.timeout => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                outcome => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(fail(match outcome {
                        Err(e) => e.to_string(),
                        _ => "notifier timed out".into(),
                    }));
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Told {
    Pending,
    Done,
    Failed,
}

/// A line owed to the human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ForHuman {
    body: String,
    state: Told,
    failures: u32,
}

/// What was sent about one finding to one recipient. Every field has a
/// default, so a ledger written before a field existed still reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Entry {
    /// Part of every event id, so a finding that returns after its entry was
    /// dropped never reuses an event pager has already seen.
    first_seen: Ms,
    /// The round last attempted; 0 before the first.
    round: u32,
    /// That round's text. A retry must send exactly this.
    body: String,
    sent: bool,
    last_sent_at: Option<Ms>,
    failures: u32,
    absent_since: Option<Ms>,
    human: Option<ForHuman>,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Ledger {
    entries: BTreeMap<String, Entry>,
}

/// What one run did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Messages pager accepted.
    pub sent: usize,
    /// Lines delivered to the human.
    pub human: usize,
    /// Sends and notifier runs that failed. Each is tried again on later
    /// runs until its third failure.
    pub failed: usize,
    /// Findings held back by the per-recipient limit.
    pub deferred: usize,
}

/// Find stalls and report each one that is due.
pub fn run(
    store: &Store,
    pager: &dyn Pager,
    human: &dyn Human,
    after: Ms,
    now: Ms,
) -> Result<Summary> {
    // One run at a time: a timer and a person may start two.
    let _lock = store::lock(store.home(), LOCK, IO_DEADLINE)?;
    let path = store.home().join(LEDGER);
    let mut ledger = load(&path, now)?;
    // A ledger that replaced an unreadable one owes the human a line about
    // it; that must survive a run that fails before it gets to say so.
    save(&path, &ledger)?;
    let report = super::find(store, pager, after, now)?;
    let mut summary = Summary::default();
    let mut per_recipient: BTreeMap<String, usize> = BTreeMap::new();
    let mut present = Vec::new();

    for finding in report
        .findings
        .iter()
        .filter(|finding| finding.kind != Kind::Unknown)
    {
        let to = match &finding.report_to {
            Recipient::Member(party) => party.member.as_str(),
            Recipient::Human => "human",
        };
        let id = format!("{}:{to}", finding.key);
        present.push(id.clone());
        let entry = ledger.entries.entry(id).or_insert_with(|| Entry {
            first_seen: now,
            ..Entry::default()
        });
        entry.absent_since = None;
        match &finding.report_to {
            Recipient::Human => {
                if entry.human.is_none() {
                    entry.human = Some(owed(finding, "보고받을 구성원이 없음"));
                }
            }
            Recipient::Member(party) if entry.human.is_none() => {
                let due = if entry.round == 0 || !entry.sent {
                    true
                } else {
                    // Twice as long before each later round. The round is
                    // clamped because the ledger is a file a person can edit.
                    let doublings = entry.round.clamp(1, MAX_ROUNDS) - 1;
                    let wait = after.saturating_mul(1 << doublings);
                    now.saturating_sub(entry.last_sent_at.unwrap_or(0)) >= wait
                };
                if due && entry.sent && entry.round >= MAX_ROUNDS {
                    entry.human = Some(owed(finding, "세 번 알린 뒤에도 그대로임"));
                } else if due {
                    let count = per_recipient.entry(party.member.clone()).or_default();
                    if *count >= PER_RECIPIENT {
                        summary.deferred += 1;
                        continue;
                    }
                    *count += 1;
                    if entry.sent || entry.round == 0 {
                        entry.round += 1;
                        entry.body = body(finding, Some(entry.round));
                        entry.sent = false;
                        entry.failures = 0;
                    }
                    let event = format!("{}-{}-r{}", finding.key, entry.first_seen, entry.round);
                    let message = (entry.body.clone(), party.alias.clone());
                    // Recorded before it is sent: a run that dies in between
                    // sends this same event and text again, which pager drops.
                    save(&path, &ledger)?;
                    let entry = ledger.entries.get_mut(present.last().unwrap()).unwrap();
                    let accepted = pager.send(&Message {
                        target: &message.1,
                        body: &message.0,
                        event: &event,
                        kind: NoticeKind::Failure,
                        automation: Some(Automation {
                            name: AUTOMATION,
                            task: &finding.team,
                        }),
                    });
                    match accepted {
                        Ok(()) => {
                            entry.sent = true;
                            entry.failures = 0;
                            entry.last_sent_at = Some(now);
                            summary.sent += 1;
                        }
                        Err(error) => {
                            eprintln!("gaebu: stall report {event} not sent: {error}");
                            entry.failures += 1;
                            summary.failed += 1;
                            // A recipient pager cannot resolve would be
                            // retried by every run for ever.
                            if entry.failures >= MAX_FAILURES {
                                entry.human = Some(owed(finding, "알림을 전달하지 못함"));
                            }
                        }
                    }
                    save(&path, &ledger)?;
                }
            }
            Recipient::Member(_) => {}
        }
    }

    // A finding that is gone keeps its rounds for a while and then its entry.
    ledger.entries.retain(|id, entry| {
        if present.contains(id) || entry.human.as_ref().is_some_and(is_pending) {
            return true;
        }
        let since = *entry.absent_since.get_or_insert(now);
        now.saturating_sub(since) < RETENTION
    });

    let owing: Vec<String> = ledger
        .entries
        .iter()
        .filter(|(_, entry)| entry.human.as_ref().is_some_and(is_pending))
        .map(|(id, _)| id.clone())
        .collect();
    for id in owing {
        save(&path, &ledger)?;
        let line = ledger.entries[&id].human.as_ref().unwrap().body.clone();
        let told = human.tell(&line);
        let owed = ledger.entries.get_mut(&id).unwrap().human.as_mut().unwrap();
        match told {
            Ok(()) => {
                owed.state = Told::Done;
                summary.human += 1;
            }
            Err(error) => {
                owed.failures += 1;
                summary.failed += 1;
                if owed.failures >= MAX_FAILURES {
                    owed.state = Told::Failed;
                    eprintln!("gaebu: giving up telling the human ({error}): {line}");
                }
            }
        }
    }
    save(&path, &ledger)?;
    Ok(summary)
}

fn is_pending(owed: &ForHuman) -> bool {
    owed.state == Told::Pending
}

fn owed(finding: &Finding, why: &str) -> ForHuman {
    ForHuman {
        body: format!("{} — {why}", body(finding, None)),
        state: Told::Pending,
        failures: 0,
    }
}

/// The sentence for a finding. It holds nothing that changes between two
/// runs, because pager refuses one event sent with two texts.
fn body(finding: &Finding, round: Option<u32>) -> String {
    let who = finding
        .waiting_on
        .as_ref()
        .map_or("?", |party| party.alias.as_str());
    let subject = &finding.subject;
    let what = match finding.kind {
        Kind::SeatPending => format!("{who}의 자리가 아직 확정되지 않음"),
        Kind::TeamIdle => format!(
            "팀 {}에 열린 소임과 일정이 없고 지휘관이 쉬는 것으로 관측됨",
            finding.team_name
        ),
        Kind::IdleAfterStop => format!("소임 {subject}의 담당 {who}가 쉬는 것으로 관측됨"),
        Kind::CounterpartIdle => {
            format!("소임 {subject}가 {who}를 기다리는데 쉬는 것으로 관측됨")
        }
        Kind::ReadyButIdle => {
            format!("일정 {subject}가 배분 가능한데 {who}가 쉬는 것으로 관측됨")
        }
        Kind::HostGone => format!("소임 {subject}의 담당 {who} 세션이 떠난 것으로 보임"),
        Kind::Unknown => finding.detail.clone(),
    };
    match round {
        Some(round) => format!("[어사대] {what} ({round}회차). 확인: gaebu stalled"),
        None => format!("[어사대] {what}. 확인: gaebu stalled"),
    }
}

/// Read the ledger. One that cannot be parsed is set aside and replaced by an
/// empty one that owes the human a line about it; the entries it starts get
/// a new `first_seen`, so their events cannot collide with the lost ones.
fn load(path: &Path, now: Ms) -> Result<Ledger> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Ledger::default()),
        Err(e) => return Err(Error::new(Reason::StoreWriteFailed, e.to_string())),
    };
    if let Ok(ledger) = serde_json::from_slice(&bytes) {
        return Ok(ledger);
    }
    let aside = path.with_file_name(format!("stalled-notify.corrupt-{now}.json"));
    fs::rename(path, &aside).map_err(|e| Error::new(Reason::StoreWriteFailed, e.to_string()))?;
    let mut ledger = Ledger::default();
    ledger.entries.insert(
        format!("ledger-corrupt-{now}:human"),
        Entry {
            human: Some(ForHuman {
                body: format!(
                    "[어사대] 보낸 기록을 읽을 수 없어 {}로 옮기고 회차를 처음부터 다시 셈",
                    aside.display()
                ),
                state: Told::Pending,
                failures: 0,
            }),
            ..Entry::default()
        },
    );
    Ok(ledger)
}

fn save(path: &Path, ledger: &Ledger) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(ledger)
        .map_err(|e| Error::new(Reason::StoreWriteFailed, e.to_string()))?;
    store::replace_until(path, &bytes, Instant::now() + IO_DEADLINE)
}
