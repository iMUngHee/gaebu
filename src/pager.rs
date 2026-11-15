//! pager adapter. gaebu never reads pager's database; it only runs the
//! `pager` CLI (`whoami`, `who`, `send`). A trait so tests use a fake.

use crate::error::{Error, Reason, Result};
use crate::model::{NoticeKind, RecordKind, now_ms};
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

/// One reading of `pager who --all`: the HOST status of every listed alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosts {
    rows: Vec<(String, HostStatus)>,
    unlisted: HostStatus,
}

impl Hosts {
    /// An alias the roster does not list is `Unknown`.
    pub fn from_rows(rows: Vec<(String, HostStatus)>) -> Self {
        Hosts {
            rows,
            unlisted: HostStatus::Unknown,
        }
    }
    /// Every alias has `status`.
    pub fn all(status: HostStatus) -> Self {
        Hosts {
            rows: Vec::new(),
            unlisted: status,
        }
    }
    pub fn status(&self, alias: &str) -> HostStatus {
        self.rows
            .iter()
            .find(|(name, _)| name == alias)
            .map_or(self.unlisted, |(_, status)| *status)
    }
}

/// One `pager send`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message<'a> {
    /// A session id or an alias.
    pub target: &'a str,
    pub body: &'a str,
    /// Stable for one message, so pager returns the original instead of
    /// storing a retry by the same sender as new mail.
    pub event: &'a str,
    pub kind: NoticeKind,
}

pub trait Pager {
    /// `pager whoami [--session <id>]`.
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI>;
    /// `pager who --all`, read once for every alias.
    fn hosts(&self) -> Result<Hosts>;
    /// `pager send --event <event> --kind <kind> <target> <body>`.
    fn send(&self, message: &Message<'_>) -> Result<()>;
}

/// Runs the real `pager` binary with a per-call timeout.
pub struct CliPager {
    pub timeout: std::time::Duration,
    /// The executable to run; `pager` on `PATH` outside tests.
    pub program: std::path::PathBuf,
}

impl Pager for CliPager {
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
        let mut args = vec!["whoami"];
        if let Some(id) = session {
            args.extend(["--session", id]);
        }
        parse_whoami(&self.output(&args)?)
    }

    fn hosts(&self) -> Result<Hosts> {
        // Without `--all` pager hides a session that has been gone for long,
        // which would read as unknown and keep its seat bound.
        Ok(Hosts::from_rows(parse_who(
            &self.output(&["who", "--all"])?,
        )))
    }

    fn send(&self, message: &Message<'_>) -> Result<()> {
        self.output(&send_args(message)).map(|_| ())
    }
}

/// The arguments after `pager`. Flags precede `--` so a target or body that
/// starts with a dash is still read as text.
pub fn send_args<'a>(message: &Message<'a>) -> Vec<&'a str> {
    vec![
        "send",
        "--event",
        message.event,
        "--kind",
        message.kind.as_str(),
        "--",
        message.target,
        message.body,
    ]
}

impl CliPager {
    fn output(&self, args: &[&str]) -> Result<String> {
        let started = Instant::now();
        let fail = |message: String| Error::new(Reason::PagerUnavailable, message);
        let mut child = Command::new(&self.program)
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
///
/// The HOST cell is read by its position under the header. A row has other
/// cells that hold the same words (THREAD is `unknown` for every Claude
/// session), and ROOT may contain spaces. pager aligns columns by character
/// count, so positions are counted in characters, not bytes.
pub fn parse_who(text: &str) -> Vec<(String, HostStatus)> {
    let mut lines = text.lines().skip_while(|line| !line.starts_with("NAME"));
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let column = |name: &str| header.find(name).map(|at| header[..at].chars().count());
    let (Some(host), Some(last)) = (column("HOST"), column("LAST")) else {
        return Vec::new();
    };
    // Rows follow the header without a gap; the first line that has no HOST
    // cell ends the table (pager may print a note after it).
    lines
        .map_while(|line| {
            let name = line.split_whitespace().next()?;
            let cell: String = line
                .chars()
                .skip(host)
                .take(last.saturating_sub(host))
                .collect();
            let status = match cell.trim() {
                "live" => HostStatus::Live,
                "gone" => HostStatus::Gone,
                "unknown" => HostStatus::Unknown,
                _ => return None,
            };
            Some((name.to_owned(), status))
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
    let Ok((runtime, batch)) = store.transact(store::CLI_DEADLINE, |st| {
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
        Ok((st.runtime_id.clone(), batch))
    }) else {
        return;
    };
    if batch.is_empty() {
        return;
    }
    let mut delivered = Vec::new();
    let mut failed = Vec::new();
    for n in &batch {
        // The kind is part of the id: pager refuses an event resent with a
        // different kind, which is what a notice becomes once an older
        // binary has rewritten the state without its kind.
        let event = format!("gaebu-{runtime}-{}-{}", n.id, n.kind.as_str());
        let message = Message {
            target: &n.target,
            body: &n.body,
            event: &event,
            kind: n.kind,
        };
        match pager.send(&message) {
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
