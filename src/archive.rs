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

/// Append a complete batch and publish its directory entry before the caller
/// can replace the document that references these rows.
pub(crate) fn append(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .map_err(io_error)?;
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
    file.sync_all().map_err(io_error)?;
    // Retry the directory sync even if a failed first creation left the file.
    crate::store::sync_dir(
        path.parent()
            .ok_or_else(|| Error::new(Reason::InvalidArgument, "archive path has no parent"))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_append_repairs_a_torn_tail() {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-archive-unit-{}-{}",
            std::process::id(),
            crate::model::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("archive.jsonl");
        std::fs::write(&path, b"{broken").unwrap();
        append(&path, b"{\"value\":7}\n").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, b"{broken\n{\"value\":7}\n");
        let mut values = vec![];
        scan::<serde_json::Value>(&path, |value| values.push(value)).unwrap();
        assert_eq!(values, vec![serde_json::json!({"value":7})]);
        assert!(scan::<serde_json::Value>(&path.join("not-a-directory"), |_| {}).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
