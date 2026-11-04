//! Domain operations. Each runs on `&mut State` inside one `Store::transact`,
//! so every check and write is serialized (Decision 12). Notices for other
//! members are pushed to `st.outbox`; `cli`/`mcp` flush them after commit.
//!
//! Root directives take an `Evidence` already verified by `evidence`.

use crate::error::{Error, Reason, Result, refuse};
use crate::model::*;
use crate::{authz, scope, titles};
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
/// grant (all verbs, scopes `**` and `res:**`). Idempotent on
/// (session id, prompt sequence, action, target).
pub fn root_appoint(
    st: &mut State,
    now: Ms,
    ev: &Evidence,
    alias: &str,
    team: Option<&str>,
    project: &str,
    title: Option<&str>,
) -> Result<Outcome> {
    let key = root_key(ev, "appoint", alias);
    if let Some(action) = st.root_actions.get(&key) {
        return Ok(out(
            "already appointed",
            vec![
                st.teams[&action.team].commander.clone(),
                action.team.clone(),
            ],
        ));
    }
    if alias.trim().is_empty()
        || st
            .members
            .values()
            .any(|m| m.status == MemberStatus::Active && m.binding.alias == alias)
    {
        return refuse(Reason::Conflict, "alias already assigned or empty");
    }
    let name = team.unwrap_or(alias);
    if st.teams.values().any(|t| t.name == name) {
        return refuse(Reason::Conflict, "team name already exists");
    }
    let tid = st.mint("t");
    let bid = st.mint("b");
    let mid = st.mint("m");
    let gid = st.mint("g");
    let title = title
        .map(str::to_owned)
        .unwrap_or_else(|| titles::pick(titles::Seat::Commander, &[]));
    st.teams.insert(
        tid.clone(),
        Team {
            id: tid.clone(),
            name: name.into(),
            project: project.into(),
            commander: mid.clone(),
            root_bu: bid.clone(),
            status: TeamStatus::Active,
            created_at: now,
            cancelled_at: None,
        },
    );
    st.bus.insert(
        bid.clone(),
        Bu {
            id: bid.clone(),
            team: tid.clone(),
            parent: None,
            name: titles::bu_name(&title),
            head: mid.clone(),
            status: BuStatus::Open,
            created_at: now,
            hold: None,
        },
    );
    st.members.insert(
        mid.clone(),
        Member {
            id: mid.clone(),
            team: tid.clone(),
            bu: bid,
            superior: None,
            title,
            status: MemberStatus::Active,
            binding: pending(alias, now),
            created_at: now,
        },
    );
    st.grants.insert(
        gid.clone(),
        Grant {
            id: gid,
            team: tid.clone(),
            grantor: None,
            grantee: mid.clone(),
            verbs: Verb::ALL.into_iter().collect(),
            scope: vec!["**".into(), "res:**".into()],
            status: GrantStatus::Active,
            evidence: Some(ev.clone()),
            created_at: now,
        },
    );
    root_done(st, now, ev, key, &tid);
    changed(st, now, &mid, std::slice::from_ref(&mid), "root appointed");
    Ok(out("appointed; awaiting target hook", vec![mid, tid]))
}

/// Put `aliases` under the commander held by `to_alias`, as 속관 of its root bu.
pub fn root_assign(
    st: &mut State,
    now: Ms,
    ev: &Evidence,
    aliases: &[String],
    to_alias: &str,
) -> Result<Outcome> {
    let commander = alias_member(st, to_alias)?.clone();
    if st.teams[&commander.team].commander != commander.id || authz::bu_cancelled(st, &commander.bu)
    {
        return refuse(Reason::NoGrant, "target is not an active commander");
    }
    let mut ids = Vec::new();
    for alias in aliases {
        let key = root_key(ev, "assign", alias);
        if st.root_actions.contains_key(&key) {
            continue;
        }
        if alias == to_alias {
            return refuse(Reason::Conflict, "superior cycle");
        }
        if st
            .members
            .values()
            .any(|m| m.status == MemberStatus::Active && m.binding.alias == *alias)
        {
            return refuse(
                Reason::Conflict,
                "alias already assigned; use move or rebind",
            );
        }
        let id = add_member(
            st,
            now,
            &commander.team,
            &commander.bu,
            &commander.id,
            alias,
            None,
        );
        root_done(st, now, ev, key, &commander.team);
        changed(
            st,
            now,
            &commander.id,
            &[id.clone(), commander.id.clone()],
            &format!("member {id} ({alias}) assigned; confirmation pending"),
        );
        ids.push(id);
    }
    Ok(out("assigned; awaiting target hooks", ids))
}

/// Dismiss the member held by `alias`; revokes everything delegated below it.
/// The evidence prompt must be newer than the member's appointment.
pub fn root_dismiss(st: &mut State, now: Ms, ev: &Evidence, alias: &str) -> Result<Outcome> {
    let key = root_key(ev, "dismiss", alias);
    if st.root_actions.contains_key(&key) {
        return Ok(out("already dismissed", vec![]));
    }
    let m = alias_member(st, alias)?.clone();
    newer(ev, m.created_at)?;
    st.members.get_mut(&m.id).unwrap().status = MemberStatus::Dismissed;
    let affected: Vec<_> = st
        .members
        .values()
        .filter(|c| c.id == m.id || authz::is_below(st, &m.id, &c.id))
        .map(|m| m.id.clone())
        .collect();
    st.leases.retain(|_, l| !affected.contains(&l.holder));
    for d in st
        .duties
        .values_mut()
        .filter(|d| affected.contains(&d.assignee) && duty_open(d.status))
    {
        d.status = DutyStatus::Cancelled;
        d.version += 1;
        d.updated_at = now;
    }
    root_done(st, now, ev, key, &m.team);
    let author = authz::member_for_session(st, &ev.session_id)
        .filter(|actor| actor.team == m.team)
        .map(|actor| actor.id.clone())
        .unwrap_or_else(|| st.teams[&m.team].commander.clone());
    changed(
        st,
        now,
        &author,
        &affected,
        &format!(
            "member {} ({alias}) dismissed; downstream grants revoked",
            m.id
        ),
    );
    Ok(out("dismissed", vec![m.id]))
}

