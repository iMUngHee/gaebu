//! Retire terminal runtime history behind committed JSONL boundaries.

use crate::model::{Duty, DutyStatus, Escalation, EscalationStatus, Ms, Record, State, TeamStatus};
use crate::store::{self, Store};
use crate::{Error, Reason, Result, authz};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const DAY: Ms = 24 * 60 * 60 * 1000;
const HOUR: Ms = 60 * 60 * 1000;

pub struct Retired {
    pub schedule_id: String,
    pub items: BTreeSet<String>,
}

#[derive(Default)]
struct Batch {
    duties: Vec<Duty>,
    escalations: Vec<Escalation>,
    records: Vec<Record>,
}

fn candidates(
    st: &State,
    now: Ms,
    team: &str,
    retired: Option<&Retired>,
) -> BTreeMap<String, Batch> {
    let mut batches: BTreeMap<String, Batch> = BTreeMap::new();
    let open_duties: BTreeSet<_> = st
        .escalations
        .values()
        .filter(|e| e.status == EscalationStatus::Open)
        .filter_map(|e| e.duty.as_deref())
        .collect();
    for duty in st.duties.values() {
        if !matches!(
            duty.status,
            DutyStatus::Done | DutyStatus::Declined | DutyStatus::Cancelled | DutyStatus::HandedOff
        ) || now.saturating_sub(duty.updated_at) < DAY
            || !authz::holds(st, duty).is_empty()
            || open_duties.contains(duty.id.as_str())
        {
            continue;
        }
        if let Some(link) = &duty.schedule_ref {
            let cancelled = st
                .teams
                .get(&duty.team)
                .is_some_and(|t| t.status == TeamStatus::Cancelled);
            let retired_here = duty.team == team
                && st
                    .teams
                    .get(team)
                    .is_some_and(|t| t.status == TeamStatus::Active)
                && st.schedules.get(team).is_some_and(|reg| {
                    retired.is_some_and(|r| {
                        reg.schedule_id == r.schedule_id
                            && link.schedule_id == r.schedule_id
                            && r.items.contains(&link.item_id)
                    })
                });
            if !cancelled && !retired_here {
                continue;
            }
        }
        batches
            .entry(duty.team.clone())
            .or_default()
            .duties
            .push(duty.clone());
    }
    for escalation in st.escalations.values() {
        let closed = escalation
            .closed_at
            .or(escalation.answered_at)
            .unwrap_or(escalation.created_at);
        if escalation.status != EscalationStatus::Open && now.saturating_sub(closed) >= DAY {
            batches
                .entry(escalation.team.clone())
                .or_default()
                .escalations
                .push(escalation.clone());
        }
    }
    for record in &st.records {
        if now.saturating_sub(record.at) >= HOUR {
            batches
                .entry(record.team.clone())
                .or_default()
                .records
                .push(record.clone());
        }
    }
    batches
}

fn counts(batches: &BTreeMap<String, Batch>) -> [usize; 3] {
    batches.values().fold([0; 3], |mut n, batch| {
        n[0] += batch.duties.len();
        n[1] += batch.escalations.len();
        n[2] += batch.records.len();
        n
    })
}

/// Runs after the caller's commit; archival failure never fails that operation.
pub fn auto(store: &Store, team: &str, retired: Option<&Retired>) {
    if let Err(error) = compact(store, team, retired, 64) {
        eprintln!("gaebu: runtime compaction deferred: {error}");
    }
}

/// Explicit retirement, including batches smaller than the automatic threshold.
pub fn run(store: &Store, team: &str, retired: Option<&Retired>) -> Result<Value> {
    compact(store, team, retired, 1)
}

