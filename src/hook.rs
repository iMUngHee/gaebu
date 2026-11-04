//! Hook adapter for Claude Code and Codex (Decision 15).
//!
//! Always exits 0, never blocks a prompt, prints only
//! `{"hookSpecificOutput":{"hookEventName":..,"additionalContext":..}}`
//! (nothing on Stop), and finishes within a 4 s budget.

use crate::model::Tool;
use crate::pager::Pager;
use crate::store::Store;
use crate::{Error, Reason, Result, card, evidence, model, ops, store};
use serde::Deserialize;
use serde_json::json;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct Input {
    session_id: String,
    hook_event_name: String,
    #[serde(default)]
    prompt: String,
}

/// Production entry point: reading a pipe whose writer stays open must not
/// consume the host's hook timeout. The host closes this process on return.
pub fn run_stdio(tool: Tool, store: &Store, pager: &dyn Pager) {
    let started = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().take(1_048_577);
        let mut buffer = [0; 8192];
        loop {
            let packet = stdin.read(&mut buffer).map(|n| buffer[..n].to_vec());
            let finished = !matches!(&packet, Ok(chunk) if !chunk.is_empty());
            if tx.send(packet).is_err() || finished {
                break;
            }
        }
    });
    let mut raw = Vec::new();
    loop {
        let remaining = Duration::from_millis(250).saturating_sub(started.elapsed());
        match rx.recv_timeout(remaining) {
            Ok(Ok(chunk)) if chunk.is_empty() => break,
            Ok(Ok(chunk)) => {
                raw.extend_from_slice(&chunk);
                if raw.len() > 1_048_576 {
                    break;
                }
            }
            _ => {
                mark_failed_input(store, &raw);
                eprintln!("gaebu: hook stdin unavailable within budget");
                return;
            }
        }
    }
    run_budget(
        tool,
        &mut raw.as_slice(),
        &mut std::io::stdout(),
        store,
        pager,
        || Duration::from_secs(4).saturating_sub(started.elapsed()),
    );
}