pub fn root_cancel_team(st: &mut State, now: Ms, ev: &Evidence, team: &str) -> Result<Outcome> {
    let t = st
        .teams
        .values()
        .find(|t| t.id == team || t.name == team)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "team not found"))?;
    let key = root_key(ev, "cancel-team", &t.id);
    if st.root_actions.contains_key(&key) {
        return Ok(out("already cancelled", vec![t.id]));
    }
    let latest = st
        .members
        .values()
        .filter(|m| m.team == t.id)
        .map(|m| m.created_at)
        .max()
        .unwrap_or(t.created_at);
    newer(ev, latest)?;
    let team = st.teams.get_mut(&t.id).unwrap();
    team.status = TeamStatus::Cancelled;
    team.cancelled_at = Some(now);
    let affected = cancel_bu(st, now, &t.root_bu);
    let author = authz::member_for_session(st, &ev.session_id)
        .filter(|actor| actor.team == t.id)
        .map(|m| m.id.clone())
        .unwrap_or_else(|| t.commander.clone());
    changed(
        st,
        now,
        &author,
        &affected,
        &format!("team {} cancelled", t.id),
    );
    root_done(st, now, ev, key, &t.id);
    Ok(out("team cancelled", vec![t.id]))
}

/// Re-point a member at a new alias (pending until that session's hook confirms).
pub fn root_rebind(
    st: &mut State,
    now: Ms,
    ev: &Evidence,
    member: &str,
    new_alias: &str,
) -> Result<Outcome> {
    let m = find_member(st, member)?.clone();
    let key = root_key(ev, "rebind", &m.id);
    if st.root_actions.contains_key(&key) {
        return Ok(out("already rebound", vec![m.id]));
    }
    newer(ev, m.created_at)?;
    let author = authz::member_for_session(st, &ev.session_id)
        .filter(|actor| actor.team == m.team)
        .map(|actor| actor.id.clone())
        .unwrap_or_else(|| st.teams[&m.team].commander.clone());
    rebind(st, now, &author, &m.id, new_alias)?;
    root_done(st, now, ev, key, &m.team);
    Ok(out("rebind pending", vec![m.id]))
}

// ----- binding lifecycle (hook / observers) -----

/// Called from the target session's hook: confirm a pending binding when the
/// alias pager reports for `session_id` matches. Returns the member confirmed.
/// Root directive: hold a duty (`d-…`) or a bu (`b-…`) and everything in it
/// until the human lifts it. The caller's session must belong to the team.
pub fn root_hold(
    st: &mut State,
    now: Ms,
    ev: &Evidence,
    target: &str,
    until: &str,
) -> Result<Outcome> {
    let until = until.trim();
    if until.is_empty() || until.chars().count() > 120 {
        return refuse(Reason::InvalidArgument, "until must be 1-120 characters");
    }
    let key = root_key(ev, "hold", target);
    if st.root_actions.contains_key(&key) {
        return Ok(out("already held", vec![target.into()]));
    }
    let (team, author) = hold_target(st, ev, target)?;
    let hold = crate::model::Hold {
        evidence: ev.clone(),
        until: until.into(),
        at: now,
    };
    let slot = hold_slot(st, target)?;
    if slot.is_some() {
        return refuse(Reason::Conflict, "already held; release it first");
    }
    *slot = Some(hold);
    root_done(st, now, ev, key, &team);
    let affected = held_assignees(st, target);
    changed(
        st,
        now,
        &author,
        &affected,
        &format!("{target} held until: {until}"),
    );
    Ok(out("held", vec![target.into()]))
}

/// Root directive: lift the human hold on a duty or bu. Holds above or below
/// it stay.
pub fn root_release(st: &mut State, now: Ms, ev: &Evidence, target: &str) -> Result<Outcome> {
    let key = root_key(ev, "release", target);
    if st.root_actions.contains_key(&key) {
        return Ok(out("already released", vec![target.into()]));
    }
    let (team, author) = hold_target(st, ev, target)?;
    let Some(old) = hold_slot(st, target)?.take() else {
        return refuse(Reason::NotFound, "no hold on target");
    };
    root_done(st, now, ev, key, &team);
    let affected = held_assignees(st, target);
    changed(
        st,
        now,
        &author,
        &affected,
        &format!(
            "{target} released (held for '{}', released for '{}')",
            old.evidence.quote, ev.quote
        ),
    );
    if let Some(r) = st.records.last_mut() {
        r.kind = RecordKind::Decision;
    }
    Ok(out("released", vec![target.into()]))
}

/// The team of a hold target and the caller's member in it.
fn hold_target(st: &State, ev: &Evidence, target: &str) -> Result<(String, String)> {
    let team = if let Some(d) = st.duties.get(target) {
        d.team.clone()
    } else if let Some(b) = st.bus.get(target) {
        b.team.clone()
    } else {
        return refuse(Reason::NotFound, "hold target must be a duty or bu id");
    };
    let caller = st
        .members
        .values()
        .find(|m| {
            m.team == team
                && m.status == MemberStatus::Active
                && m.binding.state == BindingState::Bound
                && m.binding.session_id.as_deref() == Some(&ev.session_id)
        })
        .ok_or_else(|| Error::new(Reason::NotMember, "caller session is not in that team"))?;
    Ok((team, caller.id.clone()))
}

fn hold_slot<'a>(st: &'a mut State, target: &str) -> Result<&'a mut Option<crate::model::Hold>> {
    if let Some(d) = st.duties.get_mut(target) {
        return Ok(&mut d.hold);
    }
    match st.bus.get_mut(target) {
        Some(b) => Ok(&mut b.hold),
        None => refuse(Reason::NotFound, "hold target must be a duty or bu id"),
    }
}

