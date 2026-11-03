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
