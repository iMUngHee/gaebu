//! Canonical project schedules. Runtime duties join immutable dispatches by token;
//! execution progress is never copied into the Markdown document.

use crate::{Error, Reason, Result, model::*, ops::DutySpec, scope};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
    time::Instant,
};

mod commands;
mod dispatch;
mod operations;
mod presentation;
mod runtime;
pub use commands::{ScheduleCmd, call, human_hold, reserve, run_cli, tools};
pub use presentation::view;
pub use runtime::{register, set_manager, transaction};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_ITEMS: usize = 2_000;
const MARKER: &str = "<!-- gaebu-schedule -->";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginRef {
    pub runtime_id: String,
    pub team: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PmRef {
    pub task: String,
    pub item: String,
    #[serde(default)]
    pub plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub by: OriginRef,
    pub at: Ms,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub by: OriginRef,
    pub at: Ms,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchSpec {
    pub intent: String,
    pub outcome: String,
    pub scope: Vec<String>,
    pub decision_rights: String,
    pub escalate_on: Vec<String>,
    pub done_criteria: String,
    #[serde(default)]
    pub plan_ref: Option<String>,
    #[serde(default)]
    pub exec_root: Option<String>,
    #[serde(default)]
    pub source_quote: Option<String>,
}

impl DispatchSpec {
    fn duty_spec(&self, source: Option<Source>) -> DutySpec {
        DutySpec {
            intent: self.intent.clone(),
            outcome: self.outcome.clone(),
            scope: self.scope.clone(),
            decision_rights: self.decision_rights.clone(),
            escalate_on: self.escalate_on.clone(),
            done_criteria: self.done_criteria.clone(),
            plan_ref: self.plan_ref.clone(),
            exec_root: self.exec_root.clone(),
            source_quote: self.source_quote.clone(),
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dispatch {
    pub key: String,
    pub token: String,
    pub assignee: OriginRef,
    pub issuer: OriginRef,
    pub spec: DispatchSpec,
    /// DutySpec skips its resolved Source on serialization; preserve that verified
    /// source here so replay does not depend on the rolling prompt log.
    #[serde(default, deserialize_with = "strict_source")]
    pub source: Option<Source>,
    pub manifest_hash: String,
    pub predecessor: Option<String>,
    pub retired: Option<Decision>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub pm_ref: Option<PmRef>,
    pub priority: u16,
    pub depends_on: Vec<String>,
    pub scope: Vec<String>,
    pub owner_bu: Option<OriginRef>,
    #[serde(default, deserialize_with = "strict_hold")]
    pub hold: Option<Hold>,
    pub cancellation: Option<Decision>,
    pub dispatches: Vec<Dispatch>,
    pub completion: Option<Completion>,
}

// Runtime provenance types tolerate future fields; the canonical writer must
// reject unknown artifact fields instead of silently dropping them on rewrite.
fn strict_source<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Source>, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StoredSource {
        session_id: String,
        prompt_seq: u64,
        sha256: String,
        quote: String,
        at: Ms,
    }
    Ok(
        Option::<StoredSource>::deserialize(deserializer)?.map(|s| Source {
            session_id: s.session_id,
            prompt_seq: s.prompt_seq,
            sha256: s.sha256,
            quote: s.quote,
            at: s.at,
        }),
    )
}

fn strict_hold<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Hold>, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StoredEvidence {
        session_id: String,
        prompt_seq: u64,
        sha256: String,
        attest: Attest,
        quote: String,
        at: Ms,
        #[serde(default)]
        strength: Strength,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StoredHold {
        evidence: StoredEvidence,
        until: String,
        at: Ms,
    }
    Ok(
        Option::<StoredHold>::deserialize(deserializer)?.map(|h| Hold {
            evidence: Evidence {
                session_id: h.evidence.session_id,
                prompt_seq: h.evidence.prompt_seq,
                sha256: h.evidence.sha256,
                attest: h.evidence.attest,
                quote: h.evidence.quote,
                at: h.evidence.at,
                strength: h.evidence.strength,
            },
            until: h.until,
            at: h.at,
        }),
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub schema_version: u32,
    pub id: String,
    pub revision: u64,
    pub next_item: u64,
    pub next_dispatch: u64,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub schedule: Schedule,
    prefix: String,
    suffix: String,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(Reason::InvalidArgument, message)
}
fn conflict(message: impl Into<String>) -> Error {
    Error::new(Reason::Conflict, message)
}
fn io_error(error: impl std::fmt::Display) -> Error {
    Error::new(Reason::StoreWriteFailed, error.to_string())
}

pub fn read_document(path: &Path) -> Result<Document> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(io_error)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("schedule exceeds 2 MiB"));
    }
    let text = String::from_utf8(bytes).map_err(|_| invalid("schedule is not UTF-8"))?;
    parse_document(&text)
}

pub fn parse_document(text: &str) -> Result<Document> {
    if text.len() > MAX_BYTES || text.matches(MARKER).count() != 1 {
        return Err(invalid("schedule needs exactly one gaebu-schedule marker"));
    }
    let marker = text.find(MARKER).unwrap();
    let rest = &text[marker + MARKER.len()..];
    let whitespace = rest.len() - rest.trim_start().len();
    let fence = marker + MARKER.len() + whitespace;
    let after = text[fence..]
        .strip_prefix("```json\r\n")
        .or_else(|| text[fence..].strip_prefix("```json\n"))
        .ok_or_else(|| invalid("schedule marker must precede a JSON fence"))?;
    let start = text.len() - after.len();
    let end = after
        .find("\n```")
        .map(|offset| start + offset)
        .ok_or_else(|| invalid("schedule JSON fence is not closed"))?;
    let closing_end = end + 4;
    if text[closing_end..]
        .chars()
        .next()
        .is_some_and(|ch| ch != '\r' && ch != '\n')
    {
        return Err(invalid("invalid closing schedule fence"));
    }
    let schedule: Schedule = serde_json::from_str(text[start..end].trim_end_matches('\r'))
        .map_err(|error| invalid(format!("invalid schedule JSON: {error}")))?;
    validate(&schedule)?;
    Ok(Document {
        schedule,
        prefix: text[..start].to_owned(),
        suffix: text[end..].to_owned(),
    })
}

impl Document {
    fn empty(id: String) -> Self {
        Self {
            schedule: Schedule {
                schema_version: 1,
                id,
                revision: 0,
                next_item: 1,
                next_dispatch: 1,
                items: vec![],
            },
            prefix: format!("# Gaebu Schedule\n\n{MARKER}\n```json\n"),
            suffix: "\n```\n".into(),
        }
    }
    fn save(&mut self, path: &Path, deadline: Instant) -> Result<()> {
        validate(&self.schedule)?;
        self.schedule.revision = self
            .schedule
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("schedule revision exhausted"))?;
        let json = serde_json::to_string_pretty(&self.schedule)
            .map_err(|error| invalid(error.to_string()))?;
        let text = format!("{}{}{}", self.prefix, json, self.suffix);
        if text.len() > MAX_BYTES {
            return Err(invalid("schedule exceeds 2 MiB"));
        }
        crate::store::replace_until(path, text.as_bytes(), deadline)
    }
}

fn nonempty(text: &str, field: &str) -> Result<()> {
    if text.trim().is_empty() || text.len() > 16_384 {
        return Err(invalid(format!("{field} must contain 1-16384 bytes")));
    }
    Ok(())
}

fn validate(schedule: &Schedule) -> Result<()> {
    if schedule.schema_version != 1 {
        return Err(Error::new(
            Reason::StoreUnsupported,
            "unsupported schedule schema",
        ));
    }
    nonempty(&schedule.id, "schedule id")?;
    if schedule.items.len() > MAX_ITEMS || schedule.next_item == 0 || schedule.next_dispatch == 0 {
        return Err(invalid("invalid schedule size or allocator"));
    }
    let mut ids = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    let mut max_item = 0;
    let mut max_dispatch = 0;
    for item in &schedule.items {
        let number = item
            .id
            .strip_prefix("w-")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("invalid item id"))?;
        max_item = max_item.max(number);
        if !ids.insert(item.id.as_str()) {
            return Err(invalid("duplicate schedule item id"));
        }
        nonempty(&item.title, "title")?;
        scope::normalize_list(&item.scope)?;
        if let Some(pm) = &item.pm_ref {
            nonempty(&pm.task, "PM task")?;
            nonempty(&pm.item, "PM item")?;
            if let Some(plan) = &pm.plan
                && (plan.contains(['\\', ':', '*'])
                    || plan.starts_with('/')
                    || plan
                        .split('/')
                        .any(|part| part.is_empty() || part == "." || part == ".."))
            {
                return Err(invalid("PM plan must be a project-relative path"));
            }
        }
        let mut keys = BTreeSet::new();
        for dispatch in &item.dispatches {
            nonempty(&dispatch.key, "dispatch key")?;
            let number = dispatch
                .token
                .strip_prefix("x-")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("invalid dispatch token"))?;
            max_dispatch = max_dispatch.max(number);
            if !tokens.insert(dispatch.token.as_str()) {
                return Err(invalid("duplicate dispatch token"));
            }
            if !keys.insert(dispatch.key.as_str()) && dispatch.predecessor.is_none() {
                return Err(invalid("dispatch key replacement needs a predecessor"));
            }
            if let Some(previous) = &dispatch.predecessor {
                let predecessor = item
                    .dispatches
                    .iter()
                    .find(|d| &d.token == previous)
                    .ok_or_else(|| invalid("dispatch predecessor missing"))?;
                if predecessor.key != dispatch.key
                    || predecessor.retired.is_none()
                    || predecessor.token == dispatch.token
                {
                    return Err(invalid("dispatch predecessor is not retired"));
                }
            }
            if dispatch.manifest_hash != manifest_hash(dispatch)? {
                return Err(conflict("dispatch manifest hash does not match"));
            }
            scope::normalize_list(&dispatch.spec.scope)?;
            if !scope::within(&dispatch.spec.scope, &item.scope)? {
                return Err(Error::new(
                    Reason::OutOfScope,
                    "dispatch exceeds item scope",
                ));
            }
        }
        if let Some(completion) = &item.completion {
            nonempty(&completion.evidence, "completion evidence")?;
        }
    }
    if schedule.next_item <= max_item || schedule.next_dispatch <= max_dispatch {
        return Err(invalid("schedule allocator would reuse an id"));
    }
    for item in &schedule.items {
        let mut seen = BTreeSet::new();
        for dependency in &item.depends_on {
            if dependency == &item.id
                || !ids.contains(dependency.as_str())
                || !seen.insert(dependency)
            {
                return Err(invalid("missing, repeated, or self dependency"));
            }
        }
    }
    // An iterative walk keeps a long but bounded dependency chain off the stack.
    let mut pending: BTreeMap<&str, usize> = schedule
        .items
        .iter()
        .map(|item| (item.id.as_str(), item.depends_on.len()))
        .collect();
    let mut ready: Vec<&str> = pending
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut completed = 0;
    while let Some(id) = ready.pop() {
        completed += 1;
        for item in &schedule.items {
            if item.depends_on.iter().any(|dep| dep == id) {
                let count = pending.get_mut(item.id.as_str()).unwrap();
                *count -= 1;
                if *count == 0 {
                    ready.push(item.id.as_str());
                }
            }
        }
    }
    if completed != schedule.items.len() {
        return Err(invalid("schedule dependency cycle"));
    }
    Ok(())
}

