mod common;

use gaebu::identity::{self, Caller, Source};
use gaebu::model::{State, Tool};
use gaebu::pager::{HostStatus, Pager, WhoAmI, parse_who, parse_whoami};
use gaebu::{Reason, Result};

struct FakePager(&'static str);
impl Pager for FakePager {
    fn whoami(&self, _: Option<&str>) -> Result<WhoAmI> {
        parse_whoami(self.0)
    }
    fn hosts(&self) -> Result<gaebu::pager::Hosts> {
        Ok(gaebu::pager::Hosts::all(HostStatus::Unknown))
    }
    fn send(&self, _: &gaebu::pager::Message<'_>) -> Result<()> {
        Ok(())
    }
}

#[test]
fn pager_parses_host_native_padding_and_unattributed() {
    for (via, tool) in [("host", Tool::Claude), ("native", Tool::Codex)] {
        let text = format!("host: codex pid=12\r\nsession: s-123 (via {via})\r\nname:    bepo\r\n");
        let parsed = parse_whoami(&text).unwrap();
        assert_eq!(parsed.session_id, "s-123");
        assert_eq!(parsed.alias.as_deref(), Some("bepo"));
        let caller = identity::from_pager(&FakePager(if via == "host" {
            "session: c (via host)\nname: gapu"
        } else {
            "session: c (via native)\nname: bepo"
        }))
        .unwrap();
        assert_eq!(caller.tool, Some(tool));
        assert!(caller.may_mutate());
    }
    for bad in [
        "session: unattributed",
        "",
        "session: s",
        "session: s (via )",
    ] {
        assert_eq!(
            parse_whoami(bad).unwrap_err().reason,
            Reason::UnresolvedCaller
        );
    }
}

/// Lay rows out as pager does: cells padded to the widest in their column,
/// counted in characters, two spaces apart.
fn roster(rows: &[[&str; 6]]) -> String {
    let header = ["NAME", "TOOL", "ROOT", "HOST", "LAST", "THREAD"];
    let all: Vec<[&str; 6]> = std::iter::once(header)
        .chain(rows.iter().copied())
        .collect();
    let width = |column: usize| all.iter().map(|r| r[column].chars().count()).max().unwrap();
    all.iter()
        .map(|row| {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                line.push_str(cell);
                if column < 5 {
                    let pad = width(column) - cell.chars().count() + 2;
                    line.push_str(&" ".repeat(pad));
                }
            }
            line + "\n"
        })
        .collect()
}

#[test]
fn parse_who_reads_host_column() {
    // THREAD is `unknown` for every Claude session and says nothing about
    // the host; ROOT may hold spaces, be long, or not be ASCII.
    let text = roster(&[
        ["gapu", "claude", "/tmp", "live", "2m ago", "unknown"],
        ["old", "claude", "/tmp", "gone", "1h ago", "unknown"],
        [
            "bepo",
            "codex",
            "C:\\a b\\c",
            "unknown",
            "just now",
            "active",
        ],
        [
            "hangul",
            "claude",
            "/home/웅희/개부 작업",
            "gone",
            "3d ago",
            "unknown",
        ],
        [
            "deep",
            "codex",
            "/home/user/projects/a-long-project-name/.agents/worktrees/a-long-branch",
            "live",
            "1m ago",
            "idle",
        ],
    ]) + "104 older session(s) have unknown presence; use pager who --all or an exact alias.\n";
    assert_eq!(
        parse_who(&text),
        vec![
            ("gapu".into(), HostStatus::Live),
            ("old".into(), HostStatus::Gone),
            ("bepo".into(), HostStatus::Unknown),
            ("hangul".into(), HostStatus::Gone),
            ("deep".into(), HostStatus::Live),
        ]
    );
}