/// A complete JSON session id can still be recovered when the event's
/// schema is invalid. Truncated or malformed JSON invalidates all sessions
/// conservatively, rather than guessing an id from text inside a prompt.
fn mark_failed_input(store: &Store, raw: &[u8]) {
    #[derive(Deserialize)]
    struct SessionOnly {
        session_id: String,
    }
    let identity = serde_json::from_slice::<SessionOnly>(raw).ok();
    let session = identity
        .as_ref()
        .map(|v| v.session_id.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("_global");
    if let Err(e) = store.session(session).mark_unrecorded(model::now_ms()) {
        eprintln!("gaebu: hook failed-input marker: {e}");
        if session != "_global"
            && let Err(e) = store.session("_global").mark_unrecorded(model::now_ms())
        {
            eprintln!("gaebu: hook global failed-input marker: {e}");
        }
    }
}

/// Run one hook invocation. `tool` comes from the registration
/// (`gaebu hook claude` / `gaebu hook codex`); the event from the payload.
pub fn run(
    tool: Tool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    store: &Store,
    pager: &dyn Pager,
) {
    let started = Instant::now();
    run_budget(tool, stdin, stdout, store, pager, || {
        Duration::from_secs(4).saturating_sub(started.elapsed())
    });
}

fn run_budget(
    tool: Tool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    store: &Store,
    pager: &dyn Pager,
    remaining: impl Fn() -> Duration,
) {
    // Session writes can wait 500 ms for their lock and retry rename for
    // another second. Reserve both before starting an uninterruptible write.
    let session_write_budget = store::SESSION_LOCK_DEADLINE + Duration::from_secs(1);
    let mut event = String::new();
    let mut member_known = false;
    let result = (|| -> Result<Option<String>> {
        let mut raw = Vec::new();
        stdin.take(1_048_577).read_to_end(&mut raw).map_err(|e| {
            mark_failed_input(store, &raw);
            Error::new(Reason::InvalidArgument, e.to_string())
        })?;
        if raw.len() > 1_048_576 {
            mark_failed_input(store, &raw);
            return Err(Error::new(Reason::InvalidArgument, "hook input too large"));
        }
        let input: Input = serde_json::from_slice(&raw).map_err(|e| {
            mark_failed_input(store, &raw);
            Error::new(Reason::InvalidArgument, e.to_string())
        })?;
        if input.session_id.is_empty() {
            mark_failed_input(store, &raw);
            return Ok(None);
        }
        event = input.hook_event_name;
        if !matches!(
            event.as_str(),
            "SessionStart" | "UserPromptSubmit" | "PostCompact" | "Stop"
        ) {
            return Ok(None);
        }
        if store.capture_enabled() {
            let capture = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(store.capture_dir())?;
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let path = store.capture_dir().join(format!(
                    "{tool:?}-{event}-{}-{stamp}.json",
                    std::process::id()
                ));
                std::fs::write(path, &raw)
            })();
            if let Err(e) = capture {
                eprintln!("gaebu: hook capture: {e}");
            }
        }
        let now = model::now_ms();
        let session = store.session(&input.session_id);
        if remaining() < session_write_budget {
            if let Err(e) = session.mark_unrecorded(now) {
                eprintln!("gaebu: hook missed-event marker: {e}");
            }
            return Err(Error::new(Reason::Busy, "hook budget exhausted"));
        }
        let entry = match event.as_str() {
            "UserPromptSubmit" => {
                let prompt = evidence::normalize(&input.prompt);
                let (kind, excluded) = match evidence::classify(&input.prompt) {
                    evidence::PromptClass::User => (store::EventKind::UserPrompt, None),
                    evidence::PromptClass::Excluded(rule) => {
                        (store::EventKind::ExcludedPrompt, Some(rule.to_owned()))
                    }
                };
                Some((
                    kind,
                    Some(evidence::sha256_hex(&prompt)),
                    Some(prompt),
                    excluded,
                ))
            }
            "Stop" => Some((store::EventKind::Stop, None, None, None)),
            "SessionStart" => Some((store::EventKind::SessionStart, None, None, None)),
            _ => None,
        };
        if let Some((kind, hash, text, excluded)) = entry
            && let Err(e) = session.append(now, kind, hash, text, excluded)
        {
            if let Err(marker_error) = session.mark_unrecorded(now) {
                eprintln!("gaebu: hook missed-event marker: {marker_error}");
            }
            eprintln!("gaebu: hook event not recorded: {e}");
        }
        let mut sighting = session.sighting()?;
        let previous_hash = sighting.last_card_hash.clone();
        if remaining() >= session_write_budget {
            if let Err(e) = session.update_sighting(|s| {
                s.tool = Some(tool);
                s.last_seen = Some(now);
            }) {
                eprintln!("gaebu: hook sighting not recorded: {e}");
            } else {
                sighting.last_seen = Some(now);
            }
        }
        if event == "Stop" {
            return Ok(None);
        }
        if remaining().is_zero() {
            return Err(Error::new(Reason::Busy, "hook budget exhausted"));
        }
        let mut st = store.read()?;
        member_known = st.members.values().any(|m| {
            m.status == model::MemberStatus::Active
                && m.binding.state == model::BindingState::Bound
                && m.binding.session_id.as_deref() == Some(&input.session_id)
        });
        let pending = st.members.values().any(|m| {
            m.status == model::MemberStatus::Active
                && m.binding.state == model::BindingState::Pending
        });
        if !member_known && !pending {
            return Ok(None);
        }
        if !member_known && pending {
            if sighting.pending_checked_rev == Some(st.rev)
                && sighting
                    .pending_checked_at
                    .is_some_and(|at| now.saturating_sub(at) < 300_000)
            {
                return Ok(None);
            }
            // Only start a check that can finish: pager and the confirming
            // transaction must fit. A skipped check is not stamped, so it is
            // tried again on the next prompt; the stamp itself is best-effort.
            if remaining() < Duration::from_millis(1_500) + store::HOOK_DEADLINE {
                return Ok(None);
            }
            let asked = pager.whoami(Some(&input.session_id));
            // Cache real attempts, failed or not, so an unavailable pager
            // cannot add its full timeout to every unrelated user prompt.
            if remaining() >= session_write_budget {
                let _ = session.update_sighting(|s| {
                    s.pending_checked_at = Some(now);
                    s.pending_checked_rev = Some(st.rev);
                });
            }
            let who = asked?;
            if who.session_id != input.session_id {
                return Err(Error::new(
                    Reason::UnresolvedCaller,
                    "pager returned a different session",
                ));
            }
            if let Some(alias) = who.alias
                && st.members.values().any(|m| {
                    m.status == model::MemberStatus::Active
                        && m.binding.state == model::BindingState::Pending
                        && m.binding.alias == alias
                })
            {
                if remaining() < store::HOOK_DEADLINE {
                    return Ok(None);
                }
                let confirmed = store.transact(store::HOOK_DEADLINE, |st| {
                    ops::confirm_binding(st, now, &input.session_id, tool, &alias)
                })?;
                member_known = confirmed.is_some();
                if remaining().is_zero() {
                    return Err(Error::new(Reason::Busy, "hook budget exhausted"));
                }
                st = store.read()?;
            }
        }
        let Some(member) = st.members.values().find(|m| {
            m.status == model::MemberStatus::Active
                && m.binding.state == model::BindingState::Bound
                && m.binding.session_id.as_deref() == Some(&input.session_id)
        }) else {
            return Ok(None);
        };
        if remaining().is_zero() {
            return Err(Error::new(Reason::Busy, "hook budget exhausted"));
        }
        let Some(card) = card::build(&st, &member.id, now, sighting.last_seen) else {
            return Ok(None);
        };
        // The mod adds the full card to the system prompt from prompt.compose
        // and stamps when it does. A mod that is alive is not enough: with
        // managed settings, or in Team and Enterprise organizations,
        // cc-plugin-sec-default passes prompt.compose on past it, the section
        // never lands, no stamp arrives, and the cards are the hook's as
        // without a mod.
        let mod_composes = tool == Tool::Claude
            && sighting
                .compose_heartbeat
                .is_some_and(|t| now.saturating_sub(t) < 3_600_000);
        let full = !mod_composes
            && (tool == Tool::Codex
                || event != "UserPromptSubmit"
                || previous_hash.as_deref() != Some(&card.hash));
        let text = if full {
            card::render(&card)
        } else {
            card::one_line(&card)
        };
        // Remember only a full card actually written to the host. A short
        // mod-owned card must not suppress the next fallback full card.
        Ok(Some(if full {
            emit(stdout, &event, &text)?;
            if remaining() >= session_write_budget
                && let Err(e) = session.update_sighting(|s| s.last_card_hash = Some(card.hash))
            {
                eprintln!("gaebu: hook card hash not recorded: {e}");
            }
            String::new()
        } else {
            text
        }))
    })();
    match result {
        Ok(Some(text)) if !text.is_empty() => {
            if let Err(e) = emit(stdout, &event, &text) {
                eprintln!("gaebu: hook output: {e}");
            }
        }
        Err(e) => {
            eprintln!("gaebu: hook: {e}");
            if member_known && event != "Stop" && e.reason == Reason::Busy {
                let _ = emit(
                    stdout,
                    &event,
                    "gaebu: 상태 확인 시간 초과 — `gaebu me`로 다시 확인",
                );
            }
        }
        _ => {}
    }
}