fn manifest_hash(dispatch: &Dispatch) -> Result<String> {
    let content = serde_json::to_string(&(
        &dispatch.key,
        &dispatch.token,
        &dispatch.assignee,
        &dispatch.issuer,
        &dispatch.spec,
        &dispatch.source,
        &dispatch.predecessor,
    ))
    .map_err(|error| invalid(error.to_string()))?;
    Ok(crate::evidence::sha256_hex(&content))
}

fn origin(st: &State, team: &str, id: &str) -> OriginRef {
    OriginRef {
        runtime_id: st.runtime_id.clone(),
        team: team.into(),
        id: id.into(),
    }
}

fn matching_origin(st: &State, team: &str, reference: &OriginRef) -> bool {
    !st.runtime_id.is_empty() && reference.runtime_id == st.runtime_id && reference.team == team
}

fn item_index(schedule: &Schedule, id: &str) -> Result<usize> {
    schedule
        .items
        .iter()
        .position(|item| item.id == id)
        .ok_or_else(|| Error::new(Reason::NotFound, "schedule item not found"))
}

fn latest_dispatches(item: &Item) -> Vec<&Dispatch> {
    let mut keys = BTreeSet::new();
    item.dispatches
        .iter()
        .rev()
        .filter(|d| keys.insert(&d.key))
        .collect()
}
