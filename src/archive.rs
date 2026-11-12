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

/// `bytes == None` is a dedup hit: reopen and sync the existing file and its
/// directory, since a previous writer may have stopped before either sync.
pub(crate) fn append(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    let mut options = OpenOptions::new();
    options.read(true).append(true);
    if bytes.is_some() {
        options.create(true);
    }
    let mut file = options.open(path).map_err(io_error)?;
    #[cfg(test)]
    OBSERVED.with(|counts| counts.set((counts.get().0 + 1, counts.get().1)));
    if let Some(bytes) = bytes {
        let length = file.metadata().map_err(io_error)?.len();
        if length > 0 {
            file.seek(SeekFrom::End(-1)).map_err(io_error)?;
            let mut last = [0];
            file.read_exact(&mut last).map_err(io_error)?;
            if last[0] != b'\n' {
                file.write_all(b"\n").map_err(io_error)?;
            }
        }
        file.write_all(bytes).map_err(io_error)?;
        file.write_all(b"\n").map_err(io_error)?;
    }
    file.sync_all().map_err(io_error)?;
    #[cfg(test)]
    OBSERVED.with(|counts| counts.set((counts.get().0, counts.get().1 + 1)));
    // Unix requires the directory entry to be durable too, including retries
    // after a failed first creation. Windows retains file-sync guarantees.
    #[cfg(unix)]
    File::open(
        path.parent()
            .ok_or_else(|| Error::new(Reason::InvalidArgument, "archive path has no parent"))?,
    )
    .and_then(|directory| directory.sync_all())
    .map_err(io_error)?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static OBSERVED: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_resyncs_and_append_repairs_a_torn_tail() {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-archive-unit-{}-{}",
            std::process::id(),
            crate::model::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("archive.jsonl");
        std::fs::write(&path, b"{broken").unwrap();
        OBSERVED.with(|counts| counts.set((0, 0)));
        append(&path, Some(b"{\"value\":7}")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, b"{broken\n{\"value\":7}\n");
        append(&path, None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(OBSERVED.with(|counts| counts.get()), (2, 2));
        let mut values = vec![];
        scan::<serde_json::Value>(&path, |value| values.push(value)).unwrap();
        assert_eq!(values, vec![serde_json::json!({"value":7})]);
        assert!(scan::<serde_json::Value>(&path.join("not-a-directory"), |_| {}).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