/// Assignees of open duties the target covers (the duty, or every duty in
/// the bu and the bus below it).
fn held_assignees(st: &State, target: &str) -> Vec<String> {
    let covered = |d: &Duty| {
        if d.id == target {
            return true;
        }
        let mut next = Some(d.bu.as_str());
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = next {
            if id == target {
                return true;
            }
            if !seen.insert(id) {
                return false;
            }
            next = st.bus.get(id).and_then(|b| b.parent.as_deref());
        }
        false
    };
    let mut out: Vec<String> = st
        .duties
        .values()
        .filter(|d| duty_open(d.status) && covered(d))
        .map(|d| d.assignee.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn confirm_binding(
    st: &mut State,
    now: Ms,
    session_id: &str,
    tool: Tool,
    reported_alias: &str,
) -> Result<Option<String>> {
    if st.members.values().any(|m| {
        m.status == MemberStatus::Active
            && m.binding.state == BindingState::Bound
            && m.binding.session_id.as_deref() == Some(session_id)
    }) {
        return Ok(None);
    }
    let id = st
        .members
        .values()
        .find(|m| {
            m.status == MemberStatus::Active
                && m.binding.state == BindingState::Pending
                && m.binding.alias == reported_alias
                && !authz::bu_cancelled(st, &m.bu)
        })
        .map(|m| m.id.clone());
    if let Some(id) = &id {
        let m = st.members.get_mut(id).unwrap();
        m.binding.state = BindingState::Bound;
        m.binding.session_id = Some(session_id.into());
        m.binding.tool = Some(tool);
        m.binding.bound_at = Some(now);
        let superior = m.superior.clone();
        let mut affected = vec![id.clone()];
        affected.extend(superior);
        changed(
            st,
            now,
            id,
            &affected,
            &format!("binding confirmed for {id}"),
        );
    }
    Ok(id)
}

/// Mark a member vacant (`pager_gone`, `alias_moved`, ...); releases its
/// exclusive leases.
pub fn mark_vacant(st: &mut State, now: Ms, member: &str, reason: &str) -> Result<()> {
    let m = st
        .members
        .get_mut(member)
        .ok_or_else(|| Error::new(Reason::NotFound, "member not found"))?;
    m.binding.state = BindingState::Vacant;
    m.binding.vacated_at = Some(now);
    m.binding.vacated_reason = Some(reason.into());
    let superior = m.superior.clone();
    st.leases
        .retain(|_, l| l.holder != member || l.mode != LeaseMode::Exclusive);
    let affected: Vec<_> = superior.into_iter().collect();
    changed(
        st,
        now,
        member,
        &affected,
        &format!("member {member} vacant: {reason}"),
    );
    Ok(())
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
    let a = authz::check_actor(st, actor)?.clone();
    authz::require(st, &a.id, Verb::Open, scope)?;
    let h = find_member(st, head)?.clone();
    below(st, &a, &h)?;
    if st
        .bus
        .values()
        .any(|b| b.head == h.id && b.status == BuStatus::Open)
    {
        return refuse(
            Reason::Conflict,
            "close or hand over headed bu before opening another",
        );
    }
    for verb in verbs {
        authz::require(st, &a.id, *verb, scope)?;
    }
    let scope = scope::normalize_list(scope)?;
    let bid = st.mint("b");
    let title = pick_title(st, &a.team, titles::Seat::Head);
    st.bus.insert(
        bid.clone(),
        Bu {
            id: bid.clone(),
            team: a.team.clone(),
            parent: Some(a.bu.clone()),
            name: name
                .map(str::to_owned)
                .unwrap_or_else(|| titles::bu_name(&title)),
            head: h.id.clone(),
            status: BuStatus::Open,
            created_at: now,
            hold: None,
        },
    );
    let m = st.members.get_mut(&h.id).unwrap();
    m.bu = bid.clone();
    m.superior = Some(a.id.clone());
    m.title = title;
    if !verbs.is_empty() {
        grant(st, now, actor, &h.id, verbs, &scope)?;
    }
    changed(st, now, &a.id, &[h.id], &format!("bu {bid} opened"));
    Ok(out("bu opened", vec![bid]))
}

/// Close a bu with no open duties or leases left.
pub fn bu_close(st: &mut State, now: Ms, actor: &Actor, bu: &str) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let b = st
        .bus
        .get(bu)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "bu not found"))?;
    if b.team != a.team || !authz::is_below(st, &a.id, &b.head) {
        return refuse(Reason::NoGrant, "not a subordinate bu");
    }
    authz::require(st, &a.id, Verb::Open, &[])?;
    if st
        .bus
        .values()
        .any(|child| child.parent.as_deref() == Some(bu) && child.status == BuStatus::Open)
        || st
            .duties
            .values()
            .any(|d| d.bu == bu && duty_open(d.status))
        || st
            .leases
            .values()
            .any(|l| l.expires_at > now && st.members.get(&l.holder).is_some_and(|m| m.bu == bu))
    {
        return refuse(Reason::Conflict, "open duties, leases or child bus remain");
    }
    if b.status == BuStatus::Cancelled {
        return refuse(Reason::Cancelled, "cancel is sticky");
    }
    st.bus.get_mut(bu).unwrap().status = BuStatus::Closed;
    changed(st, now, &a.id, &[b.head], &format!("bu {bu} closed"));
    Ok(out("bu closed", vec![bu.into()]))
}

