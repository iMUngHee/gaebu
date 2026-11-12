//! Gaebu: a thin support layer that records who commands whom, what each
//! member owes (duties), who holds which resource (leases) and what was
//! cancelled, for independent Claude Code and Codex sessions.
//!
//! The engine never decides. It stores, checks, injects and notifies.
//! See `docs/model.md` for the data contract and `docs/threat-model.md` for
//! what the checks do and do not protect against.

mod archive;
pub mod authz;
pub mod card;
pub mod cli;
pub mod compact;
pub mod error;
pub mod evidence;
pub mod hook;
pub mod identity;
pub mod mcp;
pub mod model;
pub mod ops;
pub mod pager;
pub mod schedule;
pub mod scope;
pub mod store;
pub mod titles;

pub use error::{Error, Reason, Result};