/// Whether automatic retirement could need this team's Schedule tombstones.
pub fn needs_schedule(st: &State, team: &str) -> bool {
    if !st
        .teams
        .get(team)
        .is_some_and(|t| t.status == TeamStatus::Active)
    {
        return false;
    }
    let Some(registration) = st.schedules.get(team) else {
        return false;
    };
    // Assuming every linked item retired gives an upper bound without I/O.
    let possible = Retired {
        schedule_id: registration.schedule_id.clone(),
        items: st
            .duties
            .values()
            .filter(|d| d.team == team)
            .filter_map(|d| d.schedule_ref.as_ref().map(|r| r.item_id.clone()))
            .collect(),
    };
    let batches = candidates(st, crate::model::now_ms(), team, Some(&possible));
    counts(&batches).iter().sum::<usize>() >= 64
        && batches
            .get(team)
            .is_some_and(|batch| batch.duties.iter().any(|d| d.schedule_ref.is_some()))
}

fn compact(
    store: &Store,
    team: &str,
    retired: Option<&Retired>,
    threshold: usize,
) -> Result<Value> {
    let empty = || json!({"duties": 0, "escalations": 0, "records": 0});
    let before = store.read_view()?;
    if counts(&candidates(&before, crate::model::now_ms(), team, retired))
        .iter()
        .sum::<usize>()
        < threshold
    {
        return Ok(empty());
    }
    store.transact(store::CLI_DEADLINE, |st| {
        let batches = candidates(st, crate::model::now_ms(), team, retired);
        let n = counts(&batches);
        if n.iter().sum::<usize>() < threshold {
            return Ok(empty());
        }
        let archive = store.home().join("archive");
        fs::create_dir_all(&archive).map_err(write_error)?;
        let mut lengths = st.archives.clone();
        for (team, batch) in &batches {
            let dir = team_dir(store, team)?;
            fs::create_dir_all(&dir).map_err(write_error)?;
            let lengths = lengths.entry(team.clone()).or_default();
            lengths.duties = append(&dir.join("duties.jsonl"), lengths.duties, &batch.duties)?;
            lengths.escalations = append(
                &dir.join("escalations.jsonl"),
                lengths.escalations,
                &batch.escalations,
            )?;
            lengths.records = append(&dir.join("records.jsonl"), lengths.records, &batch.records)?;
            // Retry paths may reuse links created by a failed, unsynced pass.
            store::sync_dir(&dir)?;
        }
        store::sync_dir(&archive)?;
        store::sync_dir(store.home())?;
        let mut archived_records = BTreeSet::new();
        for batch in batches.values() {
            for duty in &batch.duties {
                st.duties.remove(&duty.id);
            }
            for escalation in &batch.escalations {
                st.escalations.remove(&escalation.id);
            }
            archived_records.extend(batch.records.iter().map(|r| r.id.as_str()));
        }
        st.records
            .retain(|r| !archived_records.contains(r.id.as_str()));
        st.archives = lengths;
        Ok(json!({"duties": n[0], "escalations": n[1], "records": n[2]}))
    })
}

fn team_dir(store: &Store, team: &str) -> Result<PathBuf> {
    if team.is_empty() || store::sanitize(team) != team {
        return Err(corrupt("invalid archive team id"));
    }
    Ok(store.home().join("archive").join(team))
}

/// Check the boundary before any truncation; parsing belongs to streaming lookup.
fn boundary(file: &mut File, committed: u64) -> Result<()> {
    if file.metadata().map_err(write_error)?.len() < committed {
        return Err(corrupt("archive is shorter than its committed boundary"));
    }
    if committed > 0 {
        file.seek(SeekFrom::Start(committed - 1))
            .map_err(write_error)?;
        let mut last = [0];
        file.read_exact(&mut last).map_err(write_error)?;
        if last != *b"\n" {
            return Err(corrupt(
                "archive committed boundary does not end in newline",
            ));
        }
    }
    file.seek(SeekFrom::Start(0)).map_err(write_error)?;
    Ok(())
}

fn append<T: Serialize>(path: &Path, committed: u64, entries: &[T]) -> Result<u64> {
    if entries.is_empty() {
        return Ok(committed);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(committed == 0)
        .truncate(false)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                corrupt("committed archive is missing")
            } else {
                write_error(e)
            }
        })?;
    boundary(&mut file, committed)?;
    file.set_len(committed).map_err(write_error)?;
    file.seek(SeekFrom::Start(committed)).map_err(write_error)?;
    for entry in entries {
        serde_json::to_writer(&mut file, entry).map_err(write_error)?;
        file.write_all(b"\n").map_err(write_error)?;
    }
    file.sync_all().map_err(write_error)?;
    file.stream_position().map_err(write_error)
}

