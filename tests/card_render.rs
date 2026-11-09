use gaebu::card::{self, MAX_LINES};
use gaebu::model::*;
use std::collections::BTreeSet;

fn binding(alias: &str, session: &str) -> Binding {
    Binding {
        state: BindingState::Bound,
        alias: alias.into(),
        tool: Some(Tool::Codex),
        session_id: Some(session.into()),
        designated_at: 0,
        bound_at: Some(0),
        vacated_at: None,
        vacated_reason: None,
    }
}

fn member(id: &str, bu: &str, sup: Option<&str>, title: &str, alias: &str) -> Member {
    Member {
        id: id.into(),
        team: "t-1".into(),
        bu: bu.into(),
        superior: sup.map(Into::into),
        title: title.into(),
        status: MemberStatus::Active,
        binding: binding(alias, &format!("s-{alias}")),
        created_at: 0,
    }
}

/// 승상 gapu commands 좌장군 nuzu, who heads 좌장군부 with 속관 주부 bepo.
fn fixture() -> State {
    let mut st = State {
        rev: 7,
        ..Default::default()
    };
    st.teams.insert(
        "t-1".into(),
        Team {
            id: "t-1".into(),
            name: "gaebu".into(),
            project: "gaebu".into(),
            commander: "m-1".into(),
            root_bu: "b-1".into(),
            status: TeamStatus::Active,
            created_at: 0,
            cancelled_at: None,
        },
    );
    for (id, parent, name, head) in [
        ("b-1", None, "승상부", "m-1"),
        ("b-2", Some("b-1"), "좌장군부", "m-2"),
    ] {
        st.bus.insert(
            id.into(),
            Bu {
                id: id.into(),
                team: "t-1".into(),
                parent: parent.map(Into::into),
                name: name.into(),
                head: head.into(),
                status: BuStatus::Open,
                created_at: 0,
                hold: None,
            },
        );
    }
    st.members
        .insert("m-1".into(), member("m-1", "b-1", None, "승상", "gapu"));
    st.members.insert(
        "m-2".into(),
        member("m-2", "b-2", Some("m-1"), "좌장군", "nuzu"),
    );
    st.members.insert(
        "m-3".into(),
        member("m-3", "b-2", Some("m-2"), "주부", "bepo"),
    );
    st.grants.insert(
        "g-1".into(),
        Grant {
            id: "g-1".into(),
            team: "t-1".into(),
            grantor: None,
            grantee: "m-1".into(),
            verbs: Verb::ALL.into_iter().collect::<BTreeSet<_>>(),
            scope: vec!["**".into(), "res:**".into()],
            status: GrantStatus::Active,
            evidence: None,
            created_at: 0,
        },
    );
    st.duties.insert(
        "d-1".into(),
        Duty {
            id: "d-1".into(),
            team: "t-1".into(),
            bu: "b-2".into(),
            assignee: "m-3".into(),
            issuer: "m-2".into(),
            intent: "hooks must never block a prompt".into(),
            outcome: "hook adapter for Claude and Codex".into(),
            scope: vec!["src/hook.rs".into()],
            decision_rights: "internal structure is yours".into(),
            escalate_on: vec!["changing public signatures".into()],
            done_criteria: "cargo test passes".into(),
            plan_ref: None,
            exec_root: None,
            status: DutyStatus::Accepted,
            version: 1,
            note: None,
            created_at: 0,
            updated_at: 0,
            hold: None,
            source: None,
            schedule_ref: None,
        },
    );
    st.leases.insert(
        "l-1".into(),
        Lease {
            id: "l-1".into(),
            team: "t-1".into(),
            key: "src/hook.rs".into(),
            mode: LeaseMode::Exclusive,
            holder: "m-3".into(),
            holder_session: "s-bepo".into(),
            reason: "editing".into(),
            acquired_at: 0,
            expires_at: 30 * 60_000,
        },
    );
    st
}

#[test]
fn duty_view_preserves_the_full_contract_after_context_loss() {
    let mut st = fixture();
    let duty = st.duties.get_mut("d-1").unwrap();
    duty.plan_ref = Some(".agents/plans/hook-adapter.md".into());
    duty.exec_root = Some("checkout/hooks".into());
    duty.note = Some("waiting for shared interface decision".into());
    let view = card::duty_view(&st, &st.duties["d-1"], 1_000, Some(1_000));
    let value = serde_json::to_value(view).unwrap();
    assert_eq!(value["intent"], "hooks must never block a prompt");
    assert_eq!(value["plan_ref"], ".agents/plans/hook-adapter.md");
    assert_eq!(value["exec_root"], "checkout/hooks");
    assert_eq!(value["note"], "waiting for shared interface decision");
    assert_eq!(value["version"], 1);
    assert_eq!(value["escalate_on"][0], "changing public signatures");
    assert_eq!(value["origin_bu"], "좌장군부");
}

