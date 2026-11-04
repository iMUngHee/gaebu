//! Storage (Decision 11a). One JSON state file under an OS lock, plus
//! per-session event logs that need no global lock.
//!
//! Layout under the home (`GAEBU_HOME`, default `~/.gaebu`):
//!
//! ```text
//! state.json            the whole State, replaced atomically
//! state.prev.json       the previous generation (recovery)
//! state.v1.json         the schema-1 original, kept by the v2 migration
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

pub const CLI_DEADLINE: Duration = Duration::from_secs(5);
pub const HOOK_DEADLINE: Duration = Duration::from_secs(1);
pub const SESSION_LOCK_DEADLINE: Duration = Duration::from_millis(500);

pub struct Store {
    home: PathBuf,
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
        let home = home.into();
        fs::create_dir_all(&home).map_err(write_error)?;
        Ok(Store { home })
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
    /// A schema-1 file is migrated in memory only. A newer schema is refused
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
        let (mut state, v1) = match read_state(&path)? {
            Some(loaded) => (loaded.state, loaded.v1_bytes),
            None => (State::default(), None),
        };
        ensure_budget(deadline)?;
        if let Some(original) = v1 {
            // Its own commit, before `f`: a failure here leaves the v1 file
            // as it was and runs no user change.
            self.publish_migration(&mut state, &original, deadline)?;
        }
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
        match loaded.v1_bytes {
            Some(original) => self.publish_migration(&mut state, &original, deadline),
            None => replace(
                &self.home.join("state.json"),
                &serde_json::to_vec(&state).map_err(write_error)?,
            ),
        }
    }

    /// Publish a state migrated from schema 1 under the held state lock:
    /// keep the original once in `state.v1.json`, then write the v2 bytes to
    /// `state.prev.json` and only then to `state.json`. An interruption at any
    /// point leaves `state.json` at schema 1, so the next run repeats this, and
    /// an older binary never finds a schema-1 generation to fall back to once
    /// `state.json` is v2.
    fn publish_migration(
        &self,
        state: &mut State,
        original: &[u8],
        deadline: Instant,
    ) -> Result<()> {
        let backup = self.home.join("state.v1.json");
        if !backup.exists() {
            replace_until(&backup, original, deadline)?;
        }
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
    /// The original bytes when the file was schema 1 (migrated in `state`).
    v1_bytes: Option<Vec<u8>>,
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
        1 => {
            // Schema 1 is schema 2 without the fields that default.
            let mut state: State = serde_json::from_slice(&bytes).map_err(corrupt_error)?;
            migrate_v1(&mut state);
            Ok(Some(Loaded {
                state,
                v1_bytes: Some(bytes),
            }))
        }
        crate::model::SCHEMA_VERSION => Ok(Some(Loaded {
            state: serde_json::from_slice(&bytes).map_err(corrupt_error)?,
            v1_bytes: None,
        })),
        v => Err(Error::new(
            Reason::StoreUnsupported,
            format!("schema_version {v} is newer than this gaebu"),
        )),
    }
}

/// Bring a schema-1 state to schema 2: one open escalation per asker and
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

fn lock(dir: &Path, name: &str, deadline: Duration) -> Result<File> {
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
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    replace_until(path, bytes, Instant::now() + Duration::from_secs(1))
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

fn replace_until(path: &Path, bytes: &[u8], deadline: Instant) -> Result<()> {
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
