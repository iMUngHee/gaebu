use gaebu::error::Reason;
use gaebu::evidence::{
    PromptClass, classify, current_turn_evidence, mod_active_at, normalize, sha256_hex,
};
use gaebu::model::Attest;
use gaebu::store::{ComposerRecord, EventKind, SessionEvent};

const TEXT: &str = "가절월을 줄게. 개부해라.";
const QUOTE: &str = "가절월을 줄게";

fn event(seq: u64, at: u64, kind: EventKind, text: Option<&str>) -> SessionEvent {
    SessionEvent {
        seq,
        at,
        kind,
        sha256: text.map(|s| sha256_hex(&normalize(s))),
        text: text.map(str::to_string),
        excluded_by: None,
    }
}
fn prompt() -> SessionEvent {
    event(2, 100, EventKind::UserPrompt, Some(TEXT))
}
fn composer(at: u64) -> ComposerRecord {
    ComposerRecord {
        at,
        sha256: sha256_hex(TEXT),
    }
}
fn refused(events: &[SessionEvent], composers: &[ComposerRecord], quote: &str) {
    assert_eq!(
        current_turn_evidence("caller", events, composers, quote, 1000)
            .unwrap_err()
            .reason,
        Reason::NoUserEvidence
    );
}

#[test]
fn root_requires_user_evidence() {
    refused(&[], &[], QUOTE);
    refused(&[prompt()], &[], "줄게");
    refused(&[prompt()], &[], "다른 이를 가절월로");
    refused(&[event(2, 100, EventKind::UserPrompt, None)], &[], QUOTE);
    let evidence = current_turn_evidence("caller", &[prompt()], &[], QUOTE, 1000).unwrap();
    assert_eq!(evidence.session_id, "caller");
    assert_eq!(evidence.prompt_seq, 2);
    assert_eq!(evidence.sha256, sha256_hex(TEXT));
    assert_eq!(evidence.at, 100);
    assert_eq!(evidence.quote, QUOTE);
    assert_eq!(evidence.attest, Attest::HookFiltered);
    refused(
        &[event(2, 1001, EventKind::UserPrompt, Some(TEXT))],
        &[],
        QUOTE,
    );
}

#[test]
fn peer_wrapper_prompts_not_recorded() {
    for text in [
        "",
        " \n",
        "pager: new mail for nawi",
        "text [pager-poke] text",
        "Another Claude session sent a message: hi",
        "<agent-message name=x>hi",
        "[SYSTEM NOTIFICATION hi",
        "<side-sessions-event>hi",
        "<task-notification>hi",
    ] {
        assert!(matches!(classify(text), PromptClass::Excluded(_)), "{text}");
    }
    assert_eq!(classify(TEXT), PromptClass::User);
    assert_eq!(classify("Fromage recipe please"), PromptClass::User);
    assert_eq!(classify("From now on use Gaebu"), PromptClass::User);
}

#[test]
fn evidence_from_other_session_rejected() {
    // The caller's SessionLog is the isolation boundary; a different session
    // has no caller prompt even if a composer digest happens to match.
    refused(&[], &[composer(100)], QUOTE);
}

#[test]
fn evidence_after_stop_rejected() {
    refused(
        &[prompt(), event(3, 101, EventKind::Stop, None)],
        &[],
        QUOTE,
    );
}

#[test]
fn evidence_after_peer_prompt_rejected() {
    refused(
        &[
            prompt(),
            event(
                3,
                101,
                EventKind::ExcludedPrompt,
                Some("pager: new mail for nawi"),
            ),
        ],
        &[],
        QUOTE,
    );
}

#[test]
fn stale_prompt_evidence_rejected() {
    refused(
        &[
            prompt(),
            event(3, 101, EventKind::UserPrompt, Some("다른 업무를 먼저 하자")),
        ],
        &[],
        QUOTE,
    );
    // Sequence, rather than input vector order or wall clock, defines the turn.
    refused(&[event(3, 99, EventKind::Stop, None), prompt()], &[], QUOTE);
}

