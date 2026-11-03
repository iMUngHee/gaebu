//! Caller resolution (Decision 8).
//!
//! - hook: the host's stdin `session_id`.
//! - CLI: `pager whoami`; only `via host` (Claude) and `via native` (Codex)
//!   may change state; flag/env sources are read-only.
//! - MCP: Claude via the server process's `pager whoami`; Codex requires
//!   `_meta.threadId` on each tools/call, seen recently by gaebu's hook.

use crate::error::Result;
use crate::model::{Actor, State, Tool};
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
    let _ = p;
    todo!("bepo: identity")
}

/// Resolve an MCP caller. `thread_id` is `_meta.threadId` when present.
pub fn for_mcp(p: &dyn Pager, store: &Store, thread_id: Option<&str>) -> Result<Caller> {
    let _ = (p, store, thread_id);
    todo!("bepo: identity")
}

/// Map a mutating caller to the acting member. Refuses `unresolved_caller`
/// for read-only sources and `not_member` when no member is bound.
pub fn actor(st: &State, caller: &Caller) -> Result<Actor> {
    let _ = (st, caller);
    todo!("bepo: identity")
}
