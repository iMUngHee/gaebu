//! Durable JSONL writes and streaming scans. Policy and commit boundaries belong
//! to the caller; malformed, unreferenced lines may be left by interrupted writes.

#![expect(
    dead_code,
    reason = "Segment primitives form the internal archive interface."
)]

use crate::{Error, Reason, Result};
use serde::de::DeserializeOwned;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

fn io_error(error: std::io::Error) -> Error {
    Error::new(Reason::StoreWriteFailed, error.to_string())
}

pub(crate) fn scan<T: DeserializeOwned>(path: &Path, mut visit: impl FnMut(T)) -> Result<()> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    for line in BufReader::new(file).split(b'\n') {
        if let Ok(value) = serde_json::from_slice(&line.map_err(io_error)?) {
            visit(value);
        }
    }
    Ok(())
}

/// Read only a published prefix; incomplete lines are corruption, never leftovers.
pub(crate) fn scan_prefix(
    path: &Path,
    committed: u64,
    mut visit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    if committed == 0 {
        return Ok(());
    }
    scan_file_prefix(open_prefix(path, committed, false)?, committed, &mut visit)
}

fn scan_file_prefix(
    mut file: File,
    committed: u64,
    mut visit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    boundary(&mut file, committed)?;
    let mut reader = BufReader::new(file.take(committed));
    let mut consumed = 0;
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).map_err(io_error)? > 0 {
        consumed += line.len() as u64;
        if line.last() != Some(&b'\n') {
            return Err(corrupt("archive row does not end in newline"));
        }
        visit(&line)?;
        line.clear();
    }
    if consumed != committed {
        return Err(corrupt("archive is shorter than its committed boundary"));
    }
    Ok(())
}

fn corrupt(message: &str) -> Error {
    Error::new(Reason::StoreCorrupt, message)
}

fn open_prefix(path: &Path, committed: u64, write: bool) -> Result<File> {
    retry_open(
        || {
            OpenOptions::new()
                .read(true)
                .write(write)
                .create(write && committed == 0)
                .truncate(false)
                .open(path)
        },
        cfg!(windows),
    )
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            corrupt("committed archive is missing")
        } else {
            io_error(error)
        }
    })
}

fn boundary(file: &mut File, committed: u64) -> Result<()> {
    if file.metadata().map_err(io_error)?.len() < committed {
        return Err(corrupt("archive is shorter than its committed boundary"));
    }
    if committed > 0 {
        file.seek(SeekFrom::Start(committed - 1))
            .map_err(io_error)?;
        let mut last = [0];
        file.read_exact(&mut last).map_err(io_error)?;
        if last != *b"\n" {
            return Err(corrupt(
                "archive committed boundary does not end in newline",
            ));
        }
    }
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    Ok(())
}

/// Validate before truncating so a missing or short prefix cannot be extended.
pub(crate) fn append_prefix(path: &Path, committed: u64, bytes: &[u8]) -> Result<()> {
    let mut file = open_prefix(path, committed, true)?;
    boundary(&mut file, committed)?;
    file.set_len(committed).map_err(io_error)?;
    file.seek(SeekFrom::Start(committed)).map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    crate::store::sync_dir(
        path.parent()
            .ok_or_else(|| corrupt("archive path has no parent"))?,
    )
}

const OPEN_ATTEMPTS: usize = 6;

fn retry_open(
    mut open: impl FnMut() -> std::io::Result<File>,
    retry_permission: bool,
) -> std::io::Result<File> {
    for attempt in 1..=OPEN_ATTEMPTS {
        match open() {
            Err(error)
                if retry_permission
                    && error.kind() == std::io::ErrorKind::PermissionDenied
                    && attempt < OPEN_ATTEMPTS =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            result => return result,
        }
    }
    unreachable!("the final open attempt returns its result")
}

