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
holds no confirmed seat, or has a host pager reports gone. Without
`--notify` the command only computes this and sends nothing.

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

## Reporting findings

```sh
gaebu stalled --notify            # report what is due, then exit
gaebu stalled --notify --after 30
```

`--notify` is meant for a timer. It finds the same stalls and reports each
one that is due: to the member in `report_to` through pager, or to the human
through a command the human configures. `unknown` findings are never
reported; they only appear in the list.

It still writes nothing to the state, the session logs or the Schedules. It
keeps its own two files in the Gaebu home: `stalled-notify.json`, the ledger
of what was sent, and `stalled-notify.lock`, so that a timer and a person do
not run it at once.

### To a member

Each finding is one pager message, sent without a session:

```sh
pager send --event <key>-<first seen>-r<round> --kind failure \
  --automation gaebu-stalled --task <team id> -- <alias> <text>
```

The recipient sees the sender `automation:gaebu-stalled`. The wake arrives as
an ordinary pager poke, which Gaebu's hook never takes for the human's words,
so a report cannot authorize a root directive. This needs pager 0.8.0 or
later; an older pager refuses `--automation` and the send counts as failed.

- **Rounds.** The first report goes out when the finding is first seen, the
  second `--after` minutes after the first was accepted, the third twice
  that later. If the finding is still there four times `--after` after the
  third, the human is told once and that finding is not reported again.
- **One text per round.** The text holds nothing that changes between runs.
  A run that dies between recording a round and sending it sends the same
  event and text again, which pager treats as the message it already has.
- **Failures.** A send pager rejects leaves the round as it was. After three
  rejections of the same round the human is told instead, so a recipient
  pager cannot resolve is not retried by every run for ever. "Accepted" means
  pager stored the message, not that anyone read it.
- **Limit.** One member gets at most five reports from one run. The rest go
  out on later runs.
- **A change of recipient** starts over at round one for the new recipient.
- **A finding that goes away** keeps its rounds for seven days. If it comes
  back under the same key, as it does after a hold is lifted, the rounds
  continue where they stopped. After seven days the entry is dropped, and a
  later return starts at round one under an event pager has not seen.

### To the human

The human is told when a finding has nobody to report to, when three rounds
changed nothing, and when a report could not be delivered. Set
`GAEBU_STALLED_NOTIFY` to a command:

```sh
export GAEBU_STALLED_NOTIFY='notify-send "개부" "$GAEBU_STALLED_BODY"'
```

The command runs through `sh -c` with the line in `GAEBU_STALLED_BODY`. The
line is never part of the command text. What the command prints goes to
standard error, and so does the line itself when the variable is not set, so
standard output carries only the run's summary. The shell is killed if it
has not finished in 30 seconds; a process it started in the background can
outlive it. Each line is delivered once; a command that fails is tried on
three runs and then given up with a line on standard error. `sh` has to
exist, which on Windows it does not by default. If the run dies after the command
succeeded and before that is recorded, the line is delivered once more.

### When the ledger cannot be read

A ledger that does not parse is moved to
`stalled-notify.corrupt-<time>.json`, the human is told, and an empty ledger
takes its place. Findings still present then get up to three rounds again,
under new events. A ledger that cannot be opened at all stops the run with
an error and nothing is sent.

### Timers

Run it as the user who owns the Gaebu home and the pager database, every few
minutes. The interval only decides how soon a due report goes out; rounds
are spaced by `--after`.

A timer is registered once per machine and survives reboots and Gaebu
reinstalls at the same path, because it only names the binary. It is not carried to
another machine: register it again there.

systemd (`~/.config/systemd/user/gaebu-stalled.service` and `.timer`):

```ini
[Unit]
Description=Report stalled gaebu seats and duties

[Service]
Type=oneshot
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin
Environment="GAEBU_STALLED_NOTIFY=notify-send -a gaebu gaebu \"$GAEBU_STALLED_BODY\""
ExecStart=%h/.local/bin/gaebu stalled --notify
```

```ini
[Unit]
Description=Run gaebu stalled --notify every five minutes

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min

[Install]
WantedBy=timers.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now gaebu-stalled.timer
```

The service needs `PATH` to reach `pager`: a user unit does not inherit the
login shell's `PATH`, and Gaebu runs `pager` by name. systemd leaves `$` in
`Environment=` alone, so `sh` expands `$GAEBU_STALLED_BODY` when the command
runs. Leave the second `Environment=` line out to get the human's lines in
the journal instead (`journalctl --user -u gaebu-stalled.service`).

launchd (`~/Library/LaunchAgents/dev.gaebu.stalled.plist`): a job whose
`ProgramArguments` are `gaebu`, `stalled`, `--notify` with `StartInterval`
set to `300`. Give it `PATH` through `EnvironmentVariables` for the same
reason.

Windows Task Scheduler:

```powershell
schtasks /Create /SC MINUTE /MO 5 /TN gaebu-stalled /TR "gaebu stalled --notify"
```

The systemd entry has been installed and run on Linux, with no finding to
report at the time, so its `notify-send` line has not fired. The launchd and
Task Scheduler entries are sketches that have not been installed.

### Order on another machine

1. Upgrade pager to 0.8.0 or later.
2. Install Gaebu 0.9.0 or later.
3. Restart every session that was running before step 1.
4. Register the timer, if this machine should send reports.

### Moving to pager 0.8.0

pager 0.8.0 moves its database to schema 6, which an older pager cannot
open. Upgrade the installed pager, then restart every running pager MCP
server and session that holds the old binary. Until then `--notify` fails
its sends and reports nothing to members; `gaebu stalled` without `--notify`
works with any pager.
