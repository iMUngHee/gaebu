//! pager adapter. gaebu never reads pager's database; it only runs the
//! `pager` CLI (`whoami`, `who`, `send`). A trait so tests use a fake.

use crate::error::{Error, Reason, Result};
use crate::model::{RecordKind, now_ms};
use crate::store::{self, Store};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhoAmI {
    pub session_id: String,
    /// The `(via ...)` source: `host` (Claude), `native` (Codex), or other.
    pub via: String,
    pub alias: Option<String>,
    pub tool: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStatus {
    Live,
    Gone,
    Unknown,
}

pub trait Pager {
    /// `pager whoami [--session <id>]`.
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI>;
    /// HOST column of `pager who` for `alias`; `Unknown` when absent.
    fn host_status(&self, alias: &str) -> Result<HostStatus>;
    /// `pager send <session_id> <body>`.
    fn send(&self, to_session: &str, body: &str) -> Result<()>;
}

/// Runs the real `pager` binary with a per-call timeout.
pub struct CliPager {
    pub timeout: std::time::Duration,
}

impl Pager for CliPager {
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
        let mut args = vec!["whoami"];
        if let Some(id) = session {
            args.extend(["--session", id]);
        }
        parse_whoami(&self.output(&args)?)
    }

    fn host_status(&self, alias: &str) -> Result<HostStatus> {
        Ok(parse_who(&self.output(&["who"])?)
            .into_iter()
            .find(|(name, _)| name == alias)
            .map(|(_, status)| status)
            .unwrap_or(HostStatus::Unknown))
    }

    fn send(&self, to_session: &str, body: &str) -> Result<()> {
        self.output(&["send", to_session, body]).map(|_| ())
    }
}

impl CliPager {
    fn output(&self, args: &[&str]) -> Result<String> {
        let started = Instant::now();
        let fail = |message: String| Error::new(Reason::PagerUnavailable, message);
        let mut child = Command::new("pager")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| fail(e.to_string()))?;
        let pipe = child
            .stdout
            .take()
            .ok_or_else(|| fail("missing stdout".into()))?;
        // Drain concurrently: a full pipe must not prevent timeout enforcement.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe.take(1_048_576).read_to_end(&mut bytes).map(|_| bytes);
            let _ = tx.send(result);
        });
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return Err(fail(format!("pager exited {status}")));
                    }
                    let remaining = self.timeout.saturating_sub(started.elapsed());
                    let bytes = rx
                        .recv_timeout(remaining)
                        .map_err(|_| fail("pager output timeout".into()))?
                        .map_err(|e| fail(e.to_string()))?;
                    return String::from_utf8(bytes).map_err(|e| fail(e.to_string()));
                }
                Ok(None) if started.elapsed() < self.timeout => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                outcome => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(fail(match outcome {
                        Err(e) => e.to_string(),
                        _ => "pager timeout".into(),
                    }));
                }
            }
        }
    }
}

/// Parse `pager whoami` output. Tolerates padding; `session: unattributed`
/// is an error (`unresolved_caller`).
pub fn parse_whoami(text: &str) -> Result<WhoAmI> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(name).map(str::trim))
    };
    let session = field("session:").unwrap_or("");
    let (id, source) = session.split_once(" (via ").ok_or_else(|| {
        Error::new(
            Reason::UnresolvedCaller,
            "pager did not resolve a session source",
        )
    })?;
    let via = source.strip_suffix(')').unwrap_or("").trim();
    let id = id.trim();
    if id.is_empty()
        || id == "unattributed"
        || id.chars().any(char::is_whitespace)
        || via.is_empty()
    {
        return Err(Error::new(
            Reason::UnresolvedCaller,
            "invalid pager session identity",
        ));
    }
    Ok(WhoAmI {
        session_id: id.into(),
        via: via.into(),
        alias: field("name:").filter(|s| !s.is_empty()).map(str::to_owned),
        tool: field("host:")
            .and_then(|s| s.split_whitespace().next())
            .map(str::to_owned),
    })
}

