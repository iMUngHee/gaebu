//! The persisted data model. `docs/model.md` is the prose contract for these
//! types; the JSON written to `~/.gaebu/state.json` is exactly their serde form.
//!
//! Times are Unix milliseconds. Ids are short prefixed strings minted by
//! `State::mint` (`t-`, `b-`, `m-`, `g-`, `d-`, `l-`, `r-`, `e-`, `n-`).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA_VERSION: u32 = 1;

pub type Ms = u64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub schema_version: u32,
    /// Bumped by every committed write.
    pub rev: u64,
    /// Id allocator; ids are `<prefix>-<n>`.
    pub next_id: u64,
    pub teams: BTreeMap<String, Team>,
    pub bus: BTreeMap<String, Bu>,
    pub members: BTreeMap<String, Member>,
    pub grants: BTreeMap<String, Grant>,
    pub duties: BTreeMap<String, Duty>,
    pub leases: BTreeMap<String, Lease>,
    pub escalations: BTreeMap<String, Escalation>,
    pub records: Vec<Record>,
    /// Idempotency keys of root directives: `<prompt sha256>:<action>:<target>`.
    pub root_actions: BTreeMap<String, RootAction>,
    /// Notices to deliver through pager after commit.
    pub outbox: Vec<Notice>,
}

impl Default for State {
    fn default() -> Self {
        State {
            schema_version: SCHEMA_VERSION,
            rev: 0,
            next_id: 1,
            teams: BTreeMap::new(),
            bus: BTreeMap::new(),
            members: BTreeMap::new(),
            grants: BTreeMap::new(),
            duties: BTreeMap::new(),
            leases: BTreeMap::new(),
            escalations: BTreeMap::new(),
            records: Vec::new(),
            root_actions: BTreeMap::new(),
            outbox: Vec::new(),
        }
    }
}

