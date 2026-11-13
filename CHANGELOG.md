# Changelog

Every release has an entry here, newest first, written in the commit that
bumps the version. `tests/changelog.rs` fails while the version in
`Cargo.toml` has no `## <version>` heading below, and the mod's
`plugin.json` must carry the same version.

Each entry says what changed for someone running Gaebu: commands, refusals,
cards, state, the mod. It also says whether an older binary can still read
the state, and anything to do after upgrading.

## 0.5.0 — 2026-11-14

### Changed

- Archive expiry permanently deletes the oldest history by default once each
  Schedule or team/stream exceeds 64 MiB. Set `GAEBU_ARCHIVE_LIMIT_MIB` to a
  positive integer, raise it before upgrading, or back up the files when history
  must be kept. The newest segment is retained, including a sole oversized
  legacy segment. Failed deletion/sync defers enforcement and is reported.
- Schedule schema 3 and runtime schema 5 list numbered JSONL segments. Existing
  archive filenames remain segment 1; later files use `.<n>.jsonl`. New writes
  validate and trim the last committed prefix before append or rollover.
  Explicit `schedule archive` and `compact` apply a lowered limit without new
  rows and remove unlisted files left by failed publication or repair.
- Missing oldest prefixes, including a wholly missing archive, are treated as
  expired history. A missing file behind an older retained file is corruption.
  Schedule item detail reports expiry; dependencies on expired tombstone-less
  items are refused rather than assumed complete. Retained tombstones still
  satisfy dependencies and duty guards even after their detail rows expire.
- Schedule lookup scans candidate segments by item bounds; `--all` and moves
  verify all retained segments and duplicate identities. Moves require every
  listed segment at the destination. `counts.retired` counts retained rows;
  `counts.expired` counts expired rows. Runtime expired duty lookup returns
  `not_found`: `duty not found in hot state or retained team archive`.
- A Schedule file can no longer be registered or moved under an archive segment
  name (`*.archive.jsonl`, `*.archive.<n>.jsonl`); explicit maintenance deletes
  unlisted files with those names next to a Schedule.
- Maintenance results report `expired`, `retained_bytes`, `limit_bytes`, and
  optional `expiry_deferred`. Runtime `over_limit` identifies individual team
  streams still above their limit; retained byte totals can span several teams.

### Compatibility

- Schema 2 Schedules and schema 4 runtime archives adopt segment 1 in place,
  without rewriting archive data. Schema 1 Schedule migration rewrites only its
  verified tombstone rows and postpones expiry until a later write. Runtime
  migration preserves the exact original as `state.v4.json` (or its earlier
  schema backup), publishing both generations as schema 5.
- Older binaries refuse the new document/state formats. Upgrade writers and
  reconnect long-running MCP processes before continuing coordinated work.
  Windows has no portable directory fsync; a power-loss reorder may leave a
  missing middle segment, reported as `store_corrupt`.

## 0.4.1 — 2026-11-14

### Changed

- Schedule schema 2 commits archive bytes, rows, SHA-256 chain and allocator
  maxima. Historical reads verify the complete committed prefix; unpublished
  tails are ignored and truncated by the next archive write. Missing, short or
  unterminated committed prefixes refuse writes without extending the file.
- Sweeps prune tombstones unless a live dependency or any hot linked duty
  references them, including tombstones created in that sweep. Tombstones do
  not retain their predecessors. `counts.retired` and `retired_total` count all
  archived rows; `schedule archive` also reports `pruned` IDs. Pruned items
  remain readable by ID and refuse mutation or dispatch replay. Restored duties
  retain the same complete/cancelled guard and compaction behavior.
- Add/update normalizes dependencies on pruned complete items after input and
  immutability validation, omits them from storage and reports `satisfied` IDs.
  New or changed cancelled dependencies are refused, including live items and
  retained tombstones. Existing cancelled edges remain valid. Moving a Schedule
  requires its committed sidecar even when all tombstones were pruned.

### Upgrading

Runtime schema remains 4. Upgrade all Schedule writers and restart resident MCP
processes. 0.4.1 reads Schedule schema 1 and upgrades it on the first Schedule
write, rewriting the sidecar to exactly the verified tombstone rows by synced
temporary file and rename. Unreferenced rows, foreign-schedule rows and
interrupted-publish duplicates are dropped. A crash between rename and document
publication can rerun migration. A 0.4.0 binary refuses schema 2 with
`invalid_argument` (unknown `archive` field) during decoding. Back up the Schedule
together with `<filename>.archive.jsonl`, and runtime state with `archive/`.

The 2 MiB document and 2,000-live-item limits remain. Tombstones are bounded by
live dependency references plus hot linked duties; the sidecar is append-only
and unbounded, without rotation. Retired-item detail, `--all`, move verification,
pruned dependency resolution and recovered-duty guards each scan the committed
prefix linearly. Boundary and recovery scans hold the Schedule lock followed by
`state.lock`, delaying writers in other teams and potentially exhausting their
5-second lock wait. A Linux debug fixture of 10,000 Schedule rows (3,288,894 bytes)
measured 287.26 ms for a verified scan and 286.49 ms holding Schedule then state
locks through a verified lookup and lock release. These synthetic local timings
are observations, not latency guarantees. Windows retains file-sync/rename
guarantees; physical power-loss behavior is untested. See [platform checks](PLATFORMS.md).

