//! Storage (Decision 11a). One JSON state file under an OS lock, plus
//! per-session event logs that need no global lock.
//!
//! Layout under the home (`GAEBU_HOME`, default `~/.gaebu`):
//!
//! ```text
//! state.json            the whole State, replaced atomically
//! state.prev.json       the previous generation (recovery)
//! state.lock            OS lock (never deleted)
//! sessions/<id>/events.jsonl    SessionEvent lines (hook writes)
//! sessions/<id>/composer.jsonl  ComposerRecord lines (mod writes via CLI)
//! sessions/<id>/sighting.json   Sighting
//! sessions/<id>/lock            per-session OS lock
//! capture.on            dev switch: hook saves raw stdin to capture/
//! ```

use crate::error::Result;
use crate::model::{Ms, State, Tool};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CLI_DEADLINE: Duration = Duration::from_secs(5);
pub const HOOK_DEADLINE: Duration = Duration::from_secs(1);

pub struct Store {
    home: PathBuf,
}

impl Store {
    /// `GAEBU_HOME` if set, else `~/.gaebu`.
    pub fn from_env() -> Result<Store> {
        todo!("nuzu: store")
    }

    pub fn open(home: impl Into<PathBuf>) -> Result<Store> {
        let _ = home.into();
        todo!("nuzu: store")
    }

    /// The default home (`~/.gaebu`), whether or not it exists.
    pub fn default_home() -> PathBuf {
        todo!("nuzu: store")
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// True when this store is the default home (dev seed is refused there).
    pub fn is_default_home(&self) -> bool {
        todo!("nuzu: store")
    }

    /// Lock-free read of the current state (falls back to `state.prev.json`
    /// with a warning on stderr when `state.json` does not parse). A missing
    /// file is an empty `State::default()`.
    pub fn read(&self) -> Result<State> {
        todo!("nuzu: store")
    }

    /// Run `f` on the state under the exclusive lock and commit on `Ok`
    /// (bumping `rev`). Refuses with `busy` after `deadline`, `store_corrupt`
    /// when `state.json` does not parse, `store_write_failed` on replace failure.
    pub fn transact<T>(
        &self,
        deadline: Duration,
        f: impl FnOnce(&mut State) -> Result<T>,
    ) -> Result<T> {
        let _ = (deadline, f);
        todo!("nuzu: store")
    }

    /// Restore `state.json` from `state.prev.json` (`gaebu repair`).
    pub fn repair(&self) -> Result<()> {
        todo!("nuzu: store")
    }

    pub fn session(&self, session_id: &str) -> SessionLog {
        SessionLog {
            dir: self.home.join("sessions").join(sanitize(session_id)),
        }
    }

    pub fn capture_enabled(&self) -> bool {
        self.home.join("capture.on").exists()
    }

    pub fn capture_dir(&self) -> PathBuf {
        self.home.join("capture")
    }
}

/// Session ids come from hosts; keep them path-safe.
pub fn sanitize(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionStart,
    UserPrompt,
    ExcludedPrompt,
    Stop,
    ModHeartbeat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Monotonic within the session; assigned by `SessionLog::append`.
    pub seq: u64,
    pub at: Ms,
    pub kind: EventKind,
    pub sha256: Option<String>,
    /// Prompt text (user prompts only), at most 4 KB.
    pub text: Option<String>,
    /// Why an excluded prompt was excluded.
    pub excluded_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComposerRecord {
    pub at: Ms,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sighting {
    pub tool: Option<Tool>,
    pub last_seen: Option<Ms>,
    pub last_card_hash: Option<String>,
    pub mod_heartbeat: Option<Ms>,
}

pub struct SessionLog {
    dir: PathBuf,
}

impl SessionLog {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append under the per-session lock; returns the assigned seq. Keeps the
    /// most recent 50 user prompts (rotation under the same lock).
    pub fn append(
        &self,
        at: Ms,
        kind: EventKind,
        sha256: Option<String>,
        text: Option<String>,
        excluded_by: Option<String>,
    ) -> Result<u64> {
        let _ = (at, kind, sha256, text, excluded_by);
        todo!("nuzu: session log")
    }

    pub fn events(&self) -> Result<Vec<SessionEvent>> {
        todo!("nuzu: session log")
    }

    pub fn append_composer(&self, rec: &ComposerRecord) -> Result<()> {
        let _ = rec;
        todo!("nuzu: session log")
    }

    pub fn composers(&self) -> Result<Vec<ComposerRecord>> {
        todo!("nuzu: session log")
    }

    pub fn sighting(&self) -> Result<Sighting> {
        todo!("nuzu: session log")
    }

    pub fn update_sighting(&self, f: impl FnOnce(&mut Sighting)) -> Result<()> {
        let _ = f;
        todo!("nuzu: session log")
    }
}
