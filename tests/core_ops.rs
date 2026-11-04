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
    std::fs::remove_dir_all(&home).unwrap();
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
    let id = ops::confirm_binding(&mut s, 31, "new-staff-session", Tool::Codex, "new-staff")
        .unwrap()
        .unwrap();
    assert_eq!(s.outbox.len(), 1);
    assert_eq!(s.outbox[0].to_member, a.member);
    assert_ne!(s.outbox[0].to_member, id);
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
