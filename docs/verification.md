# Verification snapshot

## 0.2.0 (plan `gaebu-v0-2`)

Checked on 2026-11-04 on Windows with `stable-x86_64-pc-windows-gnu`, branch
`agent/gaebu-v0-2` from base `62a0cd3`. The 0.1 record follows below unchanged.

| Criterion | Status | Evidence |
| --- | --- | --- |
| Static checks and suite | PASS | `cargo fmt --check` 0, `cargo clippy --all-targets -- -D warnings` 0, `cargo test` 172 passed, 0 failed, 2 helper entry points ignored (0.1: 137), after the code-review fixes below. |
| Named criteria tests | PASS | Each of the 31 tests named in the plan's criteria, run alone with `--exact`, reports `1 passed; 0 failed` (list below). |
| Mod | PASS | `claude plugin test mod` 9 pass, 0 fail (stage order, status count, board token, discarded `card-1`/`board-1` values); `claude plugin validate mod` passes with the 0.1 warnings; `tsc` 0 errors (the 17 type errors 0.1 carried are fixed). |
| Release MCP round trip | PASS | Release `gaebu mcp` in an isolated home: protocol `2025-06-18`, 12 tools including `escalation_withdraw`, no `hold`/`release`; the same question twice returns the same `e-10`, the second `escalation reused`; `board` returns a view with a token. |
| Live natural-language hold | PASS | Isolated home, a real `claude -p` session as the aide with `rules/gaebu.md` appended and the Claude hook registered through `--settings`. See below. |
| Per-commit code review | PASS | A fresh reviewer read all 9 commits: 0 high, 1 medium, 5 low. Fixed: a dead asker's escalation now goes to the user's queue; dedupe compares only the newest queued notice, so held/released/held arrives in order; the board token ignores `rev`; `until` collapses to one line; a misplaced doc comment. The cancel-and-reissue path around a duty hold is documented in the threat model. Regression tests: `flipping_change_keeps_notice_order`, `hold_until_stays_one_line`, the asker rows of `escalations_reroute_after_commit`, and the rev check in `board_since_detects_quiet_transition`. |
| Codex hook and MCP host | UNVERIFIED | The Codex hook is still not deployed; no Codex host ran 0.2. |
| Global deployment | UNVERIFIED | Not installed globally; `~/.local/bin/gaebu` is still 0.1.0. |
| Hosted CI | PASS | `.github/workflows/ci.yml` at `42f32cc`, run `41788533905`: fmt, clippy `-D warnings`, test and release build succeed on ubuntu-latest, macos-latest and windows-latest (MSVC). Annotations are GitHub notices only (actions/checkout@v4 runs on Node 24; the ubuntu-latest label migration). |

```text
core_store::v1_fixture_migrates_to_v2 -> 1 passed; 0 failed
core_store::migration_keeps_v1_backup_and_v2_prev -> 1 passed; 0 failed
core_store::future_schema_refused_without_fallback -> 1 passed; 0 failed
core_store::repair_refuses_unsupported_state -> 1 passed; 0 failed
core_store::repair_from_v1_prev_migrates_consistently -> 1 passed; 0 failed
core_store::migration_resumes_after_partial_write -> 1 passed; 0 failed
core_store::failed_migration_skips_user_change -> 1 passed; 0 failed
core_ops::hold_guard_matrix -> 1 passed; 0 failed
core_ops::bu_hold_covers_descendant_duties -> 1 passed; 0 failed
core_ops::amend_keeps_hold -> 1 passed; 0 failed
core_ops::lease_scope_excludes_held_duties -> 1 passed; 0 failed
core_ops::lease_expiry_keeps_hold -> 1 passed; 0 failed
e2e_cli::hold_requires_user_evidence -> 1 passed; 0 failed
e2e_cli::root_ops_record_strength -> 1 passed; 0 failed
evidence_rules::recorded_source_must_match_prompt -> 1 passed; 0 failed
core_ops::escalation_reuse_and_supersede -> 1 passed; 0 failed
core_ops::escalation_reuse_reroutes_when_recipient_unavailable -> 1 passed; 0 failed
core_ops::escalations_reroute_after_commit -> 1 passed; 0 failed
core_ops::nested_holds_release_independently -> 1 passed; 0 failed
core_ops::escalation_withdraw_only_by_asker -> 1 passed; 0 failed
core_ops::notice_classification_matrix -> 1 passed; 0 failed
core_ops::composite_ops_notice_totals -> 1 passed; 0 failed
core_ops::hold_survives_cancel_with_terminal_stage -> 1 passed; 0 failed
core_ops::duplicate_pending_notice_not_queued -> 1 passed; 0 failed
card_render::stage_first_matching_row -> 1 passed; 0 failed
card_render::card_keeps_holds_under_truncation -> 1 passed; 0 failed
card_render::inferred_root_grant_render -> 1 passed; 0 failed
e2e_cli::board_and_card_agree_on_stage -> 1 passed; 0 failed
e2e_cli::board_since_detects_quiet_transition -> 1 passed; 0 failed
e2e_cli::source_from_other_session_refused -> 1 passed; 0 failed
mcp_protocol::escalation_withdraw_tool -> 1 passed; 0 failed
```