/// Sticky cancel of a bu and everything below (verb `cancel`).
pub fn bu_cancel(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    bu: &str,
    reason: &str,
) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let b = st
        .bus
        .get(bu)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "bu not found"))?;
    if b.team != a.team || !authz::is_below(st, &a.id, &b.head) {
        return refuse(Reason::NoGrant, "not a subordinate bu");
    }
    let bus = bu_descendants(st, bu);
    let scopes: Vec<_> = st
        .duties
        .values()
        .filter(|d| bus.contains(&d.bu) && duty_open(d.status))
        .flat_map(|d| d.scope.iter().cloned())
        .collect();
    authz::require(st, &a.id, Verb::Cancel, &scopes)?;
    let reason = if reason.trim().is_empty() {
        "사유 없음"
    } else {
        reason
    };
    let affected = cancel_bu(st, now, bu);
    changed(
        st,
        now,
        &a.id,
        &affected,
        &format!("bu {bu} cancelled: {reason}"),
    );
    Ok(out("bu cancelled", vec![bu.into()]))
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
    let a = authz::check_actor(st, actor)?.clone();
    authz::require(st, &a.id, Verb::Open, &[])?;
    let b = st
        .bus
        .get(bu)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "bu not found"))?;
    if b.team != a.team || !(b.head == a.id || authz::is_below(st, &a.id, &b.head)) {
        return refuse(Reason::NoGrant, "not a subordinate bu");
    }
    if authz::bu_cancelled(st, bu) {
        return refuse(Reason::Cancelled, "bu cancelled");
    }
    if st
        .members
        .values()
        .any(|m| m.status == MemberStatus::Active && m.binding.alias == alias)
    {
        return refuse(Reason::Conflict, "alias already assigned");
    }
    let id = add_member(st, now, &a.team, bu, &b.head, alias, title);
    changed(
        st,
        now,
        &a.id,
        &[b.head],
        &format!("member {id} ({alias}) added; confirmation pending"),
    );
    Ok(out("member pending", vec![id]))
}

/// Move a member to another bu below the actor (verb `reassign`).
pub fn member_move(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    member: &str,
    bu: &str,
) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    authz::require(st, &a.id, Verb::Reassign, &[])?;
    let m = find_member(st, member)?.clone();
    let b = st
        .bus
        .get(bu)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "bu not found"))?;
    if b.head == m.id || authz::is_below(st, &m.id, &b.head) {
        return refuse(Reason::Conflict, "superior cycle");
    }
    below(st, &a, &m)?;
    if b.team != a.team || !(b.head == a.id || authz::is_below(st, &a.id, &b.head)) {
        return refuse(Reason::NoGrant, "target bu is outside command");
    }
    if authz::bu_cancelled(st, bu) {
        return refuse(Reason::Cancelled, "target bu cancelled");
    }
    if st
        .bus
        .values()
        .any(|b| b.head == m.id && b.status == BuStatus::Open)
    {
        return refuse(
            Reason::Conflict,
            "close or hand over headed bu before moving",
        );
    }
    let m = st.members.get_mut(&m.id).unwrap();
    m.bu = bu.into();
    m.superior = Some(b.head);
    let id = m.id.clone();
    changed(
        st,
        now,
        &a.id,
        std::slice::from_ref(&id),
        &format!("member {id} moved to bu {bu}"),
    );
    Ok(out("member moved", vec![id]))
}

/// Re-point a member below the actor at a new alias (verb `reassign`).
pub fn member_rebind(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    member: &str,
    new_alias: &str,
) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    authz::require(st, &a.id, Verb::Reassign, &[])?;
    let m = find_member(st, member)?.clone();
    below(st, &a, &m)?;
    rebind(st, now, &a.id, &m.id, new_alias)?;
    Ok(out("rebind pending", vec![m.id]))
}

/// Change a display title (commander, or superior of the member).
pub fn title_set(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    member: &str,
    title: &str,
) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let m = find_member(st, member)?.clone();
    if !(st.teams[&a.team].commander == a.id && m.team == a.team
        || authz::is_below(st, &a.id, &m.id))
    {
        return refuse(
            Reason::NoGrant,
            "only commander or superior may change title",
        );
    }
    if title.trim().is_empty() {
        return refuse(Reason::InvalidArgument, "empty title");
    }
    st.members.get_mut(&m.id).unwrap().title = title.into();
    changed(
        st,
        now,
        &a.id,
        std::slice::from_ref(&m.id),
        &format!("member {} title changed to '{title}'", m.id),
    );
    Ok(out("title changed", vec![m.id]))
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
    let a = authz::check_actor(st, actor)?.clone();
    let m = find_member(st, grantee)?.clone();
    below(st, &a, &m)?;
    if verbs.is_empty() {
        return refuse(Reason::InvalidArgument, "empty grant verbs");
    }
    let scope = scope::normalize_list(scope)?;
    for verb in verbs {
        authz::require(st, &a.id, *verb, &scope)?;
    }
    let id = st.mint("g");
    st.grants.insert(
        id.clone(),
        Grant {
            id: id.clone(),
            team: a.team,
            grantor: Some(a.id.clone()),
            grantee: m.id.clone(),
            verbs: verbs.iter().copied().collect(),
            scope,
            status: GrantStatus::Active,
            evidence: None,
            created_at: now,
        },
    );
    changed(st, now, &a.id, &[m.id], &format!("grant {id} delegated"));
    Ok(out("delegated", vec![id]))
}

