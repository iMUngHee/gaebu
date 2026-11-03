# Gaebu data model

This is the contract for what Gaebu stores and prints. The Rust types in
`src/model.rs` and `src/card.rs` are its source of truth; this page explains
them. The JSON in `~/.gaebu/state.json` is exactly their serde form, with
`schema_version: 1`.

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
escalation, cancel, vacant.

Display titles (승상, 대도독, 좌장군, 주부, ...) are cosmetic. Authority comes
only from grants.

## Entities

- **Team**: one commander and everyone under them. `project` is a label that
  scopes are relative to. `root_bu` is the commander's own bu (승상부 by
  default). `status` is `active` or `cancelled`.
- **Bu**: `parent` (none for the root bu), `head` (a member), `name` (the
  head's title + 부 by default), `status`: `open`, `closed` or `cancelled`.
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
- **Duty**: `intent`, `outcome`, `scope`, `decision_rights`, `escalate_on`,
  `done_criteria`, optional `plan_ref` and `exec_root`, `status` (`proposed`,
  `accepted`, `declined`, `blocked`, `done`, `cancelled`, `handed_off`), and a
  `version` for optimistic concurrency.
- **Lease**: a `key` (a scope pattern or `res:<name>`), `mode` (`exclusive` or
  `shared`), a holder member and session, and `expires_at`. A lease resolves
  conflicts. It confers no decision rights.
- **Escalation**: `from`, `to` (none means the user's queue), an optional
  `duty`, a `question` and an `answer`.
- **Record**: `kind` (`decision`, `handoff`, `change`, `note`), `author`,
  `refs`, `text`.
- **Outbox**: notices waiting to be delivered with `pager send <session_id>`.

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

## Refusal reasons

`leased_by`, `stale_binding`, `pending`, `cancelled`, `out_of_scope`,
`not_member`, `no_grant`, `invalid_scope`, `no_user_evidence`,
`unresolved_caller`, `not_found`, `invalid_argument`, `conflict`, `busy`,
`store_write_failed`, `store_corrupt`, `pager_unavailable`.

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
| `chain` | string[] | 대협 → commander → ... → me |
| `superior` | string or null | superior's display name |
| `duties` | object[] | open duties: `id`, `status`, `outcome`, `scope`, `done_criteria`, `decision_rights` |
| `peers` | string[] | same-bu members, the bu head, direct subordinates, holders of leases overlapping my duties |
| `leases` | string[] | my live leases |
| `escalations` | string[] | open escalations I raised or must answer |
| `cancels` | string[] | cancelled team, bu or duties that affect me |
| `norms` | string[] | one or two behaviour reminders (see `docs/protocol.md`) |
| `hash` | string | sha256 of the card text, used to decide full or one-line injection |

The text form (`gaebu me --card`, also what hooks inject) is at most 12 lines.

## Board JSON

`gaebu board --json` prints `{rev, teams: [...], user_queue: [...]}`. Each team
lists its bus, and each bu lists its members with display name, binding, open
duties and live leases. `--since <rev>` prints `{"rev": N, "unchanged": true}`
when nothing changed.

## Files

```text
~/.gaebu/state.json            whole state, replaced atomically under state.lock
~/.gaebu/state.prev.json       previous generation
~/.gaebu/state.lock            OS lock, never deleted
~/.gaebu/sessions/<id>/events.jsonl    hook events: prompts, excluded prompts, Stop, mod heartbeats
~/.gaebu/sessions/<id>/composer.jsonl  composer-attested prompt hashes (Claude mod)
~/.gaebu/sessions/<id>/sighting.json   last seen, last injected card hash, mod heartbeat
~/.gaebu/capture.on                    dev switch: save raw hook stdin under capture/
```

`GAEBU_HOME` overrides `~/.gaebu`.
