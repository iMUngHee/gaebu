use gaebu::identity::{self, Caller, Source};
use gaebu::model::{State, Tool};
use gaebu::pager::{HostStatus, Pager, WhoAmI, parse_who, parse_whoami};
use gaebu::{Reason, Result};

struct FakePager(&'static str);
impl Pager for FakePager {
    fn whoami(&self, _: Option<&str>) -> Result<WhoAmI> {
        parse_whoami(self.0)
    }
    fn host_status(&self, _: &str) -> Result<HostStatus> {
        Ok(HostStatus::Unknown)
    }
    fn send(&self, _: &str, _: &str) -> Result<()> {
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

#[test]
fn pager_roster_handles_roots_with_spaces_and_unknown_hosts() {
    let rows = parse_who(
        "NAME TOOL ROOT HOST LAST\nbepo codex C:\\a b\\c unknown just now\ngapu claude /tmp live 2m ago\nold codex /tmp gone 1h ago\n",
    );
    assert_eq!(
        rows,
        vec![
            ("bepo".into(), HostStatus::Unknown),
            ("gapu".into(), HostStatus::Live),
            ("old".into(), HostStatus::Gone)
        ]
    );
}

#[test]
fn cli_flag_identity_rejected_for_mutation() {
    for source in ["flag", "env PAGER_SESSION", "future"] {
        let caller = Caller {
            session_id: "s".into(),
            tool: None,
            source: Source::Other(source.into()),
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
        fn host_status(&self, _: &str) -> Result<HostStatus> {
            Ok(HostStatus::Unknown)
        }
        fn send(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }
    let error = identity::from_pager(&Unavailable).unwrap_err();
    assert_eq!(error.reason, Reason::UnresolvedCaller);
    assert!(error.message.contains("transport failed"));
}