pub fn revoke(st: &mut State, now: Ms, actor: &Actor, grant: &str) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let g = st
        .grants
        .get(grant)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "grant not found"))?;
    if g.grantor.is_none() {
        return refuse(
            Reason::NoGrant,
            "root grant can only be removed by a user root directive",
        );
    }
    if g.team != a.team
        || g.grantor.as_deref() != Some(&a.id) && st.teams[&a.team].commander != a.id
    {
        return refuse(Reason::NoGrant, "not grant issuer or commander");
    }
    st.grants.get_mut(grant).unwrap().status = GrantStatus::Revoked;
    changed(
        st,
        now,
        &a.id,
        &[g.grantee],
        &format!("grant {grant} revoked"),
    );
    Ok(out("revoked", vec![grant.into()]))
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
    let a = authz::check_actor(st, actor)?.clone();
    let m = find_member(st, assignee)?.clone();
    below(st, &a, &m)?;
    authz::require(st, &a.id, Verb::Assign, &spec.scope)?;
    let scope = scope::normalize_list(&spec.scope)?;
    let id = st.mint("d");
    st.duties.insert(
        id.clone(),
        Duty {
            id: id.clone(),
            team: a.team,
            bu: m.bu,
            assignee: m.id.clone(),
            issuer: a.id.clone(),
            intent: spec.intent,
            outcome: spec.outcome,
            scope,
            decision_rights: spec.decision_rights,
            escalate_on: spec.escalate_on,
            done_criteria: spec.done_criteria,
            plan_ref: spec.plan_ref,
            exec_root: spec.exec_root,
            status: DutyStatus::Proposed,
            version: 1,
            note: None,
            created_at: now,
            updated_at: now,
            hold: None,
            source: None,
        },
    );
    let message = format!("new duty {id} '{}' proposed", st.duties[&id].outcome);
    changed(st, now, &a.id, &[m.id], &message);
    Ok(out("duty issued", vec![id]))
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
    let a = authz::check_actor(st, actor)?.clone();
    let d = find_duty(st, duty)?.clone();
    if d.assignee != a.id {
        return refuse(Reason::NoGrant, "only assignee may respond");
    }
    if d.status == DutyStatus::Cancelled || authz::bu_cancelled(st, &d.bu) {
        return refuse(Reason::Cancelled, "duty cancelled");
    }
    if version.is_some_and(|v| v != d.version) || d.status != DutyStatus::Proposed {
        return refuse(Reason::Conflict, "duty version or status changed");
    }
    if !accept && note.is_none_or(|n| n.trim().is_empty()) {
        return refuse(Reason::InvalidArgument, "decline requires reason");
    }
    if accept {
        authz::not_held(st, &d)?;
    }
    let d = st.duties.get_mut(duty).unwrap();
    d.status = if accept {
        DutyStatus::Accepted
    } else {
        DutyStatus::Declined
    };
    d.note = note.map(str::to_owned);
    d.version += 1;
    d.updated_at = now;
    let issuer = d.issuer.clone();
    changed(
        st,
        now,
        &a.id,
        &[issuer],
        &format!(
            "duty {duty} {}",
            if accept { "accepted" } else { "declined" }
        ),
    );
    Ok(out("duty response recorded", vec![duty.into()]))
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
    let a = authz::check_actor(st, actor)?.clone();
    let d = find_duty(st, duty)?.clone();
    if d.assignee != a.id {
        return refuse(Reason::NoGrant, "only assignee may declare progress");
    }
    if d.status == DutyStatus::Cancelled || authz::bu_cancelled(st, &d.bu) {
        return refuse(Reason::Cancelled, "duty cancelled");
    }
    if !matches!(d.status, DutyStatus::Accepted | DutyStatus::Blocked)
        || !matches!(
            status,
            DutyStatus::Accepted | DutyStatus::Blocked | DutyStatus::Done | DutyStatus::HandedOff
        )
    {
        return refuse(Reason::Conflict, "invalid duty transition");
    }
    if matches!(status, DutyStatus::Blocked | DutyStatus::HandedOff)
        && note.is_none_or(|n| n.trim().is_empty())
    {
        return refuse(Reason::InvalidArgument, "blocked/handoff requires note");
    }
    authz::not_held(st, &d)?;
    let d = st.duties.get_mut(duty).unwrap();
    d.status = status;
    d.note = note.map(str::to_owned);
    d.version += 1;
    d.updated_at = now;
    let issuer = d.issuer.clone();
    changed(
        st,
        now,
        &a.id,
        &[issuer],
        &format!("duty {duty} progress: {status:?}"),
    );
    Ok(out("duty progress recorded", vec![duty.into()]))
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
    let a = authz::check_actor(st, actor)?.clone();
    let d = find_duty(st, duty)?.clone();
    if a.team != d.team || !(a.id == d.issuer || authz::is_below(st, &a.id, &d.assignee)) {
        return refuse(Reason::NoGrant, "duty outside command");
    }
    if d.status == DutyStatus::Cancelled || authz::bu_cancelled(st, &d.bu) {
        return refuse(Reason::Cancelled, "duty cancelled");
    }
    if version != d.version || !duty_open(d.status) {
        return refuse(Reason::Conflict, "duty version or status changed");
    }
    let scope = scope::normalize_list(&spec.scope)?;
    let mut required = d.scope.clone();
    required.extend(scope.iter().cloned());
    authz::require(st, &a.id, Verb::Amend, &required)?;
    let d = st.duties.get_mut(duty).unwrap();
    d.intent = spec.intent;
    d.outcome = spec.outcome;
    d.scope = scope;
    d.decision_rights = spec.decision_rights;
    d.escalate_on = spec.escalate_on;
    d.done_criteria = spec.done_criteria;
    d.plan_ref = spec.plan_ref;
    d.exec_root = spec.exec_root;
    d.version += 1;
    d.updated_at = now;
    d.status = DutyStatus::Proposed;
    let assignee = d.assignee.clone();
    changed(
        st,
        now,
        &a.id,
        &[assignee],
        &format!("duty {duty} amended; acceptance required"),
    );
    Ok(out("duty amended", vec![duty.into()]))
}