fn emit(stdout: &mut dyn Write, event: &str, text: &str) -> Result<()> {
    serde_json::to_writer(
        &mut *stdout,
        &json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":text}}),
    )
    .map_err(|e| Error::new(Reason::StoreWriteFailed, e.to_string()))?;
    writeln!(stdout)
        .and_then(|_| stdout.flush())
        .map_err(|e| Error::new(Reason::StoreWriteFailed, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pager::{HostStatus, WhoAmI};
    use std::cell::Cell;

    #[test]
    fn hook_budget_rechecked_after_sighting_write() {
        struct CountingPager(Cell<usize>);
        impl Pager for CountingPager {
            fn whoami(&self, _: Option<&str>) -> Result<WhoAmI> {
                self.0.set(self.0.get() + 1);
                Ok(WhoAmI {
                    session_id: "budget".into(),
                    via: "flag".into(),
                    alias: Some("target".into()),
                    tool: None,
                })
            }
            fn host_status(&self, _: &str) -> Result<HostStatus> {
                unreachable!()
            }
            fn send(&self, _: &str, _: &str) -> Result<()> {
                unreachable!()
            }
        }
        let home = std::env::temp_dir().join(format!(
            "gaebu-hook-budget-{}-{}",
            std::process::id(),
            model::now_ms()
        ));
        let store = Store::open(&home).unwrap();
        let mut value = serde_json::to_value(model::State::default()).unwrap();
        value["members"]["m"] = json!({"id":"m","team":"t","bu":"b","superior":null,"title":"fixture","status":"active","binding":{"state":"pending","alias":"target","tool":null,"session_id":null,"designated_at":1,"bound_at":null,"vacated_at":null,"vacated_reason":null},"created_at":1});
        store
            .transact(store::CLI_DEADLINE, |st| {
                *st = serde_json::from_value(value).unwrap();
                Ok(())
            })
            .unwrap();
        let pager = CountingPager(Cell::new(0));
        let mut output = Vec::new();
        run_budget(
            Tool::Codex,
            &mut br#"{"session_id":"budget","hook_event_name":"SessionStart"}"#.as_slice(),
            &mut output,
            &store,
            &pager,
            // Too little left for pager + stamp + transaction: the check is
            // not started, and not stamped, so the next prompt tries again.
            || Duration::from_secs(2),
        );
        assert_eq!(pager.0.get(), 0);
        assert!(output.is_empty());
        assert!(
            store
                .session("budget")
                .sighting()
                .unwrap()
                .pending_checked_rev
                .is_none(),
            "a skipped check must not start the five-minute throttle"
        );
        store
            .session("budget")
            .update_sighting(|s| {
                s.pending_checked_at = None;
                s.pending_checked_rev = None;
            })
            .unwrap();
        run_budget(
            Tool::Codex,
            &mut br#"{"session_id":"budget","hook_event_name":"SessionStart"}"#.as_slice(),
            &mut output,
            &store,
            &pager,
            || {
                if pager.0.get() > 0 {
                    Duration::from_millis(500)
                } else {
                    Duration::from_secs(4)
                }
            },
        );
        assert_eq!(pager.0.get(), 1);
        assert_eq!(
            store.read().unwrap().rev,
            1,
            "insufficient budget must skip the state transaction"
        );
        assert!(output.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }
}
