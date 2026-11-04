//! Shared by the integration tests that declare `mod common;`.

/// Removes a test's store directory when the test ends, panics included, so a
/// failing run (and every mutant cargo-mutants tries) leaves nothing behind in
/// the temp directory.
pub struct RemoveOnDrop(pub std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
