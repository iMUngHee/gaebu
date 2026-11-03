use gaebu::error::Reason;
use gaebu::scope::{normalize_list, parse, within};

#[test]
fn scope_grammar_rejects_dotdot_backslash_drive_wildcards() {
    for raw in [
        "", "/src", "src/", "src//a", ".", "..", "src/../a", "src/./a", "src\\a", "C:/a", "a*",
        "a?", "a[b]", "a{b}", "**/a", "a/**/b", "res:", "res:a/b", "res:a:b", "res:..", "res:a*",
    ] {
        assert_eq!(
            parse(raw).unwrap_err().reason,
            Reason::InvalidScope,
            "{raw}"
        );
    }
}

#[test]
fn scope_containment_cases() {
    let cases = [
        ("**", "src/a", true),
        ("**", "**", true),
        ("src/**", "src", true),
        ("src/**", "src/a", true),
        ("src/**", "src/nested/**", true),
        ("src/**", "src/**", true),
        ("src/**", "src2/a", false),
        ("src/**", "**", false),
        ("src", "src", true),
        ("src", "src/a", false),
        ("src", "src/**", false),
        ("res:**", "res:gpu", true),
        ("res:gpu", "res:gpu", true),
        ("res:gpu", "res:cpu", false),
        ("res:gpu", "res:**", false),
        ("**", "res:gpu", false),
        ("res:**", "src/a", false),
        ("res:**", "res:**", true),
    ];
    for (a, b, expected) in cases {
        assert_eq!(
            parse(a).unwrap().contains(&parse(b).unwrap()),
            expected,
            "{a} contains {b}"
        );
    }
}

#[test]
fn scope_case_folding() {
    for (raw, expected) in [
        ("SRC/Ä.rs", "src/ä.rs"),
        ("SRC/**", "src/**"),
        ("res:GPU", "res:gpu"),
        ("**", "**"),
        ("res:**", "res:**"),
    ] {
        assert_eq!(parse(raw).unwrap().as_string(), expected);
    }
    assert!(
        parse("SRC/**")
            .unwrap()
            .contains(&parse("src/main.rs").unwrap())
    );
}

#[test]
fn scope_overlap_and_lists() {
    for (a, b, overlap) in [
        ("a/**", "a/b/**", true),
        ("a/b", "a/**", true),
        ("a/b", "a/b", true),
        ("a/**", "ab/**", false),
        ("res:**", "res:gpu", true),
        ("**", "res:**", false),
        ("a", "b", false),
    ] {
        let a = parse(a).unwrap();
        let b = parse(b).unwrap();
        assert_eq!(a.overlaps(&b), overlap);
        assert_eq!(b.overlaps(&a), overlap);
    }
    let strings = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        normalize_list(&strings(&["SRC/**", "res:GPU"])).unwrap(),
        strings(&["src/**", "res:gpu"])
    );
    assert!(normalize_list(&strings(&["a", "../b"])).is_err());
    assert!(
        within(
            &strings(&["src/a", "res:gpu"]),
            &strings(&["src/**", "res:**"])
        )
        .unwrap()
    );
    assert!(!within(&strings(&["src/a", "elsewhere"]), &strings(&["src/**"])).unwrap());
    assert!(within(&[], &[]).unwrap());
    assert!(!within(&strings(&["src"]), &[]).unwrap());
    assert!(within(&strings(&["x"]), &strings(&["**", "../invalid"])).is_err());
    assert!(within(&strings(&["bad?", "x"]), &strings(&["**"])).is_err());
}
