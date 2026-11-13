//! Storage (Decision 11a). One JSON state file under an OS lock, plus
//! per-session event logs that need no global lock.
//!
//! Layout under the home (`GAEBU_HOME`, default `~/.gaebu`):
//!
//! ```text
//! state.json            the whole State, replaced atomically
//! state.prev.json       the previous generation (recovery)
//! state.v1.json         the schema-1 original, kept by the v2 migration
//! state.v3.json         the schema-3 original, kept by the v4 migration
//! archive/<team>/{duties,escalations,records}.jsonl  committed history
//! state.lock            OS lock (never deleted)
//! sessions/<id>/events.jsonl    SessionEvent lines (hook writes)
//! sessions/<id>/composer.jsonl  ComposerRecord lines (mod writes via CLI)
//! sessions/<id>/sighting.json   Sighting
//! sessions/<id>/lock            per-session OS lock
//! capture.on            dev switch: hook saves raw stdin to capture/
//! ```

use crate::error::{Error, Reason, Result};
use crate::model::{Ms, State, Tool};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

pub const GLOBAL_MARKERS: &str = "_global";

pub const CLI_DEADLINE: Duration = Duration::from_secs(5);
pub const HOOK_DEADLINE: Duration = Duration::from_secs(1);
pub const SESSION_LOCK_DEADLINE: Duration = Duration::from_millis(500);

pub struct Store {
    home: PathBuf,
    archive_limit: u64,
}

