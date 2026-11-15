# Stall detection

`gaebu stalled` lists work that waits on a session observed to be resting.
It answers one question a person otherwise answers by walking every session
window: who has to move, and are they moving?

```sh
gaebu stalled                 # one line per finding
gaebu stalled --json          # the same as data
gaebu stalled --after 30      # waits of at least 30 minutes (default 10)
```

The command needs no session and belongs to no team. It reads the state, the
session logs the hooks write, each team's registered Schedule and one
`pager who --all`. It changes none of them: it does not refresh bindings, so
a seat pager reports gone stays bound until a member's own command observes
it. The only thing it may create is an empty Gaebu home, when there was none.

## What "resting" means

A session rests when the last event its hook recorded is a `Stop` at least
`--after` minutes old. A prompt, a pager poke and a mod heartbeat after the
`Stop` all start a turn, so that session is then working.

This is an observation, not a verdict. Three cases look the same from the
log, and the command cannot tell them apart:

- A session whose person is reading its answer rests the same way.
- Another `Stop` hook may have continued the turn, and a peer message may
  have arrived without a `UserPromptSubmit`; see the
  [threat model](threat-model.md).
- A turn that is running has no later event either, so a turn that died
  midway is never reported. Nothing is recorded inside a turn.

When a prompt could not be recorded after the `Stop`, or the session has no
log at all, the session is not judged and the finding is `unknown`. Codex
sessions record the same three events and are judged the same way; that path
has not been verified on a Codex host.

## Findings

| Kind | When | Waits on |
| --- | --- | --- |
| `seat_pending` | A seat was designated at least `--after` ago and its session has not confirmed it | that seat |
| `team_idle` | A team has no open duty and no live Schedule item, and its commander rests | the commander |
| `idle_after_stop` | A duty is accepted, not held and not waiting on a question, and its assignee rests | the assignee |
| `counterpart_idle` | A duty waits on someone else who rests: the member a question went to, the assignee of a proposed duty, or the issuer of a blocked one | that member |
| `ready_but_idle` | A Schedule item is `ready` and its task master, the head of the item's unit, rests | the task master |
| `host_gone` | An open duty's assignee is bound to a session pager reports gone | the assignee, whose session is gone |
| `unknown` | A session log, a Schedule or pager's roster could not be read | — |

Left out on purpose:

- Anything under a human hold: a held duty, and every seat, duty and
  Schedule item of a held unit or of a unit below it. Only the human lifts
  a hold.
- A question in the human's own queue. `gaebu board` shows it.
- Cancelled teams, and duties whose unit was cancelled.
- A Schedule that cannot be read. It is reported as `unknown`, never counted
  as empty, so it cannot make a team look idle.

`idle_after_stop` is judged from the duty and the binding, not from the
stage a card shows. After two hours a card calls a resting seat quiet and its
duty vacant; the finding stays.

## Who should hear of it

Each finding carries `report_to`: the team's Schedule manager, else its
commander, else the human. A member is skipped when it is the one waited on,
holds no confirmed seat, or has a host pager reports gone. The command only
computes this; it sends nothing.

## Keys

Each finding has a 16-character `key`. It stays the same while the same wait
lasts and changes when the wait recurs, so a caller can tell a finding it has
already acted on from a new one.

| Kind | The key covers |
| --- | --- |
| `seat_pending` | the seat, its alias and when it was designated |
| `team_idle` | the team, its commander, that session and its last `Stop` |
| `idle_after_stop`, `counterpart_idle` | the duty and its version, the member waited on, that session and its last `Stop` |
| `ready_but_idle` | the item, its task master, that session and its last `Stop` |
| `host_gone` | the duty, its assignee and the bound session |
| `unknown` | the team and the subject that could not be judged, not the cause |

Every key also covers the store's runtime id, so two machines never share
one.

The session is part of the key because event numbers count within one
session only. The member who would hear of the finding is not part of it.
A hold, or one failed read, can hide a finding and bring it back under the
same key: the wait did not end in between.