/// Cancel a duty (verb `cancel` over its assignee).
pub fn duty_cancel(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    duty: &str,
    reason: &str,
) -> Result<Outcome> {
    let reason = if reason.trim().is_empty() {
        "사유 없음"
    } else {
        reason
    };
    let a = authz::check_actor(st, actor)?.clone();
    let d = find_duty(st, duty)?.clone();
    let m = find_member(st, &d.assignee)?;
    below(st, &a, m)?;
    authz::require(st, &a.id, Verb::Cancel, &d.scope)?;
    if !duty_open(d.status) {
        return refuse(Reason::Conflict, "duty already terminal");
    }
    let d = st.duties.get_mut(duty).unwrap();
    d.status = DutyStatus::Cancelled;
    d.note = Some(reason.into());
    d.version += 1;
    d.updated_at = now;
    let assignee = d.assignee.clone();
    changed(
        st,
        now,
        &a.id,
        &[assignee],
        &format!("duty {duty} cancelled: {reason}"),
    );
    Ok(out("duty cancelled", vec![duty.into()]))
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
    let a = authz::check_actor(st, actor)?.clone();
    let key = scope::parse(key)?;
    let key_text = key.as_string();
    lease_scope(st, &a.id, &key_text, "lease requires accepted duty scope")?;
    let ttl = ttl(ttl_ms)?;
    expire_leases(st, now);
    for l in st.leases.values().filter(|l| l.team == a.team) {
        if (mode == LeaseMode::Exclusive || l.mode == LeaseMode::Exclusive)
            && scope::parse(&l.key)?.overlaps(&key)
        {
            return refuse(
                Reason::LeasedBy,
                format!("{} holds {} ({})", l.holder, l.key, l.id),
            );
        }
    }
    let id = st.mint("l");
    st.leases.insert(
        id.clone(),
        Lease {
            id: id.clone(),
            team: a.team,
            key: key_text,
            mode,
            holder: a.id.clone(),
            holder_session: a.binding.session_id.unwrap(),
            reason: reason.into(),
            acquired_at: now,
            expires_at: now.saturating_add(ttl),
        },
    );
    changed(st, now, &a.id, &[], "lease acquired");
    Ok(out("lease acquired", vec![id]))
}

/// A lease key must sit inside an accepted duty of `member` that no hold
/// covers. Inside only held duties it is refused `held`; elsewhere
/// `out_of_scope`.
fn lease_scope(st: &State, member: &str, key: &str, missing: &str) -> Result<()> {
    let accepted: Vec<_> = st
        .duties
        .values()
        .filter(|d| {
            d.assignee == member
                && d.status == DutyStatus::Accepted
                && !authz::bu_cancelled(st, &d.bu)
        })
        .collect();
    let (held, free): (Vec<_>, Vec<_>) = accepted
        .into_iter()
        .partition(|d| !authz::holds(st, d).is_empty());
    let scopes =
        |ds: &[&Duty]| -> Vec<String> { ds.iter().flat_map(|d| d.scope.iter().cloned()).collect() };
    let key = std::slice::from_ref(&key.to_owned()).to_vec();
    if scope::within(&key, &scopes(&free))? {
        return Ok(());
    }
    for d in &held {
        if scope::within(&key, &d.scope)? {
            return authz::not_held(st, d);
        }
    }
    if scope::within(&key, &scopes(&held))? {
        return refuse(Reason::Held, "lease scope lies in held duties");
    }
    refuse(Reason::OutOfScope, missing)
}

pub fn lease_renew(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    lease: &str,
    ttl_ms: Option<Ms>,
) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let ttl = ttl(ttl_ms)?;
    let l = st
        .leases
        .get(lease)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "lease not found"))?;
    if l.holder != a.id || l.holder_session != actor.session_id {
        return refuse(Reason::NoGrant, "not lease holder");
    }
    if l.expires_at <= now {
        return refuse(Reason::Conflict, "lease expired");
    }
    lease_scope(st, &a.id, &l.key, "accepted duty no longer covers lease")?;
    st.leases.get_mut(lease).unwrap().expires_at = now.saturating_add(ttl);
    changed(st, now, &a.id, &[], "lease renewed");
    Ok(out("lease renewed", vec![lease.into()]))
}

pub fn lease_release(st: &mut State, now: Ms, actor: &Actor, lease: &str) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let l = st
        .leases
        .get(lease)
        .ok_or_else(|| Error::new(Reason::NotFound, "lease not found"))?;
    if l.holder != a.id || l.holder_session != actor.session_id {
        return refuse(Reason::NoGrant, "not lease holder");
    }
    st.leases.remove(lease);
    changed(st, now, &a.id, &[], "lease released");
    Ok(out("lease released", vec![lease.into()]))
}

/// Force release by a holder of `arbitrate` above the lease holder.
pub fn lease_force_release(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    lease: &str,
    reason: &str,
) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let l = st
        .leases
        .get(lease)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "lease not found"))?;
    authz::require(st, &a.id, Verb::Arbitrate, std::slice::from_ref(&l.key))?;
    below(st, &a, find_member(st, &l.holder)?)?;
    st.leases.remove(lease);
    changed(
        st,
        now,
        &a.id,
        &[l.holder],
        &format!("lease {lease} force released: {reason}"),
    );
    Ok(out("lease force released", vec![lease.into()]))
}

/// Drop expired leases (called at the start of every transaction).
pub fn expire_leases(st: &mut State, now: Ms) {
    st.leases.retain(|_, lease| {
        lease.expires_at > now
            && st.members.get(&lease.holder).is_some_and(|m| {
                m.binding.state == crate::model::BindingState::Bound
                    && m.binding.session_id.as_deref() == Some(&lease.holder_session)
                    && m.status == crate::model::MemberStatus::Active
            })
    });
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
    let a = authz::check_actor(st, actor)?.clone();
    if let Some(duty) = duty {
        let d = find_duty(st, duty)?;
        if d.assignee != a.id {
            return refuse(Reason::NoGrant, "not duty assignee");
        }
    }
    if question.trim().is_empty() {
        return refuse(Reason::InvalidArgument, "empty escalation");
    }
    let to = live_superior(st, &a.id);
    let id = st.mint("e");
    st.escalations.insert(
        id.clone(),
        Escalation {
            id: id.clone(),
            team: a.team,
            from: a.id.clone(),
            to: to.clone(),
            duty: duty.map(str::to_owned),
            question: question.into(),
            status: EscalationStatus::Open,
            answer: None,
            created_at: now,
            answered_at: None,
            superseded_by: None,
        },
    );
    changed(
        st,
        now,
        &a.id,
        &to.into_iter().collect::<Vec<_>>(),
        &format!("escalation {id} raised: {question}"),
    );
    Ok(out("escalation raised", vec![id]))
}

