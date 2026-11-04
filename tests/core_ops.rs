mod common;

use gaebu::{
    Reason, authz,
    model::*,
    ops::{self, DutySpec, SeedSpec},
};

fn setup() -> (State, Actor, Actor, Actor) {
    let mut st = State::default();
    ops::dev_seed(
        &mut st,
        10,
        &SeedSpec {
            team: "test".into(),
            project: "project".into(),
            commander_alias: "root".into(),
            commander_session: Some("root-session".into()),
            commander_tool: Some(Tool::Codex),
            members: vec![
                (
                    "child".into(),
                    Some("child-session".into()),
                    Some(Tool::Codex),
                ),
                (
                    "other".into(),
                    Some("other-session".into()),
                    Some(Tool::Codex),
                ),
            ],
        },
    )
    .unwrap();
    let actor = |session: &str| Actor {
        member: authz::member_for_session(&st, session).unwrap().id.clone(),
        session_id: session.into(),
    };
    let a = actor("root-session");
    let b = actor("child-session");
    let c = actor("other-session");
    (st, a, b, c)
}
fn scope() -> Vec<String> {
    vec!["src/**".into()]
}
fn spec() -> DutySpec {
    DutySpec {
        intent: "why".into(),
        outcome: "deliver".into(),
        scope: scope(),
        done_criteria: "tests pass".into(),
        ..Default::default()
    }
}
fn accept(st: &mut State, a: &Actor, b: &Actor) -> String {
    let id = ops::duty_issue(st, 20, a, &b.member, spec()).unwrap().ids[0].clone();
    ops::duty_respond(st, 21, b, &id, Some(1), true, None).unwrap();
    id
}
fn ev(seq: u64) -> Evidence {
    Evidence {
        session_id: "user".into(),
        prompt_seq: seq,
        sha256: format!("hash{seq}"),
        attest: Attest::Fixture,
        quote: "user instruction".into(),
        at: seq,
        strength: Default::default(),
    }
}

