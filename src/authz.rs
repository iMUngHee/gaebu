//! Authority checks (Decisions 5, 6). Grant validity is computed at use time,
//! never stored: a grant counts only while it is active, its grantor is not
//! dismissed, the grantor's own authorizing grant is (recursively) valid and
//! the team is not cancelled.

use crate::error::Result;
use crate::model::{Actor, Grant, Member, State, Verb};

/// Grants currently valid for `member` (computed lineage).
pub fn effective_grants<'a>(st: &'a State, member: &str) -> Vec<&'a Grant> {
    let _ = (st, member);
    todo!("nuzu: authz")
}

/// Check `member` holds `verb` over every pattern in `scope`.
/// Refuses `no_grant` or `out_of_scope`.
pub fn require(st: &State, member: &str, verb: Verb, scope: &[String]) -> Result<()> {
    let _ = (st, member, verb, scope);
    todo!("nuzu: authz")
}

/// True when `member` sits below `superior` in the command tree.
pub fn is_below(st: &State, superior: &str, member: &str) -> bool {
    let _ = (st, superior, member);
    todo!("nuzu: authz")
}

/// True when the bu or any ancestor bu, or the team, is cancelled.
pub fn bu_cancelled(st: &State, bu: &str) -> bool {
    let _ = (st, bu);
    todo!("nuzu: authz")
}

/// Resolve the acting member and check it may act now: active member, bound
/// binding whose session matches, team active. Refuses `not_member`,
/// `pending`, `stale_binding` or `cancelled`.
pub fn check_actor<'a>(st: &'a State, actor: &Actor) -> Result<&'a Member> {
    let _ = (st, actor);
    todo!("nuzu: authz")
}

/// The member bound to `session_id`, if any (any binding state).
pub fn member_for_session<'a>(st: &'a State, session_id: &str) -> Option<&'a Member> {
    let _ = (st, session_id);
    todo!("nuzu: authz")
}