#[test]
fn commander_card_does_not_wait_for_a_nonexistent_superior() {
    let st = fixture();
    let text = card::render(&card::build(&st, "m-1", 1_000, Some(1_000)).unwrap());
    assert!(!text.contains("상관의 발령을 기다리거나"), "{text}");
    assert!(text.contains("배정") || text.contains("지휘"));
}

#[test]
fn board_orders_parent_before_child_independent_of_allocated_ids() {
    let mut st = fixture();
    let mut root = st.bus.remove("b-1").unwrap();
    root.id = "b-z".into();
    st.bus.insert("b-z".into(), root);
    st.bus.get_mut("b-2").unwrap().parent = Some("b-z".into());
    st.teams.get_mut("t-1").unwrap().root_bu = "b-z".into();
    st.members.get_mut("m-1").unwrap().bu = "b-z".into();
    let board = card::board(&st, 1_000, None, &|_| Some(1_000));
    assert_eq!(board.teams[0].bus[0].id, "b-z");
    assert_eq!(board.teams[0].bus[1].parent.as_deref(), Some("b-z"));
}

#[test]
fn card_at_most_12_lines() {
    let mut st = fixture();
    // Pile on duties to force truncation.
    for i in 2..10 {
        let mut d = st.duties["d-1"].clone();
        d.id = format!("d-{i}");
        st.duties.insert(d.id.clone(), d);
    }
    let c = card::build(&st, "m-3", 1_000, Some(1_000)).expect("card");
    let text = card::render(&c);
    assert!(
        text.lines().count() <= MAX_LINES,
        "{} lines:\n{text}",
        text.lines().count()
    );
}

#[test]
fn card_contains_chain_superior_duty_peers_leases() {
    let st = fixture();
    let c = card::build(&st, "m-3", 1_000, Some(1_000)).expect("card");
    let text = card::render(&c);
    assert!(
        text.contains("대협 → 가절월을 받은 승상 gapu → 좌장군 nuzu → 좌장군부 속관 주부 bepo"),
        "{text}"
    );
    assert_eq!(c.superior.as_deref(), Some("좌장군 nuzu"));
    assert!(text.contains("hook adapter for Claude and Codex"));
    assert!(text.contains("cargo test passes"));
    assert!(c.peers.iter().any(|p| p == "좌장군 nuzu"));
    assert!(text.contains("src/hook.rs"));
    assert!(c.leases.iter().any(|l| l.starts_with("src/hook.rs")));
    assert_eq!(c.binding, "bound");
    assert_eq!(c.rev, 7);
}

#[test]
fn card_quiet_after_two_hours_unseen() {
    let st = fixture();
    let now = 3 * 60 * 60 * 1000;
    let c = card::build(&st, "m-3", now, Some(0)).expect("card");
    assert_eq!(c.binding, "quiet");
}

#[test]
fn card_hash_changes_only_with_content() {
    let mut st = fixture();
    let a = card::build(&st, "m-3", 1_000, Some(1_000)).unwrap();
    // An unrelated change elsewhere does not change bepo's card hash.
    st.records.push(Record {
        id: "r-1".into(),
        team: "t-1".into(),
        kind: RecordKind::Note,
        author: "m-1".into(),
        refs: vec![],
        text: "unrelated".into(),
        at: 0,
    });
    let b = card::build(&st, "m-3", 1_000, Some(1_000)).unwrap();
    assert_eq!(a.hash, b.hash);
    st.duties.get_mut("d-1").unwrap().outcome = "changed".into();
    let c = card::build(&st, "m-3", 1_000, Some(1_000)).unwrap();
    assert_ne!(a.hash, c.hash);
}

#[test]
fn card_json_fields_match_contract() {
    let st = fixture();
    let c = card::build(&st, "m-3", 1_000, Some(1_000)).unwrap();
    let v = serde_json::to_value(&c).unwrap();
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    // Keep in sync with docs/model.md, section "Card JSON".
    let mut expected = vec![
        "rev",
        "member",
        "alias",
        "title",
        "team",
        "bu",
        "binding",
        "chain",
        "superior",
        "duties",
        "peers",
        "leases",
        "escalations",
        "cancels",
        "norms",
        "hash",
    ];
    expected.sort();
    assert_eq!(keys, expected);
}

