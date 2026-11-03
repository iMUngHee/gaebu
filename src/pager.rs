//! pager adapter. gaebu never reads pager's database; it only runs the
//! `pager` CLI (`whoami`, `who`, `send`). A trait so tests use a fake.

use crate::error::Result;

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
        let _ = session;
        todo!("bepo: pager adapter")
    }

    fn host_status(&self, alias: &str) -> Result<HostStatus> {
        let _ = alias;
        todo!("bepo: pager adapter")
    }

    fn send(&self, to_session: &str, body: &str) -> Result<()> {
        let _ = (to_session, body);
        todo!("bepo: pager adapter")
    }
}

/// Parse `pager whoami` output. Tolerates padding; `session: unattributed`
/// is an error (`unresolved_caller`).
pub fn parse_whoami(text: &str) -> Result<WhoAmI> {
    let _ = text;
    todo!("bepo: pager adapter")
}

/// Parse `pager who` output into (alias, host status) rows.
pub fn parse_who(text: &str) -> Vec<(String, HostStatus)> {
    let _ = text;
    todo!("bepo: pager adapter")
}
