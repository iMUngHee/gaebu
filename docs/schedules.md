# Schedules and nested units

**Archive expiry permanently deletes old history by default above 64 MiB per
Schedule.** Raise `GAEBU_ARCHIVE_LIMIT_MIB` (positive integer) or back up all
segments before upgrading when history must be kept. The newest segment stays;
a sole oversized legacy segment expires only after its successor is written.

A Schedule keeps operational work between the project's PM items and the duties
issued to individual agents. PM owns the goal, plan and project lifecycle;
Schedule owns execution order, dependencies, assignment and review acceptance;
Gaebu's local runtime owns command authority, sessions, duties and leases.

## Start with the commander, delegate when useful

The commander manages scheduling by default. A separate manager is optional;
changing it does not change anyone's superior or move existing duties. The
separate scheduling and liaison coordinator uses the display title 황문시랑.
The commander retains its title when scheduling is combined. Display titles
never grant authority; set the title explicitly when appointing the coordinator.

```sh
gaebu schedule init /absolute/project/schedule.md
gaebu title coordinator 황문시랑
gaebu schedule manager coordinator --scope 'src/**'
gaebu schedule manager commander
```

The separate manager must be a bound subordinate with a valid, scoped `assign`
grant. Supplying `--scope` delegates that scope through the existing grant checks.
The manager chooses operational priority and responsible units. It cannot use
that appointment to issue duties to sibling units' workers or lift a human hold.
The current head of a work item's responsible unit is its task master: it
dispatches work to its own descendants and reviews their results.

Use `schedule attach <path>` to register an existing recognized Schedule. The
runtime registers one canonical absolute path for the team. All its local
worktrees and clones use that path, rather than discovering another source from
the current directory. A missing registration is distinct from an empty Schedule.

## Form and reuse units

```sh
gaebu bu form --head task-master --members worker-a,worker-b \
  --name '좌장군부' --verbs assign,amend,cancel,reassign,open --scope 'src/**'
```

Formation checks the whole change before publishing it: the head, selected
workers, parent, narrowed grants and required `reassign` authority must all be
valid. A failure leaves the previous seats, IDs, grants and notices intact.
Existing `bu open` remains available when only a head should move.

`gaebu bu close <bu>` requires a clean subordinate unit: no open duties, live
leases, open child units, or incomplete, uncancelled Schedule items owned by
that unit. A refusal lists those item IDs: transfer them with
`schedule update <item> --owner-bu <open-unit>` or cancel them explicitly.
If a registered Schedule cannot be read, close is refused; unit cancellation
remains available. A successful close returns active members to the parent unit,
preserves their superior relationships and grants, and connects the former head
to the parent head. Those same seat IDs can then be used in another `bu form`.
A repeated close can recover seats left in an old closed unit; cancelled units
remain cancelled.

Personnel movement does not rewrite a duty's original unit. Its original holds,
cancellation ancestry, source and issuer remain meaningful. Transfer operational
work explicitly in Schedule after its old attempts are known finished, rather
than moving a person to bypass a stop.

## Add work before assigning a worker

```sh
gaebu schedule add --title 'Search API' --scope 'src/**' --owner-bu '좌장군부' \
  --pm-ref '{"task":"SEARCH","item":"search-api","plan":".agents/plans/search-api.md"}'
gaebu schedule add --title 'Integration review' --scope 'src/**' --depends-on w-1
gaebu schedule show
gaebu schedule update w-2 --priority 10 --expected-revision 2
```

An item can be unassigned and survive session replacement. Its ID is stable and
never reused. PM references are optional structured references, not imported
authority: Gaebu does not approve plans, change PM checkboxes or complete PM
items. An unresolved PM reference is shown explicitly. Operational priority is
Schedule's ordering decision, not a second copy of PM's project priority.

Dependencies must refer to existing items in this Schedule and cannot form a
cycle. A cancelled or unverified predecessor does not satisfy a dependency.
Readiness, progress, vacancy and review waiting are derived from canonical facts
and joined runtime duties; duty status is not copied into Markdown.

## Dispatch, recover and review

The task master issues a worker duty with a stable key for each independent
piece of work:

```sh
gaebu schedule dispatch w-1 --key api --to worker-a \
  --spec '{"intent":"Enable search","outcome":"Search API","scope":["src/**"],"decision_rights":"Implementation internals","escalate_on":["Public API change"],"done_criteria":"Tests and integration evidence"}'
gaebu schedule reconcile w-1
gaebu schedule complete w-1 --evidence 'Reviewed tests and integrated result'
```