### Live hold

The commander seat was this verification session (`gapu`); the aide seat was a
fresh `claude -p` session. Duty `d-10` (`docs/**`) was issued to the aide.
Four turns, each prompt typed as the user:

1. "개부 소임 d-10을 수락해." The agent ran `gaebu duty accept d-10`.
2. "d-10 잠깐 멈춰. 내가 확인할 때까지 기다려." The agent ran
   `gaebu hold d-10 --until "대협이 확인할 때까지" --evidence "d-10 잠깐 멈춰.
   내가 확인할 때까지 기다려." --explicit` and reported `held`.
3. "d-10 끝났으면 완료로 처리해." The agent did not finish it and kept the hold:
   it judged a conditional finish request no release.
4. Asked to run `gaebu duty done d-10` and show the answer, it reported
   `gaebu: held: duty d-10 is held until: 대협이 확인할 때까지` with exit 1.

The stored state afterwards:

```text
schema 2 status accepted version 2
hold until: 대협이 확인할 때까지
evidence: d-10 잠깐 멈춰. 내가 확인할 때까지 기다려. | explicit | hook_filtered | session c1ad56ff seq 6
records: ['binding confirmed for m-7', "new duty d-10 'docs 오탈자 정리' proposed", 'duty d-10 accepted', 'd-10 held until: 대협이 확인할 때까지']
  승상부 속관 장사 live02aide · bound
    소임 d-10 [보류] docs 오탈자 정리 — 대협이 확인할 때까지
```

The issue notice queued before the aide session existed was delivered once
pager knew that session (the aide reported pager message `#2819`). This is a
seeded team, not an operational human appointment.

## 0.1.0

This intermediate record covers commit `ea740d7`, checked on 2026-11-04 on
Windows with `stable-x86_64-pc-windows-gnu`. Release acceptance remains open for
re-review, hosted CI, global deployment and the runtime checks identified below.

PASS means the stated check was observed. FAIL means an observed acceptance gap.
UNVERIFIED means the complete criterion has not been observed, even when a
component check passed. Automated fixtures, captured host payloads and live
session workflows are separate evidence classes.

## Plan success criteria

Every row from the plan's Verifiable Success Criteria appears below.