impl Store {
    /// `GAEBU_HOME` if set, else `~/.gaebu`.
    pub fn from_env() -> Result<Store> {
        Self::open(
            std::env::var_os("GAEBU_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(Self::default_home),
        )
    }

    pub fn open(home: impl Into<PathBuf>) -> Result<Store> {
        let raw = std::env::var_os("GAEBU_ARCHIVE_LIMIT_MIB");
        Self::open_with_archive_limit(home, parse_archive_limit(raw.as_deref())?)
    }

    pub fn open_with_archive_limit(home: impl Into<PathBuf>, archive_limit: u64) -> Result<Store> {
        if archive_limit < 8 {
            return Err(Error::new(
                Reason::InvalidArgument,
                "archive_limit must be at least 8 bytes",
            ));
        }
        let home = home.into();
        fs::create_dir_all(&home).map_err(write_error)?;
        Ok(Store {
            home,
            archive_limit,
        })
    }

    pub fn archive_limit(&self) -> u64 {
        self.archive_limit
    }

    /// The default home (`~/.gaebu`), whether or not it exists.
    pub fn default_home() -> PathBuf {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".gaebu")
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// True when this store is the default home (dev seed is refused there).
    pub fn is_default_home(&self) -> bool {
        let absolute = |p: &Path| {
            fs::canonicalize(p)
                .or_else(|_| std::path::absolute(p))
                .unwrap_or_else(|_| p.to_path_buf())
        };
        absolute(&self.home) == absolute(&Self::default_home())
    }

    /// Lock-free read of the current state (falls back to `state.prev.json`
    /// with a warning on stderr when `state.json` does not parse). A missing
    /// file is an empty `State::default()`.
    /// A legacy file is migrated in memory only. A newer schema is refused
    /// without falling back.
    pub fn read(&self) -> Result<State> {
        match read_state(&self.home.join("state.json")) {
            Ok(loaded) => Ok(loaded.map(|l| l.state).unwrap_or_default()),
            Err(err) if err.reason == Reason::StoreCorrupt => {
                eprintln!("gaebu: state.json corrupt; reading state.prev.json (run gaebu repair)");
                Ok(read_state(&self.home.join("state.prev.json"))?
                    .ok_or(err)?
                    .state)
            }
            Err(err) => Err(err),
        }
    }

    /// Read the current generation only. Recovery snapshots cannot prove that
    /// a dispatch never existed in the authoritative runtime.
    pub fn read_current(&self) -> Result<State> {
        read_state(&self.home.join("state.json"))?
            .map(|loaded| loaded.state)
            .ok_or_else(|| Error::new(Reason::NotFound, "current runtime state is missing"))
    }

    /// A fresh install is empty; an existing runtime whose current generation
    /// disappeared or became corrupt is unavailable, not a healthy old snapshot.
    pub fn read_view(&self) -> Result<State> {
        match self.read_current() {
            Err(error)
                if error.reason == Reason::NotFound
                    && !self.home.join("state.prev.json").exists() =>
            {
                Ok(State::default())
            }
            result => result,
        }
    }

    /// Run `f` on the state under the exclusive lock and commit on `Ok`
    /// (bumping `rev`). Refuses with `busy` after `deadline`, `store_corrupt`
    /// when `state.json` does not parse, `store_write_failed` on replace failure.
    /// `deadline` is the total budget, shared by lock wait and both replacements.
    /// Synchronous filesystem calls and `f` cannot be interrupted; expiry is
    /// checked between stages and immediately before publishing the state.
    pub fn transact<T>(
        &self,
        deadline: Duration,
        f: impl FnOnce(&mut State) -> Result<T>,
    ) -> Result<T> {
        let deadline = Instant::now()
            .checked_add(deadline)
            .ok_or_else(|| Error::new(Reason::InvalidArgument, "deadline overflow"))?;
        let _lock = lock(
            &self.home,
            "state.lock",
            deadline.saturating_duration_since(Instant::now()),
        )?;
        ensure_budget(deadline)?;
        clean_tmp(&self.home)?;
        ensure_budget(deadline)?;
        let path = self.home.join("state.json");
        let (mut state, legacy) = match read_state(&path)? {
            Some(loaded) => (loaded.state, loaded.legacy_bytes),
            None => (State::default(), None),
        };
        ensure_budget(deadline)?;
        if let Some((version, original)) = legacy {
            // Its own commit, before `f`: a failure here leaves the legacy file
            // as it was and runs no user change.
            self.publish_migration(&mut state, version, &original, deadline)?;
        }
        initialize_runtime_id(&mut state);
        crate::ops::expire_leases(&mut state, crate::model::now_ms());
        let result = f(&mut state)?;
        crate::ops::reroute_escalations(&mut state, crate::model::now_ms());
        ensure_budget(deadline)?;
        state.rev = state
            .rev
            .checked_add(1)
            .ok_or_else(|| Error::new(Reason::StoreWriteFailed, "revision overflow"))?;
        let bytes = serde_json::to_vec(&state).map_err(write_error)?;
        ensure_budget(deadline)?;
        if path.exists() {
            let prev = fs::read(&path).map_err(write_error)?;
            replace_until(&self.home.join("state.prev.json"), &prev, deadline)?;
        }
        replace_until(&path, &bytes, deadline)?;
        Ok(result)
    }

    /// Restore `state.json` from `state.prev.json` (`gaebu repair`). A newer
    /// schema in `state.json` is never overwritten; a schema-1 previous
    /// generation goes through the same migration as a schema-1 state.
    pub fn repair(&self) -> Result<()> {
        let _lock = lock(&self.home, "state.lock", CLI_DEADLINE)?;
        let deadline = Instant::now() + CLI_DEADLINE;
        if let Err(err) = read_state(&self.home.join("state.json"))
            && err.reason == Reason::StoreUnsupported
        {
            return Err(err);
        }
        let loaded = read_state(&self.home.join("state.prev.json"))?
            .ok_or_else(|| Error::new(Reason::StoreCorrupt, "previous state missing"))?;
        let mut state = loaded.state;
        match loaded.legacy_bytes {
            Some((version, original)) => {
                self.publish_migration(&mut state, version, &original, deadline)
            }
            None => replace_until(
                &self.home.join("state.json"),
                &serde_json::to_vec(&state).map_err(write_error)?,
                deadline,
            ),
        }
    }

    /// Preserve each legacy generation once, then publish current-schema prev
    /// before current state so older binaries cannot recover past new guards.
    fn publish_migration(
        &self,
        state: &mut State,
        version: u32,
        original: &[u8],
        deadline: Instant,
    ) -> Result<()> {
        let backup = self.home.join(format!("state.v{version}.json"));
        if !backup.exists() {
            replace_until(&backup, original, deadline)?;
        }
        initialize_runtime_id(state);
        state.rev = state
            .rev
            .checked_add(1)
            .ok_or_else(|| Error::new(Reason::StoreWriteFailed, "revision overflow"))?;
        let bytes = serde_json::to_vec(&*state).map_err(write_error)?;
        replace_until(&self.home.join("state.prev.json"), &bytes, deadline)?;
        replace_until(&self.home.join("state.json"), &bytes, deadline)
    }

    /// The last hook sighting of `session_id`, when one was recorded.
    pub fn last_seen(&self, session_id: &str) -> Option<Ms> {
        self.session(session_id).sighting().ok()?.last_seen
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

fn parse_archive_limit(raw: Option<&std::ffi::OsStr>) -> Result<u64> {
    let invalid = || {
        Error::new(
            Reason::InvalidArgument,
            "GAEBU_ARCHIVE_LIMIT_MIB must be a positive integer in MiB within the supported byte range",
        )
    };
    let Some(raw) = raw else {
        return Ok(64 * 1024 * 1024);
    };
    raw.to_str()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .and_then(|n| n.checked_mul(1024 * 1024))
        .ok_or_else(invalid)
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
    /// Last time the mod's prompt.compose hook added the card section, so it
    /// reached the model. Never set where another plugin passes
    /// prompt.compose on past the mod.
    pub compose_heartbeat: Option<Ms>,
    pub pending_checked_at: Option<Ms>,
    pub pending_checked_rev: Option<u64>,
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
        let _lock = lock(&self.dir, "lock", SESSION_LOCK_DEADLINE)?;
        let mut events = self.events()?;
        let seq = events.last().map_or(1, |e| e.seq.saturating_add(1));
        let text = text.map(|mut s| {
            let mut end = s.len().min(4096);
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s
        });
        events.push(SessionEvent {
            seq,
            at,
            kind,
            sha256,
            text,
            excluded_by,
        });
        if let Some((start, _)) = events
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, e)| e.kind == EventKind::UserPrompt)
            .nth(49)
        {
            events.drain(..start);
        }
        let mut bytes = Vec::new();
        for event in &events {
            serde_json::to_writer(&mut bytes, event).map_err(write_error)?;
            bytes.push(b'\n');
        }
        replace(&self.dir.join("events.jsonl"), &bytes)?;
        if kind == EventKind::UserPrompt {
            for entry in fs::read_dir(&self.dir).map_err(write_error)? {
                let entry = entry.map_err(write_error)?;
                if marker_at(&entry.file_name().to_string_lossy()).is_some_and(|t| t < at) {
                    match fs::remove_file(entry.path()) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(write_error(e)),
                    }
                }
            }
        }
        Ok(seq)
    }

    pub fn events(&self) -> Result<Vec<SessionEvent>> {
        read_lines(&self.dir.join("events.jsonl"))
    }

    /// Fail closed when a prompt event could not be written under its lock.
    pub fn mark_unrecorded(&self, at: Ms) -> Result<()> {
        fs::create_dir_all(&self.dir).map_err(write_error)?;
        let path = self.dir.join(format!(
            "unrecorded-{at}-{}-{}",
            std::process::id(),
            TMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(write_error)?;
        file.sync_all().map_err(write_error)
    }

    /// A marker at the same millisecond is conservatively considered later.
    pub fn unrecorded_after(&self, at: Ms) -> Result<bool> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(write_error(e)),
        };
        for entry in entries {
            let entry = entry.map_err(write_error)?;
            if marker_at(&entry.file_name().to_string_lossy()).is_some_and(|t| t >= at) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn append_composer(&self, rec: &ComposerRecord) -> Result<()> {
        let _lock = lock(&self.dir, "lock", SESSION_LOCK_DEADLINE)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("composer.jsonl"))
            .map_err(write_error)?;
        let mut bytes = serde_json::to_vec(rec).map_err(write_error)?;
        bytes.push(b'\n');
        file.write_all(&bytes).map_err(write_error)?;
        file.sync_all().map_err(write_error)
    }

    pub fn composers(&self) -> Result<Vec<ComposerRecord>> {
        read_lines(&self.dir.join("composer.jsonl"))
    }

    pub fn sighting(&self) -> Result<Sighting> {
        match fs::read(self.dir.join("sighting.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(corrupt_error),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Sighting::default()),
            Err(e) => Err(write_error(e)),
        }
    }

    pub fn update_sighting(&self, f: impl FnOnce(&mut Sighting)) -> Result<()> {
        let _lock = lock(&self.dir, "lock", SESSION_LOCK_DEADLINE)?;
        let mut sighting = self.sighting()?;
        f(&mut sighting);
        replace(
            &self.dir.join("sighting.json"),
            &serde_json::to_vec(&sighting).map_err(write_error)?,
        )
    }
}

fn write_error(e: impl std::fmt::Display) -> Error {
    Error::new(Reason::StoreWriteFailed, e.to_string())
}

fn marker_at(name: &str) -> Option<Ms> {
    name.strip_prefix("unrecorded-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}
fn corrupt_error(e: impl std::fmt::Display) -> Error {
    Error::new(Reason::StoreCorrupt, e.to_string())
}

struct Loaded {
    state: State,
    /// Original legacy bytes and their schema before the in-memory migration.
    legacy_bytes: Option<(u32, Vec<u8>)>,
}

/// Decide the schema before parsing the whole state, so a newer format that
/// this model cannot parse is refused as unsupported instead of corrupt.
fn read_state(path: &Path) -> Result<Option<Loaded>> {
    #[derive(Deserialize)]
    struct Probe {
        schema_version: u32,
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(write_error(e)),
    };
    let probe: Probe = serde_json::from_slice(&bytes).map_err(corrupt_error)?;
    match probe.schema_version {
        version @ (1..=3) => {
            // Legacy additions default; schema 1 also needs notice/evidence normalization.
            let mut state: State = serde_json::from_slice(&bytes).map_err(corrupt_error)?;
            if version == 1 {
                migrate_v1(&mut state);
            }
            state.schema_version = crate::model::SCHEMA_VERSION;
            Ok(Some(Loaded {
                state,
                legacy_bytes: Some((version, bytes)),
            }))
        }
        crate::model::SCHEMA_VERSION => Ok(Some(Loaded {
            state: serde_json::from_slice(&bytes).map_err(corrupt_error)?,
            legacy_bytes: None,
        })),
        v => Err(Error::new(
            Reason::StoreUnsupported,
            format!("schema_version {v} is newer than this gaebu"),
        )),
    }
}

/// Normalize legacy commitments: one open escalation per asker and
/// duty (the newest wins), and a dedupe key for every queued notice.
fn migrate_v1(state: &mut State) {
    use crate::model::EscalationStatus;
    type Key = (String, Option<String>);
    let mut newest: std::collections::BTreeMap<Key, (u64, u64, String)> = Default::default();
    for e in state.escalations.values() {
        if e.status != EscalationStatus::Open {
            continue;
        }
        let rank = (e.created_at, id_number(&e.id));
        let entry = newest.entry((e.from.clone(), e.duty.clone())).or_insert((
            rank.0,
            rank.1,
            e.id.clone(),
        ));
        if rank > (entry.0, entry.1) {
            *entry = (rank.0, rank.1, e.id.clone());
        }
    }
    for e in state.escalations.values_mut() {
        if e.status != EscalationStatus::Open {
            continue;
        }
        let keep = &newest[&(e.from.clone(), e.duty.clone())].2;
        if &e.id != keep {
            e.status = EscalationStatus::Superseded;
            e.superseded_by = Some(keep.clone());
        }
    }
    for n in &mut state.outbox {
        n.text = crate::model::notice_key(&n.body);
    }
    state.schema_version = crate::model::SCHEMA_VERSION;
}

fn id_number(id: &str) -> u64 {
    id.rsplit_once('-')
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or(0)
}

fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(write_error(e)),
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(corrupt_error))
        .collect()
}

pub(crate) fn lock(dir: &Path, name: &str, deadline: Duration) -> Result<File> {
    fs::create_dir_all(dir).map_err(write_error)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(name))
        .map_err(write_error)?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                if start.elapsed() >= deadline {
                    return Err(Error::new(Reason::Busy, "lock deadline exceeded"));
                }
                std::thread::sleep(
                    Duration::from_millis(10).min(deadline.saturating_sub(start.elapsed())),
                );
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(write_error(e)),
        }
    }
}

