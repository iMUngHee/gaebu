# Verification snapshot

This is an intermediate verification record for commit `44c9973`, checked on
2026-11-04 on Windows with `stable-x86_64-pc-windows-gnu`. It is not a release
completion claim. Later integration, deployment and runtime evidence must update
this record before final acceptance.

PASS means the stated check was observed. FAIL means an observed acceptance gap.
UNVERIFIED means that the full criterion has not been observed, even when a
narrower component check passed. Synthetic fixtures, local process tests and live
host workflows are separate evidence classes.

## Plan success criteria

Every row from the plan's Verifiable Success Criteria appears below.

| Criterion | Status | Observed evidence and remaining check |
| --- | --- | --- |
| Build and static checks | UNVERIFIED | Local release build exits 0 at `5a00f31`; clippy with `-D warnings` and `cargo fmt --check` exit 0 at `44c9973`. Linux, macOS and Windows MSVC CI have not been observed in this snapshot. |
| Tests | PASS | `cargo test` exits 0 at `44c9973`: 108 passed, 0 failed, 2 helper tests ignored in the parent runner. `dev_seed_refused_on_default_home` was added in this commit and passes. |
| Root directives with quoted evidence | UNVERIFIED | All 15 `evidence_rules` tests pass, plus `dismiss_requires_newer_evidence`, `same_prompt_multi_target_idempotent`, `root_not_exposed_over_mcp`, `dev_seed_refused_on_default_home` and the CLI evidence tests. Wrapper fixtures are synthetic, not captured host payloads. The oversized-hook repro below also found a pending evidence-invalidation fix. |
| Threat-model limitations documented | PASS | `docs/threat-model.md` covers same-user forgery, semantic interpretation limits, 50 retained prompt texts, 4 KB cap, all-session raw capture and cleanup, undetected mid-turn peer delivery, composer command access, heartbeat expiry and conservative Stop closure. README links the document. |
| Attenuating delegation | PASS | `delegation_cannot_widen_verbs`, `delegation_cannot_widen_scope`, `open_requires_explicit_grant` pass in `core_ops`. |
| Grant lineage | PASS | `dismissed_issuer_revokes_downstream`, `cancelled_team_revokes_all`, `superior_cycle_rejected` pass. |
| Binding | PASS | `pending_binding_confirms_on_target_hook`, `other_session_is_stale_binding`, `rebind_requires_reassign`, `cli_flag_identity_rejected_for_mutation` pass as automated fixtures. Live confirmation is a separate row below. |
| Scope grammar | PASS | All four `scope_rules` tests pass, including the three named plan tests. Mutation results are below. |
| Sticky cancellation and races | PASS | `cancel_blocks_issue_accept_lease`, `concurrent_cancel_vs_issue_is_serialized` and `multiprocess_writers_serialize` pass. The race test repeats 100 times; the writer test starts four test-binary child processes and checks 100 committed revisions. |
| Storage failures | PASS | `lock_released_when_holder_killed`, `stale_tmp_ignored_and_cleaned`, `corrupt_state_refuses_write_reads_prev`, `rename_retry_then_store_write_failed` pass on this Windows host. |
| Binding state | PASS | `quiet_after_no_sighting`, `vacant_when_pager_reports_gone`, `unknown_host_stays_quiet`, `vacant_releases_exclusive_leases` pass. |
| Titles | PASS | `default_titles_unique_per_team`, `bu_default_name_from_head_title`, `title_change_does_not_change_authority` pass. |
| Leases | PASS | `exclusive_conflict_leased_by`, `ttl_expiry_allows_acquire`, `arbitrate_force_release`, `lease_requires_duty_scope` pass. |
| Position cards | PASS | Seven `card_render` tests pass, including the 12-line cap, chain/duty/peer/lease content and JSON contract fields. |
| Hook adapter | FAIL | All 19 `hook_runtime` tests pass, including fail-open, open-stdin deadline, missed-event markers, Codex full cards and Claude hash-dependent cards. A separate oversized-input repro preserves prior evidence without a marker; the fix needs revalidation. Real captured host fixture replay is also unverified. |
| MCP | PASS | Three `mcp_protocol` tests and `mcp_codex_requires_thread_meta` pass against the implemented protocol and identity fixtures. This does not establish live Codex MCP interoperability. |
| Mod | PASS | `claude plugin validate ./mod` exits 0 (author-metadata warning, managed-install gating notice). `claude plugin test ./mod`: 5 pass, 0 fail at `3e64c1a`, including `evidence follows the current session id after /clear`. nawi re-ran its independent session-change fixture against `3e64c1a`: 1 pass, 0 fail. Live UI observation is part of global deployment below. |
| Instruction sources | UNVERIFIED | `docs/protocol.md` Sources and verification inventory records the 12 opened URLs, titles, dates, supporting claims and evidence types. `rules/gaebu.md` exists. Deployment into the instruction chain has not been observed here. |
| Public repository | UNVERIFIED | Public visibility and MIT remote metadata have not been checked in this snapshot. |
| Global deployment | UNVERIFIED | The commander's automatic approval review rejected Claude settings/hook/permission/mod edits as Self-Modification; this was not bypassed and awaits human confirmation. Codex deployment-source changes were committed locally on the deployment branch but were not bootstrapped at the reported checkpoint. Command resolution and runtime observation still require evidence. |
| Real pending to bound | UNVERIFIED | No live existing session confirmation has been observed by this verifier. Synthetic confirmation tests do not meet this criterion. |
| Real-host hook proof | UNVERIFIED | Claude sub-check PASS: the isolated `claude -p --session-id` output in local `gaebu-demo/logs/host-hook-claude.log` quotes the seeded proposed duty and `docs/**` scope. This verifier read the output. Codex quoting is not established here, so the combined criterion remains unverified. |
| Live session organization | UNVERIFIED | The nawi/nuzu/bepo fixture-team accept, lease conflict, cancellation and Pager message-id record have not been observed here. |
| Operational root appointment | UNVERIFIED | Requires a real human natural-language appointment followed by the commander's quoted operation; the human is away. Fixture appointments do not establish this. |
| Final acceptance | UNVERIFIED | This is an intermediate snapshot, with an explicit failing acceptance item and pending runtime/deployment evidence. Step 11 must remain open. |

