//! Caller resolution (Decision 8).
//!
//! - hook: the host's stdin `session_id`.
//! - CLI: `pager whoami`; only `via host` (Claude) and `via native` (Codex)
//!   may change state; flag/env sources are read-only.
//! - MCP: Claude via the server process's `pager whoami`; Codex requires
//!   `_meta.threadId` on each tools/call, seen recently by gaebu's hook.

use crate::error::{Error, Reason, Result};
use crate::model::{Actor, MemberStatus, State, Tool, now_ms};
use crate::pager::Pager;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    HookStdin,
    PagerHost,
    PagerNative,
    McpThreadMeta,
    /// Anything else (`flag`, `env PAGER_SESSION`, ...): read-only.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub session_id: String,
    pub tool: Option<Tool>,
    pub source: Source,
}

impl Caller {
    pub fn may_mutate(&self) -> bool {
        !matches!(self.source, Source::Other(_))
    }
}

/// Resolve the CLI caller through pager.
pub fn from_pager(p: &dyn Pager) -> Result<Caller> {
    let who = p.whoami(None).map_err(|e| {
        Error::new(
            Reason::UnresolvedCaller,
            format!("pager identity unavailable: {e}"),
        )
    })?;
    let (source, tool) = match who.via.as_str() {
        "host" => (Source::PagerHost, Some(Tool::Claude)),
        "native" => (Source::PagerNative, Some(Tool::Codex)),
        _ => (Source::Other(who.via), None),
    };
    Ok(Caller {
        session_id: who.session_id,
        tool,
        source,
    })
}

/// Resolve an MCP caller. `thread_id` is `_meta.threadId` when present.
pub fn for_mcp(p: &dyn Pager, store: &Store, thread_id: Option<&str>) -> Result<Caller> {
    if let Some(id) = thread_id {
        if id.is_empty() {
            return Err(Error::new(Reason::UnresolvedCaller, "empty MCP thread id"));
        }
        let sighting = store.session(id).sighting().map_err(|e| {
            Error::new(
                Reason::UnresolvedCaller,
                format!("MCP thread sighting unavailable: {e}"),
            )
        })?;
        if sighting.tool != Some(Tool::Codex)
            || !sighting
                .last_seen
                .is_some_and(|seen| now_ms().saturating_sub(seen) <= 7_200_000)
        {
            return Err(Error::new(
                Reason::UnresolvedCaller,
                "MCP thread has no recent Codex hook sighting",
            ));
        }
        return Ok(Caller {
            session_id: id.into(),
            tool: Some(Tool::Codex),
            source: Source::McpThreadMeta,
        });
    }
    let caller = from_pager(p)?;
    if caller.source != Source::PagerHost {
        return Err(Error::new(
            Reason::UnresolvedCaller,
            "Codex MCP requires _meta.threadId",
        ));
    }
    Ok(caller)
}

/// Map a mutating caller to the acting member. Refuses `unresolved_caller`
/// for read-only sources and `not_member` when no member is bound.
pub fn actor(st: &State, caller: &Caller) -> Result<Actor> {
    if !caller.may_mutate() {
        return Err(Error::new(
            Reason::UnresolvedCaller,
            "pager flag/env identities are read-only",
        ));
    }
    let member = st
        .members
        .values()
        .find(|m| {
            m.status == MemberStatus::Active
                && m.binding.session_id.as_deref() == Some(&caller.session_id)
        })
        .ok_or_else(|| Error::new(Reason::NotMember, "session has no active member"))?;
    let actor = Actor {
        member: member.id.clone(),
        session_id: caller.session_id.clone(),
    };
    crate::authz::check_actor(st, &actor)?;
    Ok(actor)
}

/// Observe live bindings on CLI/MCP paths. Unavailable or unknown pager
/// observations leave the binding intact; hook paths never call this.
pub fn refresh_bindings(store: &Store, pager: &dyn Pager) -> Result<Vec<String>> {
    let st = store.read()?;
    let mut observations = Vec::new();
    for member in st.members.values().filter(|m| {
        m.status == MemberStatus::Active && m.binding.state == crate::model::BindingState::Bound
    }) {
        let Some(session) = member.binding.session_id.as_deref() else {
            continue;
        };
        let status = pager
            .host_status(&member.binding.alias)
            .unwrap_or(crate::pager::HostStatus::Unknown);
        let reason = if status == crate::pager::HostStatus::Gone {
            Some("pager_gone")
        } else if status == crate::pager::HostStatus::Unknown {
            None
        } else {
            match pager.whoami(Some(session)) {
                Ok(who)
                    if who.session_id == session
                        && who.alias.as_deref() != Some(&member.binding.alias) =>
                {
                    Some("alias_moved")
                }
                _ => None,
            }
        };
        if let Some(reason) = reason {
            observations.push((member.id.clone(), member.binding.clone(), reason));
        }
    }
    if observations.is_empty() {
        return Ok(Vec::new());
    }
    store.transact(crate::store::CLI_DEADLINE, |st| {
        let mut changed = Vec::new();
        for (id, binding, reason) in observations {
            if st
                .members
                .get(&id)
                .is_some_and(|m| m.status == MemberStatus::Active && m.binding == binding)
            {
                crate::ops::mark_vacant(st, now_ms(), &id, reason)?;
                changed.push(id);
            }
        }
        Ok(changed)
    })
}