Schedule commits an immutable dispatch token and manifest before materializing
the runtime duty. Retrying the same key and manifest reuses that token and duty,
including after a response was lost or the duty reached a terminal state while
the item is still active. Archived items refuse dispatch and reconciliation
with a `retired` conflict; their tokens cannot issue another duty. A
crash before runtime materialization stays visible as dispatching; it does not
expire into another assignment. Different keys can run in parallel.

Worker `duty done` means the worker reported its result. The item remains in
review until its current task master signs off with evidence. Only that signoff
satisfies downstream Schedule dependencies. `handed_off` and `cancelled` are
not successful completion. A task master may complete direct work without a
synthetic self-duty, but Schedule ownership alone grants no lease. Shared-resource
reservations still require an in-scope, unheld accepted duty from a real superior.

Before the first dispatch, an incomplete item's title, scope, dependencies and
owner may change under revision checks. After dispatch, its semantic contract
is immutable; changing priority remains possible. Completed work cannot silently
acquire a different scope or dependencies under old acceptance evidence. Corrected
work needs an explicit retired predecessor and a fresh dispatch token.

```sh
gaebu schedule retire w-1 <token> --reason 'Known attempt terminated'
gaebu schedule abandon w-1 <token> --reason 'Verified never materialized'
```

Retirement requires known termination. Abandonment can resolve an assignment
that was committed but never created a duty, even if its former assignee or
grant disappeared. It locks Schedule before runtime state, reads the current
matching runtime and proves that token absent, then tombstones it. A delayed
caller cannot materialize the retired token. Missing, corrupt, foreign or
fallback runtime data is uncertainty, never proof of absence.

## Human stops and full context

```sh
gaebu schedule hold w-1 --until 'After my review' --evidence '<current user words>'
gaebu schedule release w-1 --evidence '<current user words lifting the stop>'
gaebu duty show d-12
gaebu --json duty show d-12
```

Schedule holds follow the same current-prompt evidence boundary as duty and unit
holds. Neither an agent question nor review feedback lifts one. Linked duty
acceptance, progress and new or renewed leases recheck Schedule's token and
stops as well as runtime grants and holds. Declining, recording, escalation,
cancellation and lease release remain available for cleanup. An independent
accepted duty can still cover its own unheld resource use.

`duty show` and MCP `duty_get` recover the entire contract after completion or
context loss: intent, original unit, decision rights, escalation triggers,
source, plan, execution root, version and notes. A read-only `--session` is
available for the mod; it does not authorize mutations.

`schedule show` and the Schedule section of cards list open work with compact
dispatch metadata; they omit full dispatch `spec` and `source`. Complete items
disappear from this default list. Cancelled items remain visible as cancelled
while they have live linked duties, so those duties are not presented as
independent working tasks. Their next action is cleanup.

```sh
gaebu schedule show w-1     # Full item and dispatch contracts, active or archived.
gaebu schedule show --all   # Include deferred and archived terminal items.
gaebu schedule archive --expected-revision 12
gaebu compact
```

MCP exposes the same operations as `schedule_show` (`item` or `all`, mutually
exclusive), `schedule_archive`, and `compact`. Archive is restricted to the
commander or current Schedule manager; it reports retired IDs, pruned tombstone
IDs (`pruned`), deferred items with reasons, and `retired_total` (retained archived rows). Completion, cancellation, and later Schedule writes also
try to retire eligible items without adding a second revision increment.

Retirement requires completion or cancellation, no item hold, known terminal
worker attempts and no remaining leases in their scope. Unmaterialized attempts
must be explicitly abandoned first. Automatic archive I/O failures defer
retirement while preserving the successful operation; explicit archive reports
the error. Completed tombstones still satisfy dependencies; cancelled ones do
not. A new or changed dependency on a cancelled live item or tombstone is refused
with a conflict naming the ID; existing cancelled edges stay valid. For a pruned
ID, add/update verifies the archive after validating the submitted dependency list
and update immutability. Complete dependencies are permanently satisfied, omitted
from the stored list, and reported in `satisfied`; cancelled ones are refused.
Unknown IDs and repeated/self dependencies remain invalid. A refusal preserves
the revision. `schedule show <item>` retains the complete original contract and evidence.

## The operations pane

`/gaebu` opens the native operations desk. It prioritizes decisions waiting for
attention and links the selected work to its responsible unit, next action and
evidence. Wide panes split queue and inspector; narrower panes stack the same
reading order. Organization is a parent-first tree, and full detail preserves
long Korean text, paths and evidence.

