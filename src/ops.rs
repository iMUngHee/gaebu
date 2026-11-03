//! Domain operations. Each runs on `&mut State` inside one `Store::transact`,
//! so every check and write is serialized (Decision 12). Notices for other
//! members are pushed to `st.outbox`; `cli`/`mcp` flush them after commit.
//!
//! Root directives take an `Evidence` already verified by `evidence`.

use crate::error::Result;
use crate::model::{Actor, Evidence, LeaseMode, Ms, RecordKind, State, Tool, Verb};
use serde::{Deserialize, Serialize};

/// What an operation did, for CLI/MCP output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub summary: String,
    pub ids: Vec<String>,
}

// ----- root directives (from the user, via evidence) -----

/// Give 가절월 to `alias`: creates the team, the root bu named after the
/// commander's title, the commander member (pending binding) and the root
/// grant (all verbs, scope `**`). Idempotent on (prompt sha, action, target).
pub fn root_appoint(
    st: &mut State,
    now: Ms,
    ev: &Evidence,
    alias: &str,
    team: Option<&str>,
    project: &str,
    title: Option<&str>,
) -> Result<Outcome> {
    let _ = (st, now, ev, alias, team, project, title);
    todo!("nuzu: ops")
}

/// Put `aliases` under the commander held by `to_alias`, as 속관 of its root bu.
pub fn root_assign(
    st: &mut State,
    now: Ms,
    ev: &Evidence,
    aliases: &[String],
    to_alias: &str,
) -> Result<Outcome> {
    let _ = (st, now, ev, aliases, to_alias);
    todo!("nuzu: ops")
}

/// Dismiss the member held by `alias`; revokes everything delegated below it.
/// The evidence prompt must be newer than the member's appointment.
pub fn root_dismiss(st: &mut State, now: Ms, ev: &Evidence, alias: &str) -> Result<Outcome> {
    let _ = (st, now, ev, alias);
    todo!("nuzu: ops")
}

pub fn root_cancel_team(st: &mut State, now: Ms, ev: &Evidence, team: &str) -> Result<Outcome> {
    let _ = (st, now, ev, team);
    todo!("nuzu: ops")
}

/// Re-point a member at a new alias (pending until that session's hook confirms).
pub fn root_rebind(
    st: &mut State,
    now: Ms,
    ev: &Evidence,
    member: &str,
    new_alias: &str,
) -> Result<Outcome> {
    let _ = (st, now, ev, member, new_alias);
    todo!("nuzu: ops")
}

// ----- binding lifecycle (hook / observers) -----

/// Called from the target session's hook: confirm a pending binding when the
/// alias pager reports for `session_id` matches. Returns the member confirmed.
pub fn confirm_binding(
    st: &mut State,
    now: Ms,
    session_id: &str,
    tool: Tool,
    reported_alias: &str,
) -> Result<Option<String>> {
    let _ = (st, now, session_id, tool, reported_alias);
    todo!("nuzu: ops")
}

/// Mark a member vacant (`pager_gone`, `alias_moved`, ...); releases its
/// exclusive leases.
pub fn mark_vacant(st: &mut State, now: Ms, member: &str, reason: &str) -> Result<()> {
    let _ = (st, now, member, reason);
    todo!("nuzu: ops")
}

// ----- organization (by members holding verbs) -----

/// 개부: open a bu below the actor's bu, headed by `head` (a member below the
/// actor), and delegate `verbs`/`scope` to the head (attenuated). The bu
/// name defaults to the head's title + 부.
pub fn bu_open(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    head: &str,
    name: Option<&str>,
    verbs: &[Verb],
    scope: &[String],
) -> Result<Outcome> {
    let _ = (st, now, actor, head, name, verbs, scope);
    todo!("nuzu: ops")
}

/// Close a bu with no open duties or leases left.
pub fn bu_close(st: &mut State, now: Ms, actor: &Actor, bu: &str) -> Result<Outcome> {
    let _ = (st, now, actor, bu);
    todo!("nuzu: ops")
}

/// Sticky cancel of a bu and everything below (verb `cancel`).
pub fn bu_cancel(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    bu: &str,
    reason: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, bu, reason);
    todo!("nuzu: ops")
}

/// Recruit `alias` into `bu` (verb `open` over that bu) as a pending member.
pub fn member_add(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    bu: &str,
    alias: &str,
    title: Option<&str>,
) -> Result<Outcome> {
    let _ = (st, now, actor, bu, alias, title);
    todo!("nuzu: ops")
}

/// Move a member to another bu below the actor (verb `reassign`).
pub fn member_move(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    member: &str,
    bu: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, member, bu);
    todo!("nuzu: ops")
}

/// Re-point a member below the actor at a new alias (verb `reassign`).
pub fn member_rebind(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    member: &str,
    new_alias: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, member, new_alias);
    todo!("nuzu: ops")
}