#[test]
fn parse_who_reads_pager_output_verbatim() {
    let text = "NAME  TOOL    ROOT                 HOST     LAST      THREAD\n\
                gapu  claude  /home/user/gaebu     live     just now  unknown\n\
                bepo  codex   /home/user/player    unknown  3m ago    active\n\
                nuzu  claude  /home/user/player    gone     10m ago   unknown\n";
    let expected = vec![
        ("gapu".to_owned(), HostStatus::Live),
        ("bepo".to_owned(), HostStatus::Unknown),
        ("nuzu".to_owned(), HostStatus::Gone),
    ];
    assert_eq!(parse_who(text), expected);
    assert!(parse_who("no recent or confirmed available sessions\n").is_empty());

    // A roster from before THREAD existed, and one with a column after it.
    let older = "NAME  TOOL    ROOT               HOST     LAST\n\
                 gapu  claude  /home/user/gaebu   live     just now\n\
                 bepo  codex   /home/user/player  unknown  3m ago\n\
                 nuzu  claude  /home/user/player  gone     10m ago\n";
    assert_eq!(parse_who(older), expected);
    let wider = "NAME  TOOL    ROOT               HOST     LAST      THREAD   PURPOSE\n\
                 gapu  claude  /home/user/gaebu   live     just now  unknown  gone fishing\n\
                 bepo  codex   /home/user/player  unknown  3m ago    active   live demo\n\
                 nuzu  claude  /home/user/player  gone     10m ago   unknown  \n";
    assert_eq!(parse_who(wider), expected);
}

/// The real adapter, run against a script that records how it was called.
#[cfg(unix)]
#[test]
fn cli_pager_runs_pager_with_the_documented_arguments() {
    use gaebu::model::NoticeKind;
    use gaebu::pager::{CliPager, Message};
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("gaebu-pager-argv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _cleanup = common::RemoveOnDrop(dir.clone());
    let (script, calls) = (dir.join("pager"), dir.join("calls"));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\"; done >> '{}'\n\
             printf 'NAME  TOOL    ROOT  HOST  LAST    THREAD\\nold   claude  /tmp  gone  3d ago  unknown\\n'\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pager = CliPager {
        timeout: std::time::Duration::from_secs(5),
        program: script,
    };

    // The whole history is asked for, so a session long gone is still listed.
    assert_eq!(pager.hosts().unwrap().status("old"), HostStatus::Gone);
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "who\n--all\n");

    std::fs::remove_file(&calls).unwrap();
    pager
        .send(&Message {
            target: "nawi",
            body: "--human is only text here",
            event: "gaebu-r-n-1-release",
            kind: NoticeKind::Release,
            automation: None,
        })
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        "send\n--event\ngaebu-r-n-1-release\n--kind\nrelease\n--\nnawi\n--human is only text here\n"
    );
}

#[test]
fn cli_flag_identity_rejected_for_mutation() {
    for source in ["flag", "env PAGER_SESSION", "future"] {
        let caller = Caller {
            session_id: "s".into(),
            tool: None,
            source: Source::Other(source.into()),
            alias: None,
        };
        assert!(!caller.may_mutate());
        assert_eq!(
            identity::actor(&State::default(), &caller)
                .unwrap_err()
                .reason,
            Reason::UnresolvedCaller
        );
    }
    let caller = identity::from_pager(&FakePager("session: s (via flag)\nname: bepo")).unwrap();
    assert!(!caller.may_mutate());
}

#[test]
fn caller_transport_failure_is_unresolved() {
    struct Unavailable;
    impl Pager for Unavailable {
        fn whoami(&self, _: Option<&str>) -> Result<WhoAmI> {
            Err(gaebu::Error::new(
                Reason::PagerUnavailable,
                "transport failed",
            ))
        }
        fn hosts(&self) -> Result<gaebu::pager::Hosts> {
            Ok(gaebu::pager::Hosts::all(HostStatus::Unknown))
        }
        fn send(&self, _: &gaebu::pager::Message<'_>) -> Result<()> {
            Ok(())
        }
    }
    let error = identity::from_pager(&Unavailable).unwrap_err();
    assert_eq!(error.reason, Reason::UnresolvedCaller);
    assert!(error.message.contains("transport failed"));
}