## 0.4.0 — 2026-11-13

### Added

- Automatic and explicit Schedule archival to a JSONL sidecar, compact
  tombstones, `schedule archive`, `schedule show --all`, full
  `schedule show <item>`, and matching MCP tools. Retired IDs and tokens cannot
  issue new work; completed tombstones continue to satisfy dependencies.
- Runtime compaction (`gaebu compact` and MCP `compact`) archives terminal
  duties and closed escalations after 24 hours, and records after one hour.
  Member operations trigger a separate best-effort pass from 64 eligible
  entries; hooks never compact. `duty show` and `duty_get` recover archived
  contracts within the caller's team.
- Commander-only `schedule move <path>` rebinds a moved, identity-preserving
  document after the old file disappears and its archived history is verified.

### Changed

- Default Schedule output omits complete items and dispatch `spec`/`source`.
  Cancelled items remain until their live duties end, preserving cleanup rows
  and preventing standalone working duplicates in the mod.
- Unit close refuses while the unit owns open Schedule items and lists the
  IDs to transfer or cancel. Unrelated member operations defer Schedule parsing
  until a linked guard, close check, or compaction requires it.
- Leases conflict across active teams with the same nonempty project label.
  Different or empty project labels retain independent namespaces.
- Unix archive and state publication sync parent directories as well as file
  contents. Runtime archive commit boundaries preserve recovery after an
  interrupted append or `repair`. Existing foreign hot duties still return
  `no_grant`; foreign archived duties return `not_found` because only the
  caller team's archive is searched.

### Upgrading

Runtime schema is 4. The first write keeps the previous schema's original
`state.vN.json` and publishes schema 4 to current and previous generations.
Upgrade binaries and restart resident MCP processes before continuing work.
0.3.x refuses schema 4 and Schedule documents containing `retired`; restoring
an older binary alone does not undo archival. Back up the Schedule together
with `<filename>.archive.jsonl`, and runtime state together with `archive/`.

The 2 MiB document limit remains, with 2,000 active items; tombstones still grow.
Runtime historical lookup scans committed data while holding `state.lock` and
can exhaust another caller's 5-second wait budget on large archives. Windows
retains file-sync/rename guarantees; Unix directory-sync ordering is reviewed
in code, without a physical power-loss test. See [platform checks](PLATFORMS.md).

## 0.3.1 — 2026-11-12

### Changed