/// The nearest superior of `member` that can answer: active, bound and not in
/// a cancelled bu. `None` is the user's queue.
fn live_superior(st: &State, member: &str) -> Option<String> {
    let mut to = st.members.get(member).and_then(|m| m.superior.clone());
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = &to {
        if !seen.insert(id.clone()) {
            return None;
        }
        let m = st.members.get(id)?;
        if live_recipient(st, m) {
            return to;
        }
        to = m.superior.clone();
    }
    None
}

fn live_recipient(st: &State, m: &crate::model::Member) -> bool {
    m.status == MemberStatus::Active
        && m.binding.state == BindingState::Bound
        && !authz::bu_cancelled(st, &m.bu)
}

/// Run after every committed change: an open escalation whose recipient can
/// no longer answer moves to the asker's nearest live superior (or the
/// user's queue), so a vacancy, dismissal, cancel or rebind never strands it.
pub fn reroute_escalations(st: &mut State, now: Ms) {
    let stranded: Vec<(String, String, Option<String>)> = st
        .escalations
        .values()
        .filter(|e| e.status == EscalationStatus::Open)
        .filter_map(|e| {
            let to = e.to.as_ref()?;
            let live = st.members.get(to).is_some_and(|m| live_recipient(st, m));
            (!live).then(|| (e.id.clone(), e.from.clone(), live_superior(st, &e.from)))
        })
        .collect();
    for (id, from, to) in stranded {
        let e = st.escalations.get_mut(&id).unwrap();
        e.to = to.clone();
        let question = e.question.clone();
        changed(
            st,
            now,
            &from,
            &to.into_iter().collect::<Vec<_>>(),
            &format!("escalation {id} rerouted: {question}"),
        );
    }
}

pub fn escalation_answer(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    escalation: &str,
    answer: &str,
) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let e = st
        .escalations
        .get(escalation)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "escalation not found"))?;
    if e.to.as_deref() != Some(&a.id) || e.team != a.team {
        return refuse(Reason::NoGrant, "not escalation recipient");
    }
    if e.status != EscalationStatus::Open {
        return refuse(Reason::Conflict, "already answered");
    }
    let e = st.escalations.get_mut(escalation).unwrap();
    e.status = EscalationStatus::Answered;
    e.answer = Some(answer.into());
    e.answered_at = Some(now);
    let from = e.from.clone();
    changed(
        st,
        now,
        &a.id,
        &[from],
        &format!("escalation {escalation} answered: {answer}"),
    );
    Ok(out("escalation answered", vec![escalation.into()]))
}

pub fn record_add(
    st: &mut State,
    now: Ms,
    actor: &Actor,
    kind: RecordKind,
    refs: &[String],
    text: &str,
) -> Result<Outcome> {
    let a = authz::check_actor(st, actor)?.clone();
    let id = st.mint("r");
    st.records.push(Record {
        id: id.clone(),
        team: a.team,
        kind,
        author: a.id,
        refs: refs.to_vec(),
        text: text.into(),
        at: now,
    });
    Ok(out("record added", vec![id]))
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
    let ev = Evidence {
        session_id: "fixture".into(),
        prompt_seq: now,
        sha256: format!("fixture-{}-{now}", spec.team),
        attest: Attest::Fixture,
        quote: "development fixture".into(),
        at: now,
        strength: crate::model::Strength::Explicit,
    };
    let appointed = root_appoint(
        st,
        now,
        &ev,
        &spec.commander_alias,
        Some(&spec.team),
        &spec.project,
        None,
    )?;
    if let Some(session) = &spec.commander_session {
        confirm_binding(
            st,
            now,
            session,
            spec.commander_tool.unwrap_or(Tool::Codex),
            &spec.commander_alias,
        )?;
    }
    let aliases: Vec<_> = spec.members.iter().map(|(a, _, _)| a.clone()).collect();
    root_assign(st, now, &ev, &aliases, &spec.commander_alias)?;
    for (alias, session, tool) in &spec.members {
        if let Some(session) = session {
            confirm_binding(st, now, session, tool.unwrap_or(Tool::Codex), alias)?;
        }
    }
    Ok(appointed)
}

