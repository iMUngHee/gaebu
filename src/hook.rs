//! Hook adapter for Claude Code and Codex (Decision 15).
//!
//! Always exits 0, never blocks a prompt, prints only
//! `{"hookSpecificOutput":{"hookEventName":..,"additionalContext":..}}`
//! (nothing on Stop), and finishes within a 4 s budget.

use crate::model::Tool;
use crate::pager::Pager;
use crate::store::Store;
use std::io::{Read, Write};

/// Run one hook invocation. `tool` comes from the registration
/// (`gaebu hook claude` / `gaebu hook codex`); the event from the payload.
pub fn run(
    tool: Tool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    store: &Store,
    pager: &dyn Pager,
) {
    let _ = (tool, stdin, stdout, store, pager);
    todo!("bepo: hook adapter")
}
