use gaebu::titles::{
    COMMANDER, HEAD, LIAISON, ROOT_STAFF, SUB_STAFF, Seat, bu_name, follows_seat, pick,
};

const SEATS: [(Seat, &[&str]); 4] = [
    (Seat::Commander, COMMANDER),
    (Seat::Head, HEAD),
    (Seat::RootStaff, ROOT_STAFF),
    (Seat::SubStaff, SUB_STAFF),
];

#[test]
fn pools_match_spec() {
    assert_eq!(COMMANDER, ["대장군", "표기장군", "거기장군", "위장군"]);
    assert_eq!(
        HEAD,
        [
            "정동장군",
            "정서장군",
            "정남장군",
            "정북장군",
            "진동장군",
            "진서장군",
            "진남장군",
            "진북장군",
            "전장군",
            "후장군",
            "좌장군",
            "우장군",
        ]
    );
    assert_eq!(
        ROOT_STAFF,
        ["장사", "사마", "종사중랑", "주부", "기실독", "참군"]
    );
    assert_eq!(
        SUB_STAFF,
        ["장사", "사마", "주부", "공조사", "문하독", "녹사"]
    );
    assert_eq!(LIAISON, "황문시랑");
}

#[test]
fn pick_is_deterministic() {
    for (seat, pool) in SEATS {
        let first = pick(seat, &[], "t-1:m-1");
        assert_eq!(pick(seat, &[], "t-1:m-1"), first);
        assert!(pool.contains(&first.as_str()));
        // A taken title is never drawn, whatever the key.
        for n in 0..32 {
            let title = pick(seat, &[first.as_str()], &format!("t-{n}:m-{n}"));
            assert_ne!(title, first);
            assert!(pool.contains(&title.as_str()));
        }
    }
    // Teams do not all get the same commander title.
    let commanders: std::collections::BTreeSet<String> = (0..16)
        .map(|n| pick(Seat::Commander, &[], &format!("t-{n}:m-{n}")))
        .collect();
    assert!(commanders.len() >= 2, "{commanders:?}");
}

#[test]
fn exhausted_pool_gets_suffix() {
    // Exhausting a display-title pool must still return promptly. Run the
    // allocation checks off-thread so an accidental endless suffix search is
    // a test failure, rather than hanging the entire verification process.
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for (seat, pool) in SEATS {
            let mut used: Vec<String> = Vec::new();
            // Every pool title is handed out once before any suffix appears.
            for n in 0..pool.len() {
                let taken: Vec<&str> = used.iter().map(String::as_str).collect();
                let title = pick(seat, &taken, &format!("t:m-{n}"));
                assert!(pool.contains(&title.as_str()), "{title}");
                assert!(!used.contains(&title));
                used.push(title);
            }
            for n in 0..6 {
                let taken: Vec<&str> = used.iter().map(String::as_str).collect();
                let title = pick(seat, &taken, &format!("t:extra-{n}"));
                assert!(!used.contains(&title));
                let base = title.trim_end_matches(|c: char| c.is_ascii_digit());
                assert!(pool.contains(&base), "{title}");
                assert_ne!(base, title, "a full pool yields a numbered title");
                used.push(title);
            }
        }
        send.send(()).unwrap();
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("title allocation must terminate");
}

#[test]
fn only_pool_titles_follow_the_seat() {
    for title in ["대장군", "좌장군", "종사중랑", "녹사", "녹사2", "황문시랑"] {
        assert!(follows_seat(title), "{title}");
    }
    // Titles from before the pools changed, and titles a person chose.
    for title in ["승상", "연", "기실", "공조", "안국장군", "책사", "2"] {
        assert!(!follows_seat(title), "{title}");
    }
}

#[test]
fn bu_default_name_from_head_title() {
    assert_eq!(bu_name("좌장군"), "좌장군부");
    assert_eq!(bu_name("대장군"), "대장군부");
}