| Criterion | Status | Evidence and remaining check |
| --- | --- | --- |
| Build and static checks | UNVERIFIED | At `ea740d7`, local release build, clippy with `-D warnings` and `cargo fmt --check` each exit 0. Linux, macOS and Windows MSVC hosted CI are not established by this snapshot. |
| Tests | PASS | A fresh `cargo test` at `ea740d7` exits 0: 134 passed, 0 failed, 2 helper entry points ignored in the parent runner. An earlier one-off failed aggregate remains an unresolved observation, described below. |
| Root directives with quoted evidence | UNVERIFIED | All 16 `evidence_rules` tests pass, including late-heartbeat ordering, current-turn quotes, Stop/peer invalidation and one-use composer pairing. Default-home seed refusal, newer dismiss evidence, idempotency and MCP exclusion tests also pass. Actual peer-wrapper captures and operational human appointment are not established. |
| Threat-model limitations documented | PASS | `docs/threat-model.md` covers deliberate same-user forgery, semantic interpretation limits, recent 50 prompt texts and 4 KB cap, all-session raw capture and cleanup, undetected mid-turn peer delivery, agent access to composer recording, heartbeat arrival skew and conservative Stop closure. README links the document. |
| Attenuating delegation | PASS | `delegation_cannot_widen_verbs`, `delegation_cannot_widen_scope` and `open_requires_explicit_grant` pass, along with descendant-scope checks in `core_ops`. |
| Grant lineage | PASS | `dismissed_issuer_revokes_downstream`, `cancelled_team_revokes_all`, `superior_cycle_rejected` and grant-chain lifecycle checks pass. |
| Binding | PASS | `pending_binding_confirms_on_target_hook`, `other_session_is_stale_binding`, `rebind_requires_reassign` and `cli_flag_identity_rejected_for_mutation` pass as fixtures. Real hook confirmation is a separate row below. |
| Scope grammar | PASS | All four `scope_rules` tests pass, including the three named plan tests. Historical mutation checks are below. |
| Sticky cancellation and races | PASS | `cancel_blocks_issue_accept_lease`, `concurrent_cancel_vs_issue_is_serialized` and `multiprocess_writers_serialize` pass. The race test repeats 100 times; four test-binary child processes commit 100 revisions. LIVE-C also verifies cancellation in the actual nawi session. |
| Storage failures | PASS | `lock_released_when_holder_killed`, `stale_tmp_ignored_and_cleaned`, `corrupt_state_refuses_write_reads_prev` and `rename_retry_then_store_write_failed` pass on this Windows host. |
| Binding state | PASS | `quiet_after_no_sighting`, `vacant_when_pager_reports_gone`, `unknown_host_stays_quiet` and `vacant_releases_exclusive_leases` pass. The exact two-hour card boundary was corrected and tested. |
| Titles | PASS | `default_titles_unique_per_team`, `bu_default_name_from_head_title` and `title_change_does_not_change_authority` pass. |
| Leases | PASS | `exclusive_conflict_leased_by`, `ttl_expiry_allows_acquire`, `arbitrate_force_release` and `lease_requires_duty_scope` pass. LIVE-A/B observe a successful reservation, a conflicting reservation refusal and an independent reservation. |
| Position cards | PASS | All 11 `card_render` tests pass, covering the 12-line cap, chain/duty/peer/lease content and JSON fields. The missing bu name found during LIVE-A was addressed in `71b3e73` and its regression test. |
| Hook adapter | UNVERIFIED | All 26 `hook_runtime` tests and the captured-Claude replay test pass. The oversized-input marker fix was independently reproduced successfully. Actual Claude SessionStart/UserPromptSubmit/Stop payloads are available; actual Codex hook execution and the complete peer-wrapper capture set are not established. |
| MCP | PASS | All six `mcp_protocol` tests and the Codex thread-metadata identity checks pass. Live Codex MCP interoperability is not established by these automated checks. |
| Mod | PASS | The commander reports plugin validation exit 0 and plugin tests 5/5 at `3e64c1a`. Validation carries an author-metadata warning and managed-install gating notice. nawi independently re-ran its session-change fixture against that source: 1 pass, 0 fail. Global loading and UI observation remain pending. |
| Instruction sources | UNVERIFIED | `docs/protocol.md` records the 12 opened URLs, titles, dates, supporting claims and evidence types. `rules/gaebu.md` exists. Deployment into the active instruction chain is not established. |
| Public repository | UNVERIFIED | Public remote visibility, MIT metadata and hosted CI have not been verified by this snapshot's author. |
| Global deployment | UNVERIFIED | Automatic approval review rejected the commander's Claude hook/permission/mod settings changes as Self-Modification. The rejection was not bypassed. Codex deployment-source changes are local and the hook is not deployed. Application instructions are in the canonical PM store at `.agents/verification/deploy/claude-settings-change.md`. |
| Real pending to bound | UNVERIFIED | The live fixture sessions were prebound with `dev seed`. This does not establish a pending designation confirmed by a subsequently deployed Codex hook. |
| Real-host hook proof | UNVERIFIED | Claude sub-check PASS: the actual isolated host output quotes its seeded proposed duty and `docs/**` scope. The author read the log. Codex hook quoting is unverified because the hook is not deployed. |
| Live session organization | PASS | LIVE-A/B/C run through actual commander gapu and subordinate sessions nawi/bepo against the isolated `gaebu-live` fixture store. They verify duty acceptance, lease conflict, out-of-scope refusal and cancellation; Pager IDs and raw reports are below. This is not an operational human appointment. |
| Operational root appointment | UNVERIFIED | Requires a real human natural-language appointment followed by a quoted root operation. The human is away; seeded fixture teams do not establish it. |
| Final acceptance | UNVERIFIED | Re-review is in progress. Hosted CI, global deployment, actual pending confirmation, Codex hook proof and human appointment remain open. Step 11 must not be represented as complete on this evidence. |

## Local command evidence

The fresh commands below ran at `ea740d7` in the verifier checkout. Raw logs are
ignored local artifacts under that checkout's `.agents/verification/`.