/// Re-read authority and boundaries under the same lock that protects truncation.
pub fn full_duty(store: &Store, _st: &State, session: &str, duty: &str) -> Result<Duty> {
    let _lock = store::lock(store.home(), "state.lock", store::CLI_DEADLINE)?;
    let st = store.read_view()?;
    let member = authz::member_for_session(&st, session)
        .ok_or_else(|| Error::new(Reason::NotMember, "session has no active seat"))?;
    if st.duties.contains_key(duty) {
        return crate::card::full_duty(&st, session, duty).cloned();
    }
    let committed = st.archives.get(&member.team).map_or(0, |a| a.duties);
    let mut found = None;
    if committed > 0 {
        let mut file =
            File::open(team_dir(store, &member.team)?.join("duties.jsonl")).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    corrupt("committed archive is missing")
                } else {
                    write_error(e)
                }
            })?;
        boundary(&mut file, committed)?;
        let mut reader = BufReader::new(file.take(committed));
        let mut line = String::new();
        while reader.read_line(&mut line).map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                corrupt(e.to_string())
            } else {
                write_error(e)
            }
        })? > 0
        {
            let entry: Duty = serde_json::from_str(&line).map_err(|e| corrupt(e.to_string()))?;
            if entry.team != member.team {
                return Err(corrupt("archive entry belongs to another team"));
            }
            if entry.id == duty {
                found = Some(entry);
            }
            line.clear();
        }
    }
    found.ok_or_else(|| {
        Error::new(
            Reason::NotFound,
            "duty not found in hot state or team archive",
        )
    })
}

fn write_error(error: impl std::fmt::Display) -> Error {
    Error::new(Reason::StoreWriteFailed, error.to_string())
}

fn corrupt(message: impl Into<String>) -> Error {
    Error::new(Reason::StoreCorrupt, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::ops::{self, DutySpec, SeedSpec};

    #[test]
    fn retention_boundaries_use_unix_milliseconds() {
        let mut st = State::default();
        ops::dev_seed(
            &mut st,
            1,
            &SeedSpec {
                team: "test".into(),
                project: "test".into(),
                commander_alias: "root".into(),
                commander_session: Some("root".into()),
                commander_tool: Some(Tool::Codex),
                members: vec![("worker".into(), Some("worker".into()), Some(Tool::Codex))],
            },
        )
        .unwrap();
        let root = Actor {
            member: authz::member_for_session(&st, "root").unwrap().id.clone(),
            session_id: "root".into(),
        };
        let worker = authz::member_for_session(&st, "worker").unwrap().id.clone();
        let id = ops::duty_issue(
            &mut st,
            1,
            &root,
            &worker,
            DutySpec {
                outcome: "deliver".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .ids[0]
            .clone();
        let duty = st.duties.get_mut(&id).unwrap();
        duty.status = DutyStatus::Done;
        duty.updated_at = 1;
        let e = ops::escalate(&mut st, 1, &root, None, "which path?")
            .unwrap()
            .ids[0]
            .clone();
        ops::escalation_withdraw(&mut st, 1, &root, &e, None).unwrap();
        let team = st.members[&root.member].team.clone();
        st.records.clear();
        st.records.push(Record {
            id: "r-test".into(),
            team: team.clone(),
            kind: RecordKind::Note,
            author: root.member,
            refs: vec![],
            text: "history".into(),
            at: 1,
        });
        assert_eq!(counts(&candidates(&st, 3_600_000, &team, None)), [0, 0, 0]);
        assert_eq!(counts(&candidates(&st, 3_600_001, &team, None)), [0, 0, 1]);
        assert_eq!(counts(&candidates(&st, 86_400_000, &team, None)), [0, 0, 1]);
        assert_eq!(counts(&candidates(&st, 86_400_001, &team, None)), [1, 1, 1]);
        assert_eq!(counts(&candidates(&st, 0, &team, None)), [0, 0, 0]);
    }
}