#[test]
fn delegation_cannot_widen_verbs() {
    let (mut s, a, b, c) = setup();
    ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope()).unwrap();
    s.members.get_mut(&c.member).unwrap().superior = Some(b.member.clone());
    assert_eq!(
        ops::grant(&mut s, 13, &b, &c.member, &[Verb::Open], &scope())
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
}
fn s_bu(a: &Actor, s: &State) -> String {
    s.members[&a.member].bu.clone()
}
#[test]
fn delegation_cannot_widen_scope() {
    let (mut s, a, b, _) = setup();
    ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope()).unwrap();
    assert_eq!(
        authz::require(&s, &b.member, Verb::Assign, &["docs/**".into()])
            .unwrap_err()
            .reason,
        Reason::OutOfScope
    );
}
#[test]
fn open_requires_explicit_grant() {
    let (mut s, a, b, c) = setup();
    ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope()).unwrap();
    assert_eq!(
        ops::bu_open(&mut s, 12, &b, &c.member, None, &[], &scope())
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
}
#[test]
fn dismissed_issuer_revokes_downstream() {
    let (mut s, a, b, c) = setup();
    ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope()).unwrap();
    s.members.get_mut(&c.member).unwrap().superior = Some(b.member.clone());
    ops::grant(&mut s, 12, &b, &c.member, &[Verb::Assign], &scope()).unwrap();
    assert!(authz::require(&s, &c.member, Verb::Assign, &scope()).is_ok());
    s.members.get_mut(&b.member).unwrap().status = MemberStatus::Dismissed;
    assert!(authz::effective_grants(&s, &c.member).is_empty());
}
#[test]
fn cancelled_team_revokes_all() {
    let (mut s, a, _, _) = setup();
    let t = s.members[&a.member].team.clone();
    ops::root_cancel_team(&mut s, 30, &ev(30), &t).unwrap();
    assert!(authz::effective_grants(&s, &a.member).is_empty());
    assert_eq!(
        authz::check_actor(&s, &a).unwrap_err().reason,
        Reason::Cancelled
    );
}
#[test]
fn superior_cycle_rejected() {
    let (mut s, a, b, _) = setup();
    let bu = ops::bu_open(&mut s, 11, &a, &b.member, None, &[Verb::Reassign], &scope())
        .unwrap()
        .ids[0]
        .clone();
    assert_eq!(
        ops::member_move(&mut s, 12, &a, &a.member, &bu)
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
}
#[test]
fn pending_binding_confirms_on_target_hook() {
    let mut s = State::default();
    ops::root_appoint(&mut s, 1, &ev(1), "alias", None, "project", None).unwrap();
    assert_eq!(
        ops::confirm_binding(&mut s, 2, "target", Tool::Codex, "wrong").unwrap(),
        None
    );
    let id = ops::confirm_binding(&mut s, 3, "target", Tool::Codex, "alias")
        .unwrap()
        .unwrap();
    assert!(
        authz::check_actor(
            &s,
            &Actor {
                member: id,
                session_id: "target".into()
            }
        )
        .is_ok()
    );
}
#[test]
fn other_session_is_stale_binding() {
    let (s, mut a, _, _) = setup();
    a.session_id = "stranger".into();
    assert_eq!(
        authz::check_actor(&s, &a).unwrap_err().reason,
        Reason::StaleBinding
    );
}
#[test]
fn rebind_requires_reassign() {
    let (mut s, _, b, c) = setup();
    assert_eq!(
        ops::member_rebind(&mut s, 12, &b, &c.member, "new")
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
}
#[test]
fn cancel_blocks_issue_accept_lease() {
    let (mut s, a, b, _) = setup();
    ops::bu_open(&mut s, 11, &a, &b.member, None, &[], &scope()).unwrap();
    let d = ops::duty_issue(&mut s, 20, &a, &b.member, spec())
        .unwrap()
        .ids[0]
        .clone();
    let bu = s_bu(&b, &s);
    ops::bu_cancel(&mut s, 21, &a, &bu, "stop").unwrap();
    assert_eq!(
        ops::duty_issue(&mut s, 22, &a, &b.member, spec())
            .unwrap_err()
            .reason,
        Reason::Cancelled
    );
    assert_eq!(
        ops::duty_respond(&mut s, 22, &b, &d, None, true, None)
            .unwrap_err()
            .reason,
        Reason::Cancelled
    );
    assert_eq!(
        ops::lease_acquire(&mut s, 22, &b, "src/a", LeaseMode::Exclusive, None, "work")
            .unwrap_err()
            .reason,
        Reason::Cancelled
    );
}
#[test]
fn vacant_releases_exclusive_leases() {
    let (mut s, a, b, _) = setup();
    accept(&mut s, &a, &b);
    ops::lease_acquire(&mut s, 22, &b, "src/a", LeaseMode::Exclusive, None, "work").unwrap();
    ops::mark_vacant(&mut s, 23, &b.member, "pager_gone").unwrap();
    assert!(s.leases.is_empty());
    assert_eq!(
        authz::check_actor(&s, &b).unwrap_err().reason,
        Reason::StaleBinding
    );
}
#[test]
fn exclusive_conflict_leased_by() {
    let (mut s, a, b, c) = setup();
    accept(&mut s, &a, &b);
    accept(&mut s, &a, &c);
    ops::lease_acquire(&mut s, 22, &b, "src/**", LeaseMode::Exclusive, None, "work").unwrap();
    assert_eq!(
        ops::lease_acquire(&mut s, 23, &c, "src/a", LeaseMode::Shared, None, "work")
            .unwrap_err()
            .reason,
        Reason::LeasedBy
    );
    assert!(
        ops::lease_acquire(&mut s, 23, &c, "src2/a", LeaseMode::Exclusive, None, "work").is_err()
    );
}
#[test]
fn ttl_expiry_allows_acquire() {
    let (mut s, a, b, c) = setup();
    accept(&mut s, &a, &b);
    accept(&mut s, &a, &c);
    ops::lease_acquire(
        &mut s,
        22,
        &b,
        "src/a",
        LeaseMode::Exclusive,
        Some(10),
        "work",
    )
    .unwrap();
    assert!(
        ops::lease_acquire(&mut s, 32, &c, "src/a", LeaseMode::Exclusive, None, "work").is_ok()
    );
}
#[test]
fn arbitrate_force_release() {
    let (mut s, a, b, c) = setup();
    accept(&mut s, &a, &b);
    let id = ops::lease_acquire(&mut s, 22, &b, "src/a", LeaseMode::Exclusive, None, "work")
        .unwrap()
        .ids[0]
        .clone();
    assert_eq!(
        ops::lease_force_release(&mut s, 23, &c, &id, "dispute")
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
    ops::lease_force_release(&mut s, 24, &a, &id, "dispute").unwrap();
    assert!(s.leases.is_empty());
}
#[test]
fn lease_requires_duty_scope() {
    let (mut s, _, b, _) = setup();
    assert_eq!(
        ops::lease_acquire(&mut s, 22, &b, "src/a", LeaseMode::Exclusive, None, "work")
            .unwrap_err()
            .reason,
        Reason::OutOfScope
    );
}
#[test]
fn same_prompt_multi_target_idempotent() {
    let mut s = State::default();
    let e = ev(1);
    ops::root_appoint(&mut s, 1, &e, "root", None, "p", None).unwrap();
    ops::root_assign(&mut s, 2, &e, &["a".into(), "b".into()], "root").unwrap();
    let n = s.next_id;
    ops::root_assign(&mut s, 2, &e, &["a".into(), "b".into()], "root").unwrap();
    assert_eq!(s.members.len(), 3);
    assert_eq!(s.next_id, n);
}
#[test]
fn explicit_reappoint_confirms_an_inferred_seat() {
    let explicit = |seq: u64| Evidence {
        strength: Strength::Explicit,
        quote: "너 가절월 맞아".into(),
        ..ev(seq)
    };
    let mut s = State::default();
    let first = ops::root_appoint(&mut s, 1, &ev(1), "root", Some("t"), "p", None).unwrap();
    let mid = first.ids[0].clone();
    assert_eq!(gaebu::card::root_title(&s, &mid), "가절월(추론)");

    // Inferred words again: still the old refusal, and no downgrade path.
    assert_eq!(
        ops::root_appoint(&mut s, 2, &ev(2), "root", None, "p", None)
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
    // Another team's name is a different appointment.
    assert_eq!(
        ops::root_appoint(&mut s, 3, &explicit(3), "root", Some("other"), "p", None)
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
    // Evidence not newer than the appointment's cannot confirm it.
    let mut stale = explicit(4);
    stale.at = 1;
    assert_eq!(
        ops::root_appoint(&mut s, 4, &stale, "root", None, "p", None)
            .unwrap_err()
            .reason,
        Reason::NoUserEvidence
    );

    let (members, grants, records) = (s.members.len(), s.grants.len(), s.records.len());
    let done = ops::root_appoint(&mut s, 5, &explicit(5), "root", None, "p", None).unwrap();
    assert_eq!(done.summary, "appointment confirmed explicitly");
    assert_eq!(done.ids, first.ids);
    assert_eq!((s.members.len(), s.grants.len()), (members, grants));
    assert_eq!(gaebu::card::root_title(&s, &mid), "가절월");
    let note = &s.records[records..];
    assert_eq!(note.len(), 1);
    assert!(
        note[0].text.contains("was inferred: user instruction"),
        "{}",
        note[0].text
    );

    // The same prompt again is idempotent; a later explicit one is a no-op.
    let retry = ops::root_appoint(&mut s, 6, &explicit(5), "root", None, "p", None).unwrap();
    assert_eq!(retry.summary, "already appointed");
    let again = ops::root_appoint(&mut s, 7, &explicit(7), "root", None, "p", None).unwrap();
    assert_eq!(again.summary, "already appointed explicitly");
    assert_eq!(s.records.len(), records + 1);
}
#[test]
fn dismiss_requires_newer_evidence() {
    let mut s = State::default();
    ops::root_appoint(&mut s, 1, &ev(1), "root", None, "p", None).unwrap();
    assert_eq!(
        ops::root_dismiss(&mut s, 2, &ev(1), "root")
            .unwrap_err()
            .reason,
        Reason::NoUserEvidence
    );
    ops::root_dismiss(&mut s, 3, &ev(3), "root").unwrap();
}
#[test]
fn title_change_does_not_change_authority() {
    let (mut s, a, b, _) = setup();
    let grants = s.grants.clone();
    ops::title_set(&mut s, 11, &a, &b.member, "승상").unwrap();
    assert_eq!(s.grants, grants);
    assert!(authz::effective_grants(&s, &b.member).is_empty());
}
#[test]
fn duty_cas_and_escalation_flow() {
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    assert_eq!(
        ops::duty_amend(&mut s, 22, &a, &d, 1, spec())
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
    let e = ops::escalate(&mut s, 23, &b, Some(&d), "decision?")
        .unwrap()
        .ids[0]
        .clone();
    ops::escalation_answer(&mut s, 24, &a, &e, "proceed").unwrap();
    assert_eq!(s.escalations[&e].answer.as_deref(), Some("proceed"));
    ops::duty_progress(&mut s, 25, &b, &d, DutyStatus::Done, Some("tested")).unwrap();
    assert_eq!(s.duties[&d].status, DutyStatus::Done);
    assert!(!s.outbox.is_empty());
}

#[test]
fn concurrent_cancel_vs_issue_is_serialized() {
    use gaebu::store::Store;
    use std::{
        sync::{Arc, Barrier},
        time::Duration,
    };
    let home = std::env::temp_dir().join(format!("gaebu-cancel-race-{}", std::process::id()));
    let _cleanup = common::RemoveOnDrop(home.clone());
    std::fs::create_dir_all(&home).unwrap();
    for n in 0..100 {
        let path = home.join(n.to_string());
        let store = Store::open(&path).unwrap();
        let (mut s, a, b, _) = setup();
        let bu = ops::bu_open(&mut s, 11, &a, &b.member, None, &[], &scope())
            .unwrap()
            .ids[0]
            .clone();
        store
            .transact(Duration::from_secs(1), |state| {
                *state = s;
                Ok(())
            })
            .unwrap();
        let gate = Arc::new(Barrier::new(2));
        std::thread::scope(|threads| {
            let gate2 = gate.clone();
            let path2 = &path;
            let a2 = &a;
            let b2 = &b;
            threads.spawn(move || {
                gate2.wait();
                let r = Store::open(path2)
                    .unwrap()
                    .transact(Duration::from_secs(5), |s| {
                        ops::duty_issue(s, 30, a2, &b2.member, spec())
                    });
                if let Err(e) = r {
                    assert_eq!(e.reason, Reason::Cancelled);
                }
            });
            gate.wait();
            store
                .transact(Duration::from_secs(5), |s| {
                    ops::bu_cancel(s, 30, &a, &bu, "cancel")
                })
                .unwrap();
        });
        let state = store.read().unwrap();
        assert!(
            state
                .duties
                .values()
                .all(|d| d.status == DutyStatus::Cancelled)
        );
    }
}

#[test]
fn revoked_parent_grant_invalidates_child_without_binding_authority() {
    let (mut s, a, b, c) = setup();
    let g = ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope())
        .unwrap()
        .ids[0]
        .clone();
    s.members.get_mut(&c.member).unwrap().superior = Some(b.member.clone());
    ops::grant(&mut s, 12, &b, &c.member, &[Verb::Assign], &scope()).unwrap();
    ops::mark_vacant(&mut s, 13, &b.member, "pager_gone").unwrap();
    assert!(authz::require(&s, &c.member, Verb::Assign, &scope()).is_ok());
    ops::revoke(&mut s, 14, &a, &g).unwrap();
    assert!(authz::effective_grants(&s, &c.member).is_empty());
}

#[test]
fn actor_pending_dismissed_and_missing_refusals() {
    let (mut s, _, b, _) = setup();
    s.members.get_mut(&b.member).unwrap().binding.state = BindingState::Pending;
    assert_eq!(
        authz::check_actor(&s, &b).unwrap_err().reason,
        Reason::Pending
    );
    s.members.get_mut(&b.member).unwrap().status = MemberStatus::Dismissed;
    assert_eq!(
        authz::check_actor(&s, &b).unwrap_err().reason,
        Reason::NotMember
    );
    assert_eq!(
        authz::check_actor(
            &s,
            &Actor {
                member: "absent".into(),
                session_id: "x".into()
            }
        )
        .unwrap_err()
        .reason,
        Reason::NotMember
    );
}

#[test]
fn grant_missing_cross_team_and_cyclic_lineage_fail_closed() {
    let (mut s, a, b, c) = setup();
    let g = ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope())
        .unwrap()
        .ids[0]
        .clone();
    s.grants.get_mut(&g).unwrap().grantor = Some(c.member.clone());
    assert!(authz::effective_grants(&s, &b.member).is_empty());
    s.grants.get_mut(&g).unwrap().grantor = Some(a.member.clone());
    s.members.get_mut(&b.member).unwrap().team = "foreign".into();
    assert!(authz::effective_grants(&s, &b.member).is_empty());
    s.members.get_mut(&b.member).unwrap().team = s.members[&a.member].team.clone();
    s.grants.get_mut(&g).unwrap().grantor = Some(b.member.clone());
    assert!(authz::effective_grants(&s, &b.member).is_empty());
    s.members.get_mut(&b.member).unwrap().superior = Some(c.member.clone());
    s.members.get_mut(&c.member).unwrap().superior = Some(b.member.clone());
    assert!(!authz::is_below(&s, &a.member, &b.member));
}

#[test]
fn cancelled_or_missing_ancestor_bu_is_rejected() {
    let (mut s, a, b, _) = setup();
    let bu = ops::bu_open(&mut s, 11, &a, &b.member, None, &[Verb::Assign], &scope())
        .unwrap()
        .ids[0]
        .clone();
    let root = s.members[&a.member].bu.clone();
    s.bus.get_mut(&root).unwrap().status = BuStatus::Cancelled;
    assert!(authz::bu_cancelled(&s, &bu));
    assert!(authz::effective_grants(&s, &b.member).is_empty());
    assert!(authz::bu_cancelled(&s, "missing"));
    s.bus.get_mut(&root).unwrap().status = BuStatus::Open;
    s.bus.get_mut(&bu).unwrap().parent = Some(bu.clone());
    assert!(authz::bu_cancelled(&s, &bu));
}

#[test]
fn grant_covers_all_patterns_and_resources_separately() {
    let (mut s, a, b, _) = setup();
    let both = vec!["src/**".into(), "res:gpu".into()];
    ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &both).unwrap();
    authz::require(
        &s,
        &b.member,
        Verb::Assign,
        &["src/a".into(), "res:gpu".into()],
    )
    .unwrap();
    assert_eq!(
        authz::require(
            &s,
            &b.member,
            Verb::Assign,
            &["src/a".into(), "res:cpu".into()]
        )
        .unwrap_err()
        .reason,
        Reason::OutOfScope
    );
    assert_eq!(
        authz::require(
            &s,
            &b.member,
            Verb::Assign,
            &["src/a".into(), "../bad".into()]
        )
        .unwrap_err()
        .reason,
        Reason::InvalidScope
    );
}

#[test]
fn lease_shared_overlap_is_allowed_and_rebind_invalidates_old_session() {
    let (mut s, a, b, c) = setup();
    accept(&mut s, &a, &b);
    accept(&mut s, &a, &c);
    ops::lease_acquire(
        &mut s,
        22,
        &b,
        "src/**",
        LeaseMode::Shared,
        Some(1_000),
        "read",
    )
    .unwrap();
    ops::lease_acquire(&mut s, 22, &c, "src/a", LeaseMode::Shared, None, "read").unwrap();
    ops::member_rebind(&mut s, 23, &a, &b.member, "new-alias").unwrap();
    assert!(!s.leases.values().any(|l| l.holder == b.member));
    ops::confirm_binding(&mut s, 24, "new-session", Tool::Codex, "new-alias").unwrap();
    assert_eq!(
        authz::check_actor(&s, &b).unwrap_err().reason,
        Reason::StaleBinding
    );
}

#[test]
fn quiet_after_no_sighting() {
    let (s, a, _, _) = setup();
    let card = gaebu::card::build(&s, &a.member, 7_200_001, Some(0)).unwrap();
    assert_eq!(card.binding, "quiet");
    assert!(authz::check_actor(&s, &a).is_ok());
}

#[test]
fn root_grant_needs_evidence_and_commander_seat() {
    let (mut s, a, b, _) = setup();
    let root = s
        .grants
        .values()
        .find(|g| g.grantee == a.member)
        .unwrap()
        .id
        .clone();
    let evidence = s.grants[&root].evidence.clone();
    s.grants.get_mut(&root).unwrap().evidence = None;
    assert!(authz::effective_grants(&s, &a.member).is_empty());
    s.grants.get_mut(&root).unwrap().evidence = evidence;
    s.grants.get_mut(&root).unwrap().grantee = b.member.clone();
    assert!(authz::effective_grants(&s, &b.member).is_empty());
}

#[test]
fn grant_revalidates_own_status_team_and_cancelled_bu() {
    let (mut s, a, b, _) = setup();
    let id = ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope())
        .unwrap()
        .ids[0]
        .clone();
    s.grants.get_mut(&id).unwrap().status = GrantStatus::Revoked;
    assert!(authz::effective_grants(&s, &b.member).is_empty());
    s.grants.get_mut(&id).unwrap().status = GrantStatus::Active;
    s.members.get_mut(&b.member).unwrap().status = MemberStatus::Dismissed;
    assert!(authz::effective_grants(&s, &b.member).is_empty());
    s.members.get_mut(&b.member).unwrap().status = MemberStatus::Active;
    let oldteam = s.members[&b.member].team.clone();
    s.members.get_mut(&b.member).unwrap().team = "foreign".into();
    assert!(authz::effective_grants(&s, &b.member).is_empty());
    s.members.get_mut(&b.member).unwrap().team = oldteam;
    let bu = ops::bu_open(&mut s, 12, &a, &b.member, None, &[Verb::Assign], &scope())
        .unwrap()
        .ids[0]
        .clone();
    s.bus.get_mut(&bu).unwrap().status = BuStatus::Cancelled;
    assert!(authz::effective_grants(&s, &b.member).is_empty());
}

#[test]
fn persisted_delegation_is_revalidated_against_issuers_own_scope() {
    let (mut s, a, b, c) = setup();
    ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope()).unwrap();
    s.members.get_mut(&c.member).unwrap().superior = Some(b.member.clone());
    let id = ops::grant(&mut s, 12, &b, &c.member, &[Verb::Assign], &scope())
        .unwrap()
        .ids[0]
        .clone();
    s.grants.get_mut(&id).unwrap().scope = vec!["docs/**".into()];
    assert!(authz::effective_grants(&s, &c.member).is_empty());
    s.grants.get_mut(&id).unwrap().scope = scope();
    s.grants.get_mut(&id).unwrap().verbs.insert(Verb::Open);
    assert!(authz::effective_grants(&s, &c.member).is_empty());
}

#[test]
fn empty_delegation_has_no_effective_authority() {
    let (mut s, a, b, _) = setup();
    let id = ops::grant(&mut s, 11, &a, &b.member, &[Verb::Assign], &scope())
        .unwrap()
        .ids[0]
        .clone();
    s.grants.get_mut(&id).unwrap().verbs.clear();
    assert!(authz::effective_grants(&s, &b.member).is_empty());
}

#[test]
fn session_lookup_prefers_current_binding_after_dismissal() {
    let (mut s, a, mut b, _) = setup();
    let mut old = s.members.remove(&b.member).unwrap();
    old.id = "m-0".into();
    b.member = old.id.clone();
    s.members.insert(old.id.clone(), old);
    ops::root_dismiss(&mut s, 30, &ev(30), "child").unwrap();
    let bu = s.members[&a.member].bu.clone();
    let id = ops::member_add(&mut s, 31, &a, &bu, "replacement", None)
        .unwrap()
        .ids[0]
        .clone();
    ops::confirm_binding(&mut s, 32, "child-session", Tool::Codex, "replacement").unwrap();
    assert_eq!(
        authz::member_for_session(&s, "child-session").unwrap().id,
        id
    );
    assert_eq!(
        authz::check_actor(&s, &b).unwrap_err().reason,
        Reason::NotMember
    );
}

#[test]
fn root_grant_does_not_cross_team_membership() {
    let (mut s, a, _, _) = setup();
    s.members.get_mut(&a.member).unwrap().team = "other-team".into();
    assert!(authz::effective_grants(&s, &a.member).is_empty());
}

#[test]
fn lease_cannot_borrow_another_members_accepted_duty() {
    let (mut s, a, b, c) = setup();
    accept(&mut s, &a, &b);
    assert_eq!(
        ops::lease_acquire(&mut s, 22, &c, "src/a", LeaseMode::Exclusive, None, "work")
            .unwrap_err()
            .reason,
        Reason::OutOfScope
    );
    ops::duty_issue(&mut s, 23, &a, &c.member, spec()).unwrap();
    assert_eq!(
        ops::lease_acquire(&mut s, 24, &c, "src/a", LeaseMode::Exclusive, None, "work")
            .unwrap_err()
            .reason,
        Reason::OutOfScope
    );
}

#[test]
fn lease_cannot_reuse_duty_from_cancelled_bu_after_move() {
    let (mut s, a, b, c) = setup();
    let root = s.members[&a.member].bu.clone();
    let child = ops::bu_open(&mut s, 11, &a, &b.member, None, &[], &scope())
        .unwrap()
        .ids[0]
        .clone();
    ops::member_move(&mut s, 12, &a, &c.member, &child).unwrap();
    accept(&mut s, &a, &c);
    ops::member_move(&mut s, 22, &a, &c.member, &root).unwrap();
    ops::bu_cancel(&mut s, 23, &a, &child, "stop").unwrap();
    assert_eq!(
        ops::lease_acquire(&mut s, 24, &c, "src/a", LeaseMode::Exclusive, None, "work")
            .unwrap_err()
            .reason,
        Reason::OutOfScope
    );
}

#[test]
fn bu_cancel_checks_descendant_duty_scopes() {
    let (mut s, a, b, c) = setup();
    let bu = ops::bu_open(
        &mut s,
        11,
        &a,
        &b.member,
        None,
        &[Verb::Open, Verb::Cancel],
        &scope(),
    )
    .unwrap()
    .ids[0]
        .clone();
    ops::member_move(&mut s, 12, &a, &c.member, &bu).unwrap();
    let child = ops::bu_open(&mut s, 13, &b, &c.member, None, &[], &scope())
        .unwrap()
        .ids[0]
        .clone();
    ops::grant(&mut s, 14, &a, &c.member, &[Verb::Open], &scope()).unwrap();
    let grand = ops::member_add(&mut s, 14, &a, &child, "grandchild", None)
        .unwrap()
        .ids[0]
        .clone();
    ops::bu_open(&mut s, 14, &c, &grand, None, &[], &scope()).unwrap();
    let mut wide = spec();
    wide.scope = vec!["docs/**".into()];
    ops::duty_issue(&mut s, 14, &a, &grand, wide).unwrap();
    assert_eq!(
        ops::bu_cancel(&mut s, 15, &b, &child, "stop")
            .unwrap_err()
            .reason,
        Reason::OutOfScope
    );
}

#[test]
fn notice_not_sent_to_actor() {
    let (mut s, a, _, _) = setup();
    s.outbox.clear();
    ops::root_assign(&mut s, 30, &ev(30), &["new-staff".into()], "root").unwrap();
    assert!(
        s.outbox.is_empty(),
        "pending target needs no notice; root actor must not receive one"
    );
    let records = s.records.len();
    ops::confirm_binding(&mut s, 31, "new-staff-session", Tool::Codex, "new-staff")
        .unwrap()
        .unwrap();
    // A confirmed seat is Record-only (plan gaebu-v0-2, Decision 13): the
    // commander sees it on the next card.
    assert!(s.outbox.is_empty());
    assert_eq!(s.records.len(), records + 1);
    // An Act change from the commander still skips the commander's session.
    let d = ops::duty_issue(&mut s, 31, &a, &a.member, spec());
    assert!(d.is_err(), "nobody issues a duty to themself");
    s.outbox.clear();
    let id = ops::root_assign(&mut s, 32, &ev(32), &["same-session".into()], "root")
        .unwrap()
        .ids[0]
        .clone();
    s.members.get_mut(&id).unwrap().binding.session_id = Some(a.session_id.clone());
    ops::title_set(&mut s, 33, &a, &id, "참군").unwrap();
    assert!(
        s.outbox.is_empty(),
        "same actor session must be excluded too"
    );
    ops::member_rebind(&mut s, 34, &a, "child", "new-child").unwrap();
    assert!(
        s.outbox.iter().all(|n| n.to_session != a.session_id),
        "rebind must not notify its actual actor"
    );
}

#[test]
fn notice_body_names_team_and_actor() {
    let (mut s, a, b, _) = setup();
    s.outbox.clear();
    let duty = ops::duty_issue(&mut s, 20, &a, &b.member, spec())
        .unwrap()
        .ids[0]
        .clone();
    assert_eq!(s.outbox.len(), 1);
    let body = &s.outbox[0].body;
    assert!(body.starts_with("[gaebu test] 승상 root:"), "{body}");
    assert!(body.contains(&duty), "{body}");
    assert!(body.contains("deliver"), "{body}");
    assert!(body.contains("gaebu me"), "{body}");
}

#[test]
fn cancel_notices_name_actual_actor_once_per_recipient() {
    let (mut s, a, b, c) = setup();
    let bu = ops::bu_open(&mut s, 11, &a, &b.member, None, &[], &scope())
        .unwrap()
        .ids[0]
        .clone();
    ops::member_move(&mut s, 12, &a, &c.member, &bu).unwrap();
    s.outbox.clear();
    ops::bu_cancel(&mut s, 13, &a, &bu, "pause").unwrap();
    assert_eq!(s.outbox.len(), 2);
    assert!(
        s.outbox
            .iter()
            .all(|n| n.to_member != a.member && n.body.starts_with("[gaebu test] 승상 root:"))
    );
    let recipients: std::collections::BTreeSet<_> =
        s.outbox.iter().map(|n| n.to_member.clone()).collect();
    assert_eq!(recipients.len(), 2);
}

#[test]
fn root_key_distinguishes_new_prompt_same_text() {
    let (mut s, _, _, _) = setup();
    let mut first = ev(30);
    first.sha256 = "identical-text".into();
    ops::root_assign(&mut s, 30, &first, &["target".into()], "root").unwrap();
    ops::root_dismiss(&mut s, 40, &ev(40), "target").unwrap();
    let mut later = ev(50);
    later.sha256 = first.sha256.clone();
    ops::root_assign(&mut s, 50, &later, &["target".into()], "root").unwrap();
    assert_eq!(
        s.members
            .values()
            .filter(|m| m.binding.alias == "target" && m.status == MemberStatus::Active)
            .count(),
        1
    );
}

#[test]
fn same_prompt_retry_idempotent() {
    let mut s = State::default();
    let e = ev(1);
    let first = ops::root_appoint(&mut s, 1, &e, "root", None, "p", None).unwrap();
    let after = s.clone();
    let retry = ops::root_appoint(&mut s, 2, &e, "root", None, "p", None).unwrap();
    assert_eq!(s, after);
    assert_eq!(first.ids, retry.ids);
}

#[test]
fn commander_cannot_cancel_root_bu() {
    let (mut s, a, _, _) = setup();
    let root = s.members[&a.member].bu.clone();
    assert_eq!(
        ops::bu_cancel(&mut s, 20, &a, &root, "stop")
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
    assert_eq!(
        ops::bu_close(&mut s, 20, &a, &root).unwrap_err().reason,
        Reason::NoGrant
    );
    assert_eq!(s.bus[&root].status, BuStatus::Open);
}

#[test]
fn head_cannot_cancel_own_bu() {
    let (mut s, a, b, _) = setup();
    let bu = ops::bu_open(
        &mut s,
        11,
        &a,
        &b.member,
        None,
        &[Verb::Open, Verb::Cancel],
        &scope(),
    )
    .unwrap()
    .ids[0]
        .clone();
    assert_eq!(
        ops::bu_cancel(&mut s, 20, &b, &bu, "stop")
            .unwrap_err()
            .reason,
        Reason::NoGrant
    );
    assert_eq!(
        ops::bu_close(&mut s, 20, &b, &bu).unwrap_err().reason,
        Reason::NoGrant
    );
}

#[test]
fn root_grant_cannot_be_revoked_by_member() {
    let (mut s, a, _, _) = setup();
    let root = s
        .grants
        .values()
        .find(|g| g.grantor.is_none())
        .unwrap()
        .id
        .clone();
    assert_eq!(
        ops::revoke(&mut s, 20, &a, &root).unwrap_err().reason,
        Reason::NoGrant
    );
    assert_eq!(s.grants[&root].status, GrantStatus::Active);
}

#[test]
fn bu_open_cannot_move_an_existing_head() {
    let (mut s, a, b, _) = setup();
    ops::bu_open(&mut s, 11, &a, &b.member, None, &[], &scope()).unwrap();
    assert_eq!(
        ops::bu_open(&mut s, 12, &a, &b.member, None, &[], &scope())
            .unwrap_err()
            .reason,
        Reason::Conflict
    );
}

#[test]
fn cancelled_notice_has_fallback_for_empty_reason() {
    let (mut s, a, b, _) = setup();
    let bu = ops::bu_open(&mut s, 11, &a, &b.member, None, &[], &scope())
        .unwrap()
        .ids[0]
        .clone();
    let d = ops::duty_issue(&mut s, 12, &a, &b.member, spec())
        .unwrap()
        .ids[0]
        .clone();
    s.outbox.clear();
    ops::duty_cancel(&mut s, 13, &a, &d, "").unwrap();
    assert!(s.outbox.iter().any(|n| n.body.contains("사유 없음")));
    s.outbox.clear();
    ops::bu_cancel(&mut s, 14, &a, &bu, "  ").unwrap();
    assert!(s.outbox.iter().any(|n| n.body.contains("사유 없음")));
}

#[test]
fn superior_closes_only_clean_subordinate_bu() {
    let (mut s, a, b, _) = setup();
    let bu = ops::bu_open(&mut s, 11, &a, &b.member, None, &[], &scope())
        .unwrap()
        .ids[0]
        .clone();
    let duty = ops::duty_issue(&mut s, 12, &a, &b.member, spec())
        .unwrap()
        .ids[0]
        .clone();
    assert_eq!(
        ops::bu_close(&mut s, 13, &a, &bu).unwrap_err().reason,
        Reason::Conflict
    );
    ops::duty_respond(&mut s, 14, &b, &duty, Some(1), false, Some("handoff")).unwrap();
    ops::bu_close(&mut s, 15, &a, &bu).unwrap();
    assert_eq!(s.bus[&bu].status, BuStatus::Closed);
}

// ----- human holds (plan gaebu-v0-2, Decisions 2, 5–7) -----

/// Root evidence quoted in `session`'s current prompt.
fn hev(session: &str, seq: u64) -> Evidence {
    Evidence {
        session_id: session.into(),
        ..ev(seq)
    }
}
fn root_bu(s: &State) -> String {
    s.teams.values().next().unwrap().root_bu.clone()
}
fn held(r: Result<ops::Outcome, gaebu::Error>) -> bool {
    r.is_err_and(|e| e.reason == Reason::Held)
}

#[test]
fn hold_guard_matrix() {
    let (mut s, a, b, _) = setup();
    let done = accept(&mut s, &a, &b);
    let blocked = accept(&mut s, &a, &b);
    ops::duty_progress(&mut s, 22, &b, &blocked, DutyStatus::Blocked, Some("stuck")).unwrap();
    let proposed = ops::duty_issue(&mut s, 23, &a, &b.member, spec())
        .unwrap()
        .ids[0]
        .clone();
    let declined = ops::duty_issue(&mut s, 23, &a, &b.member, spec())
        .unwrap()
        .ids[0]
        .clone();
    let lease = ops::lease_acquire(&mut s, 24, &b, "src/a", LeaseMode::Exclusive, None, "w")
        .unwrap()
        .ids[0]
        .clone();
    for (i, d) in [&done, &blocked, &proposed, &declined]
        .into_iter()
        .enumerate()
    {
        ops::root_hold(
            &mut s,
            30,
            &hev("root-session", 100 + i as u64),
            d,
            "대협 확인",
        )
        .unwrap();
    }
    // Refused for the assignee while held.
    assert!(held(ops::duty_respond(
        &mut s, 31, &b, &proposed, None, true, None
    )));
    assert!(held(ops::duty_progress(
        &mut s,
        31,
        &b,
        &done,
        DutyStatus::Done,
        None
    )));
    assert!(held(ops::duty_progress(
        &mut s,
        31,
        &b,
        &done,
        DutyStatus::Blocked,
        Some("x")
    )));
    assert!(held(ops::duty_progress(
        &mut s,
        31,
        &b,
        &done,
        DutyStatus::HandedOff,
        Some("x")
    )));
    assert!(held(ops::duty_progress(
        &mut s,
        31,
        &b,
        &blocked,
        DutyStatus::Accepted,
        None
    )));
    assert!(held(ops::lease_acquire(
        &mut s,
        31,
        &b,
        "src/b",
        LeaseMode::Exclusive,
        None,
        "w"
    )));
    let err = ops::duty_progress(&mut s, 31, &b, &done, DutyStatus::Done, None).unwrap_err();
    assert!(err.message.contains("대협 확인"), "{}", err.message);
    // Allowed while held: letting go and asking.
    ops::duty_respond(&mut s, 32, &b, &declined, None, false, Some("no")).unwrap();
    ops::escalate(&mut s, 32, &b, Some(&done), "may I?").unwrap();
    ops::record_add(&mut s, 32, &b, RecordKind::Note, &[], "noted").unwrap();
    ops::lease_release(&mut s, 32, &b, &lease).unwrap();
    // The superior may still amend and cancel; neither lifts the hold.
    let v = s.duties[&done].version;
    ops::duty_amend(&mut s, 33, &a, &done, v, spec()).unwrap();
    assert!(s.duties[&done].hold.is_some());
    ops::duty_cancel(&mut s, 33, &a, &blocked, "drop").unwrap();
    assert!(s.duties[&blocked].hold.is_some());
    // Released: back to what it was.
    ops::root_release(&mut s, 34, &hev("root-session", 200), &done).unwrap();
    assert!(
        s.records
            .iter()
            .any(|r| r.kind == RecordKind::Decision && r.text.contains("released"))
    );
    assert_eq!(
        s.duties[&done].status,
        DutyStatus::Proposed,
        "the amend asked again"
    );
    ops::duty_respond(&mut s, 35, &b, &done, None, true, None).unwrap();
    ops::duty_progress(&mut s, 35, &b, &done, DutyStatus::Done, None).unwrap();
}

#[test]
fn hold_needs_a_team_session_and_refuses_duplicates() {
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    let e = ops::root_hold(&mut s, 30, &hev("stranger", 1), &d, "later").unwrap_err();
    assert_eq!(e.reason, Reason::NotMember);
    let e = ops::root_hold(&mut s, 30, &hev("child-session", 1), &d, "").unwrap_err();
    assert_eq!(e.reason, Reason::InvalidArgument);
    // The assignee's own session may hold (the human spoke there).
    ops::root_hold(&mut s, 30, &hev("child-session", 2), &d, "later").unwrap();
    let e = ops::root_hold(&mut s, 31, &hev("root-session", 3), &d, "again").unwrap_err();
    assert_eq!(e.reason, Reason::Conflict);
    // Same prompt, same target: idempotent.
    ops::root_hold(&mut s, 31, &hev("child-session", 2), &d, "later").unwrap();
    let e = ops::root_release(&mut s, 32, &hev("root-session", 4), "d-999").unwrap_err();
    assert_eq!(e.reason, Reason::NotFound);
    ops::root_release(&mut s, 32, &hev("root-session", 5), &d).unwrap();
    let e = ops::root_release(&mut s, 33, &hev("root-session", 6), &d).unwrap_err();
    assert_eq!(e.reason, Reason::NotFound);
}

#[test]
fn bu_hold_covers_descendant_duties() {
    let (mut s, a, b, _) = setup();
    ops::bu_open(&mut s, 15, &a, &b.member, None, &[], &[]).unwrap();
    let d = accept(&mut s, &a, &b);
    let rb = root_bu(&s);
    assert_ne!(s.duties[&d].bu, rb, "duty lives in the child bu");
    ops::root_hold(&mut s, 30, &hev("root-session", 1), &rb, "전체 대기").unwrap();
    assert!(held(ops::duty_progress(
        &mut s,
        31,
        &b,
        &d,
        DutyStatus::Done,
        None
    )));
    ops::root_release(&mut s, 32, &hev("root-session", 2), &rb).unwrap();
    ops::duty_progress(&mut s, 33, &b, &d, DutyStatus::Done, None).unwrap();
}

#[test]
fn amend_keeps_hold() {
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    ops::root_hold(&mut s, 30, &hev("root-session", 1), &d, "wait").unwrap();
    let v = s.duties[&d].version;
    ops::duty_amend(&mut s, 31, &a, &d, v, spec()).unwrap();
    assert_eq!(s.duties[&d].hold.as_ref().unwrap().until, "wait");
    assert!(held(ops::duty_respond(
        &mut s, 32, &b, &d, None, true, None
    )));
}

#[test]
fn nested_holds_release_independently() {
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    let rb = root_bu(&s);
    ops::root_hold(&mut s, 30, &hev("root-session", 1), &d, "own").unwrap();
    ops::root_hold(&mut s, 30, &hev("root-session", 2), &rb, "unit").unwrap();
    let e = ops::duty_progress(&mut s, 31, &b, &d, DutyStatus::Done, None).unwrap_err();
    assert!(
        e.message.contains("own") && e.message.contains("(+1)"),
        "{}",
        e.message
    );
    ops::root_release(&mut s, 32, &hev("root-session", 3), &d).unwrap();
    let e = ops::duty_progress(&mut s, 33, &b, &d, DutyStatus::Done, None).unwrap_err();
    assert_eq!(e.reason, Reason::Held);
    assert!(e.message.contains("unit"), "{}", e.message);
    ops::root_release(&mut s, 34, &hev("root-session", 4), &rb).unwrap();
    ops::duty_progress(&mut s, 35, &b, &d, DutyStatus::Done, None).unwrap();
}

#[test]
fn lease_scope_excludes_held_duties() {
    let (mut s, a, b, _) = setup();
    let free = ops::duty_issue(
        &mut s,
        20,
        &a,
        &b.member,
        DutySpec {
            scope: vec!["docs/**".into()],
            ..spec()
        },
    )
    .unwrap()
    .ids[0]
        .clone();
    ops::duty_respond(&mut s, 21, &b, &free, None, true, None).unwrap();
    let held_duty = accept(&mut s, &a, &b);
    let renewable = ops::lease_acquire(&mut s, 22, &b, "src/r", LeaseMode::Exclusive, None, "w")
        .unwrap()
        .ids[0]
        .clone();
    ops::root_hold(&mut s, 30, &hev("root-session", 1), &held_duty, "확인 후").unwrap();
    assert!(held(ops::lease_acquire(
        &mut s,
        31,
        &b,
        "src/x",
        LeaseMode::Exclusive,
        None,
        "w"
    )));
    assert!(ops::lease_acquire(&mut s, 31, &b, "docs/x", LeaseMode::Exclusive, None, "w").is_ok());
    let e =
        ops::lease_acquire(&mut s, 31, &b, "other/x", LeaseMode::Exclusive, None, "w").unwrap_err();
    assert_eq!(e.reason, Reason::OutOfScope);
    assert!(held(ops::lease_renew(&mut s, 32, &b, &renewable, None)));
}

#[test]
fn lease_expiry_keeps_hold() {
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    ops::lease_acquire(&mut s, 22, &b, "src/a", LeaseMode::Exclusive, Some(10), "w").unwrap();
    ops::root_hold(&mut s, 25, &hev("root-session", 1), &d, "wait").unwrap();
    ops::expire_leases(&mut s, 1_000);
    assert!(s.leases.is_empty());
    assert!(s.duties[&d].hold.is_some());
    assert!(held(ops::duty_progress(
        &mut s,
        1_001,
        &b,
        &d,
        DutyStatus::Done,
        None
    )));
}

// ----- structured escalations (plan gaebu-v0-2, Decisions 9, 9a) -----

/// a commands b, who heads a child bu that c belongs to (a → b → c).
fn chain() -> (State, Actor, Actor, Actor) {
    let (mut s, a, b, c) = setup();
    let bu = ops::bu_open(&mut s, 15, &a, &b.member, None, &[], &[])
        .unwrap()
        .ids[0]
        .clone();
    ops::member_move(&mut s, 16, &a, &c.member, &bu).unwrap();
    assert_eq!(
        s.members[&c.member].superior.as_deref(),
        Some(b.member.as_str())
    );
    (s, a, b, c)
}

#[test]
fn escalation_reuse_and_supersede() {
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    let first = ops::escalate(&mut s, 30, &b, Some(&d), "may I change the API?").unwrap();
    let id = first.ids[0].clone();
    let queued = s.outbox.len();
    // The same question (whitespace aside) is the same request: no new notice.
    let again = ops::escalate(&mut s, 31, &b, Some(&d), "may I  change the API?").unwrap();
    assert_eq!(again.ids, vec![id.clone()]);
    assert!(again.summary.contains("reused"));
    assert_eq!(s.outbox.len(), queued);
    // A different question replaces it, with one notice.
    let other = ops::escalate(&mut s, 32, &b, Some(&d), "may I drop the API?").unwrap();
    let new = other.ids[0].clone();
    assert_ne!(new, id);
    assert!(other.summary.contains(&format!("superseded {id}")));
    assert_eq!(s.outbox.len(), queued + 1);
    assert_eq!(s.escalations[&id].status, EscalationStatus::Superseded);
    assert_eq!(
        s.escalations[&id].superseded_by.as_deref(),
        Some(new.as_str())
    );
    assert_eq!(s.escalations[&new].status, EscalationStatus::Open);
    // A question on another duty (or none) is a separate request.
    ops::escalate(&mut s, 33, &b, None, "may I drop the API?").unwrap();
    let open = s
        .escalations
        .values()
        .filter(|e| e.status == EscalationStatus::Open)
        .count();
    assert_eq!(open, 2);
}

#[test]
fn escalation_withdraw_only_by_asker() {
    let (mut s, a, b, c) = setup();
    let e = ops::escalate(&mut s, 30, &b, None, "which branch?")
        .unwrap()
        .ids[0]
        .clone();
    for who in [&a, &c] {
        let err = ops::escalation_withdraw(&mut s, 31, who, &e, None).unwrap_err();
        assert_eq!(err.reason, Reason::NoGrant);
    }
    let queued = s.outbox.len();
    ops::escalation_withdraw(&mut s, 32, &b, &e, Some("found it")).unwrap();
    assert_eq!(s.escalations[&e].status, EscalationStatus::Withdrawn);
    assert_eq!(
        s.outbox.len(),
        queued + 1,
        "the recipient stops considering it"
    );
    let err = ops::escalation_withdraw(&mut s, 33, &b, &e, None).unwrap_err();
    assert_eq!(err.reason, Reason::Conflict);
    let err = ops::escalation_answer(&mut s, 34, &a, &e, "main").unwrap_err();
    assert_eq!(err.reason, Reason::Conflict);
}

#[test]
fn escalation_reuse_reroutes_when_recipient_unavailable() {
    let (mut s, a, b, c) = chain();
    let e = ops::escalate(&mut s, 30, &c, None, "is the schema frozen?")
        .unwrap()
        .ids[0]
        .clone();
    assert_eq!(s.escalations[&e].to.as_deref(), Some(b.member.as_str()));
    // b's seat goes vacant without any commit hook running.
    s.members.get_mut(&b.member).unwrap().binding.state = BindingState::Vacant;
    let queued = s.outbox.len();
    let again = ops::escalate(&mut s, 31, &c, None, "is the schema frozen?").unwrap();
    assert_eq!(again.ids, vec![e.clone()]);
    assert!(again.summary.contains("rerouted"));
    assert_eq!(s.escalations[&e].to.as_deref(), Some(a.member.as_str()));
    assert_eq!(s.outbox.len(), queued + 1);
    assert_eq!(s.outbox.last().unwrap().to_session, a.session_id);
    let queued = s.outbox.len();
    let third = ops::escalate(&mut s, 32, &c, None, "is the schema frozen?").unwrap();
    assert!(third.summary.contains("reused"));
    assert_eq!(s.outbox.len(), queued);
    ops::escalation_answer(&mut s, 33, &a, &e, "yes").unwrap();
}

#[test]
fn escalations_reroute_after_commit() {
    use gaebu::store::Store;
    use std::time::Duration;
    type Cause = fn(&mut State, &Actor, &Actor) -> Result<ops::Outcome, gaebu::Error>;
    let causes: [(&str, Cause, bool); 7] = [
        (
            "mark_vacant",
            |s, _, b| {
                let m = b.member.clone();
                ops::mark_vacant(s, 40, &m, "pager_gone").map(|_| ops::Outcome {
                    summary: String::new(),
                    ids: vec![],
                })
            },
            true,
        ),
        (
            "dismiss",
            |s, _, _| ops::root_dismiss(s, 40, &hev("root-session", 1_000), "child"),
            true,
        ),
        (
            "bu cancel",
            |s, a, b| {
                let bu = s.members[&b.member].bu.clone();
                ops::bu_cancel(s, 40, a, &bu, "reorg")
            },
            // c sits in the cancelled bu too: its question interrupts nobody.
            false,
        ),
        (
            "asker dismissed, recipient vacant",
            |s, _, b| {
                ops::root_dismiss(s, 40, &hev("root-session", 1_003), "other")?;
                let m = b.member.clone();
                ops::mark_vacant(s, 41, &m, "pager_gone").map(|_| ops::Outcome {
                    summary: String::new(),
                    ids: vec![],
                })
            },
            false,
        ),
        (
            "team cancel",
            |s, _, _| {
                let t = s.teams.keys().next().unwrap().clone();
                ops::root_cancel_team(s, 40, &hev("root-session", 1_001), &t)
            },
            false,
        ),
        (
            "root rebind",
            |s, _, b| {
                let m = b.member.clone();
                ops::root_rebind(s, 40, &hev("root-session", 1_002), &m, "child2")
            },
            true,
        ),
        (
            "member rebind",
            |s, a, b| {
                let m = b.member.clone();
                ops::member_rebind(s, 40, a, &m, "child3")
            },
            true,
        ),
    ];
    for (i, (name, cause, to_commander)) in causes.into_iter().enumerate() {
        let (mut s, a, b, c) = chain();
        let e = ops::escalate(&mut s, 30, &c, None, "is the schema frozen?")
            .unwrap()
            .ids[0]
            .clone();
        let home = std::env::temp_dir().join(format!(
            "gaebu-reroute-{}-{}-{i}",
            std::process::id(),
            now_ms()
        ));
        let _cleanup = common::RemoveOnDrop(home.clone());
        let store = Store::open(&home).unwrap();
        store
            .transact(Duration::from_secs(5), |st| {
                *st = s.clone();
                Ok(())
            })
            .unwrap();
        store
            .transact(Duration::from_secs(5), |st| cause(st, &a, &b))
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        let mut st = store.read().unwrap();
        let to = st.escalations[&e].to.clone();
        if to_commander {
            assert_eq!(to.as_deref(), Some(a.member.as_str()), "{name}");
            ops::escalation_answer(&mut st, 50, &a, &e, "yes")
                .unwrap_or_else(|err| panic!("{name}: {err}"));
        } else {
            assert_eq!(
                to, None,
                "{name}: no live superior or asker → the user's queue"
            );
        }
    }
}

// ----- attention budget (plan gaebu-v0-2, Decisions 4, 13, 15) -----

/// Notices a change should queue under the rule: Act → each distinct bound
/// recipient session other than the author's own; Record → none.
fn expected_notices(s: &State, r: &Record, act: bool) -> usize {
    if !act {
        return 0;
    }
    let own = s
        .members
        .get(&r.author)
        .and_then(|m| m.binding.session_id.clone());
    let mut sessions: Vec<_> = r
        .refs
        .iter()
        .filter_map(|m| s.members.get(m).and_then(|m| m.binding.session_id.clone()))
        .filter(|sid| Some(sid) != own.as_ref())
        .collect();
    sessions.sort();
    sessions.dedup();
    sessions.len()
}

/// Every `changed()` call site in src/ops.rs, one row per branch:
/// root_appoint, root_assign, root_dismiss, root_cancel_team, root_hold,
/// root_release, confirm_binding, mark_vacant, bu_open, bu_close, bu_cancel,
/// member_add, member_move, title_set, grant, revoke, duty_issue,
/// duty_respond (accept, decline), duty_progress (unblock, blocked, done,
/// handoff), duty_amend, duty_cancel, lease_acquire, lease_renew,
/// lease_release, lease_force_release, escalate (rerouted, raised and
/// superseded), escalation_withdraw, reroute_escalations,
/// escalation_answer, rebind (via member_rebind).
#[test]
fn notice_classification_matrix() {
    type Prep = fn(&mut State, &Actor, &Actor, &Actor) -> String;
    type Op = fn(&mut State, &Actor, &Actor, &Actor, &str);
    fn none(_: &mut State, _: &Actor, _: &Actor, _: &Actor) -> String {
        String::new()
    }
    fn duty(s: &mut State, a: &Actor, b: &Actor, _: &Actor) -> String {
        accept(s, a, b)
    }
    fn proposed(s: &mut State, a: &Actor, b: &Actor, _: &Actor) -> String {
        ops::duty_issue(s, 20, a, &b.member, spec()).unwrap().ids[0].clone()
    }
    fn blocked(s: &mut State, a: &Actor, b: &Actor, _: &Actor) -> String {
        let d = accept(s, a, b);
        ops::duty_progress(s, 22, b, &d, DutyStatus::Blocked, Some("stuck")).unwrap();
        d
    }
    fn lease(s: &mut State, a: &Actor, b: &Actor, _: &Actor) -> String {
        accept(s, a, b);
        ops::lease_acquire(s, 22, b, "src/a", LeaseMode::Exclusive, None, "w")
            .unwrap()
            .ids[0]
            .clone()
    }
    fn child_bu(s: &mut State, a: &Actor, b: &Actor, _: &Actor) -> String {
        ops::bu_open(s, 15, a, &b.member, None, &[], &[])
            .unwrap()
            .ids[0]
            .clone()
    }
    fn question(s: &mut State, _: &Actor, b: &Actor, _: &Actor) -> String {
        ops::escalate(s, 30, b, None, "which branch?").unwrap().ids[0].clone()
    }
    fn stranded(s: &mut State, a: &Actor, b: &Actor, c: &Actor) -> String {
        let bu = child_bu(s, a, b, c);
        ops::member_move(s, 16, a, &c.member, &bu).unwrap();
        let e = ops::escalate(s, 30, c, None, "is it frozen?").unwrap().ids[0].clone();
        s.members.get_mut(&b.member).unwrap().binding.state = BindingState::Vacant;
        e
    }
    fn held(s: &mut State, a: &Actor, b: &Actor, _: &Actor) -> String {
        let d = accept(s, a, b);
        ops::root_hold(s, 25, &hev("root-session", 900), &d, "wait").unwrap();
        d
    }
    fn granted(s: &mut State, a: &Actor, b: &Actor, _: &Actor) -> String {
        ops::grant(s, 39, a, &b.member, &[Verb::Assign], &scope())
            .unwrap()
            .ids[0]
            .clone()
    }
    fn assigned(s: &mut State, _: &Actor, _: &Actor, _: &Actor) -> String {
        ops::root_assign(s, 30, &ev(30), &["new-staff".into()], "root").unwrap();
        String::new()
    }
    let rows: Vec<(&str, Prep, Op, bool)> = vec![
        (
            "root_appoint",
            none,
            |s, _, _, _, _| {
                ops::root_appoint(s, 40, &ev(40), "newcmdr", Some("x"), "p", None).unwrap();
            },
            false,
        ),
        (
            "root_appoint (explicit confirmation)",
            |s, _, _, _| {
                for g in s.grants.values_mut().filter(|g| g.grantor.is_none()) {
                    g.evidence = Some(ev(5));
                }
                String::new()
            },
            |s, _, _, _, _| {
                let e = Evidence {
                    strength: Strength::Explicit,
                    ..ev(40)
                };
                ops::root_appoint(s, 40, &e, "root", None, "p", None).unwrap();
            },
            false,
        ),
        (
            "root_assign",
            none,
            |s, _, _, _, _| {
                ops::root_assign(s, 40, &ev(40), &["new-staff".into()], "root").unwrap();
            },
            false,
        ),
        (
            "root_dismiss",
            none,
            |s, _, _, _, _| {
                ops::root_dismiss(s, 40, &hev("root-session", 1_000), "other").unwrap();
            },
            true,
        ),
        (
            "root_cancel_team",
            none,
            |s, _, _, _, _| {
                let t = s.teams.keys().next().unwrap().clone();
                ops::root_cancel_team(s, 40, &hev("root-session", 1_000), &t).unwrap();
            },
            true,
        ),
        (
            "root_hold",
            duty,
            |s, _, _, _, d| {
                ops::root_hold(s, 40, &hev("root-session", 1_000), d, "wait").unwrap();
            },
            true,
        ),
        (
            "root_release",
            held,
            |s, _, _, _, d| {
                ops::root_release(s, 40, &hev("root-session", 1_000), d).unwrap();
            },
            true,
        ),
        (
            "confirm_binding",
            assigned,
            |s, _, _, _, _| {
                ops::confirm_binding(s, 40, "new-staff-session", Tool::Codex, "new-staff").unwrap();
            },
            false,
        ),
        (
            "mark_vacant",
            none,
            |s, _, b, _, _| {
                ops::mark_vacant(s, 40, &b.member, "pager_gone").unwrap();
            },
            false,
        ),
        (
            "bu_open",
            none,
            |s, a, b, _, _| {
                ops::bu_open(s, 40, a, &b.member, None, &[], &[]).unwrap();
            },
            true,
        ),
        (
            "bu_close",
            child_bu,
            |s, a, _, _, bu| {
                ops::bu_close(s, 40, a, bu).unwrap();
            },
            false,
        ),
        (
            "bu_cancel",
            child_bu,
            |s, a, _, _, bu| {
                ops::bu_cancel(s, 40, a, bu, "reorg").unwrap();
            },
            true,
        ),
        (
            "member_add",
            none,
            |s, a, _, _, _| {
                let bu = s.members[&a.member].bu.clone();
                ops::member_add(s, 40, a, &bu, "recruit", None).unwrap();
            },
            false,
        ),
        (
            "member_move",
            child_bu,
            |s, a, _, c, bu| {
                ops::member_move(s, 40, a, &c.member, bu).unwrap();
            },
            true,
        ),
        (
            "title_set",
            none,
            |s, a, b, _, _| {
                ops::title_set(s, 40, a, &b.member, "참군").unwrap();
            },
            false,
        ),
        (
            "grant",
            none,
            |s, a, b, _, _| {
                ops::grant(s, 40, a, &b.member, &[Verb::Assign], &scope()).unwrap();
            },
            false,
        ),
        (
            "revoke",
            granted,
            |s, a, _, _, g| {
                ops::revoke(s, 40, a, g).unwrap();
            },
            true,
        ),
        (
            "duty_issue",
            none,
            |s, a, b, _, _| {
                ops::duty_issue(s, 40, a, &b.member, spec()).unwrap();
            },
            true,
        ),
        (
            "duty_respond accept",
            proposed,
            |s, _, b, _, d| {
                ops::duty_respond(s, 40, b, d, None, true, None).unwrap();
            },
            false,
        ),
        (
            "duty_respond decline",
            proposed,
            |s, _, b, _, d| {
                ops::duty_respond(s, 40, b, d, None, false, Some("no")).unwrap();
            },
            true,
        ),
        (
            "duty_progress unblock",
            blocked,
            |s, _, b, _, d| {
                ops::duty_progress(s, 40, b, d, DutyStatus::Accepted, None).unwrap();
            },
            false,
        ),
        (
            "duty_progress blocked",
            duty,
            |s, _, b, _, d| {
                ops::duty_progress(s, 40, b, d, DutyStatus::Blocked, Some("x")).unwrap();
            },
            true,
        ),
        (
            "duty_progress done",
            duty,
            |s, _, b, _, d| {
                ops::duty_progress(s, 40, b, d, DutyStatus::Done, None).unwrap();
            },
            true,
        ),
        (
            "duty_progress handoff",
            duty,
            |s, _, b, _, d| {
                ops::duty_progress(s, 40, b, d, DutyStatus::HandedOff, Some("to c")).unwrap();
            },
            true,
        ),
        (
            "duty_amend",
            duty,
            |s, a, _, _, d| {
                let v = s.duties[d].version;
                ops::duty_amend(s, 40, a, d, v, spec()).unwrap();
            },
            true,
        ),
        (
            "duty_cancel",
            duty,
            |s, a, _, _, d| {
                ops::duty_cancel(s, 40, a, d, "drop").unwrap();
            },
            true,
        ),
        (
            "lease_acquire",
            duty,
            |s, _, b, _, _| {
                ops::lease_acquire(s, 40, b, "src/b", LeaseMode::Exclusive, None, "w").unwrap();
            },
            false,
        ),
        (
            "lease_renew",
            lease,
            |s, _, b, _, l| {
                ops::lease_renew(s, 40, b, l, None).unwrap();
            },
            false,
        ),
        (
            "lease_release",
            lease,
            |s, _, b, _, l| {
                ops::lease_release(s, 40, b, l).unwrap();
            },
            false,
        ),
        (
            "lease_force_release",
            lease,
            |s, a, _, _, l| {
                ops::lease_force_release(s, 40, a, l, "needed").unwrap();
            },
            true,
        ),
        (
            "escalate rerouted",
            stranded,
            |s, _, _, c, _| {
                ops::escalate(s, 40, c, None, "is it frozen?").unwrap();
            },
            true,
        ),
        (
            "escalate raised",
            none,
            |s, _, b, _, _| {
                ops::escalate(s, 40, b, None, "which branch?").unwrap();
            },
            true,
        ),
        (
            "escalate superseded",
            question,
            |s, _, b, _, _| {
                ops::escalate(s, 40, b, None, "which tag?").unwrap();
            },
            true,
        ),
        (
            "escalation_withdraw",
            question,
            |s, _, b, _, e| {
                ops::escalation_withdraw(s, 40, b, e, None).unwrap();
            },
            true,
        ),
        (
            "reroute_escalations",
            stranded,
            |s, _, _, _, _| {
                ops::reroute_escalations(s, 40);
            },
            true,
        ),
        (
            "escalation_answer",
            question,
            |s, a, _, _, e| {
                ops::escalation_answer(s, 40, a, e, "main").unwrap();
            },
            true,
        ),
        (
            "rebind",
            none,
            |s, a, b, _, _| {
                ops::member_rebind(s, 40, a, &b.member, "child-next").unwrap();
            },
            false,
        ),
    ];
    for (name, prep, op, act) in rows {
        let (mut s, a, b, c) = setup();
        let arg = prep(&mut s, &a, &b, &c);
        s.outbox.clear();
        let records = s.records.len();
        op(&mut s, &a, &b, &c, &arg);
        let added = &s.records[records..];
        assert_eq!(added.len(), 1, "{name}: one team record per event");
        let want: usize = added.iter().map(|r| expected_notices(&s, r, act)).sum();
        assert_eq!(s.outbox.len(), want, "{name}: notices");
        if act {
            assert!(
                want > 0,
                "{name}: an Act row must reach someone in this fixture"
            );
        }
    }
}

#[test]
fn record_rows_would_have_reached_someone() {
    // Record really suppresses: accept's record names the bound issuer.
    let (mut s, a, b, _) = setup();
    let d = ops::duty_issue(&mut s, 20, &a, &b.member, spec())
        .unwrap()
        .ids[0]
        .clone();
    s.outbox.clear();
    ops::duty_respond(&mut s, 21, &b, &d, None, true, None).unwrap();
    let r = s.records.last().unwrap();
    assert_eq!(r.refs, vec![a.member.clone()]);
    assert_eq!(expected_notices(&s, r, true), 1);
    assert!(s.outbox.is_empty());
}

#[test]
fn composite_ops_notice_totals() {
    // bu_open with verbs: the bu opening (Act) and the grant inside it (Record).
    let (mut s, a, b, _) = setup();
    s.outbox.clear();
    let records = s.records.len();
    ops::bu_open(&mut s, 40, &a, &b.member, None, &[Verb::Assign], &scope()).unwrap();
    let added: Vec<_> = s.records[records..]
        .iter()
        .map(|r| r.text.clone())
        .collect();
    assert_eq!(added.len(), 2, "{added:?}");
    assert!(added.iter().any(|t| t.contains("delegated")));
    assert!(added.iter().any(|t| t.contains("opened")));
    assert_eq!(
        s.outbox.len(),
        1,
        "only the opening interrupts the new head"
    );
    assert_eq!(s.outbox[0].to_session, b.session_id);
    // A dismissal that strands an escalation: the dismissal, then the reroute.
    let (mut s, a, b, c) = chain();
    ops::escalate(&mut s, 30, &c, None, "is it frozen?").unwrap();
    s.outbox.clear();
    let records = s.records.len();
    ops::root_dismiss(&mut s, 40, &hev("root-session", 1_000), "child").unwrap();
    ops::reroute_escalations(&mut s, 41);
    let added = &s.records[records..];
    assert!(added.iter().any(|r| r.text.contains("rerouted")));
    let want: usize = added.iter().map(|r| expected_notices(&s, r, true)).sum();
    assert_eq!(s.outbox.len(), want);
    assert!(
        s.outbox
            .iter()
            .any(|n| n.to_session == a.session_id && n.body.contains("rerouted"))
    );
    let _ = b;
}

#[test]
fn duplicate_pending_notice_not_queued() {
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    s.outbox.clear();
    // The same change twice (new record ids): one pending notice.
    ops::duty_progress(&mut s, 30, &b, &d, DutyStatus::Blocked, Some("stuck")).unwrap();
    ops::duty_progress(&mut s, 31, &b, &d, DutyStatus::Accepted, None).unwrap();
    ops::duty_progress(&mut s, 32, &b, &d, DutyStatus::Blocked, Some("stuck")).unwrap();
    assert_eq!(s.outbox.len(), 1);
    let ids: Vec<_> = s
        .records
        .iter()
        .rev()
        .take(3)
        .map(|r| r.id.clone())
        .collect();
    assert_eq!(ids.len(), 3);
    // Still one while a flush holds it.
    s.outbox[0].claimed_at = Some(33);
    ops::duty_progress(&mut s, 34, &b, &d, DutyStatus::Accepted, None).unwrap();
    ops::duty_progress(&mut s, 35, &b, &d, DutyStatus::Blocked, Some("stuck")).unwrap();
    assert_eq!(s.outbox.len(), 1);
    // A notice migrated from schema 1 (text derived from its v1 body) dedupes too.
    let n = s.outbox[0].clone();
    s.outbox[0].text = notice_key(&n.body.replacen(" — r-", " — r-9", 1));
    assert_eq!(s.outbox[0].text, n.text);
    // Delivered and gone: the same change again is a new notice.
    s.outbox.clear();
    ops::duty_progress(&mut s, 36, &b, &d, DutyStatus::Accepted, None).unwrap();
    ops::duty_progress(&mut s, 37, &b, &d, DutyStatus::Blocked, Some("stuck")).unwrap();
    assert_eq!(s.outbox.len(), 1);
}

#[test]
fn hold_survives_cancel_with_terminal_stage() {
    use gaebu::card::{Stage, stage};
    type Cancel = fn(&mut State, &Actor, &str);
    let cancels: [(&str, Cancel); 3] = [
        ("duty", |s, a, d| {
            ops::duty_cancel(s, 40, a, d, "drop").unwrap();
        }),
        ("bu", |s, a, d| {
            let bu = s.duties[d].bu.clone();
            ops::bu_cancel(s, 40, a, &bu, "reorg").unwrap();
        }),
        ("team", |s, _, _| {
            let t = s.teams.keys().next().unwrap().clone();
            ops::root_cancel_team(s, 40, &hev("root-session", 1_000), &t).unwrap();
        }),
    ];
    for (name, cancel) in cancels {
        let (mut s, a, b, _) = setup();
        ops::bu_open(&mut s, 15, &a, &b.member, None, &[], &[]).unwrap();
        let d = accept(&mut s, &a, &b);
        ops::root_hold(&mut s, 30, &hev("root-session", 1), &d, "확인 후").unwrap();
        assert_eq!(
            stage(&s, &s.duties[&d], 31, Some(31)),
            Stage::Held,
            "{name}"
        );
        cancel(&mut s, &a, &d);
        assert!(s.duties[&d].hold.is_some(), "{name}: the hold record stays");
        assert_eq!(
            stage(&s, &s.duties[&d], 41, Some(41)),
            Stage::Ended,
            "{name}"
        );
    }
}

#[test]
fn flipping_change_keeps_notice_order() {
    // held, released, held while queued: the recipient must end on "held".
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    s.outbox.clear();
    ops::root_hold(&mut s, 30, &hev("root-session", 1), &d, "wait").unwrap();
    ops::root_release(&mut s, 31, &hev("root-session", 2), &d).unwrap();
    ops::root_hold(&mut s, 32, &hev("root-session", 3), &d, "wait").unwrap();
    let texts: Vec<_> = s.outbox.iter().map(|n| n.text.clone()).collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(texts[2].contains("held until: wait"), "{texts:?}");
}

#[test]
fn hold_until_stays_one_line() {
    let (mut s, a, b, _) = setup();
    let d = accept(&mut s, &a, &b);
    ops::root_hold(
        &mut s,
        30,
        &hev("root-session", 1),
        &d,
        "대협 확인\n소임 d-9 [진행]: 가짜 줄\t끝",
    )
    .unwrap();
    let until = &s.duties[&d].hold.as_ref().unwrap().until;
    assert_eq!(until, "대협 확인 소임 d-9 [진행]: 가짜 줄 끝");
    let e = ops::root_hold(&mut s, 31, &hev("root-session", 2), "d-404", " \n\t ").unwrap_err();
    assert_eq!(e.reason, Reason::InvalidArgument);
}
