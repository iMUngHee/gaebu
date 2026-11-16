# Changelog

Every release has an entry here, newest first, written in the commit that
bumps the version. `tests/changelog.rs` fails while the version in
`Cargo.toml` has no `## <version>` heading below, and the mod's
`plugin.json` must carry the same version.

Each entry says what changed for someone running Gaebu: commands, refusals,
cards, state, the mod. It also says whether an older binary can still read
the state, and anything to do after upgrading.

## 0.9.0 — 2026-11-16

### Added

- `gaebu stalled --notify` reports each stall finding that is due, for a
  timer to run. A member in `report_to` gets one pager message per finding
  from the sessionless sender `automation:gaebu-stalled`, in at most three
  rounds at growing intervals; then, or when nobody can be told or a report
  cannot be delivered, the human is told once through the command in
  `GAEBU_STALLED_NOTIFY` (the line arrives in `GAEBU_STALLED_BODY`), or on
  standard error. See [reporting findings](docs/stalled.md#reporting-findings)
  for the rounds, the limits and timer examples.
- What was sent is kept in `stalled-notify.json` in the Gaebu home, with
  `stalled-notify.lock` beside it. The state, the session logs and the
  Schedules are still only read.
- Findings carry `team_name`.

### Compatibility

- Runtime schema 6 and Schedule schema 3 are unchanged.
- `--notify` needs pager 0.8.0 or later for `--automation`. With an older
  pager every send fails and, after three runs, the human is told instead.
  pager 0.8.0 moves its own database to a schema older pagers cannot open:
  upgrade the installed pager and restart running pager MCP servers
  together. `gaebu stalled` without `--notify` works with any pager.
- Registering the timer is left to the machine's own setup.

## 0.8.0 — 2026-11-16

### Added

- `gaebu stalled [--json] [--after <minutes>]` lists work that waits on a
  session observed to be resting: a seat never confirmed, a formed team with
  no work, an accepted duty whose assignee stopped, a duty waiting on a
  member who stopped, a ready Schedule item whose task master stopped, and a
  duty whose assignee's host is gone. It needs no session, changes nothing,
  and names who should hear of each finding without sending anything. See
  [stall detection](docs/stalled.md) for what it can and cannot observe.
- Notices sent through pager carry `--event` and `--kind`. A notice the same
  session sends again is the same pager message instead of new mail; a
  resend by another session can still duplicate. Cancellations, dismissals
  and holds go out as `release`, which pager delivers to a recipient whose
  wakes are paused; everything else stays `update`.

### Fixed

- The HOST column of `pager who` is read by its position. Since pager added
  a THREAD column, whose value is `unknown` for every Claude session, a
  Claude session reported gone was read as unknown and kept its seat.
- The roster is read with `pager who --all`, so a session gone for more
  than twelve hours is still seen as gone.

### Changed

- A binding refresh runs `pager who` once instead of once per bound member.
- With the two fixes above, a seat whose Claude session pager reports gone
  now becomes vacant and its exclusive leases are released on the next
  command that refreshes bindings. This is the documented behavior; it did
  not happen before.

### Compatibility

- Runtime schema 6 and Schedule schema 3 are unchanged. Queued notices gain
  a `kind` field. An older binary that rewrites the state drops it, and such
  a notice is then sent as `update` under a different event id, so pager
  does not refuse it; it loses the paused-wake delivery and may arrive
  twice.
- `--event` and `--kind` need a pager that accepts them (0.7.2 does).

## 0.7.0 — 2026-11-16

### Changed

- Default titles are Wei-dynasty general ranks. Commanders draw from 대장군,
  표기장군, 거기장군 and 위장군; heads of units below the commander's from
  twelve 장군 titles; 속관 from one list for the commander's unit and another
  for units below it. A new team's root unit is named after its commander
  (대장군부, ...), no longer always 승상부.
- A seat draws its title by a fixed hash of its team and member ids instead
  of taking the first unused one, so commanders of different teams differ.
- 속관 titles are unique within a unit, no longer across the team. The same
  title can appear in two units, and a title then stops addressing a member:
  `no single active member matches` is returned and the alias or id still
  works.
- A member that moves to another unit, or returns from a closed one, draws a
  new title for its new seat when its title is one of the default titles.
  Any other title stays, including one set with `gaebu title` and the titles
  of earlier releases (승상, 연, ...). The check reads the title text alone,
  so a hand-set title equal to a default one is treated as a default.
- `gaebu schedule manager` requires `--evidence <words>` quoting the human's
  current turn, for seating, replacing and removing the manager, and accepts
  `--explicit`. It sets the manager's title to 황문시랑 and gives a replaced
  or removed manager a title for the seat it holds; a seated manager keeps
  the title when it moves or becomes a head. `schedule show` adds
  `manager.evidence`: `explicit` or `inferred` for a separate manager, `none`
  while the commander schedules. Repeating a directive with `--scope` no
  longer mints another grant.

### Added

- `bu open` and `bu form` are refused with the new reason `depth_limit` when
  the caller's unit is itself below the commander's: units go two deep.
- `rules/gaebu.md` and `docs/protocol.md` describe the 편성안: the format an
  agent proposes a formation in before the human grants 가절월, and the order
  in which a 황문시랑 can be seated.

### Compatibility

- Runtime schema 6 and Schedule schema 3 are unchanged. Stored titles, unit
  names and units deeper than two levels are left as they are; deeper units
  can still be closed and cancelled.
- After upgrading, add `--evidence` to any `schedule manager` call and drop
  the separate `gaebu title <member> 황문시랑`. A manager seated earlier
  keeps its seat and reports `manager.evidence: none`.
- Copy the updated `rules/gaebu.md` wherever agents load it from.

## 0.6.1 — 2026-11-16

### Fixed

- A root directive that seats the session running it (`appoint`, `assign` or
  `rebind` naming that session's own pager alias) now binds the seat in the
  same transaction and says so in its result (`appointed; bound to this
  session`). Before, the seat
  stayed pending until that session's next prompt, so a commander that
  appointed itself could not issue duties in that turn and nothing woke it
  afterwards.
- Repeating `assign` with the same evidence reports the seats it already
  made instead of none, so the repeat can still bind the caller's own seat.
- A root `rebind` reports `rebound; awaiting target hook` instead of
  `rebind pending`, matching `appoint` and `assign`.

### Changed

- A root directive that would seat the calling session is refused with
  `conflict` when that session already holds another active seat. Such a
  seat could never be confirmed. Dismiss or rebind the held seat first.

### Compatibility

- Runtime schema 6 and Schedule schema 3 are unchanged. Seats for other
  sessions are pending as before.

## 0.6.0 — 2026-11-14

### Added

- Commanders can transfer an entire canonical Schedule with `schedule offer
  --to <team>` / `--withdraw` and `schedule adopt <team>` (CLI and MCP). A
  cancelled source needs no offer. Adoption retires inline attempts, closes
  linked open duties as handed off, clears non-terminal owners and preserves
  IDs, dependencies, item holds and history. Reassign owners before redispatch.
- Any source runtime hold requires the adopter's current-turn `--evidence`.
  Only the own holds of linked open duties closed by handoff are cleared; source
  bu holds and other duty holds stay. Both teams record every hold and its effect.
  Members can read predecessor duty contracts linked to the inherited Schedule.
- Cancelled teams expire after 30 days unless protected by a keeper's Schedule
  predecessors. Compaction purges their runtime rows, then removes only listed
  archive files through retryable `purge_pending` bookkeeping. `purged` and
  `purge_deferred` expose the outcome. Sessions without an active seat can run
  this rule-based purge pass; root directives also trigger the automatic check.
  Project files, session logs, migration backups and unlisted leftovers remain.

### Changed

- `schedule adopt --path <file> --evidence <words>` continues an unregistered
  document. Populated existing files are refused by `init` and `attach`, with
  guidance to adopt; empty files and own-registration retries remain usable.
  Former runtime holds cannot be recovered after purge, so path adoption always
  requires the human's current words.
- New Schedule registrations, moves and path adoptions inside the canonical
  runtime home are refused before creating files. Legacy registrations continue
  working and their paths are protected from runtime archive mutations.
- Terminal linked duties may compact once their team no longer registers their
  Schedule, with the existing age, hold and escalation restrictions.

### Compatibility

- Runtime schema 6 adds offer/predecessor registration fields, retryable purge
  bookkeeping, protected legacy paths and a root-evidence floor that prevents
  replay after purge. Schemas 1–5 migrate with exact versioned backups; older
  binaries refuse schema 6. Schedule documents retain schema 3. Upgrade writers
  and reconnect resident MCP servers. Very old root instructions may require
  restatement; `state.prev.json` and migration backups may retain purged rows.
- On Windows, directory flush remains pending: power loss may restore purged
  files after pending bookkeeping was cleared, leaving unread leftovers. See
  `PLATFORMS.md` for native validation still owned by integration.

## 0.5.1 — 2026-11-14

### Added

- Explicit `compact --heal` and `schedule archive --heal` recover missing-segment
  holes by permanently deleting older listed and unlisted segment files. Back up
  history first when it must be kept. Both commands pre-check metadata and retained
  boundaries; Schedule also verifies surviving rows and chains before deletion.
  Ordinary and automatic maintenance never heal.
- `healed` separates listed committed segments/bytes (and Schedule rows) from
  unlisted file counts/actual bytes. `expired` excludes listed heal removals after
  normalization. Partial failures return `heal_deferred` and `hole_remaining`,
  retain waiting hot rows, and can be retried. Later errors name prior removals.
- Runtime heal requires the current commander and checks all three caller-team
  streams before deleting; Schedule heal keeps commander/manager authorization.
- Compaction no longer fails because another team's archive is damaged: that
  team's stream is skipped, its rows stay hot, and the result lists it under
  `skipped` with the reason. The caller team's own damage still fails the command.
  `schedule archive --heal` runs its full verification only when a hole exists.
- Windows directory flushing in `store::sync_dir` is recorded in `PLATFORMS.md`
  for native implementation and verification. Healing is the explicit recovery
  path while a power loss can reorder deletions there.

### Compatibility

- Runtime schema 5 and Schedule schema 3 are unchanged. Schema 1 Schedule
  migration ignores heal and reports zeros; legacy archives keep their normal
  upgrade path. Upgrade and reconnect resident MCP servers to use the new flag.

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
  [Notices](docs/model.md#notices). (e850a9a)

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
  See [the Schedule guide](docs/schedules.md). (a1e0033)
- Immutable dispatch manifests, stable retry tokens, reconciliation and
  authoritative-absence abandonment prevent duplicate duties and stale-token
  replay. Schedule holds and cancellation are checked when accepting or
  executing linked duties and taking or renewing leases. (a1e0033)
- `bu form` and MCP `bu_form` atomically create a nested unit, move selected
  members and attenuate grants. Closing a clean unit returns seats to its
  parent for reuse while preserving historical duty origin. Parent-first
  Board output, full `duty show`/`duty_get` and role-specific summaries support
  recovery after context loss. (a1e0033)

### Changed

- The native mod is an operations desk: attention queue, selected inspector,
  nested organization and full contract reader. Wide panes split; narrow panes
  stack. Keyboard navigation, stale/unknown recovery, completed-duty detail,
  Korean wrapping and the prompt band share the same CLI facts. (befaf37)

### Upgrading

Runtime schema is now 3. The first write keeps the exact legacy original in
`state.v1.json` or `state.v2.json` and publishes schema 3 to current and previous
generations. A 0.1/0.2 binary cannot read this state. Upgrade the binary and
restart resident MCP processes before continuing coordinated work. Existing
team and superior links remain; a separate scheduler is optional. (a1e0033)

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
  "already appointed explicitly". (cdec0e6)
- Delegating `assign`, `amend`, `cancel` or `arbitrate` over no scope, through
  `bu open` without `--scope` or `grant`, is refused `invalid_argument` at
  once. Before, it succeeded and left a grant whose every later use was
  `out_of_scope`. `open` and `reassign`, which also work unscoped, still
  delegate without a scope. (a9910fb)
- `out_of_scope` names the verb, the requested patterns that fall outside,
  and what the grant or the accepted duties allow. It says when there is no
  accepted duty at all, and points to `src/**` when the allowed side is a
  bare `src`, which names one path. `invalid_scope` spells out the pattern
  grammar. A duty issued with no scope says that its assignee can take no
  lease. (a9910fb)

### Upgrading

No schema change; a 0.2.1 binary reads 0.2.2 state. The mod is unchanged
apart from its version.

## 0.2.1 — 2026-11-04

### Changed

- A quoted instruction shorter than six characters counts as evidence when it
  is the whole prompt ("멈춰"). Its stored text must still re-hash to the
  recorded digest. (d4f4167)
- The hook shortens its card to one line only when the mod has stamped
  `gaebu session heartbeat --compose`, meaning its card section reached the
  system prompt. A live mod alone is no longer enough: where another plugin
  passes `prompt.compose` on past the mod, members got only the one-line card.
  (c8405ad)
- The rule points plain-word team requests at `gaebu --help`. (e1e32c0)

### Added

- The mod draws a band above the prompt: `⚑ <seat>  소임 N · 대기 W …`, or a
  dim `⚑ 개부 자리 없음`. The name opens the `/gaebu` pane. (22f5898)

### Fixed

- The writer-serialization tests allow 60 s instead of the CLI's 5 s deadline,
  which a slow Windows runner exceeded. Tests remove their temporary stores
  even when they fail. (3c514e2, 5d85784)

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
  and the board token excludes `rev`. (c7b03e5)

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
