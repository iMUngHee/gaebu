//! User-prompt evidence (Decisions 3 and 4).
//!
//! The hook classifies each UserPromptSubmit prompt as a user prompt or an
//! excluded one (pager pokes, cross-session wrappers, notifications). Root
//! directives must quote the caller session's current-turn user prompt.
//! The engine checks the quote exists there; it never judges what it means.

use crate::error::{Reason, Result, refuse};
use crate::model::{Attest, Evidence, Ms};
use crate::store::{ComposerRecord, EventKind, SessionEvent};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptClass {
    User,
    /// Excluded, with the rule that matched.
    Excluded(&'static str),
}

/// The single place the exclusion rules live.
pub fn classify(prompt: &str) -> PromptClass {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return PromptClass::Excluded("empty");
    }
    if prompt.contains("[pager-poke]") || prompt.starts_with("pager: new mail for ") {
        return PromptClass::Excluded("pager");
    }
    for marker in [
        "Another Claude session sent a message",
        "<agent-message",
        "[SYSTEM NOTIFICATION",
        "<side-sessions-event>",
        "<task-notification>",
    ] {
        if prompt.contains(marker) {
            return PromptClass::Excluded(marker);
        }
    }
    PromptClass::User
}

/// Trim and CRLF→LF before hashing.
pub fn normalize(prompt: &str) -> String {
    prompt.replace("\r\n", "\n").trim().to_string()
}

pub fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Whether the Claude mod was active at `at` given its heartbeats (1 h expiry).
pub fn mod_active_at(events: &[SessionEvent], at: Ms) -> bool {
    events.iter().any(|event| {
        event.kind == EventKind::ModHeartbeat && event.at <= at && at - event.at < 3_600_000
    })
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
    let quote = quote.split_whitespace().collect::<Vec<_>>().join(" ");
    if quote.chars().count() < 6 {
        return refuse(
            Reason::NoUserEvidence,
            "quote must contain at least 6 Unicode scalars",
        );
    }
    // The caller must load these logs through Store::session(caller.session_id).
    // SessionEvent intentionally carries no cross-session identity of its own.
    let mut ordered: Vec<_> = events.iter().collect();
    ordered.sort_by_key(|event| event.seq);
    let Some(latest) = ordered.iter().rfind(|event| {
        matches!(
            event.kind,
            EventKind::UserPrompt | EventKind::ExcludedPrompt | EventKind::Stop
        )
    }) else {
        return refuse(Reason::NoUserEvidence, "no current prompt");
    };
    if latest.kind == EventKind::Stop || latest.at > now {
        return refuse(Reason::NoUserEvidence, "prompt window is closed");
    }
    let mut records: Vec<_> = composers.iter().filter(|rec| rec.at <= now).collect();
    records.sort_by_key(|rec| rec.at);
    let mut consumed = vec![false; records.len()];
    let mut latest_composer = false;
    // Only the open turn can authorize anything: the latest event above is
    // not Stop, so there is no next Stop in this window. Pair its prompts in
    // sequence order and consume each composer once. Closed turns cannot
    // consume a record that belongs to this window.
    let previous_stop = ordered.iter().rev().find(|e| e.kind == EventKind::Stop);
    for prompt in &ordered {
        if previous_stop.is_some_and(|stop| prompt.seq <= stop.seq) {
            continue;
        }
        if !matches!(
            prompt.kind,
            EventKind::UserPrompt | EventKind::ExcludedPrompt
        ) {
            continue;
        }
        let matched = records.iter().enumerate().find(|(i, rec)| {
            !consumed[*i]
                && prompt.sha256.as_ref() == Some(&rec.sha256)
                && previous_stop.is_none_or(|stop| rec.at > stop.at)
        });
        if let Some((i, _)) = matched {
            consumed[i] = true;
            if prompt.seq == latest.seq {
                latest_composer = true;
            }
        }
    }
    if !latest_composer
        && (latest.kind != EventKind::UserPrompt || mod_active_at(events, latest.at))
    {
        return refuse(
            Reason::NoUserEvidence,
            "current prompt has no eligible user provenance",
        );
    }
    let Some(text) = latest.text.as_deref() else {
        return refuse(Reason::NoUserEvidence, "prompt text is unavailable");
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.contains(&quote) {
        return refuse(Reason::NoUserEvidence, "quote is not in the current prompt");
    }
    let Some(hash) = latest.sha256.as_ref() else {
        return refuse(Reason::NoUserEvidence, "prompt digest is unavailable");
    };
    Ok(Evidence {
        session_id: session_id.into(),
        prompt_seq: latest.seq,
        sha256: hash.clone(),
        attest: if latest_composer {
            Attest::Composer
        } else {
            Attest::HookFiltered
        },
        quote,
        at: latest.at,
    })
}
