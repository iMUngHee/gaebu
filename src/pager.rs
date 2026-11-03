//! pager adapter. gaebu never reads pager's database; it only runs the
//! `pager` CLI (`whoami`, `who`, `send`). A trait so tests use a fake.

use crate::error::{Error, Reason, Result};
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
