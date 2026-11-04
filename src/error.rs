//! Typed refusal reasons. Every refusal an agent can hit carries one of
//! these, so it learns why it cannot proceed rather than just that it cannot.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    LeasedBy,
    StaleBinding,
    Pending,
    Cancelled,
    OutOfScope,
    NotMember,
    NoGrant,
    InvalidScope,
    NoUserEvidence,
    UnresolvedCaller,
    NotFound,
    InvalidArgument,
    Conflict,
    Busy,
    StoreWriteFailed,
    StoreCorrupt,
    /// The state was written by a newer schema; nothing falls back from it.
    StoreUnsupported,
    /// A human hold covers the duty; only the human lifts it.
    Held,
    PagerUnavailable,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::LeasedBy => "leased_by",
            Reason::StaleBinding => "stale_binding",
            Reason::Pending => "pending",
            Reason::Cancelled => "cancelled",
            Reason::OutOfScope => "out_of_scope",
            Reason::NotMember => "not_member",
            Reason::NoGrant => "no_grant",
            Reason::InvalidScope => "invalid_scope",
            Reason::NoUserEvidence => "no_user_evidence",
            Reason::UnresolvedCaller => "unresolved_caller",
            Reason::NotFound => "not_found",
            Reason::InvalidArgument => "invalid_argument",
            Reason::Conflict => "conflict",
            Reason::Busy => "busy",
            Reason::StoreWriteFailed => "store_write_failed",
            Reason::StoreCorrupt => "store_corrupt",
            Reason::StoreUnsupported => "store_unsupported",
            Reason::Held => "held",
            Reason::PagerUnavailable => "pager_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub reason: Reason,
    pub message: String,
}

impl Error {
    pub fn new(reason: Reason, message: impl Into<String>) -> Self {
        Error {
            reason,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason.as_str(), self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Shorthand for building a refusal.
pub fn refuse<T>(reason: Reason, message: impl Into<String>) -> Result<T> {
    Err(Error::new(reason, message))
}