fn open_optional(path: &Path) -> Result<Option<File>> {
    match retry_open(|| File::open(path), cfg!(windows)) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

pub(crate) trait Segment {
    fn number(&self) -> u64;
    fn bytes(&self) -> u64;
}

pub(crate) fn segment_path(base: &Path, number: u64) -> PathBuf {
    if number == 1 {
        base.to_owned()
    } else {
        base.with_extension(format!("{number}.jsonl"))
    }
}

fn segment_number(base: &Path, name: &std::ffi::OsStr) -> Option<u64> {
    if Some(name) == base.file_name() {
        return Some(1);
    }
    let number = name
        .as_encoded_bytes()
        .strip_prefix(base.file_stem()?.as_encoded_bytes())?
        .strip_prefix(b".")?
        .strip_suffix(b".jsonl")?;
    let number = std::str::from_utf8(number).ok()?;
    let parsed = number.parse::<u64>().ok()?;
    (parsed > 1 && parsed.to_string() == number).then_some(parsed)
}

pub(crate) fn expired_prefix<S: Segment>(base: &Path, segments: &[S]) -> Result<usize> {
    let mut previous = 0;
    let mut present = Vec::new();
    let mut expired = 0;
    for (index, segment) in segments.iter().enumerate() {
        if segment.number() <= previous || segment.bytes() == 0 {
            return Err(corrupt("invalid archive segment bookkeeping"));
        }
        previous = segment.number();
        if open_optional(&segment_path(base, segment.number()))?.is_some() {
            present.push(segment.number());
        } else {
            // A reader may have observed an older file before a writer expired it.
            // Recheck only those observations before calling a missing file a hole.
            for number in &present {
                if open_optional(&segment_path(base, *number))?.is_some() {
                    return Err(corrupt(
                        "archive has a missing segment after a retained segment",
                    ));
                }
            }
            present.clear();
            expired = index + 1;
        }
    }
    Ok(expired)
}

fn parent(base: &Path) -> Result<&Path> {
    base.parent()
        .ok_or_else(|| corrupt("archive path has no parent"))
}

fn trim_last<S: Segment>(base: &Path, segments: &[S], expired: usize) -> Result<()> {
    if expired < segments.len() {
        let segment = segments
            .last()
            .ok_or_else(|| corrupt("missing archive segment"))?;
        let mut file = open_prefix(&segment_path(base, segment.number()), segment.bytes(), true)?;
        boundary(&mut file, segment.bytes())?;
        if file.metadata().map_err(io_error)?.len() > segment.bytes() {
            file.set_len(segment.bytes()).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
        }
    }
    Ok(())
}

/// The returned prefix can leave bookkeeping only after this directory sync.
pub(crate) fn prepare_segments<S: Segment>(base: &Path, segments: &[S]) -> Result<usize> {
    crate::store::sync_dir(parent(base)?)?;
    let expired = expired_prefix(base, segments)?;
    trim_last(base, segments, expired)?;
    Ok(expired)
}

#[derive(Debug)]
pub(crate) struct Appended {
    pub number: u64,
    pub bytes: u64,
    pub created: bool,
}

/// Callers normalize the expired prefix before choosing a segment for new rows.
pub(crate) fn append_segment<S: Segment>(
    base: &Path,
    segments: &[S],
    next: &mut u64,
    segment_bytes: u64,
    bytes: &[u8],
) -> Result<Appended> {
    if bytes.is_empty() {
        return Err(Error::new(
            Reason::InvalidArgument,
            "an archive append must contain rows",
        ));
    }
    if *next == 0 || segments.last().is_some_and(|s| s.number() >= *next) {
        return Err(corrupt("invalid next archive segment number"));
    }
    trim_last(base, segments, 0)?;
    let append_to = segments.last().filter(|s| s.bytes() < segment_bytes);
    let (number, committed, created) = match append_to {
        Some(segment) => (segment.number(), segment.bytes(), false),
        None => (*next, 0, true),
    };
    let following = if created {
        next.checked_add(1)
            .ok_or_else(|| corrupt("archive segment numbers exhausted"))?
    } else {
        *next
    };
    let total = committed
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| corrupt("archive byte count overflow"))?;
    append_prefix(&segment_path(base, number), committed, bytes)?;
    *next = following;
    Ok(Appended {
        number,
        bytes: total,
        created,
    })
}

