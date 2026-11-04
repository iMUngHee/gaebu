# Gaebu data model

This is the contract for what Gaebu stores and prints. The Rust types in
`src/model.rs` and `src/card.rs` are its source of truth; this page explains
them. The JSON in `~/.gaebu/state.json` is exactly their serde form, with
`schema_version: 2`. A schema-1 file is migrated on first use (see
[Schema 2 migration](#schema-2-migration)).

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
  The root grant keeps the appointment's evidence; every other root directive
  keeps its own in its `root_actions` entry.
- **Duty**: `intent`, `outcome`, `scope`, `decision_rights`, `escalate_on`,
  `done_criteria`, optional `plan_ref` and `exec_root`, `status` (`proposed`,
  `accepted`, `declined`, `blocked`, `done`, `cancelled`, `handed_off`), a
  `version` for optimistic concurrency, an optional `hold`, and an optional
  `source`: the user's words the duty answers (`session_id`, `prompt_seq`,
  `sha256`, `quote`, `at`), matched against a prompt the hook recorded in the
  issuer's own session. Amending without a source keeps the one recorded.
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
- **Record**: `kind` (`decision`, `handoff`, `change`, `note`), `author`,
  `refs`, `text`.
- **Outbox**: notices waiting to be delivered with `pager send <session_id>`.
  `body` is what is sent; `text` is the body without its record id, the
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

## Notices

Every change is a team record. Only a change its recipient must act on
interrupts them with a pager notice (Act); the rest wait for the next card
(Record).

| Act (pager notice) | Record (team record and card only) |
| --- | --- |
| duty issued, amended, cancelled (to the assignee) | duty accepted, unblocked |
| duty declined, blocked, done, handed off (to the issuer) | grant delegated |
| hold set or lifted (to the assignees it covers) | bu closed, title changed |
| escalation raised, superseded, rerouted, withdrawn (to the recipient) | binding confirmed or awaiting confirmation |
| escalation answered (to the asker) | appointment, assignment, member added, seat rebound |
| lease force-released (to the holder) | member vacant |
| bu or team cancelled, member dismissed, grant revoked, member moved | lease acquired, renewed, released |
| bu opened (to the new head) | |

No notice goes to the actor's own session, to a recipient with no bound
session, or when the newest notice still queued for that session has the same
`text` (also while a flush holds it), so a change that flips back and forth
still arrives in order. Once delivered, the same change raised again is a new
notice.

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

`gaebu me --json` prints a Card with exactly these fields:

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
| `duties` | object[] | open duties, held first: `id`, `status`, `stage`, `stage_label`, `outcome`, `scope`, `done_criteria`, `decision_rights`, `hold` (release condition, `(+N)` more), `source` (quote) |
| `peers` | string[] | same-bu members, the bu head, direct subordinates, holders of leases overlapping my duties |
| `leases` | string[] | my live leases |
| `escalations` | string[] | open escalations I raised or must answer |
| `cancels` | string[] | cancelled team, bu or duties that affect me |
| `norms` | string[] | one or two behaviour reminders (see `docs/protocol.md`) |
| `hash` | string | sha256 of the card text, used to decide full or one-line injection |

The text form (`gaebu me --card`, also what hooks inject) is at most 12 lines.
Cancels, open escalations and up to three `보류: 소임 <id> — <until>` lines
survive truncation; more holds collapse into `보류 +N`.

## Board JSON

`gaebu board --json` prints `{rev, token, teams: [...], user_queue: [...]}`.
Each team lists its bus (with any `hold`), and each bu lists its members with
display name, binding (`quiet` included), open duties as duty views in stage
order, and live leases. MCP `board` returns the same view for the caller's
team.

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
~/.gaebu/state.lock            OS lock, never deleted
~/.gaebu/sessions/<id>/events.jsonl    hook events: prompts, excluded prompts, Stop, mod heartbeats
~/.gaebu/sessions/<id>/composer.jsonl  composer-attested prompt hashes (Claude mod)
~/.gaebu/sessions/<id>/sighting.json   last seen, last injected card hash, mod and compose heartbeats
~/.gaebu/capture.on                    dev switch: save raw hook stdin under capture/
```

`GAEBU_HOME` overrides `~/.gaebu`.

## Schema 2 migration

The first transaction (or `gaebu repair`) that finds `schema_version: 1`
migrates it in its own commit, before any change it was asked to make:

1. If `state.v1.json` does not exist, the original bytes are written there.
   An existing backup is never replaced.
2. The v2 state is written to `state.prev.json`, then to `state.json`. An
   interruption at any point leaves `state.json` at schema 1, so the next run
   repeats the migration; once `state.json` is v2, no schema-1 generation is
   left for an older binary to fall back to.
3. Migration fills `strength: inferred` on existing evidence, keeps each
   notice's `body`, attempts and claim and derives its `text`, and keeps only
   the newest open escalation per asker and duty (the rest become
   `superseded`).

The schema is read before the rest of the file, so a newer schema is
`store_unsupported` even when this model cannot parse it, and neither reads
nor `gaebu repair` fall back from it or overwrite it. A gaebu 0.1 binary
reports a v2 file as corrupt and finds no v1 generation to fall back to, so it
fails closed. Install the new binary, restart sessions that run `gaebu mcp`,
and the next command migrates.

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