Keys: `o` operations, `g` organization, `d` detail, `j`/`k` selection or detail
navigation, `f` attention/all filter, `r` refresh, `b` back and `h` help. The
mod browses; agents make changes through CLI/MCP checks. Failed reads, stale
snapshots, unknown runtime joins, missing registration and empty work are
different states. The existing prompt band summarizes the viewer's current
responsibility without turning unobserved work into healthy zero counts.

## Storage and limits

`schedule init` creates the recognized Markdown document with one marked JSON
fence. CLI/MCP preserve surrounding prose, refuse unknown fields and stale
revisions, and replace the file under a fixed sidecar OS lock. Unknown legacy
prose is refused without overwrite; it is not silently imported.

Runtime schema 5 retains schema 3's qualified local member/unit references with runtime and
team origin. A Schedule copied from another computer cannot accidentally bind
its `m-3` to an unrelated local `m-3`. Unknown joins stay unresolved. The
supported write path is CLI/MCP against one registered canonical file; separate
machines concurrently editing separate copies and manual edits bypassing its
locks are outside that coordination contract. The engine supports cooperative
agents; it does not forcibly interrupt a process already executing a command.

Finished items move to numbered sidecar segments. Segment 1 is
`<schedule filename>.archive.jsonl`; later files are
`<schedule filename>.archive.<n>.jsonl`. Schema 3 records `base`, `expired_rows`,
monotonic `next`, global `last_item`/`last_dispatch`, and the ordered `segments`.
Each segment records `n`, committed `bytes`, `rows`, SHA-256 `chain`, and item
bounds `lo`/`hi`. The oldest chain starts from `base`, initially SHA-256 of the
Schedule ID; each row hashes the preceding 32 bytes and exact JSONL line with
its newline. Expiry advances `base`; allocator maxima survive expired files.

A segment rolls after its committed size reaches one eighth of the byte limit.
A completed enforcement keeps at most the limit plus one append batch and at
most 17 segments. The newest segment is always retained. A sole oversized legacy
segment or a reduced limit can leave it above the limit until a successor is
written. A failed unlink/sync defers enforcement; failed publication can leave
unlisted files. These exceptions are visible through `retained_bytes`,
`limit_bytes`, and optional `expiry_deferred`. `expired` reports removed segment
and row counts. Explicit maintenance removes unlisted numbered files and applies
the current limit without requiring another completed item. Because it deletes
unlisted files named like this Schedule's segments, `schedule register` and
`schedule move` refuse a Schedule file named `*.archive.jsonl` or
`*.archive.<n>.jsonl`.

Every write pass first syncs the directory, normalizes an absent oldest prefix,
validates the last committed boundary, and trims any unpublished tail, including
before rollover and maintenance without rows. Expiry deletes oldest first and
syncs the directory before the first unlink and after each unlink. Bookkeeping
for a deletion remains until the sync succeeds. `next` survives an empty list,
so a later writer cannot reuse a published filename still named by an old reader.
A missing oldest prefix, including the whole list, means expired history; a
missing middle or last segment while an older file exists is `store_corrupt`.
Accidental loss of the oldest files is indistinguishable from expiry.

Item lookup visits candidate segments newest first and verifies each visited
segment completely. `--all` verifies every retained segment and rejects duplicate
item IDs/dispatch tokens across them; duplicate identities within a segment
remain corruption. A reader rechecks expiry after its scan so already-read rows
that expired during the scan are omitted. `counts.retired` describes retained
rows and `counts.expired` expired rows; without `--all` both use bookkeeping only.
`retired_total` uses the same retained row count; expired rows are reported separately.

An expired item detail returns `not_found` with
`schedule item <id> expired from the archive`. A new dependency on an expired,
tombstone-less item returns a conflict asking the caller to omit it if complete;
it is never silently satisfied. A retained completion tombstone still satisfies
its dependants and guards even after the detail row expires. Recovered duties
whose linked item expired without a tombstone get `schedule item is retired`.
Mutation and dispatch replay retain the existing retired-item refusals.

The 2 MiB document and 2,000-live-item limits remain. Tombstones stay only for
live dependencies or hot linked duties; their own dependencies pin neither
other tombstones nor archive segments. Historical lookup, dependency resolution
and recovery guards read candidate segments. Normal duty guards and compaction
eligibility do not scan archives. Locked lookups acquire the Schedule lock before
`state.lock`; synchronous filesystem work can still exceed another caller's
five-second lock budget. Windows has no portable directory sync; a power loss
can undo deletions out of order and a resulting hole is reported as corruption.

