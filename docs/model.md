# Gaebu data model

This is the contract for what Gaebu stores and prints. The Rust types in
`src/model.rs`, `src/schedule.rs` and `src/card.rs` are its source of truth; this page explains
them. The JSON in `~/.gaebu/state.json` is exactly their serde form, with
`schema_version: 4`. A schema-1, schema-2 or schema-3 file is migrated on first write (see
[Schema 4 migration](#schema-4-migration)).

## Vocabulary

Six names come from the Three Kingdoms period. They are a motif adapted for
software, not a historical reconstruction.

| Term | Meaning in Gaebu |
| --- | --- |
| 가절월 (gajeolwol) | Command authority. The member holding a team's root grant has it. |
| 개부권 (gaebu-gwon) | The `open` verb: open a bu below you and recruit into it. |
| 부 (bu) | A unit. Every member belongs to exactly one bu. |
| 속관 (sokgwan) | A member of a bu, written "<bu name> 속관 <title> <alias>". |
| 소임 (soim) | A duty: an outcome-level commitment. |
| 팀기록 (team record) | Decisions, handoffs and changes worth keeping. |

Everything else keeps its technical name: member, binding, grant, lease,
escalation, cancel, vacant, hold.

Display titles (승상, 대도독, 좌장군, 주부, ...) are cosmetic. Authority comes
only from grants.

## Entities

- **Runtime origin**: a stable `runtime_id`, initialized on the first committed write, qualifies local organization references and prevents another machine's numeric IDs from matching accidentally. Empty fresh reads remain empty.
- **Schedule registration**: `State.schedules[team]` records a canonical absolute project artifact path, its immutable Schedule ID and optional manager member. No manager means the commander retains scheduling. Registration is local authority; the file cannot create grants. See [Schedule operations](schedules.md).
- **Team**: one commander and everyone under them. `project` is a label that
  scopes are relative to. `root_bu` is the commander's own bu (승상부 by
  default). `status` is `active` or `cancelled`.
- **Bu**: `parent` (none for the root bu), `head` (a member), `name` (the
  head's title + 부 by default), `status`: `open`, `closed` or `cancelled`, and
  an optional `hold` over every duty in it and in the bus below it.
- **Member**: a seat. It has a stable id, `bu`, `superior` (none only for the
  commander), `title`, `status` (`active` or `dismissed`) and a binding.
- **Binding**: which session currently speaks for the member.
  - `pending`: designated by pager alias, not yet confirmed.
  - `bound`: the target session's own hook confirmed that pager reports the
    same alias for it.
  - `vacant`: the session was observed gone, the alias moved, or the seat was
    rebound.
  - "quiet" is not stored. A card shows it when a bound session has gone
    unseen by the hook for more than 2 hours.
- **Grant**: `grantor` (none for the root grant from the user), `grantee`,
  `verbs` ⊆ {assign, amend, cancel, reassign, open, arbitrate}, `scope`
  patterns, `status`, and `evidence` for root grants. Validity is computed at
  use time: the grant is active, its grantor is not dismissed, the grantor's
  own authority is valid, and the team is active.
- **Evidence**: the user's words behind a root directive: `session_id`,
  `prompt_seq`, `sha256`, `attest` (`composer`, `hook_filtered`, `fixture`),
  `quote`, `at`, and `strength`. `strength` is `explicit` when the words name
  both the action and its target, `inferred` when the agent interpreted
  either ("가절월 줄게", "개부해라"). It is recorded and shown, never enforced.
  When the user later confirms an inferred appointment in explicit words,
  `appoint <alias> --explicit` replaces the root grant's evidence and keeps
  the inferred quote in the team record; inferred words never replace
  explicit ones.
  The root grant keeps the appointment's evidence; every other root directive
  keeps its own in its `root_actions` entry.
- **Duty**: `intent`, `outcome`, `scope`, `decision_rights`, `escalate_on`,
  `done_criteria`, optional `plan_ref` and `exec_root`, `status` (`proposed`,
  `accepted`, `declined`, `blocked`, `done`, `cancelled`, `handed_off`), a
  `version` for optimistic concurrency, an optional `hold`, and an optional
  `source`: the user's words the duty answers (`session_id`, `prompt_seq`,
  `sha256`, `quote`, `at`), matched against a prompt the hook recorded in the
  issuer's own session. Amending without a source keeps the one recorded. A linked duty also has an optional `schedule_ref` containing Schedule/item identity, dispatch token and immutable manifest hash; manifest-changing amendment is refused.
- **Hold**: a human stop on a duty or a bu: `evidence` (the user's words that
  set it), `until` (the release condition, 1-120 characters on one line;
  whitespace and control characters collapse) and `at`. Only
  root directives (`gaebu hold`, `gaebu release`) set or lift one. Holds are
  independent: a duty is held while its own hold or any hold on its bu or the
  bus above it remains. While held, its assignee cannot accept, finish, block,
  unblock or hand it off, nor take or renew a lease that only it covers
  (refused `held`); declining, escalating, recording and releasing leases stay
  open. A superior may still amend or cancel it; neither lifts the hold.
  Leases already held stay until they expire, are released or arbitrated.
- **Lease**: a `key` (a scope pattern or `res:<name>`), `mode` (`exclusive` or
  `shared`), a holder member and session, and `expires_at`. A lease resolves
  conflicts. It confers no decision rights.
- **Escalation**: `from`, `to` (none means the user's queue), an optional
  `duty`, a `question`, an `answer`, a `status` (`open`, `answered`,
  `withdrawn`, `superseded`) and `superseded_by`. An asker has at most one open
  escalation per duty (or per "no duty"): raising the same question again
  (whitespace aside) returns it unchanged, a different question supersedes it.
  Only the asker withdraws one. After every committed change, an open
  escalation whose recipient can no longer answer (vacant, pending, dismissed,
  in a cancelled bu) moves to the asker's nearest live superior, or to the
  user's queue when there is none or the asker itself can no longer act.
  `closed_at` records answer, withdrawal or supersession time. Legacy closed
  entries without it use `answered_at`, then `created_at`, for retention.
- **Record**: `kind` (`decision`, `handoff`, `change`, `note`), `author`,
  `refs`, `text`.
- **Outbox**: notices waiting to be delivered with `pager send <target>`, where
  the target is the recipient's session id, or its pager alias while the seat
  is pending. `body` is what is sent; `text` is the body without its record id, the
  dedupe key. See [Notices](#notices).

## Scope patterns

| Pattern | Matches |
| --- | --- |
| `**` | every path |
| `a/b/c` | exactly that path |
| `a/b/**` | that directory and everything below it |
| `res:<name>` | one resource, e.g. `res:gpu` |
| `res:**` | every resource |

Paths are project-relative, use `/`, and are compared lowercased. Anything else
is `invalid_scope`, including empty segments, `.`, `..`, `\`, `:`, a leading
`/`, and any other wildcard. The root grant's scope is `["**", "res:**"]`.

`a/b` names one path, so a duty scoped `src` does not cover `src/a.rs`: write
`src/**`. `**` covers paths only, so a resource lease needs `res:<name>` or
`res:**` in an accepted duty's scope. Delegating `assign`, `amend`, `cancel`
or `arbitrate` over no scope is refused `invalid_argument`, since every later
use would be refused; `open` and `reassign` also work unscoped. A duty may
have no scope, but its assignee then takes no lease. An `out_of_scope`
refusal names the requested patterns and what the grant or the accepted
duties allow.

## Notices

Every change is a team record. Only a change its recipient must act on
interrupts them with a pager notice (Act); the rest wait for the next card
(Record).

| Act (pager notice) | Record (team record and card only) |
| --- | --- |
| duty issued, amended, cancelled (to the assignee) | duty accepted, unblocked |
| duty declined, blocked, done, handed off (to the issuer) | grant delegated |
| hold set or lifted (to the assignees it covers) | bu closed, title changed |
| escalation raised, superseded, rerouted, withdrawn (to the recipient) | binding confirmed |
| escalation answered (to the asker) | a seat awaiting confirmation (to its superior) |
| lease force-released (to the holder) | member vacant |
| bu or team cancelled, member dismissed, grant revoked, member moved | lease acquired, renewed, released |
| bu opened (to the new head) | |
| seat designated or rebound: appointment, assignment, member added, seat rebound (to the new seat's alias) | |

A notice goes to the recipient's bound session. A pending seat has none, so
the one notice it gets, that it was designated or rebound, is sent to its
pager alias: the page gives that session a prompt, and its hook confirms the
binding. Pager accepting the page is not the confirmation; a hook that cannot
finish its check leaves the seat pending until the session's next prompt, and
a page pager refuses ten times ends as an "undelivered notice" record. A page
still queued when the seat is confirmed or rebound again is dropped. A vacant
seat gets no notice.

No notice goes to the actor's own session, or when the newest notice still
queued for that target has the same `text` (also while a flush holds it), so
a change that flips back and forth still arrives in order. Once delivered,
the same change raised again is a new notice.

## Stage

Each open duty has one stage: the first row that matches. The card, `gaebu
board` and MCP `me`/`board` all compute it with `card::duty_view`.

| # | Stage | Label | Condition |
| --- | --- | --- | --- |
| 1 | `ended` | 종료 | done, declined, cancelled, handed off, or its bu is cancelled |
| 2 | `held` | 보류 | a hold covers it |
| 3 | `awaiting_answer` | 답변 대기 | its assignee has an open escalation on it |
| 4 | `awaiting_acceptance` | 수락 대기 | proposed |
| 5 | `blocked` | 막힘 | blocked |
| 6 | `vacant` | 자리 빔 | accepted, but the assignee's seat is vacant, pending or quiet |
| 7 | `working` | 진행 | accepted |

Views list held duties first and drop ended ones. The roster pane orders
duties by stage (held, awaiting answer, awaiting acceptance, blocked, vacant,
working).

## Refusal reasons

`leased_by`, `stale_binding`, `pending`, `cancelled`, `out_of_scope`,
`not_member`, `no_grant`, `invalid_scope`, `no_user_evidence`,
`unresolved_caller`, `not_found`, `invalid_argument`, `conflict`, `busy`,
`store_write_failed`, `store_corrupt`, `store_unsupported`, `held`,
`pager_unavailable`.

`store_unsupported` means the state was written by a newer schema; nothing
falls back from it. `held` names the effective release condition and how many
more holds remain.

With `--json`, a refusal prints `{"ok": false, "reason": "...", "message": "..."}`
and exits 1.

## Card JSON

`gaebu me --json` prints the following Card fields; `schedule` is omitted for unregistered teams:

| Field | Type | Meaning |
| --- | --- | --- |
| `rev` | number | store revision the card was built from |
| `member` | string | member id |
| `alias` | string | pager alias of the binding |
| `title` | string | display title |
| `team` | string | team name |
| `bu` | string | bu name |
| `binding` | string | `pending`, `bound`, `quiet` or `vacant` |
| `chain` | string[] | 대협 → commander → ... → me; the commander reads "가절월(추론)을 받은 …" when the root grant's evidence is inferred |
| `superior` | string or null | superior's display name |
| `duties` | object[] | open duties, held first: `id`, `status`, `stage`, `stage_label`, `outcome`, `scope`, `done_criteria`, `decision_rights`, `hold` (release condition, `(+N)` more), `source` (quote), full intent/escalation/plan/execution-root/version/note/issuer/assignee and original-unit fields |
| `peers` | string[] | same-bu members, the bu head, direct subordinates, holders of leases overlapping my duties |
| `leases` | string[] | my live leases |
| `escalations` | string[] | open escalations I raised or must answer |
| `cancels` | string[] | cancelled team, bu or duties that affect me |
| `norms` | string[] | one or two behaviour reminders (see `docs/protocol.md`) |
| `schedule` | object, optional | canonical Schedule observation, qualified viewer identity, role, management counts and next judgment; errors are explicit |
| `hash` | string | sha256 of the card's semantic content, including its Schedule observation when registered |

The text form (`gaebu me --card`, also what hooks inject) is at most 12 lines.
Cancels, open escalations and up to three `보류: 소임 <id> — <until>` lines
survive truncation; more holds collapse into `보류 +N`.

## Board JSON

`gaebu board --json` prints `{rev, token, teams: [...], user_queue: [...]}`.
Each team lists its bus (with any `hold`), and each bu lists its members with
display name, binding (`quiet` included), open duties as duty views in stage
order, and live leases. MCP `board` returns the same view for the caller's
team. Units are parent-first with `parent` and `depth`, so clients do not flatten the hierarchy. Registered teams include their derived `schedule` view. `duty show` and MCP `duty_get` read complete contracts, including ended duties, within the requesting member's team.

`token` is a hash of the view itself without `rev`, so it changes when a stage
changes with time alone (a seat falling quiet) and stays when a commit changes
nothing the board shows. `--since
<token>` prints `{"rev": N, "token": "...", "unchanged": true}` when the board
still has that token.

## Files

```text
~/.gaebu/state.json            whole state, replaced atomically under state.lock
~/.gaebu/state.prev.json       previous generation
~/.gaebu/state.v1.json         the schema-1 original, kept once by the migration
~/.gaebu/state.v2.json         the schema-2 original, kept once by the migration
~/.gaebu/state.v3.json         the schema-3 original, kept once by the migration
~/.gaebu/archive/<team>/duties.jsonl      retired full duty contracts
~/.gaebu/archive/<team>/escalations.jsonl retired closed questions
~/.gaebu/archive/<team>/records.jsonl     retired team records
<project>/schedule.md          canonical operational work and review evidence
<project>/schedule.md.lock     sidecar OS lock, never deleted
~/.gaebu/state.lock            OS lock, never deleted
~/.gaebu/sessions/<id>/events.jsonl    hook events: prompts, excluded prompts, Stop, mod heartbeats
~/.gaebu/sessions/<id>/composer.jsonl  composer-attested prompt hashes (Claude mod)
~/.gaebu/sessions/<id>/sighting.json   last seen, last injected card hash, mod and compose heartbeats
~/.gaebu/capture.on                    dev switch: save raw hook stdin under capture/
```

`GAEBU_HOME` overrides `~/.gaebu`.

## Runtime history

`State.archives[team]` contains the committed byte lengths `duties`,
`escalations` and `records` of that team's JSONL files. Runtime compaction
keeps terminal duties for at least 24 hours after `updated_at`, closed
escalations for 24 hours after closure, and records for one hour. Duties
covered by any human hold or referenced by an open escalation remain hot.
Teams, bus, members, grants, root idempotency keys and Schedule registrations
remain hot; the allocator and runtime identity are preserved.

A linked duty of an Active team retires only when the caller holds that
team's Schedule observation with matching registration/document/reference ID
and a tombstone for its item. Other Active teams' linked duties remain hot.
Terminal linked duties of Cancelled teams can retire without their Schedule,
because those teams cannot materialize another dispatch. Retention, holds and
open escalations still apply to that exception.

Automatic compaction runs after a member operation has committed, in a
separate transaction, when at least 64 eligible entities exist across the
collections and teams. A failure prints a warning without failing the member
operation. Explicit compaction uses a threshold of one and reports errors.
Root operations and hooks do not trigger compaction. Neither path takes a
Schedule lock inside `state.lock`.

Archive appends first check that the committed prefix exists, is long enough,
and ends in a newline. They truncate only its uncommitted suffix, append full
entities, and sync files and directory links before publishing boundaries
and removing hot entries in one state commit. Appends do not reparse old
entries. A failed state publication leaves hot entries authoritative and an
uncommitted suffix that the next append discards.

`duty show` and MCP `duty_get` acquire `state.lock`, re-read the current state
and caller's team, and choose the hot contract or stream the caller team's
committed duty archive. They return the last matching ID and validate every
line in the committed region. Missing, short, malformed or unterminated
committed duty data returns `store_corrupt`; suffix bytes are ignored.
A foreign hot duty retains the existing `no_grant` refusal. A foreign archived
duty returns `not_found`, since only the caller team's archive is searched.
Mutating an archived duty or escalation returns `not_found` with archive
guidance. Repair restores earlier boundaries and the later-retired entities
hot from the previous state; lookup holds the same lock as repair and
truncation, so stale snapshots cannot select obsolete boundaries.

Archive files grow with history. Length/newline validation before append is
constant in archive size, but full duty lookup scans that team's committed
history under the state lock. The CLI lock/publication budget is five seconds;
synchronous archive I/O and parsing cannot be interrupted. Large histories or
slow disks can exceed that budget and delay other writers. The 10,000-entry
`runtime_compact::archive_large_cost` test reports observed costs on its host;
it does not establish a portable latency bound.

## Schema 4 migration

The first write or repair of schema 1/2/3 backs up the exact original bytes to
`state.v1.json`, `state.v2.json` or `state.v3.json` without replacing a previous backup. Migration
publishes schema 4 to `state.prev.json` and then `state.json` before running the
requested operation. A crash repeats the migration safely; older binaries cannot
fall back to an older generation and ignore Schedule guards.

Schema 1 still receives evidence strength, notice deduplication and escalation
normalization. Legacy schemas gain default Schedule/archive fields and a stable
runtime origin if absent. Older closed escalations default `closed_at` to null.
The requesting operation runs only after migration commits. Older binaries
refuse schema 4 rather than dropping archive boundaries on their next write.

The schema probe precedes full decoding. Future schemas return
`store_unsupported` and never fall back or get overwritten by repair. Install the
new binary and restart resident MCP processes before coordinated work resumes.

Diagnostic `Store::read` retains its previous-generation recovery behavior.
Current-state absence proofs use `read_current` and never use recovery data.
CLI/MCP/hook views use `read_view`: a genuinely fresh store is empty, while a
corrupt or missing current generation in an existing runtime is unavailable, so
the UI can retain a visibly stale observation instead of displaying healthy old
counts. `gaebu repair` remains the explicit recovery path.

## Rejected concepts

| Concept | Why not |
| --- | --- |
| A hold an agent sets or lifts | A stop is the person's: holds come and go only through root directives that quote the person's current prompt. A commander that wants work stopped cancels or amends it. |
| `DutyStatus::Held` | A status would lose what the duty was doing; a separate `hold` field returns it to its own status when lifted. |
| Lifting a hold on a question, a new clue or automated feedback | None of these is the person deciding to resume. |
| An unverified `source` | A quote that matches no prompt recorded in the issuer's own session is refused, not stored as provenance. |
| Notices for progress | Accepting, unblocking and other changes nobody must act on wait for the next card; interrupting for them spends attention for nothing. |
| Root directives over MCP | MCP callers cannot show the person's current prompt; holds, appointments and dismissals stay on the CLI path the hook records. |
| Blocking inferred directives | "가절월 줄게" is a valid way to appoint; `strength` records how direct the words were instead of refusing them. |
| A countdown on the card | It would change the card hash every minute and re-inject it; `gaebu board` shows expiry. |
