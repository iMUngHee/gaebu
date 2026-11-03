use gaebu::titles::{COMMANDER, HEAD, STAFF, Seat, bu_name, pick};

#[test]
fn default_titles_unique_per_team() {
    let mut used = Vec::new();
    for (seat, pool) in [
        (Seat::Commander, COMMANDER),
        (Seat::Head, HEAD),
        (Seat::Staff, STAFF),
    ] {
        for expected in pool {
            assert_eq!(
                &pick(seat, &used.iter().map(String::as_str).collect::<Vec<_>>()),
                expected
            );
            used.push(expected.to_string());
        }
        let base = pool.last().unwrap();
        for n in 2..=4 {
            let title = pick(seat, &used.iter().map(String::as_str).collect::<Vec<_>>());
            assert_eq!(title, format!("{base}{n}"));
            assert!(!used.contains(&title));
            used.push(title);
        }
    }
    assert_eq!(pick(Seat::Staff, &["장사", "사마", "주부2"]), "주부");
}

#[test]
fn bu_default_name_from_head_title() {
    assert_eq!(bu_name("좌장군"), "좌장군부");
    assert_eq!(bu_name("승상"), "승상부");
}