#[test]
fn mod_active_composer_evidence_accepted_regardless_of_record_order() {
    let events = [event(1, 90, EventKind::ModHeartbeat, None), prompt()];
    for at in [95, 100, 999] {
        let e = current_turn_evidence("caller", &events, &[composer(at)], QUOTE, 1000).unwrap();
        assert_eq!(e.attest, Attest::Composer);
    }
    // A delayed composer remains eligible well beyond the old 10-second window.
    assert!(current_turn_evidence("caller", &events, &[composer(50_000)], QUOTE, 50_001).is_ok());
}

#[test]
fn mod_active_hook_only_prompt_not_evidence_but_invalidates() {
    let events = [event(1, 90, EventKind::ModHeartbeat, None), prompt()];
    refused(&events, &[], QUOTE);
    let mut later = events.to_vec();
    later.push(event(
        3,
        110,
        EventKind::UserPrompt,
        Some("현재 다른 지시를 줄게"),
    ));
    refused(&later, &[composer(100)], QUOTE);
    // A heartbeat outside the permitted arrival-skew window does not disable
    // hook fallback for this prompt.
    assert!(
        current_turn_evidence(
            "caller",
            &[prompt(), event(3, 10_101, EventKind::ModHeartbeat, None)],
            &[],
            QUOTE,
            20_000
        )
        .is_ok()
    );
}

#[test]
fn mod_inactive_falls_back_to_hook_filtered() {
    assert!(!mod_active_at(&[], 100));
    let heartbeat = event(1, 100, EventKind::ModHeartbeat, None);
    assert!(mod_active_at(std::slice::from_ref(&heartbeat), 100));
    assert!(mod_active_at(std::slice::from_ref(&heartbeat), 3_600_099));
    assert!(mod_active_at(std::slice::from_ref(&heartbeat), 3_600_100));
    assert!(!mod_active_at(std::slice::from_ref(&heartbeat), 3_600_101));
    assert!(mod_active_at(std::slice::from_ref(&heartbeat), 99));
    assert!(!mod_active_at(&[event(1, 100, EventKind::Stop, None)], 100));
    let old = event(1, 0, EventKind::ModHeartbeat, None);
    let p = event(2, 3_600_100, EventKind::UserPrompt, Some(TEXT));
    assert_eq!(
        current_turn_evidence("caller", &[old, p], &[], QUOTE, 3_600_200)
            .unwrap()
            .attest,
        Attest::HookFiltered
    );
}

#[test]
fn mod_heartbeat_just_after_prompt_counts_active() {
    for at in [101, 10_100] {
        let events = [prompt(), event(3, at, EventKind::ModHeartbeat, None)];
        assert!(mod_active_at(&events, 100));
        assert_eq!(
            current_turn_evidence("caller", &events, &[], QUOTE, 20_000)
                .unwrap_err()
                .reason,
            Reason::NoUserEvidence
        );
        assert_eq!(
            current_turn_evidence("caller", &events, &[composer(100)], QUOTE, 20_000)
                .unwrap()
                .attest,
            Attest::Composer
        );
    }
    let too_late = [prompt(), event(3, 10_101, EventKind::ModHeartbeat, None)];
    assert!(!mod_active_at(&too_late, 100));
    assert_eq!(
        current_turn_evidence("caller", &too_late, &[], QUOTE, 20_000)
            .unwrap()
            .attest,
        Attest::HookFiltered
    );
    // Bounds must also work near the clock's integer limits.
    assert!(mod_active_at(
        &[event(1, 0, EventKind::ModHeartbeat, None)],
        0
    ));
    assert!(mod_active_at(
        &[event(1, u64::MAX, EventKind::ModHeartbeat, None)],
        u64::MAX
    ));
}

#[test]
fn composer_after_stop_not_upgraded() {
    let events = [
        event(1, 90, EventKind::ModHeartbeat, None),
        prompt(),
        event(3, 101, EventKind::Stop, None),
        event(4, 110, EventKind::UserPrompt, Some(TEXT)),
    ];
    refused(&events, &[composer(100)], QUOTE);
    refused(&events[..3], &[composer(102)], QUOTE);
    assert!(current_turn_evidence("caller", &events, &[composer(102)], QUOTE, 1000).is_ok());
}

