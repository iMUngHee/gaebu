# Gaebu (개부)

Gaebu is a thin support layer for teams of independent coding-agent sessions
(Claude Code and Codex CLI). It records who commands whom, what each member
owes, who holds which resource, and what was cancelled. Each session can re-read
its own position after compaction or a restart.

The engine never decides. Agents plan, delegate and judge. Gaebu stores, checks,
injects and notifies. It works next to [pager](https://github.com/iMUngHee/pager),
which delivers messages between sessions, but it is a separate tool.

## Why

When several equal-capability agents work on one project, three things tend
to go wrong:

- The commander keeps integration and approvals to itself and becomes the
  bottleneck.
- Authority does not carry across sessions. Agents rightly treat peer messages
  as data, so the user has to tell every worker "do what X says".
- Context is lost at compaction and session boundaries: who is my superior,
  what did I promise, what did someone cancel.

Gaebu keeps that state outside any one context window and hands each member
a short position card.

## Vocabulary

The names follow a Three Kingdoms motif, adapted for software rather than
reconstructed from history.

| Term | Meaning |
| --- | --- |
| 가절월 | Command authority over a team. The user gives it to one session. |
| 개부권 | The right to open a 부 (unit) below you and recruit into it (verb `open`). |
| 부 | A unit, named after its head's title by default (좌장군 → 좌장군부). |
| 속관 | A member of a 부, shown as "좌장군부 속관 주부 nuzu". |
| 소임 | A duty: intent, outcome, scope, decision rights, escalation triggers, done criteria. |
| 팀기록 | Decisions, handoffs and changes worth keeping. |

Titles (승상, 대도독, 좌장군, 주부, ...) are display only. Authority comes from
grants, and grants only narrow as they go down the tree.

Scheduling can stay with the commander or be delegated to a separate manager.
A canonical project `schedule.md` keeps unassigned work, dependencies, operational
priority and review acceptance between PM items and individual duties. Task
masters retain command of their workers; changing the scheduler does not add a
second superior. Units form a real parent/child tree and can be closed and reused.
See [schedules and nested units](docs/schedules.md) for setup and recovery.

## How it flows

1. The user tells a session in plain words, for example "너한테 가절월 줄게.
   nawi를 속관으로 주마." The agent interprets it and runs
   `gaebu appoint`/`gaebu assign`. It quotes the user's words with
   `--evidence`, and Gaebu checks that the quote really is in that session's
   current user prompt. Peer and pager text does not count.
2. The commander organizes autonomously. It can open a 부 (`gaebu bu open`)
   and issue duties (`gaebu duty issue`). Subordinates may sub-delegate within
   what they were given.
3. Each member's hook injects a position card (≤12 lines): its chain of
   command, open duties, peers, leases, escalations and cancellations.
4. Members talk directly through pager. They take leases for shared resources,
   escalate what is outside their decision rights and keep working on what is
   not blocked, and record handoffs.
5. Cancellation is sticky. Nothing new can be issued, accepted or leased under
   a cancelled 부.
6. The user can stop work in their own words ("잠깐 멈춰, 내가 확인할 때까지").
   The agent runs `gaebu hold <duty|bu> --until "..." --evidence "..."`, and the
   work stays held until the user lifts it with `gaebu release`. Questions,
   peer messages and bot feedback never lift a hold.
7. Members are interrupted only for changes they must act on. Everything else
   waits for the next position card, and the roster lists each duty by stage
   (보류, 답변 대기, 수락 대기, 막힘, 자리 빔, 진행).

See [docs/model.md](docs/model.md) for the data contract,
[docs/protocol.md](docs/protocol.md) for the behaviour rules agents follow
(with sources), and [docs/threat-model.md](docs/threat-model.md) for what the
checks do and do not protect against.

## Install

You need Rust (stable) and, for notifications and identity, `pager` on `PATH`.

```sh
cargo install --git https://github.com/iMUngHee/gaebu --locked
```

Or from a checkout: `scripts/install.sh` (macOS/Linux) or
`scripts/install.ps1` (Windows). Both build a release binary into
`~/.local/bin`.

On Windows without the Visual Studio Build Tools (the MSVC linker), build with
the GNU toolchain, which needs no external linker for gaebu's dependencies:

```sh
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
cargo +stable-x86_64-pc-windows-gnu install --git https://github.com/iMUngHee/gaebu --locked
```

### Upgrading to 0.5

**Old archive history is permanently deleted by default above 64 MiB per
Schedule or team/stream.** Set `GAEBU_ARCHIVE_LIMIT_MIB` to a positive integer
before opening a store, raise it or back up the files if history must be kept.
Each archive rolls at one eighth of that limit; the newest segment is retained
and a failed deletion is reported for retry rather than silently discarded.

Runtime state uses schema 5 and Schedule documents use schema 3. The first
runtime write preserves the exact old state in its versioned backup (including
`state.v4.json`) and publishes both generations in the new schema. Existing
JSONL files become segment 1 in place; later files use `.<n>.jsonl`. Upgrade
writers and reconnect persistent `gaebu mcp` processes before continuing work.
See [the migration contract](docs/model.md#schema-5-migration).

Keep all listed archive segments with a Schedule when backing up or moving it.
`schedule move` requires every listed file; run `schedule archive` first to
normalize expired prefixes. `schedule show --all` reports retained history and
`counts.expired`; an expired item is never assumed complete. Retained completion
tombstones still satisfy dependencies. `gaebu compact` and `schedule archive`
enforce a lowered limit even without new rows and remove unpublished leftovers.
A sole oversized newest segment remains until a successor can replace it.
See [Schedule storage](docs/schedules.md) for expiry and recovery behavior.

0.5.1 adds explicit `gaebu compact --heal` (commander) and
`gaebu schedule archive --heal` (commander or Schedule manager) for missing-segment
holes. Healing permanently deletes older history, including unlisted older segment
files. It reports listed and unlisted removals separately and can be retried after a
partial failure. Ordinary commands and automatic maintenance never heal. Storage
schemas remain unchanged; see [hole recovery](docs/schedules.md#healing-an-archive-hole).

### Claude Code

Register the hook for `SessionStart`, `UserPromptSubmit` and `Stop`:

```json
{ "type": "command", "command": "gaebu hook claude", "timeout": 10 }
```

Register the MCP server (optional; the CLI covers everything):

```json
{ "mcpServers": { "gaebu": { "command": "gaebu", "args": ["mcp"] } } }
```

The mod in `mod/` adds the card as a system prompt section, records
composer-stamped prompts as stronger evidence, and shows a roster pane
(`/gaebu`), a band line above the prompt and a status line. While its
`prompt.compose` hook reports adding the section, the shell hook sends only a
one-line card. On a machine with managed settings, or in Team and Enterprise
organizations, `cc-plugin-sec-default` passes `prompt.compose` on past
installed plugins, so the section never lands and the shell hook sends its
cards as it does without the mod. Load it with `claude --plugin-dir mod` or
through `CLAUDE_CODE_PLUGIN_DIRS`, or install it from this repository's
marketplace: `/plugin install gaebu --marketplace iMUngHee/gaebu`.

### Codex CLI

```toml
[[hooks.SessionStart]]
[[hooks.SessionStart.hooks]]
type = "command"
command = "gaebu hook codex"

[[hooks.UserPromptSubmit]]
[[hooks.UserPromptSubmit.hooks]]
type = "command"
command = "gaebu hook codex"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "gaebu hook codex"

[mcp_servers.gaebu]
command = "gaebu"
args = ["mcp"]
```

Codex runs a newly added hook only after you trust it: open the interactive
`codex`, which reports that hooks need review, and approve the three
`gaebu hook codex` entries with `/hooks`. Until then `codex exec` skips them
without a message. Trust is stored per machine in `~/.codex/config.toml`
(`[hooks.state.*].trusted_hash`). On Windows, a hook that Codex starts through
PowerShell must read its stdin as UTF-8, or non-ASCII prompts arrive garbled and
root directives quoting them are refused.

## Everyday commands

```sh
gaebu me                         # my card and duties
gaebu board                      # the whole roster
gaebu schedule show              # operational queue, review and next decisions
gaebu duty show d-12              # full contract, including completed work
gaebu bu open --head nuzu --verbs assign,open --scope 'src/**'
gaebu duty issue --to bepo --outcome "hook adapter" --scope src/hook.rs \
  --rights "internals are yours" --escalate-on "public signature" --done "cargo test"
gaebu duty accept d-12
gaebu lease acquire res:gpu --ttl 30 --reason "benchmark"
gaebu escalate "DTO change needed" --duty d-12
gaebu record "switched to JSON store" --kind decision
```

Every refusal names a reason (`no_grant`, `out_of_scope`, `leased_by`,
`cancelled`, `stale_binding`, `no_user_evidence`, ...). Add `--json` for
machine-readable output.

## State

Runtime organization lives under `~/.gaebu` (override with `GAEBU_HOME`). Project
Schedules remain at their registered canonical paths. The runtime state is
one JSON file replaced atomically under an OS lock, plus small per-session
event logs. Gaebu has no daemon and makes no network calls. It talks to other
sessions only through `pager send`.

## Releasing

Every version bump comes with patch notes. In the same commit:

1. Set the version in `Cargo.toml` (then `Cargo.lock`) and in
   `mod/.claude-plugin/plugin.json`.
2. Add `## <version> — <date>` at the top of [CHANGELOG.md](CHANGELOG.md):
   what changed for someone running Gaebu, with commit ids, and an
   Upgrading note on state compatibility and what to restart.

`tests/changelog.rs` fails `cargo test`, and so CI, while either is missing.

## License

MIT