## Local command evidence

Raw logs are local, ignored working artifacts under the verifier checkout's
`.agents/verification/`; they are not published project files.

```text
snapshot: 44c9973 (release-build checkpoint: 5a00f31)
cargo build --release                           exit 0 at 5a00f31
cargo clippy --all-targets --message-format short -- -D warnings
                                                exit 0
cargo fmt --check                               exit 0
cargo test                                      exit 0
  108 passed; 0 failed; 2 ignored helper entry points
claude plugin validate ./mod                    exit 0, warnings noted above
claude plugin test ./mod                        4 pass; 0 fail
```

Log files: `build-release-current.log`, `clippy-44c9973.log`, `fmt-44c9973.log`,
`cargo-test-44c9973.log`. The ignored `lock_child` and
`writer_child` tests are subprocess entry points invoked by the passing parent
tests with `--ignored`; they are not two untested failure scenarios.

The planned default-home test was absent at the earlier checkpoint; it is now
present and executed in the `44c9973` full-suite log:

```powershell
Select-String -Path .agents/verification/cargo-test-44c9973.log -Pattern '^test dev_seed_refused_on_default_home '
```

```text
test dev_seed_refused_on_default_home ... ok
```

## Mutation checks

Tool: official prebuilt `cargo-mutants 27.1.0`, SHA-256 verified against its
official release asset metadata. Mutation subprocesses used the GNU toolchain
explicitly because their scratch checkouts do not inherit directory overrides.