#[derive(Debug)]
pub(crate) struct Enforcement<S> {
    pub expired: Vec<S>,
    pub deferred: Option<String>,
}

pub(crate) fn enforce_limit<S: Segment>(
    base: &Path,
    segments: &mut Vec<S>,
    limit: u64,
) -> Result<Enforcement<S>> {
    enforce_with(
        base,
        segments,
        limit,
        || crate::store::sync_dir(parent(base)?),
        |path| fs::remove_file(path),
    )
}

fn enforce_with<S: Segment>(
    base: &Path,
    segments: &mut Vec<S>,
    limit: u64,
    mut sync: impl FnMut() -> Result<()>,
    mut unlink: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<Enforcement<S>> {
    let mut result = Enforcement {
        expired: vec![],
        deferred: None,
    };
    if let Err(error) = sync() {
        result.deferred = Some(error.message);
        return Ok(result);
    }
    let expired = expired_prefix(base, segments)?;
    trim_last(base, segments, expired)?;
    result.expired.extend(segments.drain(..expired));
    let mut total = segments.iter().try_fold(0u64, |sum, s| {
        sum.checked_add(s.bytes())
            .ok_or_else(|| corrupt("archive byte count overflow"))
    })?;
    while segments.len() > 1 && (total > limit || segments.len() > 16) {
        let path = segment_path(base, segments[0].number());
        if let Err(error) = unlink(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            result.deferred = Some(error.to_string());
            break;
        }
        if let Err(error) = sync() {
            result.deferred = Some(error.message);
            break;
        }
        let removed = segments.remove(0);
        total -= removed.bytes();
        result.expired.push(removed);
    }
    Ok(result)
}

/// False means this segment disappeared as part of an expired oldest prefix.
pub(crate) fn scan_segment<S: Segment>(
    base: &Path,
    segments: &[S],
    index: usize,
    visit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<bool> {
    let segment = segments
        .get(index)
        .ok_or_else(|| corrupt("invalid archive segment index"))?;
    match open_optional(&segment_path(base, segment.number()))? {
        Some(file) => {
            scan_file_prefix(file, segment.bytes(), visit)?;
            Ok(true)
        }
        None => {
            for older in &segments[..index] {
                if open_optional(&segment_path(base, older.number()))?.is_some() {
                    return Err(corrupt(
                        "archive has a missing segment after a retained segment",
                    ));
                }
            }
            Ok(false)
        }
    }
}

pub(crate) fn remove_unlisted<S: Segment>(base: &Path, segments: &[S]) -> Result<Option<String>> {
    let listed: std::collections::BTreeSet<_> = segments.iter().map(Segment::number).collect();
    let mut extra = std::collections::BTreeMap::new();
    for entry in fs::read_dir(parent(base)?).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if let Some(number) = segment_number(base, &entry.file_name())
            && !listed.contains(&number)
        {
            extra.insert(number, entry.path());
        }
    }
    if let Err(error) = crate::store::sync_dir(parent(base)?) {
        return Ok(Some(error.message));
    }
    for path in extra.values() {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Ok(Some(error.to_string()));
        }
        if let Err(error) = crate::store::sync_dir(parent(base)?) {
            return Ok(Some(error.message));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_skips_malformed_rows_and_missing_files() {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-archive-unit-{}-{}",
            std::process::id(),
            crate::model::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("archive.jsonl");
        std::fs::write(&path, b"{broken\n{\"value\":7}\n").unwrap();
        let mut values = vec![];
        scan::<serde_json::Value>(&path, |value| values.push(value)).unwrap();
        assert_eq!(values, vec![serde_json::json!({"value":7})]);
        // A file used as a parent can report NotFound on Windows. An invalid
        // path tests error propagation independently of that OS mapping.
        assert!(scan::<serde_json::Value>(&dir.join("invalid\0path"), |_| {}).is_err());
        assert!(scan::<serde_json::Value>(&dir, |_| {}).is_err());
        scan::<serde_json::Value>(&dir.join("missing.jsonl"), |_| {
            panic!("a missing archive must not produce entries")
        })
        .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[derive(Clone, Debug, PartialEq)]
    struct SegmentRecord {
        n: u64,
        bytes: u64,
    }
    impl Segment for SegmentRecord {
        fn number(&self) -> u64 {
            self.n
        }
        fn bytes(&self) -> u64 {
            self.bytes
        }
    }

    fn fixture() -> (crate::store::Store, std::path::PathBuf) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gaebu-segments-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let store = crate::store::Store::open_with_archive_limit(&dir, 64).unwrap();
        (store, dir.join("history.jsonl"))
    }

    #[test]
    fn segment_paths_match_only_canonical_names() {
        let base = Path::new("dir/schedule.md.archive.jsonl");
        assert_eq!(segment_path(base, 1), base);
        assert_eq!(
            segment_path(base, 12),
            Path::new("dir/schedule.md.archive.12.jsonl")
        );
        for (name, want) in [
            ("schedule.md.archive.jsonl", Some(1)),
            ("schedule.md.archive.12.jsonl", Some(12)),
            ("schedule.md.archive.01.jsonl", None),
            ("schedule.md.archive.1.jsonl", None),
            ("schedule.md.archive.0.jsonl", None),
            ("schedule.md.archive.2.jsonl.tmp", None),
            ("another.2.jsonl", None),
        ] {
            assert_eq!(segment_number(base, std::ffi::OsStr::new(name)), want);
        }
    }

    #[test]
    fn segments_validate_truncate_roll_and_keep_prefix() {
        let (store, base) = fixture();
        std::fs::write(&base, b"one\ntail").unwrap();
        let segments = vec![SegmentRecord { n: 1, bytes: 4 }];
        assert_eq!(prepare_segments(&base, &segments).unwrap(), 0);
        assert_eq!(std::fs::read(&base).unwrap(), b"one\n");
        let mut next = 2;
        let appended = append_segment(&base, &segments, &mut next, 4, b"two\n").unwrap();
        assert_eq!(
            (appended.number, appended.bytes, appended.created),
            (2, 4, true)
        );
        assert_eq!(next, 3);
        assert_eq!(std::fs::read(&base).unwrap(), b"one\n");
        assert_eq!(std::fs::read(segment_path(&base, 2)).unwrap(), b"two\n");
        std::fs::write(&base, b"bad").unwrap();
        assert_eq!(
            append_segment(&base, &segments, &mut next, 4, b"new\n")
                .unwrap_err()
                .reason,
            Reason::StoreCorrupt
        );
        assert!(!segment_path(&base, 3).exists());
        std::fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn prefix_expiry_and_holes_are_distinct() {
        let (store, base) = fixture();
        let segments: Vec<_> = (1..=3).map(|n| SegmentRecord { n, bytes: 2 }).collect();
        std::fs::write(segment_path(&base, 3), b"3\n").unwrap();
        assert_eq!(expired_prefix(&base, &segments).unwrap(), 2);
        let mut values = vec![];
        assert!(
            !scan_segment(&base, &segments, 0, |line| {
                values.push(line.to_vec());
                Ok(())
            })
            .unwrap()
        );
        assert!(
            scan_segment(&base, &segments, 2, |line| {
                values.push(line.to_vec());
                Ok(())
            })
            .unwrap()
        );
        assert_eq!(values, vec![b"3\n".to_vec()]);
        std::fs::write(&base, b"1\n").unwrap();
        assert_eq!(
            expired_prefix(&base, &segments).unwrap_err().reason,
            Reason::StoreCorrupt
        );
        assert_eq!(
            scan_segment(&base, &segments, 1, |_| Ok(()))
                .unwrap_err()
                .reason,
            Reason::StoreCorrupt
        );
        std::fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn enforcement_syncs_before_unlinking_again() {
        for restore in [false, true] {
            let (store, base) = fixture();
            let mut segments: Vec<_> = (1..=3)
                .map(|n| {
                    std::fs::write(segment_path(&base, n), b"x\n").unwrap();
                    SegmentRecord { n, bytes: 2 }
                })
                .collect();
            let calls = std::cell::Cell::new(0);
            let first = enforce_with(
                &base,
                &mut segments,
                2,
                || {
                    calls.set(calls.get() + 1);
                    if calls.get() == 2 {
                        Err(io_error(std::io::Error::other("sync failed")))
                    } else {
                        Ok(())
                    }
                },
                |path| std::fs::remove_file(path),
            )
            .unwrap();
            assert!(first.deferred.is_some());
            assert!(first.expired.is_empty());
            assert_eq!(segments.len(), 3);
            assert!(!base.exists());
            let failed = enforce_with(
                &base,
                &mut segments,
                2,
                || Err(io_error(std::io::Error::other("sync still failing"))),
                |_| panic!("must not unlink before sync"),
            )
            .unwrap();
            assert!(failed.expired.is_empty());
            assert_eq!(segments.len(), 3);
            if restore {
                std::fs::write(&base, b"x\n").unwrap();
            }
            let events = std::cell::RefCell::new(vec![]);
            let done = enforce_with(
                &base,
                &mut segments,
                2,
                || {
                    events.borrow_mut().push("sync");
                    Ok(())
                },
                |p| {
                    events.borrow_mut().push("unlink");
                    std::fs::remove_file(p)
                },
            )
            .unwrap();
            assert_eq!(events.borrow()[0], "sync");
            assert_eq!(done.expired.len(), 2);
            assert!(done.deferred.is_none());
            assert_eq!(segments, vec![SegmentRecord { n: 3, bytes: 2 }]);
            std::fs::remove_dir_all(store.home()).unwrap();
        }
    }

    #[test]
    fn pending_delete_open_error_is_not_corruption() {
        let calls = std::cell::Cell::new(0);
        let error = retry_open(
            || {
                calls.set(calls.get() + 1);
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            },
            true,
        )
        .unwrap_err();
        assert_eq!(calls.get(), OPEN_ATTEMPTS);
        assert_eq!(io_error(error).reason, Reason::StoreWriteFailed);
        let (store, base) = fixture();
        std::fs::write(&base, b"x\n").unwrap();
        let calls = std::cell::Cell::new(0);
        let file = retry_open(
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                } else {
                    File::open(&base)
                }
            },
            true,
        )
        .unwrap();
        drop(file);
        assert_eq!(calls.get(), 2);
        std::fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn explicit_cleanup_removes_only_unlisted_segments() {
        let (store, base) = fixture();
        for n in [1, 2, 9, 42] {
            std::fs::write(segment_path(&base, n), b"x\n").unwrap();
        }
        let unrelated = store.home().join("history.2.jsonl.tmp");
        std::fs::write(&unrelated, b"keep").unwrap();
        let segments = vec![SegmentRecord { n: 2, bytes: 2 }];
        assert!(remove_unlisted(&base, &segments).unwrap().is_none());
        assert!(segment_path(&base, 2).exists());
        assert!(unrelated.exists());
        for n in [1, 9, 42] {
            assert!(!segment_path(&base, n).exists());
        }
        std::fs::remove_dir_all(store.home()).unwrap();
    }
}