#[test]
fn composer_sha_mismatch_not_upgraded() {
    let events = [event(1, 90, EventKind::ModHeartbeat, None), prompt()];
    refused(
        &events,
        &[ComposerRecord {
            at: 100,
            sha256: sha256_hex("different"),
        }],
        QUOTE,
    );
    refused(&events, &[composer(1001)], QUOTE);
}

#[test]
fn composer_wins_over_exclusion_string() {
    let text = "[pager-poke] 가절월을 줄게";
    let events = [
        event(1, 90, EventKind::ModHeartbeat, None),
        event(2, 100, EventKind::ExcludedPrompt, Some(text)),
    ];
    let records = [ComposerRecord {
        at: 100,
        sha256: sha256_hex(text),
    }];
    assert_eq!(
        current_turn_evidence("caller", &events, &records, QUOTE, 1000)
            .unwrap()
            .attest,
        Attest::Composer
    );
    refused(&events, &[], QUOTE);
}

#[test]
fn composer_cannot_be_consumed_twice() {
    let events = [
        event(1, 90, EventKind::ModHeartbeat, None),
        prompt(),
        event(3, 110, EventKind::UserPrompt, Some(TEXT)),
    ];
    refused(&events, &[composer(100)], QUOTE);
    assert!(
        current_turn_evidence(
            "caller",
            &events,
            &[composer(100), composer(110)],
            QUOTE,
            1000
        )
        .is_ok()
    );
}

#[test]
fn hashing_and_whitespace_normalization() {
    assert_eq!(normalize(" \r\na\r\nb \n"), "a\nb");
    assert_eq!(
        sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let p = event(1, 100, EventKind::UserPrompt, Some("가절월을\n\t줄게"));
    assert!(current_turn_evidence("caller", &[p], &[], " 가절월을  줄게 ", 1000).is_ok());
    assert_eq!(
        sha256_hex(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn evidence_boundary_and_repeated_turns() {
    let six = event(1, 1000, EventKind::UserPrompt, Some("가나다라마바"));
    assert!(current_turn_evidence("caller", &[six], &[], "가나다라마바", 1000).is_ok());
    let events = [
        event(1, 90, EventKind::ModHeartbeat, None),
        prompt(),
        event(3, 103, EventKind::Stop, None),
        event(4, 108, EventKind::ModHeartbeat, None),
        event(5, 110, EventKind::UserPrompt, Some(TEXT)),
    ];
    refused(&events, &[composer(103)], QUOTE);
    assert!(current_turn_evidence("caller", &events, &[composer(105)], QUOTE, 1000).is_ok());
    assert!(current_turn_evidence("caller", &events, &[composer(1000)], QUOTE, 1000).is_ok());
}

#[test]
fn recorded_source_must_match_prompt() {
    use gaebu::evidence::recorded_source;
    let events = [
        event(
            1,
            10,
            EventKind::UserPrompt,
            Some("hook 어댑터를  nawi에게 맡겨"),
        ),
        event(2, 20, EventKind::Stop, None),
        event(
            3,
            30,
            EventKind::ExcludedPrompt,
            Some("[pager] bepo에게 전부 맡겨"),
        ),
        event(4, 40, EventKind::UserPrompt, Some(TEXT)),
    ];
    // An earlier, closed turn still counts; whitespace is normalized.
    let s = recorded_source("caller", &events, "hook 어댑터를 nawi에게").unwrap();
    assert_eq!((s.prompt_seq, s.at), (1, 10));
    assert_eq!(s.session_id, "caller");
    assert_eq!(s.sha256, events[0].sha256.clone().unwrap());
    // A peer message is not the user.
    let refused = |q: &str| recorded_source("caller", &events, q).unwrap_err().reason;
    assert_eq!(refused("bepo에게 전부 맡겨"), Reason::NoUserEvidence);
    assert_eq!(refused("nowhere in any prompt"), Reason::NoUserEvidence);
    assert_eq!(refused("nawi"), Reason::NoUserEvidence, "under 6 scalars");
}