| Scope | Result | Local evidence |
| --- | --- | --- |
| `src/scope.rs` | PASS: 25 mutants, 24 caught, 1 unviable, exit 0 | `scope-mutants/mutants.out/outcomes.json` |
| Final `src/evidence.rs` | PASS: 43 mutants, 41 caught, 2 unviable | `final-mutants/mutants.out/outcomes.json`, evidence scenarios only; combined run's timeout belonged to titles and was resolved separately |
| Final `src/titles.rs` | PASS: 8 mutants, 8 caught, exit 0 | `titles-mutants-r3/mutants.out/outcomes.json` |

An earlier evidence run missed boundary cases. Tests were strengthened for six
Unicode scalars, current-time equality, previous-Stop equality and repeated
identical prompts. Pairing was simplified to the currently open turn, preserving
the approved Stop-bounded contract. An earlier title mutation entered an endless
suffix search; the title test now fails on lack of termination within five
seconds. Production title selection was retained.

## Research provenance

Use `docs/protocol.md`, section **Sources and verification**, as the canonical
inventory of opened URLs, full titles, publication or revision dates, supporting
claims and evidence categories. All 12 entries there were reopened on 2026-11-04.
SharedPlans supports shared commitment and partial plans, not the brief's field
schema. ParallACT is a benchmark within *Decentralized Multi-Agent Coordination
and Communication in Shared Codebases*. ADP 6-0 is not cited. Continuing unblocked
work during escalation is labeled a practitioner convention. Preprints and
vendor reports are not treated as peer-reviewed performance guarantees.

## Review and pending evidence

Review status: INCOMPLETE. A separate read-only Codex reviewer examined the
assigned adapter, CLI, card and mod commits but ended with `Selected model is at
capacity` (process exit 1) before producing a final response. Its partial
observations are reference-only, not validated findings.

```text
Reviewer-return validation: FAIL
- final unique-commit inventory: missing
- final Context Preflight: missing
- complete source/seed provenance coverage: not verifiable
- final findings with required provenance tags: missing
- Review status COMPLETE: missing
```

Two partial leads were subsequently reproduced independently, using fixtures
without changing production source:

- **Oversized hook input:** a normal prompt followed by a valid JSON prompt of
  1 MiB makes both hook invocations exit 0. The second reports `hook input too
  large`, while the session retains one event and has zero missed-event markers.
  The rejection at `src/hook.rs:79` occurs before session parsing. Local fixture:
  `gaebu-hook-oversize-6c3650d15fdd4c51b093b801f6810dc5` under the temp directory.
  The commander assigned a per-session/global marker fix; it is not revalidated
  in this snapshot.
- **Mod session change:** a temporary copy of the plugin is started with session
  id `old-session`, then the engine mock reports `new-session` before a composer
  prompt. The captured evidence command still targets `old-session`.
  `claude plugin test` on that copy returns 1: expected `new-session`, received
  `old-session`. Local fixture: `gaebu-mod-session-8bccccf7e39b4e73b20d5d194994f91c`.

A separate report from the core author identified the card's exact two-hour quiet
boundary for its owner to check. Review and resolution evidence must be added
before final acceptance. The aborted delegated review remains INCOMPLETE; these
reproductions do not substitute for a complete provenance-validated review.

Earlier partial-checkout failures at `8b8722e` were due to unmerged `ops` stubs;
the full test run at `5a00f31` supersedes those failures.

Still pending: the two reproduced boundary fixes, real wrapper capture/replay,
fresh review resolution, published CI, global bootstrap and live mod observation,
actual pending binding, real-host hook quoting, cross-session fixture workflow,
and human operational appointment.

## Commit-scan handoff

The Codex staged-diff scanner reported `WARN SensitiveInfo` because the ordinary
algorithm name `SHA-256` in this document matched its ticket-like identifier
pattern (zero failures, one warning). The author preserved the text and did not
commit through the warning or remove it to bypass the check. These two document
changes are handed to the commander as a patch for review and integration; the
scanner warning is a separate issue from the Claude deployment approval denial.
