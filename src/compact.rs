//! Retire terminal runtime history behind committed JSONL boundaries.

use crate::archive;
use crate::model::{
    ArchiveSegment, ArchiveStream, Duty, DutyStatus, Escalation, EscalationStatus, Ms, Record,
    State, TeamStatus,
};
use crate::store::{self, Store};
use crate::{Error, Reason, Result, authz};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const DAY: Ms = 24 * 60 * 60 * 1000;
const HOUR: Ms = 60 * 60 * 1000;

pub struct Retired {
    pub schedule_id: String,
    pub items: BTreeSet<String>,
    pub next_item: u64,
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
                            && !r.items.contains(&link.item_id)
                            && link
                                .item_id
                                .strip_prefix("w-")
                                .and_then(|n| n.parse::<u64>().ok())
                                .is_some_and(|n| n > 0 && n < r.next_item)
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

/// Retire eligible rows and maintain the caller team’s retained archives.
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
        items: BTreeSet::new(),
        next_item: u64::MAX,
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
    let explicit = threshold == 1;
    if !explicit
        && counts(&candidates(
            &store.read_view()?,
            crate::model::now_ms(),
            team,
            retired,
        ))
        .iter()
        .sum::<usize>()
            < threshold
    {
        return Ok(empty());
    }
    store.transact(store::CLI_DEADLINE, |st| {
        let mut batches = candidates(st, crate::model::now_ms(), team, retired);
        let n = counts(&batches);
        if !explicit && n.iter().sum::<usize>() < threshold {
            return Ok(empty());
        }
        if explicit {
            batches.entry(team.to_owned()).or_default();
        }
        let archive = store.home().join("archive");
        fs::create_dir_all(&archive).map_err(write_error)?;
        let mut lengths = st.archives.clone();
        let mut totals: [Maintenance; 3] = std::array::from_fn(|_| Maintenance::default());
        let mut over_limit = Vec::new();
        for (batch_team, batch) in &batches {
            let dir = team_dir(store, batch_team)?;
            fs::create_dir_all(&dir).map_err(write_error)?;
            let lengths = lengths.entry(batch_team.clone()).or_default();
            let bounds = batch
                .duties
                .iter()
                .try_fold(None, |bounds, duty| {
                    let number = duty_number(&duty.id)?;
                    Some(Some(
                        bounds.map_or((number, number), |(lo, hi): (u64, u64)| {
                            (lo.min(number), hi.max(number))
                        }),
                    ))
                })
                .flatten();
            let streams = [
                ("duties", &mut lengths.duties, rows(&batch.duties)?, bounds),
                (
                    "escalations",
                    &mut lengths.escalations,
                    rows(&batch.escalations)?,
                    None,
                ),
                ("records", &mut lengths.records, rows(&batch.records)?, None),
            ];
            for (index, (name, stream, bytes, bounds)) in streams.into_iter().enumerate() {
                if bytes.is_empty() && !(explicit && batch_team == team) {
                    continue;
                }
                let result = maintain(
                    &dir.join(format!("{name}.jsonl")),
                    stream,
                    &bytes,
                    bounds,
                    store.archive_limit(),
                    explicit && batch_team == team,
                )?;
                if result.retained_bytes > store.archive_limit() {
                    over_limit.push(json!({"team": batch_team, "stream": name,
                        "bytes": result.retained_bytes, "limit_bytes": store.archive_limit()}));
                }
                let total = &mut totals[index];
                total.expired_segments += result.expired_segments;
                total.expired_bytes += result.expired_bytes;
                total.retained_bytes += result.retained_bytes;
                if let Some(reason) = result.deferred {
                    let reason = format!("team {batch_team}: {reason}");
                    total.deferred = Some(match total.deferred.take() {
                        Some(previous) => format!("{previous}; {reason}"),
                        None => reason,
                    });
                }
            }
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
        let mut expired = serde_json::Map::new();
        let mut retained = serde_json::Map::new();
        let mut deferred = serde_json::Map::new();
        for (name, total) in ["duties", "escalations", "records"].into_iter().zip(totals) {
            expired.insert(name.into(), json!({"segments": total.expired_segments, "bytes": total.expired_bytes}));
            retained.insert(name.into(), json!(total.retained_bytes));
            if let Some(reason) = total.deferred { deferred.insert(name.into(), json!(reason)); }
        }
        let mut result = json!({"duties": n[0], "escalations": n[1], "records": n[2],
            "expired": expired, "retained_bytes": retained, "limit_bytes": store.archive_limit(), "over_limit": over_limit});
        if !deferred.is_empty() {
            result["expiry_deferred"] = Value::Object(deferred);
        }
        Ok(result)
    })
}

fn team_dir(store: &Store, team: &str) -> Result<PathBuf> {
    if team.is_empty() || store::sanitize(team) != team {
        return Err(corrupt("invalid archive team id"));
    }
    Ok(store.home().join("archive").join(team))
}

impl archive::Segment for ArchiveSegment {
    fn number(&self) -> u64 {
        self.n
    }
    fn bytes(&self) -> u64 {
        self.bytes
    }
}

fn validate_stream(stream: &ArchiveStream) -> Result<()> {
    if stream.next == 0
        || stream.segments.last().is_some_and(|s| s.n >= stream.next)
        || stream.segments.iter().any(|s| match (s.lo, s.hi) {
            (Some(lo), Some(hi)) => lo > hi,
            (None, None) => false,
            _ => true,
        })
    {
        return Err(corrupt("invalid archive segment bookkeeping"));
    }
    Ok(())
}

fn duty_number(id: &str) -> Option<u64> {
    id.rsplit_once('-')?.1.parse().ok()
}

fn rows<T: Serialize>(entries: &[T]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut bytes, entry).map_err(write_error)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

#[derive(Default)]
struct Maintenance {
    expired_segments: u64,
    expired_bytes: u64,
    retained_bytes: u64,
    deferred: Option<String>,
}

fn maintain(
    path: &Path,
    stream: &mut ArchiveStream,
    bytes: &[u8],
    bounds: Option<(u64, u64)>,
    limit: u64,
    cleanup: bool,
) -> Result<Maintenance> {
    validate_stream(stream)?;
    let prefix = archive::prepare_segments(path, &stream.segments)?;
    let mut result = Maintenance::default();
    for segment in stream.segments.drain(..prefix) {
        result.expired_segments += 1;
        result.expired_bytes += segment.bytes;
    }
    if !bytes.is_empty() {
        let appended =
            archive::append_segment(path, &stream.segments, &mut stream.next, limit / 8, bytes)?;
        if appended.created {
            stream.segments.push(ArchiveSegment {
                n: appended.number,
                bytes: appended.bytes,
                lo: bounds.map(|b| b.0),
                hi: bounds.map(|b| b.1),
            });
        } else {
            let last = stream
                .segments
                .last_mut()
                .expect("append_segment selected an existing segment");
            last.bytes = appended.bytes;
            // Adopted segments stay unbounded: their older rows were never indexed.
            match (last.lo, last.hi, bounds) {
                (Some(lo), Some(hi), Some((new_lo, new_hi))) => {
                    last.lo = Some(lo.min(new_lo));
                    last.hi = Some(hi.max(new_hi));
                }
                _ => {
                    last.lo = None;
                    last.hi = None;
                }
            }
        }
    }
    let enforcement = archive::enforce_limit(path, &mut stream.segments, limit)?;
    for segment in enforcement.expired {
        result.expired_segments += 1;
        result.expired_bytes += segment.bytes;
    }
    result.deferred = enforcement.deferred;
    if cleanup {
        let cleanup_error = archive::remove_unlisted(path, &stream.segments)?;
        if result.deferred.is_none() {
            result.deferred = cleanup_error;
        }
    }
    result.retained_bytes = stream.segments.iter().map(|s| s.bytes).sum();
    Ok(result)
}

/// Hot contracts use the caller's snapshot; archive reads lock against truncation.
pub fn full_duty(store: &Store, st: &State, session: &str, duty: &str) -> Result<Duty> {
    if st.duties.contains_key(duty) {
        return crate::card::full_duty(st, session, duty).cloned();
    }
    let _lock = store::lock(store.home(), "state.lock", store::CLI_DEADLINE)?;
    let st = store.read_view()?;
    let member = authz::member_for_session(&st, session)
        .ok_or_else(|| Error::new(Reason::NotMember, "session has no active seat"))?;
    if st.duties.contains_key(duty) {
        return crate::card::full_duty(&st, session, duty).cloned();
    }
    let stream = st.archives.get(&member.team).map(|a| &a.duties);
    let mut found = None;
    if let Some(stream) = stream {
        validate_stream(stream)?;
        let path = team_dir(store, &member.team)?.join("duties.jsonl");
        let prefix = archive::expired_prefix(&path, &stream.segments)?;
        for index in (prefix..stream.segments.len()).rev() {
            let segment = &stream.segments[index];
            if let (Some(number), Some(lo), Some(hi)) = (duty_number(duty), segment.lo, segment.hi)
                && (number < lo || number > hi)
            {
                continue;
            }
            archive::scan_segment(&path, &stream.segments, index, |line| {
                let entry: Duty =
                    serde_json::from_slice(line).map_err(|e| corrupt(e.to_string()))?;
                if entry.team != member.team {
                    return Err(corrupt("archive entry belongs to another team"));
                }
                if entry.id == duty {
                    found = Some(entry);
                }
                Ok(())
            })?;
            if found.is_some() {
                break;
            }
        }
    }
    found.ok_or_else(|| {
        Error::new(
            Reason::NotFound,
            "duty not found in hot state or retained team archive",
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