Schema 2 adopts its existing head/file as segment 1 in memory and writes schema 3
on the next change. Empty legacy archives require no segment or sidecar. Schema 1
keeps its tombstone verification path; the first write rewrites exactly those
rows by synced temporary file/rename, and performs no expiry until a later write.
A schema-1 reader crossing migration and expiry rereads the document once and
rebuilds the whole response from the newer document. If its schema did not change,
the original error is returned. Older binaries refuse the new archive shape;
upgrade every writer.

To move a registration, first move the document and every listed archive segment, then
have the commander run:

```sh
gaebu schedule move /new/absolute/path/schedule.md
```

The old document must be absent. The new file must retain the registered
Schedule identity; every listed archive segment must exist and verify at the new
path, even an oldest segment that the normal reader would classify as expired. This
changes the registration only; it does not copy or rename files. Writers that
waited with an older registration are refused before publishing changes.

Runtime compaction retains terminal duties and closed escalations for 24 hours,
and records for one hour. Holds and open escalation references keep duties hot.
Linked duties of an active team retire only after that team's item is archived;
foreign active-team linked duties are deferred. Cancelled-team terminal duties
can retire without their Schedule because the team cannot issue further work.
Automatic compaction runs after member operations commit, with a 64-candidate
threshold; hooks do not compact. Explicit `gaebu compact` runs from one eligible
entry and reports I/O failures. Historical duty lookup remains available through
`duty show` and `duty_get`: a foreign hot duty returns `no_grant`; a foreign
archived duty is outside the caller team's archive and returns `not_found`.

Runtime archives live in `$GAEBU_HOME/archive/<team>/`. Their committed byte
boundaries are stored in state, so uncommitted tails are ignored and trimmed on
the next append. Append checks file length and the boundary newline; lookup
parses the committed region under `state.lock`, serialized with repair. Large
lookups can hold this lock long enough for another operation's 5-second wait
budget to expire. Unix writes sync archive files and directory links before
publishing state, and sync directories after atomic replacements. Windows
retains file-sync/rename durability; physical power-loss behavior is untested.

### Healing an archive hole

`gaebu schedule archive --heal` is an explicit, destructive recovery operation for
an absent listed segment behind a present older file. It deletes every segment
file numbered below the newest missing listed segment, including unlisted files,
oldest first. The surviving files become a valid retained suffix and the missing
prefix is normalized as expired. A missing last segment can expire the whole list;
future appends still use the monotonic `next`. Back up the files first when their
history matters: the command cannot distinguish reordered deletions after a power
loss from actual loss of a middle segment.

The current commander or Schedule manager can request it through CLI or MCP
`schedule_archive {"heal": true}`. Without the flag, holes remain `store_corrupt`;
automatic sweeps never heal. Before deleting, the command validates metadata and
all remaining rows, digests, chain commitments, bounds and cross-segment duplicate
item IDs/dispatch tokens. Damage unrelated to the hole aborts before any unlink.
Schema 1 migration ignores heal and reports zero removals; schema 2 adopts in place.

`healed` reports listed `segments`, committed `bytes` and `rows`, plus separate
`unlisted_files` and actual `unlisted_bytes`. `expired` counts already-missing
segments and normal limit expiry, excluding listed entries counted in `healed`.
After a published normalization their segment/row sums equal the bookkeeping that
left the list. Removed detail is expired, never evidence of completion; retained
complete tombstones still satisfy their dependants.

A failed sync or unlink stops healing and returns success with `heal_deferred`
and `hole_remaining` (also true when reinspection fails with an I/O error).
The pass retires nothing and leaves the document
unchanged; `healed` still reports actual deletions. `hole_remaining` can be false
when only the final directory sync failed. Retry `--heal`; a plain run works once
the hole is gone. A later save error includes the number of files already removed
and, for an incomplete heal, the reason and retry instruction. Those files remain
removed; a retry counts them as already expired.

Runtime `gaebu compact --heal` / MCP `compact {"heal": true}` requires the commander
and targets the caller team's three streams. All streams are pre-checked before
the first deletion. A partial heal skips only its stream and preserves its hot
candidates; other streams can still be maintained. Runtime reports use the same
listed/unlisted distinction, with committed bytes instead of Schedule row counts.
Windows directory flushing remains pending in `PLATFORMS.md`; this command offers
explicit recovery without changing the storage schema.
