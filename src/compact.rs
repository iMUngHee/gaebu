//! Retire terminal runtime history behind committed JSONL boundaries.

use crate::archive;
use crate::model::{
    ArchiveSegment, ArchiveStream, Duty, DutyStatus, Escalation, EscalationStatus, Ms, Record,
    State, TeamStatus,
};
use crate::store::{self, Store};
use crate::{Error, Reason, Result, authz, identity};
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
    if let Err(error) = compact(store, team, retired, 64, None) {
        eprintln!("gaebu: runtime compaction deferred: {error}");
    }
}

/// Retire eligible rows and maintain the caller team’s retained archives.
pub fn run(store: &Store, team: &str, retired: Option<&Retired>) -> Result<Value> {
    compact(store, team, retired, 1, None)
}

/// Explicit destructive recovery rechecks the caller while holding state.lock.
pub fn run_heal(
    store: &Store,
    caller: &identity::Caller,
    team: &str,
    retired: Option<&Retired>,
) -> Result<Value> {
    compact(store, team, retired, 1, Some(caller))
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
    heal_caller: Option<&identity::Caller>,
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
    let mut healing: BTreeMap<&str, HealAttempt> = BTreeMap::new();
    let result = store.transact(store::CLI_DEADLINE, |st| {
        if let Some(caller) = heal_caller {
            let actor = identity::actor(st, caller)?;
            if st.members[&actor.member].team != team
                || st.teams.get(team).is_none_or(|t| t.commander != actor.member)
            {
                return Err(Error::new(
                    Reason::NoGrant,
                    "compact --heal requires the team commander",
                ));
            }
            let archives = st.archives.get(team).cloned().unwrap_or_default();
            let dir = team_dir(store, team)?;
            for (name, stream) in [
                ("duties", &archives.duties),
                ("escalations", &archives.escalations),
                ("records", &archives.records),
            ] {
                validate_stream(stream)?;
                archive::precheck_heal(&dir.join(format!("{name}.jsonl")), &stream.segments)?;
            }
        }
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
        let mut archived = [0; 3];
        let mut archived_records = BTreeSet::new();
        let mut skipped = Vec::new();
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
            let mut completed = [true; 3];
            let batch_counts = [
                batch.duties.len(),
                batch.escalations.len(),
                batch.records.len(),
            ];
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
                let path = dir.join(format!("{name}.jsonl"));
                if heal_caller.is_some() && batch_team == team {
                    let healed = archive::heal_holes(&path, &stream.segments)?;
                    let stats = HealStats {
                        segments: healed.segments.len() as u64,
                        bytes: stream.segments.iter()
                            .filter(|s| healed.segments.contains(&s.n))
                            .map(|s| s.bytes).sum(),
                        unlisted_files: healed.unlisted_files,
                        unlisted_bytes: healed.unlisted_bytes,
                    };
                    let partial = healed.deferred.is_some();
                    healing.insert(name, HealAttempt {
                        stats,
                        deferred: healed.deferred,
                        hole_remaining: healed.hole_remaining,
                    });
                    if partial {
                        completed[index] = false;
                    }
                }
                let mut result = if completed[index] {
                    let before = stream.clone();
                    match maintain(
                        &path, stream, &bytes, bounds, store.archive_limit(),
                        explicit && batch_team == team,
                    ) {
                        Ok(result) => {
                            archived[index] += batch_counts[index];
                            result
                        }
                        // Another team's damaged archive must not block this
                        // team's pass; its rows stay hot for its own commander.
                        Err(error) if batch_team != team => {
                            *stream = before;
                            completed[index] = false;
                            skipped.push(json!({"team": batch_team, "stream": name,
                                "reason": error.message}));
                            Maintenance {
                                retained_bytes: stream.segments.iter().map(|s| s.bytes).sum(),
                                ..Default::default()
                            }
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    Maintenance {
                        retained_bytes: stream.segments.iter().map(|s| s.bytes).sum(),
                        ..Default::default()
                    }
                };
                if completed[index] && batch_team == team
                    && let Some(attempt) = healing.get(name)
                {
                    // Normalization saw those unlinks as missing prefixes; report each once.
                    result.expired_segments -= attempt.stats.segments;
                    result.expired_bytes -= attempt.stats.bytes;
                }
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
            if completed[0] {
                for duty in &batch.duties {
                    st.duties.remove(&duty.id);
                }
            }
            if completed[1] {
                for escalation in &batch.escalations {
                    st.escalations.remove(&escalation.id);
                }
            }
            if completed[2] {
                archived_records.extend(batch.records.iter().map(|r| r.id.as_str()));
            }
            // Retry paths may reuse links created by a failed, unsynced pass.
            store::sync_dir(&dir)?;
        }
        store::sync_dir(&archive)?;
        store::sync_dir(store.home())?;
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
        let mut result = json!({"duties": archived[0], "escalations": archived[1], "records": archived[2],
            "expired": expired, "retained_bytes": retained, "limit_bytes": store.archive_limit(), "over_limit": over_limit});
        if !deferred.is_empty() {
            result["expiry_deferred"] = Value::Object(deferred);
        }
        if !skipped.is_empty() {
            result["skipped"] = json!(skipped);
        }
        if heal_caller.is_some() {
            let mut healed = serde_json::Map::new();
            let mut deferred = serde_json::Map::new();
            let mut holes = serde_json::Map::new();
            for (name, attempt) in &healing {
                healed.insert((*name).into(), json!(attempt.stats));
                if let Some(reason) = &attempt.deferred {
                    deferred.insert((*name).into(), json!(reason));
                    holes.insert((*name).into(), json!(attempt.hole_remaining));
                }
            }
            result["healed"] = json!(healed);
            if !deferred.is_empty() {
                result["heal_deferred"] = json!(deferred);
                result["hole_remaining"] = json!(holes);
            }
        }
        Ok(result)
    });
    result.map_err(|mut error| {
        if healing
            .values()
            .any(|a| a.stats.segments + a.stats.unlisted_files > 0)
        {
            for (stream, attempt) in &healing {
                let removed = attempt.stats.segments + attempt.stats.unlisted_files;
                if removed > 0 {
                    error.message.push_str(&format!(
                        "; heal had removed {removed} segment(s) of {team}/{stream}"
                    ));
                }
                if let Some(reason) = &attempt.deferred {
                    error.message.push_str(&format!(
                        "; heal of {team}/{stream} is incomplete ({reason}), run --heal again"
                    ));
                }
            }
        }
        error
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
        || stream
            .segments
            .iter()
            .try_fold(0u64, |sum, s| sum.checked_add(s.bytes))
            .is_none()
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

#[derive(Default, Serialize)]
struct HealStats {
    segments: u64,
    bytes: u64,
    unlisted_files: u64,
    unlisted_bytes: u64,
}

struct HealAttempt {
    stats: HealStats,
    deferred: Option<String>,
    hole_remaining: bool,
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

    struct HealFixture {
        store: Store,
        caller: identity::Caller,
        team: String,
        hot: [String; 3],
    }

    impl HealFixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let home = std::env::temp_dir().join(format!(
                "gaebu-heal-unit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let store = Store::open(&home).unwrap();
            let (team, hot) = store
                .transact(store::CLI_DEADLINE, |st| {
                    ops::dev_seed(
                        st,
                        1,
                        &SeedSpec {
                            team: "test".into(),
                            project: "test".into(),
                            commander_alias: "root".into(),
                            commander_session: Some("root".into()),
                            commander_tool: Some(Tool::Codex),
                            members: vec![(
                                "worker".into(),
                                Some("worker".into()),
                                Some(Tool::Codex),
                            )],
                        },
                    )
                    .unwrap();
                    let root = Actor {
                        member: authz::member_for_session(st, "root").unwrap().id.clone(),
                        session_id: "root".into(),
                    };
                    let worker = authz::member_for_session(st, "worker").unwrap().id.clone();
                    let duty = ops::duty_issue(
                        st,
                        1,
                        &root,
                        &worker,
                        DutySpec {
                            outcome: "delivered".into(),
                            ..Default::default()
                        },
                    )
                    .unwrap()
                    .ids[0]
                        .clone();
                    st.duties.get_mut(&duty).unwrap().status = DutyStatus::Done;
                    let escalation = ops::escalate(st, 1, &root, None, "which path?")
                        .unwrap()
                        .ids[0]
                        .clone();
                    ops::escalation_withdraw(st, 1, &root, &escalation, None).unwrap();
                    let team = st.members[&root.member].team.clone();
                    st.records.clear();
                    st.outbox.clear();
                    let record = Record {
                        id: st.mint("r"),
                        team: team.clone(),
                        author: root.member.clone(),
                        kind: RecordKind::Note,
                        refs: vec![],
                        text: "history".into(),
                        at: 1,
                    };
                    let record_id = record.id.clone();
                    let entries = [
                        ("duties", serde_json::to_vec(&st.duties[&duty]).unwrap()),
                        (
                            "escalations",
                            serde_json::to_vec(&st.escalations[&escalation]).unwrap(),
                        ),
                        ("records", serde_json::to_vec(&record).unwrap()),
                    ];
                    st.records.push(record);
                    let dir = team_dir(&store, &team).unwrap();
                    fs::create_dir_all(&dir).unwrap();
                    let mut archives = TeamArchives::default();
                    for ((name, mut bytes), stream) in entries.into_iter().zip([
                        &mut archives.duties,
                        &mut archives.escalations,
                        &mut archives.records,
                    ]) {
                        bytes.push(b'\n');
                        stream.next = 5;
                        for n in 1..=4 {
                            stream.segments.push(ArchiveSegment {
                                n,
                                bytes: bytes.len() as u64,
                                lo: None,
                                hi: None,
                            });
                            if n < 4 {
                                fs::write(
                                    archive::segment_path(&dir.join(format!("{name}.jsonl")), n),
                                    &bytes,
                                )
                                .unwrap();
                            }
                        }
                    }
                    st.archives.insert(team.clone(), archives);
                    Ok((team, [duty, escalation, record_id]))
                })
                .unwrap();
            Self {
                store,
                caller: identity::Caller {
                    session_id: "root".into(),
                    tool: Some(Tool::Codex),
                    source: identity::Source::PagerNative,
                },
                team,
                hot,
            }
        }
        fn base(&self, name: &str) -> PathBuf {
            team_dir(&self.store, &self.team)
                .unwrap()
                .join(format!("{name}.jsonl"))
        }
        fn heal(&self) -> Result<Value> {
            run_heal(&self.store, &self.caller, &self.team, None)
        }
    }
    impl Drop for HealFixture {
        fn drop(&mut self) {
            archive::FAILED_UNLINK.with_borrow_mut(|failed| *failed = None);
            archive::FAILED_HEAL_SYNC.with_borrow_mut(|failed| *failed = None);
            let _ = fs::remove_dir_all(self.store.home());
        }
    }

    #[test]
    fn partial_heal_reports_and_resumes() {
        let f = HealFixture::new();
        let before = f.store.read_current().unwrap();
        let path = archive::segment_path(&f.base("duties"), 2);
        archive::FAILED_UNLINK.with_borrow_mut(|failed| *failed = Some(path));
        let result = f.heal().unwrap();
        assert_eq!(result["healed"]["duties"]["segments"], 1);
        assert!(result["heal_deferred"]["duties"].as_str().is_some());
        assert_eq!(result["hole_remaining"]["duties"], true);
        assert_eq!(result["duties"], 0);
        assert_eq!(result["escalations"], 1);
        assert_eq!(result["records"], 1);
        assert_eq!(
            result["retained_bytes"]["duties"],
            before.archives[&f.team]
                .duties
                .segments
                .iter()
                .map(|s| s.bytes)
                .sum::<u64>()
        );
        let state = f.store.read_current().unwrap();
        assert_eq!(
            state.archives[&f.team].duties,
            before.archives[&f.team].duties
        );
        assert!(state.duties.contains_key(&f.hot[0]));
        assert!(!state.escalations.contains_key(&f.hot[1]));
        assert!(!state.records.iter().any(|r| r.id == f.hot[2]));
        archive::FAILED_UNLINK.with_borrow_mut(|failed| *failed = None);
        let retry = f.heal().unwrap();
        assert_eq!(retry["duties"], 1);
        assert_eq!(retry["healed"]["duties"]["segments"], 2);
        assert_eq!(retry["expired"]["duties"]["segments"], 2);
        assert!(retry.get("heal_deferred").is_none());
        assert_eq!(
            f.store.read_current().unwrap().archives[&f.team]
                .duties
                .segments[0]
                .n,
            5
        );
    }

    #[test]
    fn heal_sync_failure_after_last_unlink() {
        let f = HealFixture::new();
        let before = f.store.read_current().unwrap();
        archive::FAILED_HEAL_SYNC.with_borrow_mut(|failed| *failed = Some((f.base("duties"), 4)));
        let result = f.heal().unwrap();
        assert_eq!(result["healed"]["duties"]["segments"], 3);
        assert!(result["heal_deferred"]["duties"].as_str().is_some());
        assert_eq!(result["hole_remaining"]["duties"], false);
        assert_eq!(result["duties"], 0);
        let state = f.store.read_current().unwrap();
        assert_eq!(
            state.archives[&f.team].duties,
            before.archives[&f.team].duties
        );
        assert!(state.duties.contains_key(&f.hot[0]));
        archive::FAILED_HEAL_SYNC.with_borrow_mut(|failed| *failed = None);
        let retry = run(&f.store, &f.team, None).unwrap();
        assert_eq!(retry["duties"], 1);
        assert_eq!(retry["expired"]["duties"]["segments"], 4);
    }

    #[test]
    fn heal_reports_listed_and_unlisted_separately() {
        let f = HealFixture::new();
        fs::remove_file(archive::segment_path(&f.base("duties"), 1)).unwrap();
        f.store
            .transact(store::CLI_DEADLINE, |st| {
                st.archives
                    .get_mut(&f.team)
                    .unwrap()
                    .duties
                    .segments
                    .retain(|s| s.n >= 3);
                Ok(())
            })
            .unwrap();
        let before = f.store.read_current().unwrap();
        let unlisted_bytes = fs::metadata(archive::segment_path(&f.base("duties"), 2))
            .unwrap()
            .len();
        archive::FAILED_UNLINK
            .with_borrow_mut(|failed| *failed = Some(archive::segment_path(&f.base("duties"), 3)));
        let result = f.heal().unwrap();
        assert_eq!(result["healed"]["duties"]["segments"], 0);
        assert_eq!(result["healed"]["duties"]["bytes"], 0);
        assert_eq!(result["healed"]["duties"]["unlisted_files"], 1);
        assert_eq!(result["healed"]["duties"]["unlisted_bytes"], unlisted_bytes);
        assert_eq!(result["expired"]["duties"]["segments"], 0);
        assert!(result["heal_deferred"]["duties"].as_str().is_some());
        assert_eq!(
            f.store.read_current().unwrap().archives[&f.team].duties,
            before.archives[&f.team].duties
        );
    }

    #[test]
    fn partial_heal_error_names_incomplete_archive() {
        let f = HealFixture::new();
        archive::FAILED_UNLINK
            .with_borrow_mut(|failed| *failed = Some(archive::segment_path(&f.base("duties"), 2)));
        fs::create_dir(archive::segment_path(&f.base("escalations"), 5)).unwrap();
        let error = f.heal().unwrap_err();
        assert!(
            error.message.contains(&format!(
                "heal had removed 1 segment(s) of {}/duties",
                f.team
            )),
            "{error}"
        );
        assert!(
            error
                .message
                .contains(&format!("heal of {}/duties is incomplete (", f.team)),
            "{error}"
        );
        assert!(error.message.contains("run --heal again"), "{error}");
        assert!(
            f.store
                .read_current()
                .unwrap()
                .duties
                .contains_key(&f.hot[0])
        );
    }

    #[test]
    fn heal_error_before_unlink_preserves_the_cause() {
        let mut f = HealFixture::new();
        fs::copy(
            archive::segment_path(&f.base("escalations"), 3),
            archive::segment_path(&f.base("escalations"), 4),
        )
        .unwrap();
        f.store = Store::open_with_archive_limit(f.store.home(), 8).unwrap();
        let blocked = archive::segment_path(&f.base("escalations"), 5);
        fs::create_dir(&blocked).unwrap();
        archive::FAILED_HEAL_SYNC.with_borrow_mut(|failed| *failed = Some((f.base("duties"), 1)));
        let before = f.store.read_current().unwrap();
        let error = f.heal().unwrap_err();
        assert_eq!(error.reason, Reason::StoreWriteFailed);
        assert!(!error.message.contains("; heal"), "{error}");
        assert_eq!(f.store.read_current().unwrap(), before);
        assert!(f.base("duties").is_file());
        assert!(f.base("escalations").is_file());
    }

    struct HealCliPager;
    impl crate::pager::Pager for HealCliPager {
        fn whoami(&self, _: Option<&str>) -> Result<crate::pager::WhoAmI> {
            Ok(crate::pager::WhoAmI {
                session_id: "root".into(),
                via: "native".into(),
                alias: Some("root".into()),
                tool: Some("codex".into()),
            })
        }
        fn host_status(&self, _: &str) -> Result<crate::pager::HostStatus> {
            Ok(crate::pager::HostStatus::Unknown)
        }
        fn send(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    #[ignore = "child process helper"]
    fn heal_cli_partial_child() {
        let Some(blocked) = std::env::var_os("GAEBU_TEST_HEAL_UNLINK") else {
            return;
        };
        archive::FAILED_UNLINK.with_borrow_mut(|failed| *failed = Some(blocked.into()));
        let args = ["gaebu", "compact", "--heal"]
            .into_iter()
            .map(std::ffi::OsString::from);
        std::process::exit(crate::cli::run_with(args, &HealCliPager));
    }

    #[test]
    fn heal_partial_cli_text_reports_deferred_and_remaining_hole() {
        let f = HealFixture::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "compact::tests::heal_cli_partial_child",
                "--ignored",
                "--nocapture",
            ])
            .env("GAEBU_HOME", f.store.home())
            .env(
                "GAEBU_TEST_HEAL_UNLINK",
                archive::segment_path(&f.base("duties"), 2),
            )
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(text.contains("healed:"), "{text}");
        assert!(text.contains("heal_deferred:"), "{text}");
        assert!(text.contains("hole_remaining: {\"duties\":true}"), "{text}");
    }

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