fn out(summary: &str, ids: Vec<String>) -> Outcome {
    Outcome {
        summary: summary.into(),
        ids,
    }
}
fn root_key(ev: &Evidence, action: &str, target: &str) -> String {
    format!("{}:{}:{action}:{target}", ev.session_id, ev.prompt_seq)
}
fn root_done(st: &mut State, now: Ms, ev: &Evidence, key: String, team: &str) {
    st.root_actions.insert(
        key,
        RootAction {
            team: team.into(),
            rev: st.rev.saturating_add(1),
            at: now,
            evidence: Some(ev.clone()),
        },
    );
}
fn newer(ev: &Evidence, at: Ms) -> Result<()> {
    if ev.at <= at {
        refuse(
            Reason::NoUserEvidence,
            "evidence must be recorded after designation",
        )
    } else {
        Ok(())
    }
}
fn pending(alias: &str, now: Ms) -> Binding {
    Binding {
        state: BindingState::Pending,
        alias: alias.into(),
        tool: None,
        session_id: None,
        designated_at: now,
        bound_at: None,
        vacated_at: None,
        vacated_reason: None,
    }
}
fn find_member<'a>(st: &'a State, id: &str) -> Result<&'a Member> {
    st.members
        .get(id)
        .or_else(|| {
            st.members
                .values()
                .find(|m| m.binding.alias == id && m.status == MemberStatus::Active)
        })
        .ok_or_else(|| Error::new(Reason::NotFound, "member not found"))
}
fn alias_member<'a>(st: &'a State, alias: &str) -> Result<&'a Member> {
    st.members
        .values()
        .find(|m| m.binding.alias == alias && m.status == MemberStatus::Active)
        .ok_or_else(|| Error::new(Reason::NotFound, "alias not assigned"))
}
fn find_duty<'a>(st: &'a State, id: &str) -> Result<&'a Duty> {
    st.duties
        .get(id)
        .ok_or_else(|| Error::new(Reason::NotFound, "duty not found"))
}
fn below(st: &State, a: &Member, m: &Member) -> Result<()> {
    if a.team != m.team || !authz::is_below(st, &a.id, &m.id) {
        return refuse(Reason::NoGrant, "target is outside command subtree");
    }
    if m.status != MemberStatus::Active {
        return refuse(Reason::NotMember, "target dismissed");
    }
    if authz::bu_cancelled(st, &m.bu) {
        return refuse(Reason::Cancelled, "target bu cancelled");
    }
    Ok(())
}
fn pick_title(st: &State, team: &str, seat: titles::Seat) -> String {
    let taken: Vec<_> = st
        .members
        .values()
        .filter(|m| m.team == team)
        .map(|m| m.title.as_str())
        .collect();
    titles::pick(seat, &taken)
}
fn add_member(
    st: &mut State,
    now: Ms,
    team: &str,
    bu: &str,
    superior: &str,
    alias: &str,
    title: Option<&str>,
) -> String {
    let id = st.mint("m");
    let title = title
        .map(str::to_owned)
        .unwrap_or_else(|| pick_title(st, team, titles::Seat::Staff));
    st.members.insert(
        id.clone(),
        Member {
            id: id.clone(),
            team: team.into(),
            bu: bu.into(),
            superior: Some(superior.into()),
            title,
            status: MemberStatus::Active,
            binding: pending(alias, now),
            created_at: now,
        },
    );
    id
}
fn rebind(st: &mut State, now: Ms, author: &str, member: &str, alias: &str) -> Result<()> {
    if st
        .members
        .values()
        .any(|m| m.id != member && m.status == MemberStatus::Active && m.binding.alias == alias)
    {
        return refuse(Reason::Conflict, "alias already assigned");
    }
    let mut affected = vec![member.to_owned()];
    affected.extend(st.members[member].superior.clone());
    st.leases.retain(|_, l| l.holder != member);
    st.members.get_mut(member).unwrap().binding = pending(alias, now);
    changed(
        st,
        now,
        author,
        &affected,
        &format!("member {member} binding replaced; awaiting confirmation as {alias}"),
    );
    Ok(())
}
fn duty_open(s: DutyStatus) -> bool {
    matches!(
        s,
        DutyStatus::Proposed | DutyStatus::Accepted | DutyStatus::Blocked
    )
}
fn ttl(ms: Option<Ms>) -> Result<Ms> {
    let ms = ms.unwrap_or(30 * 60 * 1000);
    if ms == 0 {
        return refuse(Reason::InvalidArgument, "TTL must be positive");
    }
    Ok(ms.min(8 * 60 * 60 * 1000))
}
fn bu_descendants(st: &State, bu: &str) -> std::collections::BTreeSet<String> {
    let mut bus = std::collections::BTreeSet::from([bu.to_owned()]);
    loop {
        let n = bus.len();
        for b in st.bus.values() {
            if b.parent.as_ref().is_some_and(|p| bus.contains(p)) {
                bus.insert(b.id.clone());
            }
        }
        if n == bus.len() {
            break;
        }
    }
    bus
}
fn cancel_bu(st: &mut State, now: Ms, bu: &str) -> Vec<String> {
    let bus = bu_descendants(st, bu);
    for b in st.bus.values_mut().filter(|b| bus.contains(&b.id)) {
        b.status = BuStatus::Cancelled;
    }
    for d in st
        .duties
        .values_mut()
        .filter(|d| bus.contains(&d.bu) && duty_open(d.status))
    {
        d.status = DutyStatus::Cancelled;
        d.version += 1;
        d.updated_at = now;
    }
    let members: Vec<_> = st
        .members
        .values()
        .filter(|m| bus.contains(&m.bu))
        .map(|m| m.id.clone())
        .collect();
    st.leases.retain(|_, l| !members.contains(&l.holder));
    members
}
fn changed(st: &mut State, now: Ms, author: &str, affected: &[String], text: &str) {
    let Some(actor) = st.members.get(author) else {
        return;
    };
    let team = actor.team.clone();
    let actor_session = actor.binding.session_id.clone();
    let actor_name = crate::card::display(st, actor);
    let team_name = st
        .teams
        .get(&team)
        .map(|t| t.name.clone())
        .unwrap_or_else(|| team.clone());
    let id = st.mint("r");
    let body = format!("[gaebu {team_name}] {actor_name}: {text} — {id} — gaebu me");
    st.records.push(Record {
        id,
        team,
        kind: RecordKind::Change,
        author: author.into(),
        refs: affected.to_vec(),
        text: text.into(),
        at: now,
    });
    for member in affected {
        let session = st
            .members
            .get(member)
            .and_then(|m| m.binding.session_id.clone());
        if let Some(session) = session {
            if actor_session.as_deref() == Some(&session) {
                continue;
            }
            let id = st.mint("n");
            st.outbox.push(Notice {
                id,
                to_member: member.clone(),
                to_session: session,
                body: body.clone(),
                text: crate::model::notice_key(&body),
                created_at: now,
                attempts: 0,
                claimed_at: None,
            });
        }
    }
}
