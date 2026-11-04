//! Replays hook payloads captured from an installed Claude Code (2.1.302)
//! through `gaebu hook`. Session ids and paths are redacted in the fixtures.

mod common;

use gaebu::error::Result;
use gaebu::model::Tool;
use gaebu::pager::{HostStatus, Pager, WhoAmI};
use gaebu::store::{EventKind, Store};

struct NoPager;

impl Pager for NoPager {
    fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
        Ok(WhoAmI {
            session_id: session.unwrap_or("captured-claude-session").into(),
            via: "host".into(),
            alias: None,
            tool: Some("claude".into()),
        })
    }
    fn host_status(&self, _alias: &str) -> Result<HostStatus> {
        Ok(HostStatus::Unknown)
    }
    fn send(&self, _to: &str, _body: &str) -> Result<()> {
        Ok(())
    }
}

fn home() -> common::RemoveOnDrop {
    let p = std::env::temp_dir().join(format!(
        "gaebu-captured-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    common::RemoveOnDrop(p)
}

fn replay(store: &Store, fixture: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/hooks/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    );
    let input = std::fs::read(path).unwrap();
    let mut stdin = input.as_slice();
    let mut out = Vec::new();
    gaebu::hook::run(Tool::Claude, &mut stdin, &mut out, store, &NoPager);
    String::from_utf8(out).unwrap()
}

#[test]
fn captured_claude_payloads_replay_cleanly() {
    let dir = home();
    let store = Store::open(&dir.0).unwrap();

    // A non-member session: SessionStart and Stop print nothing.
    assert_eq!(
        replay(&store, "captured-claude-sessionstart.json").trim(),
        ""
    );
    let prompt_out = replay(&store, "captured-claude-userpromptsubmit.json");
    assert!(
        prompt_out.trim().is_empty() || prompt_out.contains("hookSpecificOutput"),
        "{prompt_out}"
    );
    assert_eq!(replay(&store, "captured-claude-stop.json").trim(), "");

    // The real prompt was recorded as a user prompt, then the turn closed.
    let events = store.session("captured-claude-session").events().unwrap();
    let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EventKind::UserPrompt), "{kinds:?}");
    assert_eq!(kinds.last(), Some(&EventKind::Stop), "{kinds:?}");
    let prompt = events
        .iter()
        .find(|e| e.kind == EventKind::UserPrompt)
        .unwrap();
    assert_eq!(
        prompt.text.as_deref(),
        Some("Reply with the single word OK.")
    );
}