/// Change a display title (commander, or superior of the member).
pub fn title_set(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    member: &str,
    title: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, member, title);
    todo!("nuzu: ops")
}

/// Delegate `verbs`/`scope` to a member below (attenuated: must be within the
/// actor's own effective grants; `open` only if the actor holds it).
pub fn grant(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    grantee: &str,
    verbs: &[Verb],
    scope: &[String],
) -> Result<Outcome> {
    let _ = (st, now, actor, grantee, verbs, scope);
    todo!("nuzu: ops")
}

pub fn revoke(st: &mut State, now: Ms, actor: &Actor, grant: &str) -> Result<Outcome> {
    let _ = (st, now, actor, grant);
    todo!("nuzu: ops")
}

// ----- duties (소임) -----

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DutySpec {
    pub intent: String,
    pub outcome: String,
    pub scope: Vec<String>,
    pub decision_rights: String,
    pub escalate_on: Vec<String>,
    pub done_criteria: String,
    pub plan_ref: Option<String>,
    pub exec_root: Option<String>,
}

/// Issue a duty to a member below (verb `assign`, scope within grant).
pub fn duty_issue(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    assignee: &str,
    spec: DutySpec,
) -> Result<Outcome> {
    let _ = (st, now, actor, assignee, spec);
    todo!("nuzu: ops")
}

/// Assignee accepts (`accept = true`) or declines with a reason.
pub fn duty_respond(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    duty: &str,
    version: Option<u64>,
    accept: bool,
    note: Option<&str>,
) -> Result<Outcome> {
    let _ = (st, now, actor, duty, version, accept, note);
    todo!("nuzu: ops")
}

/// Assignee marks blocked (with note) or unblocked, or declares done.
pub fn duty_progress(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    duty: &str,
    status: crate::model::DutyStatus,
    note: Option<&str>,
) -> Result<Outcome> {
    let _ = (st, now, actor, duty, status, note);
    todo!("nuzu: ops")
}

/// Issuer or a holder of `amend` above changes a duty (bumps version).
pub fn duty_amend(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    duty: &str,
    version: u64,
    spec: DutySpec,
) -> Result<Outcome> {
    let _ = (st, now, actor, duty, version, spec);
    todo!("nuzu: ops")
}

/// Cancel a duty (verb `cancel` over its assignee).
pub fn duty_cancel(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    duty: &str,
    reason: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, duty, reason);
    todo!("nuzu: ops")
}

// ----- leases -----

/// Acquire a lease; the key must sit inside one of the actor's accepted
/// duty scopes. `ttl` capped at 8 h. Refuses `leased_by` on conflict.
pub fn lease_acquire(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    key: &str,
    mode: LeaseMode,
    ttl_ms: Option<Ms>,
    reason: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, key, mode, ttl_ms, reason);
    todo!("nuzu: ops")
}

pub fn lease_renew(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    lease: &str,
    ttl_ms: Option<Ms>,
) -> Result<Outcome> {
    let _ = (st, now, actor, lease, ttl_ms);
    todo!("nuzu: ops")
}

pub fn lease_release(st: &mut State, now: Ms, actor: &Actor, lease: &str) -> Result<Outcome> {
    let _ = (st, now, actor, lease);
    todo!("nuzu: ops")
}

/// Force release by a holder of `arbitrate` above the lease holder.
pub fn lease_force_release(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    lease: &str,
    reason: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, lease, reason);
    todo!("nuzu: ops")
}

/// Drop expired leases (called at the start of every transaction).
pub fn expire_leases(st: &mut State, now: Ms) {
    let _ = (st, now);
    todo!("nuzu: ops")
}

// ----- escalations and records -----

/// Raise to the superior; if vacant, the nearest live ancestor; if none,
/// the user's queue (`to = None`).
pub fn escalate(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    duty: Option<&str>,
    question: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, duty, question);
    todo!("nuzu: ops")
}

pub fn escalation_answer(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    escalation: &str,
    answer: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, escalation, answer);
    todo!("nuzu: ops")
}

pub fn record_add(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    kind: RecordKind,
    refs: &[String],
    text: &str,
) -> Result<Outcome> {
    let _ = (st, now, actor, kind, refs, text);
    todo!("nuzu: ops")
}

// ----- dev fixture -----

/// Seed spec for `gaebu dev seed` (non-default home only).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedSpec {
    pub team: String,
    pub project: String,
    pub commander_alias: String,
    pub commander_session: Option<String>,
    pub commander_tool: Option<Tool>,
    /// Members as (alias, session id, tool); bound immediately when a session id is given.
    pub members: Vec<(String, Option<String>, Option<Tool>)>,
}

/// Build a fixture team with `Attest::Fixture` evidence. The caller (CLI)
/// must have refused on the default home already.
pub fn dev_seed(st: &mut State, now: Ms, spec: &SeedSpec) -> Result<Outcome> {
    let _ = (st, now, spec);
    todo!("nuzu: ops")
}
