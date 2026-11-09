# Platform verification

## Windows

Development host: Windows, Rust 1.99.1, `x86_64-pc-windows-gnu`;
Claude Code 2.1.317. Validation is recorded in `docs/verification.md`.
Runtime changes use existing Rust dependencies and the standard OS file lock.

## Pending checks

- Linux and macOS: run fmt, strict clippy, the complete Rust suite and migration,
  multi-process Schedule lock/recovery tests on the native filesystems.
- Linux and macOS: run the mod's native engine tests and inspect narrow/wide,
  short-height, no-color, long Korean and ambiguous-width presentation.
- User terminal themes and a live multi-session project: exercise the operations
  desk with real observations; fixture and engine tests do not prove every theme
  or a remote team's behavior.
- Existing free-form `schedule.md` from another computer: inspect its actual
  contents before proposing a format conversion. Unknown prose is preserved by
  refusal; automatic migration of that unseen file is not established.

Concurrent execution from independent Schedule copies on separate computers is
outside the supported single canonical local-artifact contract.