static TMP_ID: AtomicU64 = AtomicU64::new(0);

fn initialize_runtime_id(state: &mut State) {
    if state.runtime_id.is_empty() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        state.runtime_id = format!(
            "rt-{}-{nanos:x}-{}",
            std::process::id(),
            TMP_ID.fetch_add(1, Ordering::Relaxed)
        );
    }
}
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    replace_file_until(path, bytes, Instant::now() + Duration::from_secs(1))
}

fn ensure_budget(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(Error::new(
            Reason::StoreWriteFailed,
            "transaction deadline exceeded before state publication",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn replace_until(path: &Path, bytes: &[u8], deadline: Instant) -> Result<()> {
    replace_file_until(path, bytes, deadline)?;
    sync_dir(path.parent().unwrap_or_else(|| Path::new(".")))
}

fn replace_file_until(path: &Path, bytes: &[u8], deadline: Instant) -> Result<()> {
    ensure_budget(deadline)?;
    let tmp = path.with_file_name(format!(
        "{}.tmp-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        TMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(write_error)?;
        file.write_all(bytes).map_err(write_error)?;
        ensure_budget(deadline)?;
        file.sync_all().map_err(write_error)?;
        drop(file);
        let deadline = deadline.min(Instant::now() + Duration::from_secs(1));
        loop {
            ensure_budget(deadline)?;
            match fs::rename(&tmp, path) {
                Ok(()) => return Ok(()),
                Err(e)
                    if (e.kind() == io::ErrorKind::PermissionDenied
                        || cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)))
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(
                        Duration::from_millis(50)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    )
                }
                Err(e) => return Err(write_error(e)),
            }
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Publish directory entries after file creation or rename on Unix.
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|dir| dir.sync_all())
        .map_err(write_error)?;
    // Windows has no portable directory-fsync API in std.
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(all(test, unix))]
    SYNCED_DIRS.with_borrow_mut(|paths| paths.push(path.to_path_buf()));
    Ok(())
}

#[cfg(all(test, unix))]
std::thread_local! {
    static SYNCED_DIRS: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn directory_sync_propagates_missing_parent_error() {
        let path = std::env::temp_dir().join(format!(
            "gaebu-sync-missing-{}-{}",
            std::process::id(),
            TMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        assert_eq!(
            sync_dir(&path).unwrap_err().reason,
            Reason::StoreWriteFailed
        );
    }

    #[test]
    fn state_replacements_sync_parent_but_session_replacements_do_not() {
        let home = std::env::temp_dir().join(format!(
            "gaebu-sync-policy-{}-{}",
            std::process::id(),
            TMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::open(&home).unwrap();
        SYNCED_DIRS.with_borrow_mut(Vec::clear);
        store.transact(CLI_DEADLINE, |_| Ok(())).unwrap();
        store.transact(CLI_DEADLINE, |_| Ok(())).unwrap();
        store.repair().unwrap();
        let runtime = SYNCED_DIRS.replace(Vec::new());
        let session = store.session("test");
        session
            .append(1, EventKind::UserPrompt, None, Some("prompt".into()), None)
            .unwrap();
        session.update_sighting(|s| s.last_seen = Some(1)).unwrap();
        let sessions = SYNCED_DIRS.replace(Vec::new());
        fs::remove_dir_all(&home).unwrap();
        assert_eq!(runtime, vec![home; 4]);
        assert!(sessions.is_empty());
    }
}

fn clean_tmp(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(write_error)? {
        let entry = entry.map_err(write_error)?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("state.json.tmp-")
        {
            continue;
        }
        let meta = entry.metadata().map_err(write_error)?;
        if meta.is_file()
            && meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(60))
        {
            fs::remove_file(entry.path()).map_err(write_error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod archive_limit_tests {
    use super::*;

    #[test]
    fn archive_limit_parser() {
        for (raw, want) in [
            (None, Some(64 * 1024 * 1024)),
            (Some("1"), Some(1024 * 1024)),
            (Some("64"), Some(64 * 1024 * 1024)),
            (Some("0"), None),
            (Some("abc"), None),
            (Some("18446744073709551615"), None),
        ] {
            let result = parse_archive_limit(raw.map(std::ffi::OsStr::new));
            match want {
                Some(bytes) => assert_eq!(result.unwrap(), bytes),
                None => {
                    let error = result.unwrap_err();
                    assert_eq!(error.reason, Reason::InvalidArgument);
                    assert!(error.message.contains("GAEBU_ARCHIVE_LIMIT_MIB"));
                }
            }
        }
    }
}