impl State {
    /// Mint a fresh id with the given prefix (`"m"` → `"m-12"`).
    pub fn mint(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}-{}", self.next_id);
        self.next_id += 1;
        id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamStatus {
    Active,
    Cancelled,
}

/// A command unit: one 가절월 holder and everyone under them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Team {
    pub id: String,
    pub name: String,
    /// Free label of the project the scopes are relative to.
    pub project: String,
    /// Member holding the root grant (가절월).
    pub commander: String,
    /// The commander's own bu (승상부 by default).
    pub root_bu: String,
    pub status: TeamStatus,
    pub created_at: Ms,
    pub cancelled_at: Option<Ms>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuStatus {
    Open,
    Closed,
    Cancelled,
}

/// 부(府): a unit opened under 개부권 (`open`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bu {
    pub id: String,
    pub team: String,
    pub parent: Option<String>,
    pub name: String,
    pub head: String,
    pub status: BuStatus,
    pub created_at: Ms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingState {
    /// Designated by alias, not yet confirmed by the target session's hook.
    Pending,
    /// Confirmed: `session_id` speaks for this member.
    Bound,
    /// The bound session was observed gone, superseded, or the alias moved.
    Vacant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Binding {
    pub state: BindingState,
    /// pager alias given at designation time.
    pub alias: String,
    pub tool: Option<Tool>,
    pub session_id: Option<String>,
    pub designated_at: Ms,
    pub bound_at: Option<Ms>,
    pub vacated_at: Option<Ms>,
    /// Why it went vacant (`pager_gone`, `alias_moved`, `rebound`, ...).
    pub vacated_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    Active,
    Dismissed,
}

/// A seat in the team. Authority attaches here, never to a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Member {
    pub id: String,
    pub team: String,
    /// The bu this member belongs to (속관 of that bu). The commander belongs
    /// to the root bu.
    pub bu: String,
    /// Direct superior; `None` only for the commander.
    pub superior: Option<String>,
    /// Display title (Three Kingdoms motif); never carries authority.
    pub title: String,
    pub status: MemberStatus,
    pub binding: Binding,
    pub created_at: Ms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    /// Issue duties to members below.
    Assign,
    /// Amend duties one issued or that sit below.
    Amend,
    /// Cancel bu/duties below.
    Cancel,
    /// Move members, rebind sessions below.
    Reassign,
    /// 개부권: open a bu below and recruit into it.
    Open,
    /// Force-release leases held below.
    Arbitrate,
}

impl Verb {
    pub const ALL: [Verb; 6] = [
        Verb::Assign,
        Verb::Amend,
        Verb::Cancel,
        Verb::Reassign,
        Verb::Open,
        Verb::Arbitrate,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantStatus {
    Active,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attest {
    /// Claude mod `prompt.submit` with engine-stamped `origin.kind == composer`.
    Composer,
    /// Hook UserPromptSubmit that passed the exclusion rules.
    HookFiltered,
    /// `gaebu dev seed` on a non-default home. Never valid in the default store.
    Fixture,
}

/// Where a root directive's authority came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub session_id: String,
    pub prompt_seq: u64,
    pub sha256: String,
    pub attest: Attest,
    pub quote: String,
    pub at: Ms,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Grant {
    pub id: String,
    pub team: String,
    /// `None` for the root grant (from the user).
    pub grantor: Option<String>,
    pub grantee: String,
    pub verbs: BTreeSet<Verb>,
    /// Scope patterns (see `scope`).
    pub scope: Vec<String>,
    pub status: GrantStatus,
    pub evidence: Option<Evidence>,
    pub created_at: Ms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DutyStatus {
    Proposed,
    Accepted,
    Declined,
    Blocked,
    Done,
    Cancelled,
    HandedOff,
}

/// 소임: an outcome-level commitment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Duty {
    pub id: String,
    pub team: String,
    pub bu: String,
    pub assignee: String,
    pub issuer: String,
    pub intent: String,
    pub outcome: String,
    pub scope: Vec<String>,
    pub decision_rights: String,
    pub escalate_on: Vec<String>,
    pub done_criteria: String,
    pub plan_ref: Option<String>,
    pub exec_root: Option<String>,
    pub status: DutyStatus,
    /// Optimistic-concurrency version; bumped on each change.
    pub version: u64,
    /// Decline/blocked/done/handoff note.
    pub note: Option<String>,
    pub created_at: Ms,
    pub updated_at: Ms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseMode {
    Exclusive,
    Shared,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Lease {
    pub id: String,
    pub team: String,
    /// Scope pattern or `res:<name>`.
    pub key: String,
    pub mode: LeaseMode,
    pub holder: String,
    pub holder_session: String,
    pub reason: String,
    pub acquired_at: Ms,
    pub expires_at: Ms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStatus {
    Open,
    Answered,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Escalation {
    pub id: String,
    pub team: String,
    pub from: String,
    /// Member it is addressed to; `None` = the user's queue.
    pub to: Option<String>,
    pub duty: Option<String>,
    pub question: String,
    pub status: EscalationStatus,
    pub answer: Option<String>,
    pub created_at: Ms,
    pub answered_at: Option<Ms>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Decision,
    Handoff,
    Change,
    Note,
}

/// 팀기록.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub id: String,
    pub team: String,
    pub kind: RecordKind,
    pub author: String,
    pub refs: Vec<String>,
    pub text: String,
    pub at: Ms,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RootAction {
    pub team: String,
    pub rev: u64,
    pub at: Ms,
}

/// A pending pager notification, flushed after commit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notice {
    pub id: String,
    pub to_member: String,
    pub to_session: String,
    pub body: String,
    pub created_at: Ms,
    pub attempts: u32,
    /// Set while a flush is sending this notice. A claim older than
    /// `NOTICE_CLAIM_MS` is stale (the sender died) and the notice is sent again.
    #[serde(default)]
    pub claimed_at: Option<Ms>,
}

/// How long a flush may hold a notice before another flush may retry it.
pub const NOTICE_CLAIM_MS: Ms = 60_000;

/// The member acting, after caller resolution and binding checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub member: String,
    pub session_id: String,
}

/// Wall clock in Unix ms. Tests pass explicit times into `ops` instead.
pub fn now_ms() -> Ms {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as Ms)
        .unwrap_or(0)
}
