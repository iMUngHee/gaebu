//! User-prompt evidence (Decisions 3 and 4).
//!
//! The hook classifies each UserPromptSubmit prompt as a user prompt or an
//! excluded one (pager pokes, cross-session wrappers, notifications). Root
//! directives must quote the caller session's current-turn user prompt.
//! The engine checks the quote exists there; it never judges what it means.

use crate::error::{Reason, Result, refuse};
use crate::model::{Attest, Evidence, Ms};
use crate::ops::DutySpec;
use crate::store::{ComposerRecord, EventKind, SessionEvent, Store};
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

/// Treat a heartbeat from one hour before through ten seconds after the prompt
/// as active, including both boundaries. Hook and mod callbacks can arrive in
/// either order. Evidence checks read the logs again at the root operation's
/// current `now`, so an already-arrived late heartbeat prevents hook fallback.
pub fn mod_active_at(events: &[SessionEvent], at: Ms) -> bool {
    events.iter().any(|event| {
        event.kind == EventKind::ModHeartbeat
            && event.at >= at.saturating_sub(3_600_000)
            && event.at <= at.saturating_add(10_000)
    })
}

/// A fragment of a prompt must be at least this long to count as the person's
/// words. A quote of the whole prompt may be shorter: "잠깐 멈춰" is everything
/// the person said, so there is no larger sentence it could be cut from.
const MIN_FRAGMENT_SCALARS: usize = 6;

/// `quote` occurs in `text`, the whitespace-collapsed text of prompt `e`. A
/// shorter quote counts only as all of the prompt, and only when the log kept
/// all of it: the log keeps a 4 KB prefix, and the whole of a prefix is not the
/// whole of the prompt its digest covers.
fn long_enough(quote: &str, text: &str, e: &SessionEvent) -> bool {
    quote.chars().count() >= MIN_FRAGMENT_SCALARS
        || (quote == text
            && e.text
                .as_deref()
                .zip(e.sha256.as_deref())
                .is_some_and(|(t, h)| sha256_hex(&normalize(t)) == h))
}

/// Find the caller's current-turn user prompt and check that `quote`
/// (whitespace-normalized) is a substring of it, at least 6 Unicode scalars
/// long unless it is the whole prompt.
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
    if quote.is_empty() {
        return refuse(Reason::NoUserEvidence, "quote is empty");
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
    if !long_enough(&quote, &text, latest) {
        return refuse(
            Reason::NoUserEvidence,
            "a quote under 6 Unicode scalars must be the whole prompt",
        );
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
        strength: crate::model::Strength::Inferred,
    })
}

/// Find `quote` in a user prompt the hook recorded in this session, in any
/// recent turn (the log keeps the last 50), under the same length rule as
/// root evidence. Excluded prompts never count.
/// This is provenance for a duty, not authority: it records which of the
/// user's words a duty answers.
pub fn recorded_source(
    session_id: &str,
    events: &[SessionEvent],
    quote: &str,
) -> Result<crate::model::Source> {
    let quote = quote.split_whitespace().collect::<Vec<_>>().join(" ");
    if quote.is_empty() {
        return refuse(Reason::NoUserEvidence, "source quote is empty");
    }
    let hit = events
        .iter()
        .filter(|e| e.kind == EventKind::UserPrompt)
        .filter(|e| {
            e.text.as_deref().is_some_and(|t| {
                let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                t.contains(&quote) && long_enough(&quote, &t, e)
            })
        })
        .max_by_key(|e| e.seq);
    let Some(e) = hit else {
        return refuse(
            Reason::NoUserEvidence,
            if quote.chars().count() < MIN_FRAGMENT_SCALARS {
                "a source quote under 6 Unicode scalars must be a whole recorded user prompt"
            } else {
                "source quote is not in a user prompt recorded in this session"
            },
        );
    };
    let Some(sha256) = e.sha256.clone() else {
        return refuse(Reason::NoUserEvidence, "prompt digest is unavailable");
    };
    Ok(crate::model::Source {
        session_id: session_id.into(),
        prompt_seq: e.seq,
        sha256,
        quote,
        at: e.at,
    })
}

/// Resolve a duty's source quote against the caller's own session log.
pub fn attach_source(store: &Store, session_id: &str, spec: &mut DutySpec) -> Result<()> {
    if let Some(quote) = &spec.source_quote {
        let events = store.session(session_id).events()?;
        spec.source = Some(recorded_source(session_id, &events, quote)?);
    }
    Ok(())
}