#[test]
fn card_lists_holders_of_overlapping_leases() {
    let mut st = fixture();
    let mut d = st.duties["d-1"].clone();
    d.id = "d-9".into();
    d.assignee = "m-1".into();
    d.scope = vec!["src/**".into()];
    st.duties.insert(d.id.clone(), d);
    let c = card::build(&st, "m-1", 1_000, Some(1_000)).unwrap();
    assert!(
        c.peers
            .iter()
            .any(|p| p.contains("bepo") && p.contains("src/hook.rs")),
        "{:?}",
        c.peers
    );
}

#[test]
fn dismissed_member_has_no_card() {
    let mut st = fixture();
    st.members.get_mut("m-3").unwrap().status = MemberStatus::Dismissed;
    assert!(card::build(&st, "m-3", 1_000, None).is_none());
}

#[test]
fn card_quiet_at_exactly_two_hours() {
    let st = fixture();
    let c = card::build(&st, "m-3", card::QUIET_AFTER_MS, Some(0)).unwrap();
    assert_eq!(c.binding, "quiet");
    let c = card::build(&st, "m-3", card::QUIET_AFTER_MS - 1, Some(0)).unwrap();
    assert_eq!(c.binding, "bound");
}

#[test]
fn card_keeps_cancels_when_truncated() {
    let mut st = fixture();
    for i in 2..9 {
        let mut d = st.duties["d-1"].clone();
        d.id = format!("d-{i}");
        st.duties.insert(d.id.clone(), d);
    }
    let mut gone = st.duties["d-1"].clone();
    gone.id = "d-99".into();
    gone.status = DutyStatus::Cancelled;
    gone.updated_at = 1_000;
    st.duties.insert(gone.id.clone(), gone);
    let c = card::build(&st, "m-3", 2_000, Some(2_000)).unwrap();
    let text = card::render(&c);
    assert!(text.lines().count() <= card::MAX_LINES, "{text}");
    assert!(text.contains("소임 d-99 취소됨"), "{text}");
}

#[test]
fn card_hash_stable_while_a_lease_counts_down() {
    let st = fixture();
    let a = card::build(&st, "m-3", 1_000, Some(1_000)).unwrap();
    let b = card::build(&st, "m-3", 10 * 60_000, Some(10 * 60_000)).unwrap();
    assert_eq!(a.hash, b.hash);
}

#[test]
fn card_header_names_the_bu() {
    let st = fixture();
    let c = card::build(&st, "m-2", 1_000, Some(1_000)).unwrap();
    let first = card::render(&c).lines().next().unwrap().to_string();
    assert!(first.contains("좌장군부"), "{first}");
}

// ----- stage table and holds on the card (plan gaebu-v0-2, Decisions 11–11b) -----

fn hold(until: &str) -> Hold {
    Hold {
        evidence: Evidence {
            session_id: "s-gapu".into(),
            prompt_seq: 1,
            sha256: "h".into(),
            attest: Attest::HookFiltered,
            quote: "잠깐 멈춰".into(),
            at: 0,
            strength: Strength::Explicit,
        },
        until: until.into(),
        at: 0,
    }
}

fn escalation(duty: &str) -> Escalation {
    Escalation {
        id: "e-1".into(),
        team: "t-1".into(),
        from: "m-3".into(),
        to: Some("m-2".into()),
        duty: Some(duty.into()),
        question: "may I?".into(),
        status: EscalationStatus::Open,
        answer: None,
        created_at: 0,
        answered_at: None,
        superseded_by: None,
    }
}

