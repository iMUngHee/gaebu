//! User-prompt evidence (Decisions 3 and 4).
//!
//! The hook classifies each UserPromptSubmit prompt as a user prompt or an
//! excluded one (pager pokes, cross-session wrappers, notifications). Root
//! directives must quote the caller session's current-turn user prompt.
//! The engine checks the quote exists there; it never judges what it means.

use crate::error::Result;
use crate::model::{Evidence, Ms};
use crate::store::{ComposerRecord, SessionEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptClass {
    User,
    /// Excluded, with the rule that matched.
    Excluded(&'static str),
}

/// The single place the exclusion rules live.
pub fn classify(prompt: &str) -> PromptClass {
    let _ = prompt;
    todo!("nawi: exclusion rules from captured fixtures")
}

/// Trim and CRLF→LF before hashing.
pub fn normalize(prompt: &str) -> String {
    let _ = prompt;
    todo!("nawi")
}

pub fn sha256_hex(text: &str) -> String {
    let _ = text;
    todo!("nawi")
}

/// Whether the Claude mod was active at `at` given its heartbeats (1 h expiry).
pub fn mod_active_at(events: &[SessionEvent], at: Ms) -> bool {
    let _ = (events, at);
    todo!("nawi")
}

/// Find the caller's current-turn user prompt and check that `quote`
/// (whitespace-normalized, at least 6 Unicode scalars) is a substring of it.
/// Rules: it is the latest user-or-excluded prompt event, no Stop after it,
/// and in mod-active sessions it must be upgraded by a matching composer
/// record (same sha256, before the next Stop). Refuses `no_user_evidence`.
pub fn current_turn_evidence(
    session_id: &str,
    events: &[SessionEvent],
    composers: &[ComposerRecord],
    quote: &str,
    now: Ms,
) -> Result<Evidence> {
    let _ = (session_id, events, composers, quote, now);
    todo!("nawi: current-turn evidence")
}