- `appoint`, `assign`, `member add` and a seat rebind page the new seat at its
  pager alias, so an idle session gets a prompt and its hook confirms the
  binding without anyone typing to it. Before, such a seat stayed pending until
  its session's next prompt. Only the seat is paged; its superior still reads
  the change on the next card. A session that appoints itself is paged too. A
  page still queued when the seat is confirmed or rebound again is dropped. See
  [Notices](docs/model.md#notices). (4831ff3)

### Upgrading

State is unchanged and a 0.3.0 binary still reads it. The page goes to whoever
holds the alias when it is sent: as before, a session that takes over an alias
before the first confirmation can take the pending seat, and now does so on
the page, not on its next prompt.

## 0.3.0 — 2026-11-10

### Added

- A canonical project `schedule.md` between PM references and individual
  duties. `schedule init`/`attach`, CLI/MCP item updates and commander or
  delegated-manager mode retain backlog, dependencies, ownership and explicit
  task-master review. PM references stay separate from PM lifecycle updates.
  See [the Schedule guide](docs/schedules.md). (ff4e7fb)
- Immutable dispatch manifests, stable retry tokens, reconciliation and
  authoritative-absence abandonment prevent duplicate duties and stale-token
  replay. Schedule holds and cancellation are checked when accepting or
  executing linked duties and taking or renewing leases. (ff4e7fb)
- `bu form` and MCP `bu_form` atomically create a nested unit, move selected
  members and attenuate grants. Closing a clean unit returns seats to its
  parent for reuse while preserving historical duty origin. Parent-first
  Board output, full `duty show`/`duty_get` and role-specific summaries support
  recovery after context loss. (ff4e7fb)

### Changed

- The native mod is an operations desk: attention queue, selected inspector,
  nested organization and full contract reader. Wide panes split; narrow panes
  stack. Keyboard navigation, stale/unknown recovery, completed-duty detail,
  Korean wrapping and the prompt band share the same CLI facts. (424b337)

### Upgrading

Runtime schema is now 3. The first write keeps the exact legacy original in
`state.v1.json` or `state.v2.json` and publishes schema 3 to current and previous
generations. A 0.1/0.2 binary cannot read this state. Upgrade the binary and
restart resident MCP processes before continuing coordinated work. Existing
team and superior links remain; a separate scheduler is optional. (ff4e7fb)

Unknown free-form Schedule files are refused without overwrite; there is no
automatic conversion of an unseen legacy format. Cross-machine copies retain
foreign origins rather than guessing local numeric identities. Windows GNU
and the installed native mod engine were tested; other platform and physical
terminal checks are listed in [PLATFORMS.md](PLATFORMS.md).

## 0.2.2 — 2026-11-04

### Changed

- `gaebu appoint <alias> --explicit` on an alias that already commands an
  active team confirms an appointment made from inferred words. The root
  grant takes the new evidence, which must be newer than the old, and the
  card drops "가절월(추론)". The inferred quote stays in a team record.
  Inferred words still get the old `conflict`, so nothing is downgraded; a
  different `--team` is a conflict; an already explicit seat answers
  "already appointed explicitly". (08d44de)
- Delegating `assign`, `amend`, `cancel` or `arbitrate` over no scope, through
  `bu open` without `--scope` or `grant`, is refused `invalid_argument` at
  once. Before, it succeeded and left a grant whose every later use was
  `out_of_scope`. `open` and `reassign`, which also work unscoped, still
  delegate without a scope. (72b20d1)
- `out_of_scope` names the verb, the requested patterns that fall outside,
  and what the grant or the accepted duties allow. It says when there is no
  accepted duty at all, and points to `src/**` when the allowed side is a
  bare `src`, which names one path. `invalid_scope` spells out the pattern
  grammar. A duty issued with no scope says that its assignee can take no
  lease. (72b20d1)

### Upgrading

No schema change; a 0.2.1 binary reads 0.2.2 state. The mod is unchanged
apart from its version.

## 0.2.1 — 2026-11-04

### Changed

- A quoted instruction shorter than six characters counts as evidence when it
  is the whole prompt ("멈춰"). Its stored text must still re-hash to the
  recorded digest. (8db8844)
- The hook shortens its card to one line only when the mod has stamped
  `gaebu session heartbeat --compose`, meaning its card section reached the
  system prompt. A live mod alone is no longer enough: where another plugin
  passes `prompt.compose` on past the mod, members got only the one-line card.
  (e30bb1b)
- The rule points plain-word team requests at `gaebu --help`. (3f82bd0)

### Added

- The mod draws a band above the prompt: `⚑ <seat>  소임 N · 대기 W …`, or a
  dim `⚑ 개부 자리 없음`. The name opens the `/gaebu` pane. (48df05e)

### Fixed

- The writer-serialization tests allow 60 s instead of the CLI's 5 s deadline,
  which a slow Windows runner exceeded. Tests remove their temporary stores
  even when they fail. (900147a, 7bf0824)

### Upgrading

No schema change; a 0.2.0 binary reads 0.2.1 state. The mod's `--compose`
stamp needs a 0.2.1 binary. With 0.2.0 the stamp fails quietly and the hook
keeps sending its own cards.

## 0.2.0 — 2026-11-04

### Added

- Holds: `gaebu hold <duty|bu> --until "<condition>" --evidence "<words>"`
  stops work with the user's words and release condition. Only
  `gaebu release` with the user's words lifts it: questions, peer messages,
  bot feedback and commanders cannot. Held work is refused `held`.
- Evidence strength: every root directive records `explicit` (the words name
  the action and the target, `--explicit`) or `inferred`. Commanders appointed
  from inferred words show as "가절월(추론)".
- A duty can carry the user's words it answers (`source`), verified against
  the issuer's own recorded prompt.
- Escalations reuse an open identical question, supersede a different one on
  the same duty, can be withdrawn, and are rerouted when the asked superior
  is vacant.
- One stage table (보류, 답변 대기, 수락 대기, 막힘, 자리 빔, 진행) shared by
  the card, `gaebu board` and the MCP board. The mod's roster sorts by stage
  and refreshes only when the board token changes (`board --since`).
- `docs/model.md` lists rejected concepts and why.

### Changed

- Notices interrupt a member only for changes they must act on. Everything
  else waits for the next card, and a newer queued notice replaces an older
  one for the same session.
- State moves to schema 2.

### Fixed

- Review of 0.2: escalation askers must be live, notices keep their order,
  and the board token excludes `rev`. (0e012b1)

### Upgrading

The first command after the upgrade migrates `~/.gaebu/state.json` and keeps
the 0.1 original as `state.v1.json`. A 0.1 binary cannot read schema 2 and
fails closed, so install the new binary first, then restart the sessions that
run `gaebu mcp`.

## 0.1.0 — 2026-11-04

First release.

- 가절월 by plain-word root directives (`appoint`, `assign`, `dismiss`,
  `cancel-team`), each checked against a quote from the caller's own current user
  prompt. Peer and pager text never counts.
- 부, 속관 and 소임: open bus, recruit, issue and accept duties, and delegate
  verbs over scopes that only narrow down the tree.
- Leases on paths and `res:` resources inside an accepted duty's scope, with
  TTLs and arbitration.
- Position cards injected by the Claude Code and Codex hooks, an MCP server,
  and a Claude Code mod (card section, composer evidence, roster pane, status
  line).
- One JSON state file replaced atomically under an OS lock, per-session event
  logs, at-least-once notices through `pager`.
