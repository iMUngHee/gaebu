//! Durable JSONL writes and streaming scans. Policy and commit boundaries belong
//! to the caller; malformed, unreferenced lines may be left by interrupted writes.

use crate::{Error, Reason, Result};
use serde::de::DeserializeOwned;
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
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
    let mut file = open_prefix(path, committed, false)?;
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
    OpenOptions::new()
        .read(true)
        .write(write)
        .create(write && committed == 0)
        .truncate(false)
        .open(path)
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
}