```text
cargo test                                      exit 0
  134 passed; 0 failed; 2 ignored helper entry points
cargo clippy --all-targets --message-format short -- -D warnings
                                                exit 0
cargo fmt --check                               exit 0
cargo build --release                           exit 0
```

Logs: `cargo-test-ea740d7.log`, `clippy-ea740d7.log`, `fmt-ea740d7.log`,
`build-release-ea740d7.log`. The ignored `lock_child` and `writer_child` tests
are subprocess entry points invoked with `--ignored` by passing parent tests.
They are not two untested failure scenarios.

The commander reported one earlier post-merge aggregate with `failed=1`, followed
by three reruns without recurrence. The fresh verifier run above also passes.
Unverified hypothesis: test flakiness. The identity and cause of that earlier
failure have not been established from the evidence available to this author;
the observation remains open rather than being called fixed.

## Live session organization

The isolated store is `gaebu-live` under the local temp directory. It is a
fixture team, not the operational store. nawi's `pager whoami` confirmed its
native session identity before binding (Pager report `#2783`).

The author read `gaebu-live/live-reports.txt`, directly executed LIVE-A/C and
checked the listed notifications through Pager's public `export` interface.

| Stage | Observed result | Report evidence |
| --- | --- | --- |
| LIVE-A | `me` exit 0; nawi's chain and duty `d-21` shown. `duty accept d-21` exits 0. Exclusive `docs/live.md` acquisition exits 0. `src/cli.rs` acquisition exits 1 with `out_of_scope`. JSON card shows `bu: 좌장군부`, accepted duty and exclusive lease. | Pager report `#2801`; raw reports file. |
| LIVE-B | bepo's card shows its commander and nawi's reservation. Duty `d-24` acceptance exits 0. Acquiring nawi's `docs/live.md` exits 1 with `leased_by`; acquiring `docs/other.md` exits 0. | Pager reports `#2799`, `#2800`; raw reports file. |
| LIVE-C | Commander cancels 좌장군부. nawi's card shows the cancelled bu and duty. New lease and duty issuance each exit 1 with `cancelled`. Full board output has zero matches for `d-21` and `docs/live.md`, while bepo's independent duty/reservation remains. | Pager report `#2804`; raw reports file. |

The board absence check covers its complete output, not one selected member:

```powershell
$env:GAEBU_HOME="$env:TEMP\gaebu-live"
$board=& "$HOME\.local\bin\gaebu.exe" board
$boardExit=$LASTEXITCODE
"board_exit=$boardExit"
"d21_matches=$(@($board | Select-String 'd-21').Count)"
"lease_key_matches=$(@($board | Select-String 'docs/live\.md').Count)"
```

```text
board_exit=0
d21_matches=0
lease_key_matches=0
```

LIVE-A initially exposed a missing bu name in the text-card header, despite the
correct JSON field. The commander added the bu to the header in `71b3e73`; the
current card regression suite passes. This does not establish a refreshed global
installation of that header change.

Verified Pager delivery records:

| ID | Recipient | Event |
| --- | --- | --- |
| `#2788` | nawi | grant `g-16` delegated, record `r-17` |
| `#2789` | nawi | bu `b-15` opened, record `r-19` |
| `#2790` | nawi | duty `d-21` proposed, record `r-22` |
| `#2791` | bepo | duty `d-24` proposed, record `r-25` |
| `#2797` | gapu | nawi accepted `d-21`, record `r-27` |
| `#2798` | gapu | bepo accepted `d-24`, record `r-31` |
| `#2802` | nawi | bu `b-15` cancelled, record `r-35` |

## Real-host hook evidence

The commander ran an actual isolated Claude host with a prebound seeded member
and duty. The author read `gaebu-demo/logs/host-hook-claude.log`: the model quotes
the proposed duty's outcome and `docs/**` scope. This proves the isolated Claude
card-injection path, not global deployment or a human-origin root appointment.

The redacted `tests/fixtures/hooks/captured-claude-*.json` files preserve actual
SessionStart, UserPromptSubmit and Stop payloads captured from Claude Code
2.1.302. `captured_claude_payloads_replay_cleanly` in `tests/hook_captured.rs`
passes in the fresh suite. The UserPromptSubmit payload includes `prompt_id`;
the Stop capture includes the same field. This is a host observation, not a
promise that every version or transport supplies it. Pager/peer-wrapper fixtures
remain synthetic, and actual Codex host payloads remain unverified.

## Mutation checks and reproduced fixes