/// Parse `pager who` output into (alias, host status) rows.
pub fn parse_who(text: &str) -> Vec<(String, HostStatus)> {
    text.lines()
        .filter_map(|line| {
            let columns: Vec<_> = line.split_whitespace().collect();
            if columns.len() < 5 || columns[0] == "NAME" {
                return None;
            }
            let status = columns[3..].iter().rev().find_map(|s| match *s {
                "live" => Some(HostStatus::Live),
                "gone" => Some(HostStatus::Gone),
                "unknown" => Some(HostStatus::Unknown),
                _ => None,
            })?;
            Some((columns[0].to_owned(), status))
        })
        .collect()
}

/// Deliver pending notices through pager and drop the delivered ones.
/// Failures stay queued for the next call.
pub fn flush_outbox(store: &Store, pager: &dyn Pager) {
    // Claim a batch under the lock (so concurrent CLI/MCP calls do not send
    // the same notice), send outside the lock, then drop what was delivered.
    // A notice leaves the outbox only after pager accepted it: a crash after
    // the claim leaves a stale claim that the next flush retries, so delivery
    // is at least once rather than at most once.
    let now = now_ms();
    let Ok(batch) = store.transact(store::CLI_DEADLINE, |st| {
        let mut batch = Vec::new();
        for n in st.outbox.iter_mut() {
            if batch.len() >= MAX_NOTICES_PER_FLUSH {
                break;
            }
            let stale = n
                .claimed_at
                .is_none_or(|t| now.saturating_sub(t) >= crate::model::NOTICE_CLAIM_MS);
            if stale {
                n.claimed_at = Some(now);
                batch.push(n.clone());
            }
        }
        Ok(batch)
    }) else {
        return;
    };
    if batch.is_empty() {
        return;
    }
    let mut delivered = Vec::new();
    let mut failed = Vec::new();
    for n in &batch {
        match pager.send(&n.to_session, &n.body) {
            Ok(()) => delivered.push(n.id.clone()),
            Err(e) => {
                eprintln!("gaebu: notice {} not delivered: {e}", n.id);
                failed.push(n.id.clone());
            }
        }
    }
    let result = store.transact(store::CLI_DEADLINE, |st| {
        let now = now_ms();
        st.outbox.retain(|n| !delivered.contains(&n.id));
        let mut given_up = Vec::new();
        for n in st.outbox.iter_mut() {
            if failed.contains(&n.id) {
                n.claimed_at = None;
                n.attempts += 1;
                if n.attempts >= MAX_NOTICE_ATTEMPTS {
                    given_up.push(n.clone());
                }
            }
        }
        st.outbox.retain(|n| !given_up.iter().any(|g| g.id == n.id));
        for n in given_up {
            // Give up, but leave a trace in the team record.
            if let Some(team) = st.members.get(&n.to_member).map(|m| m.team.clone()) {
                let id = st.mint("r");
                st.records.push(crate::model::Record {
                    id,
                    team,
                    kind: RecordKind::Note,
                    author: n.to_member.clone(),
                    refs: vec![n.id.clone()],
                    text: format!(
                        "undelivered notice after {} attempts: {}",
                        n.attempts, n.body
                    ),
                    at: now,
                });
            }
        }
        Ok(())
    });
    if let Err(e) = result {
        // Delivered notices may be sent again by a later flush (their claim
        // goes stale); failed ones stay claimed until then. Nothing is lost.
        eprintln!("gaebu: outbox bookkeeping deferred: {e}");
    }
}

/// Notices sent per call, so a backlog never stalls a command for long.
const MAX_NOTICES_PER_FLUSH: usize = 10;
/// Attempts before a notice is dropped into the team record.
const MAX_NOTICE_ATTEMPTS: u32 = 10;