#[test]
fn stage_first_matching_row() {
    use card::Stage::*;
    let now = 1_000;
    let stage = |st: &State, seen: Option<Ms>| card::stage(st, &st.duties["d-1"], now, seen);
    let with = |f: &dyn Fn(&mut State)| {
        let mut st = fixture();
        f(&mut st);
        st
    };
    // 7 진행
    assert_eq!(stage(&fixture(), Some(now)), Working);
    // 6 자리 빔: vacant, pending, or quiet.
    let st = with(&|s| s.members.get_mut("m-3").unwrap().binding.state = BindingState::Vacant);
    assert_eq!(stage(&st, Some(now)), Vacant);
    let st = with(&|s| s.members.get_mut("m-3").unwrap().binding.state = BindingState::Pending);
    assert_eq!(stage(&st, Some(now)), Vacant);
    assert_eq!(
        card::stage(&fixture(), &fixture().duties["d-1"], 3 * 3_600_000, Some(0)),
        Vacant
    );
    // 5 막힘, 4 수락 대기
    let st = with(&|s| s.duties.get_mut("d-1").unwrap().status = DutyStatus::Blocked);
    assert_eq!(stage(&st, Some(now)), Blocked);
    let st = with(&|s| s.duties.get_mut("d-1").unwrap().status = DutyStatus::Proposed);
    assert_eq!(stage(&st, Some(now)), AwaitingAcceptance);
    // 3 답변 대기 beats 수락 대기
    let st = with(&|s| {
        s.duties.get_mut("d-1").unwrap().status = DutyStatus::Proposed;
        s.escalations.insert("e-1".into(), escalation("d-1"));
    });
    assert_eq!(stage(&st, Some(now)), AwaitingAnswer);
    // 2 보류 beats 답변 대기 and 수락 대기; a bu hold counts too.
    let st = with(&|s| {
        s.duties.get_mut("d-1").unwrap().status = DutyStatus::Proposed;
        s.escalations.insert("e-1".into(), escalation("d-1"));
        s.duties.get_mut("d-1").unwrap().hold = Some(hold("대협 확인"));
    });
    assert_eq!(stage(&st, Some(now)), Held);
    let st = with(&|s| s.bus.get_mut("b-1").unwrap().hold = Some(hold("전체")));
    assert_eq!(stage(&st, Some(now)), Held);
    // 1 종료 beats everything, a hold included.
    for status in [
        DutyStatus::Done,
        DutyStatus::Declined,
        DutyStatus::Cancelled,
        DutyStatus::HandedOff,
    ] {
        let st = with(&|s| {
            let d = s.duties.get_mut("d-1").unwrap();
            d.status = status;
            d.hold = Some(hold("x"));
        });
        assert_eq!(stage(&st, Some(now)), Ended, "{status:?}");
    }
    let st = with(&|s| {
        s.bus.get_mut("b-2").unwrap().status = BuStatus::Cancelled;
        s.duties.get_mut("d-1").unwrap().hold = Some(hold("x"));
    });
    assert_eq!(stage(&st, Some(now)), Ended);
}

#[test]
fn card_keeps_holds_under_truncation() {
    let mut st = fixture();
    for i in 2..=5 {
        let mut d = st.duties["d-1"].clone();
        d.id = format!("d-{i}");
        st.duties.insert(d.id.clone(), d);
    }
    st.duties.get_mut("d-4").unwrap().hold = Some(hold("디자인 확정 후"));
    st.duties.get_mut("d-5").unwrap().hold = Some(hold("대협이 풀 때까지"));
    let text = card::render(&card::build(&st, "m-3", 1_000, Some(1_000)).unwrap());
    assert!(text.contains("보류: 소임 d-4 — 디자인 확정 후"), "{text}");
    assert!(text.contains("보류: 소임 d-5 — 대협이 풀 때까지"), "{text}");
    assert!(text.lines().count() <= MAX_LINES);
    // Five holds, a cancel and an open escalation at once.
    for i in 1..=5 {
        st.duties.get_mut(&format!("d-{i}")).unwrap().hold = Some(hold(&format!("조건 {i}")));
    }
    let mut cancelled = st.duties["d-1"].clone();
    cancelled.id = "d-9".into();
    cancelled.status = DutyStatus::Cancelled;
    cancelled.updated_at = 900;
    st.duties.insert("d-9".into(), cancelled);
    st.escalations.insert("e-1".into(), escalation("d-1"));
    let text = card::render(&card::build(&st, "m-3", 1_000, Some(1_000)).unwrap());
    assert_eq!(text.matches("보류: 소임").count(), 3, "{text}");
    assert!(text.contains("보류 +2"), "{text}");
    assert!(text.contains("취소: 소임 d-9 취소됨"), "{text}");
    assert!(text.contains("escalation: e-1"), "{text}");
    assert!(text.lines().count() <= MAX_LINES, "{text}");
}

#[test]
fn inferred_root_grant_render() {
    let mut st = fixture();
    let grant = |strength| Grant {
        id: "g-1".into(),
        team: "t-1".into(),
        grantor: None,
        grantee: "m-1".into(),
        verbs: Verb::ALL.into_iter().collect(),
        scope: vec!["**".into(), "res:**".into()],
        status: GrantStatus::Active,
        evidence: Some(Evidence {
            strength,
            ..hold("x").evidence
        }),
        created_at: 0,
    };
    st.grants.insert("g-1".into(), grant(Strength::Inferred));
    let text = card::render(&card::build(&st, "m-3", 1_000, Some(1_000)).unwrap());
    assert!(
        text.contains("대협 → 가절월(추론)을 받은 승상 gapu"),
        "{text}"
    );
    st.grants.insert("g-1".into(), grant(Strength::Explicit));
    let text = card::render(&card::build(&st, "m-3", 1_000, Some(1_000)).unwrap());
    assert!(text.contains("대협 → 가절월을 받은 승상 gapu"), "{text}");
}