Tool: official prebuilt `cargo-mutants 27.1.0`, SHA-256 checked against the
official release asset metadata. Scratch builds explicitly selected the GNU
toolchain. Counts below apply to their named checkpoints and changed scope.

| Scope | Result | Local evidence |
| --- | --- | --- |
| `src/scope.rs` | PASS: 25 mutants, 24 caught, 1 unviable, exit 0 | `scope-mutants/mutants.out/outcomes.json` |
| Evidence before heartbeat-skew amendment | PASS: 43 mutants, 41 caught, 2 unviable | Evidence entries in `final-mutants/mutants.out/outcomes.json`; the combined run's title timeout was resolved separately. |
| `src/titles.rs` | PASS: 8 mutants, 8 caught, exit 0 | `titles-mutants-r3/mutants.out/outcomes.json` |
| `78a62a6` heartbeat change, scoped diff | PASS: 7 mutants, 7 caught, exit 0 | `heartbeat-mutants/mutants.out/outcomes.json` |

Boundary tests cover six Unicode scalars, current-time equality, Stop equality,
repeated identical prompts and one-use composer pairing. The heartbeat amendment
treats records in the inclusive prompt-relative interval from one hour before
through ten seconds after as active; a late already-recorded heartbeat blocks
hook-only fallback when a root operation checks evidence. Its regression test is
`mod_heartbeat_just_after_prompt_counts_active`.

Two independent failing reproductions were re-run successfully after fixes:

- **Oversized hook input:** initially a valid 1 MiB prompt was rejected without
  invalidating the preceding prompt. The fixed binary still exits 0 and reports
  `hook input too large`, but creates one global missed-event marker. Independent
  rerun at `b81d184` passed; fixture directory
  `gaebu-hook-oversize-fixed-ba09da818f3c480295b1149f460fc325` under temp.
- **Mod session change:** initially a composer prompt still targeted
  `old-session` after the engine changed to `new-session`. The same independent
  fixture against `3e64c1a` passed: 1 pass, 0 fail, with the evidence command
  targeting `new-session`. Fixture directory
  `gaebu-mod-session-8bccccf7e39b4e73b20d5d194994f91c` under temp.

## Research provenance

`docs/protocol.md`, section **Sources and verification**, is the canonical
inventory of 12 opened URLs, full titles, dates, supporting claims and evidence
categories. The author reopened all 12 on 2026-11-04. SharedPlans supports shared
commitment and partial plans, not the brief's field schema. ParallACT is the
benchmark within *Decentralized Multi-Agent Coordination and Communication in
Shared Codebases*. ADP 6-0 is not cited. Continuing unblocked work during
escalation is a practitioner convention. Preprints and vendor reports are not
peer-reviewed performance guarantees.

## Review and pending acceptance

The commander reports a completed fresh Codex review with verdict FIX-FIRST and
four findings, and a fresh Claude review marked COMPLETE with 16 findings.
Fixes include `dc85861`, `082308b`, `cc8e8e2`, `88b96bb`, `78a62a6`, `ad36cf1`,
`9604277`, the card corrections and later merged follow-ups. Re-review is still
in progress at this checkpoint; the commander will add its result and evidence.
The initial findings counts are not a final approval or proof that every finding
is resolved.

A separate earlier Codex review launched by nawi ended with `Selected model is
at capacity` before returning a final report. That invocation remains INCOMPLETE;
its partial output did not pass provenance/return validation. The independent
reproductions above establish their specific fixes and do not retroactively turn
the aborted review into a completed one.

Pending acceptance evidence: completed re-review, the earlier one-off failure's
resolution or explicit disposition, hosted CI, global hook/MCP/rule/mod loading,
live mod heartbeat and status observation, actual pending-to-bound confirmation,
actual Codex hook quoting, remaining real wrapper captures and operational human
appointment.

## Deployment and document handoff

The automatic approval review rejected Claude-side global settings changes as
Self-Modification. The commander preserved the rejection and supplied reviewable
application instructions in the canonical PM store's
`.agents/verification/deploy/claude-settings-change.md`. The intended settings,
permissions, mod, MCP and shared-rule registration still need application and
bootstrap verification. Codex source changes are local; its hook is not deployed
at the reported checkpoint. These are explicit UNVERIFIED items.

The earlier Codex staged-diff scanner warned that the ordinary algorithm name
`SHA-256` matched its ticket-like identifier pattern. The author preserved the
term and handed the documents to the commander as a patch; the commander
integrated that reviewed patch in `325bc67`. This update follows the same patch
handoff without a Codex commit. The scanner warning and the Claude automatic
approval rejection are separate events.
