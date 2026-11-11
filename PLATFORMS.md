# Platform verification

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

## Pending checks

- Linux: run fmt, strict clippy, the complete Rust suite and migration,
  multi-process Schedule lock/recovery tests on the native filesystem.
- Linux: run the mod's native engine tests.
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
