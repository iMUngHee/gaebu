//! Authority checks (Decisions 5, 6). Grant validity is computed at use time,
//! never stored: a grant counts only while it is active, its grantor is not
//! dismissed, the grantor's own authorizing grant is (recursively) valid and
//! the team is not cancelled.

use crate::error::{Reason, Result, refuse};
use crate::model::{
    Actor, BindingState, BuStatus, Grant, GrantStatus, Member, MemberStatus, State, TeamStatus,
    Verb,
};
use std::collections::BTreeSet;

/// Grants currently valid for `member` (computed lineage).
pub fn effective_grants<'a>(st: &'a State, member: &str) -> Vec<&'a Grant> {
    st.grants
        .values()
        .filter(|g| g.grantee == member && valid_grant(st, g, &mut BTreeSet::new()))
        .collect()
}

/// Check `member` holds `verb` over every pattern in `scope`.
/// Refuses `no_grant` or `out_of_scope`.
pub fn require(st: &State, member: &str, verb: Verb, scope: &[String]) -> Result<()> {
    let scope = crate::scope::normalize_list(scope)?;
    let grants: Vec<_> = effective_grants(st, member)
        .into_iter()
        .filter(|g| g.verbs.contains(&verb))
        .collect();
    if grants.is_empty() {
        return refuse(Reason::NoGrant, "verb not granted");
    }
    let parent: Vec<_> = grants
        .iter()
        .flat_map(|g| g.scope.iter().cloned())
        .collect();
    if !crate::scope::within(&scope, &parent)? {
        return refuse(Reason::OutOfScope, "scope exceeds grant");
    }
    Ok(())
}

/// True when `member` sits below `superior` in the command tree.
pub fn is_below(st: &State, superior: &str, member: &str) -> bool {
    let Some(target) = st.members.get(member) else {
        return false;
    };
    let Some(parent) = st.members.get(superior) else {
        return false;
    };
    if member == superior || target.team != parent.team {
        return false;
    }
    let mut seen = BTreeSet::from([member.to_owned()]);
    let mut next = target.superior.as_deref();
    while let Some(id) = next {
        if !seen.insert(id.to_owned()) {
            return false;
        }
        if id == superior {
            return true;
        }
        next = st
            .members
            .get(id)
            .filter(|m| m.team == target.team)
            .and_then(|m| m.superior.as_deref());
    }
    false
}

/// True when the bu or any ancestor bu, or the team, is cancelled.
pub fn bu_cancelled(st: &State, bu: &str) -> bool {
    let mut next = Some(bu);
    let mut seen = BTreeSet::new();
    while let Some(id) = next {
        if !seen.insert(id) {
            return true;
        }
        let Some(b) = st.bus.get(id) else { return true };
        if b.status != BuStatus::Open
            || st
                .teams
                .get(&b.team)
                .is_none_or(|t| t.status != TeamStatus::Active)
        {
            return true;
        }
        next = b.parent.as_deref();
    }
    false
}

/// Resolve the acting member and check it may act now: active member, bound
/// binding whose session matches, team active. Refuses `not_member`,
/// `pending`, `stale_binding` or `cancelled`.
pub fn check_actor<'a>(st: &'a State, actor: &Actor) -> Result<&'a Member> {
    let Some(member) = st
        .members
        .get(&actor.member)
        .filter(|m| m.status == MemberStatus::Active)
    else {
        return refuse(Reason::NotMember, "member is absent or dismissed");
    };
    if bu_cancelled(st, &member.bu) {
        return refuse(
            Reason::Cancelled,
            "team or ancestor bu is cancelled or closed",
        );
    }
    match member.binding.state {
        BindingState::Pending => refuse(Reason::Pending, "target hook must confirm binding"),
        BindingState::Vacant => refuse(Reason::StaleBinding, "member binding is vacant"),
        BindingState::Bound if member.binding.session_id.as_deref() != Some(&actor.session_id) => {
            refuse(Reason::StaleBinding, "session does not match binding")
        }
        BindingState::Bound => Ok(member),
    }
}

/// The member bound to `session_id`, if any (any binding state).
pub fn member_for_session<'a>(st: &'a State, session_id: &str) -> Option<&'a Member> {
    st.members
        .values()
        .find(|m| m.binding.session_id.as_deref() == Some(session_id))
}

fn valid_grant(st: &State, g: &Grant, seen: &mut BTreeSet<String>) -> bool {
    if g.verbs.is_empty() || g.status != GrantStatus::Active || !seen.insert(g.id.clone()) {
        return false;
    }
    let result = (|| {
        let Some(m) = st.members.get(&g.grantee) else {
            return false;
        };
        let Some(team) = st.teams.get(&g.team) else {
            return false;
        };
        if team.status != TeamStatus::Active
            || m.team != g.team
            || m.status != MemberStatus::Active
            || bu_cancelled(st, &m.bu)
        {
            return false;
        }
        let Some(issuer) = &g.grantor else {
            return team.commander == g.grantee && g.evidence.is_some();
        };
        // A valid authorizing grant below also validates its grantee (the issuer).
        if !is_below(st, issuer, &g.grantee) {
            return false;
        }
        let parents: Vec<_> = st
            .grants
            .values()
            .filter(|p| &p.grantee == issuer && valid_grant(st, p, seen))
            .collect();
        g.verbs.iter().all(|v| {
            let scopes: Vec<_> = parents
                .iter()
                .filter(|p| p.verbs.contains(v))
                .flat_map(|p| p.scope.iter().cloned())
                .collect();
            parents.iter().any(|p| p.verbs.contains(v))
                && crate::scope::within(&g.scope, &scopes).unwrap_or(false)
        })
    })();
    seen.remove(&g.id);
    result
}
