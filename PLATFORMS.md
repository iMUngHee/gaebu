# Platform verification

## Linux

0.4.0 storage changes, 2026-11-13: Linux 7.2.15-1-omarchy, x86_64,
Btrfs, Rust 1.99.0. `cargo fmt --check`, strict clippy and `cargo test`
pass (293 passed, 0 failed, 3 helper entry points ignored). Native mod tests
pass 26 of 26; plugin validation passes with the existing missing-author and
catalog-icon warnings.

An isolated CLI process flow exercised Schedule acceptance and retirement,
the unit-close guard and explicit transfer, runtime compaction, archived duty
detail, and registration move. MCP tests recover full archived contracts and
invoke compaction. These checks do not install a global binary or modify a
live team's state.

Debug fixtures on this host: 76 items (43 complete, 1 cancelled, 32 open)
leave 4,191 bytes of tombstone overhead; `show --all` over 2,000 archived items
takes about 98 ms. The 10,000-entry runtime fixture measures about 0.43 ms
for boundary-check/append and 71 ms for locked lookup. These are synthetic
local observations, not production latency guarantees. Unix file/directory
sync ordering is reviewed in code; physical power-loss persistence is untested.

Archive fixture checks also pass with a symlinked `TMPDIR` on Linux. A Unix
subprocess regression exercises the read counter and registration-move checks
through that alias. Fixtures use canonical paths to match registration, and
I/O failure tests use invalid paths rather than assuming an OS-specific error
for a file used as a parent. The macOS and Windows CI rerun for this fixture
correction remains pending.

## Windows

Development host: Windows, Rust 1.99.1, `x86_64-pc-windows-gnu`;
Claude Code 2.1.317. Validation is recorded in `docs/verification.md`.
Runtime changes use existing Rust dependencies and the standard OS file lock.

## macOS

macOS 26.4 (arm64, APFS), Rust 1.95.0, 2026-11-11: `cargo fmt --check`,
`cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked`
(223 passed, including the migration and multi-process Schedule lock/recovery
tests) pass, and `claude plugin test mod` passes 25 of 25. Clippy 1.95 rejects
a negated `is_some_and` that newer toolchains accept; `src/identity.rs` uses
`is_none_or` so both pass.

`scripts/install.sh` replaces an installed binary by rename. Copying over it
in place keeps the inode, and macOS then kills the upgraded binary with
SIGKILL on its stale code-signature cache.

## 0.5.0 segmented archives

Linux: shared helper, Schedule and runtime segment/expiry/migration tests run
locally with fmt, strict clippy and the complete Rust suite. Forced interleavings
cover Schedule readers during prefix expiry; unit-only failure injection covers
unlink/sync deferral and publication failure. These do not simulate physical power
loss. Mutation checks cover both implementations; an equal-length extra fsync
mutation in the shared helper is equivalent for retained content.

## 0.5.1 explicit archive healing

Linux: hole recovery, read-only pre-checks, listed/unlisted accounting, partial
unlink/sync failures, retry, legacy migration and failed publication are covered
by the helper and Schedule tests. These are synthetic filesystem failures, not
physical power-loss tests.

## Pending checks

- Windows: implement directory flush in `store::sync_dir` by opening the directory
  with backup semantics and flushing it; verify on a native Windows filesystem
  with the archive tests. Until then power loss can leave an archive hole; recover
  explicitly with `gaebu compact --heal` or `gaebu schedule archive --heal`.
- macOS and Windows: run the 0.5.1 heal/pre-check/partial-failure tests natively.
  Only Linux was executed locally; integration owns the three-platform CI gate.

- macOS and Windows: run 0.5.0 segmented archives, lowered-limit maintenance,
  delete-pending open retries, migration and repair on native filesystems. Only
  Linux was executed locally; parent integration owns the three-platform CI gate.


- macOS and Windows: run the 0.4.1 Schedule schema-2 commitment, pruning,
  migration and held-snapshot truncation tests; only Linux was run locally.

- macOS and Windows: run the 0.4.0 archive, migration, repair, locking and
  native mod tests. Prior release checks below do not cover these changes.
- Windows: directory fsync is unavailable through the portable standard-library
  path; storage retains file-sync/rename guarantees. Verify the new archive
  behavior on a native Windows filesystem.
- Linux and macOS: inspect narrow/wide, short-height, no-color, long Korean
  and ambiguous-width presentation.
- User terminal themes and a live multi-session project: exercise the operations
  desk with real observations; fixture and engine tests do not prove every theme
  or a remote team's behavior.
- Existing free-form `schedule.md` from another computer: inspect its actual
  contents before proposing a format conversion. Unknown prose is preserved by
  refusal; automatic migration of that unseen file is not established.

Concurrent execution from independent Schedule copies on separate computers is
outside the supported single canonical local-artifact contract.
