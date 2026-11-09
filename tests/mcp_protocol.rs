mod common;

use gaebu::pager::{HostStatus, Pager, WhoAmI};
use gaebu::{Reason, Result};
use serde_json::{Value, json};
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct FakePager;
impl Pager for FakePager {
    fn whoami(&self, _: Option<&str>) -> Result<WhoAmI> {
        Err(gaebu::Error::new(Reason::UnresolvedCaller, "no identity"))
    }
    fn host_status(&self, _: &str) -> Result<HostStatus> {
        Ok(HostStatus::Unknown)
    }
    fn send(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}

fn exchange(messages: &[Value]) -> Vec<Value> {
    let home = std::env::temp_dir().join(format!(
        "gaebu-mcp-{}-{}-{}",
        std::process::id(),
        gaebu::model::now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _cleanup = common::RemoveOnDrop(home.clone());
    let store = gaebu::store::Store::open(&home).unwrap();
    exchange_on(&store, &FakePager, messages)
}

fn exchange_on(store: &gaebu::store::Store, pager: &dyn Pager, messages: &[Value]) -> Vec<Value> {
    let input = messages
        .iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>();
    let mut out = Vec::new();
    gaebu::mcp::serve(&mut Cursor::new(input), &mut out, store, pager).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

#[test]
fn mcp_initialize_list_call_roundtrip() {
    let out = exchange(&[
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"me","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":"p","method":"ping"}),
    ]);
    assert_eq!(out.len(), 4);
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        out[1]["result"]["tools"].as_array().unwrap().len(),
        14 + gaebu::schedule::tools().len()
    );
    assert_eq!(out[2]["result"]["isError"], true);
    assert_eq!(out[3], json!({"jsonrpc":"2.0","id":"p","result":{}}));
}

#[test]
fn root_not_exposed_over_mcp() {
    let out = exchange(&[
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"appoint","arguments":{}}}),
    ]);
    let names: Vec<_> = out[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(!names.iter().any(|s| {
        [
            "appoint",
            "assign",
            "rebind",
            "cancel_team",
            "evidence",
            "hold",
            "release",
        ]
        .contains(s)
    }));
    assert_eq!(out[1]["result"]["isError"], true);
    assert!(
        out[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("invalid_argument")
    );
}

#[test]
fn mcp_invalid_requests_have_jsonrpc_errors() {
    let out = exchange(&[
        json!({"id":1,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":2,"method":"missing"}),
        json!({"jsonrpc":"2.0","id":{},"method":"ping"}),
        json!({"jsonrpc":"2.0","id":3,"method":"ping","params":1}),
    ]);
    assert_eq!(out[0]["error"]["code"], -32600);
    assert_eq!(out[1]["error"]["code"], -32601);
    assert_eq!(out[2]["error"]["code"], -32600);
    assert_eq!(out[2]["id"], Value::Null);
    assert_eq!(out[3]["error"]["code"], -32602);
}

#[test]
fn mcp_parse_error_then_next_line_recovers() {
    let home = std::env::temp_dir().join(format!(
        "gaebu-mcp-parse-{}-{}",
        std::process::id(),
        gaebu::model::now_ms()
    ));
    let _cleanup = common::RemoveOnDrop(home.clone());
    let store = gaebu::store::Store::open(&home).unwrap();
    let mut out = Vec::new();
    gaebu::mcp::serve(
        &mut Cursor::new("broken\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n"),
        &mut out,
        &store,
        &FakePager,
    )
    .unwrap();
    let values: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["error"]["code"], -32700);
    assert_eq!(values[1]["result"], json!({}));
}

#[test]
fn mcp_invalid_thread_metadata_is_not_ignored() {
    let out = exchange(&[
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"me","_meta":{"threadId":null}}}),
    ]);
    assert_eq!(out[0]["result"]["isError"], true);
    let refusal: Value =
        serde_json::from_str(out[0]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(refusal["reason"], "unresolved_caller");
    assert_eq!(refusal["message"], "invalid MCP _meta.threadId");
}

#[test]
fn mcp_me_duty_issue_accept_lease_and_cancel_flow() {
    use gaebu::model::{Tool, now_ms};
    use gaebu::ops::{self, SeedSpec};
    struct SessionPager;
    impl Pager for SessionPager {
        fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
            let id = session.unwrap_or("commander-session");
            Ok(WhoAmI {
                session_id: id.into(),
                via: "host".into(),
                alias: Some(
                    if id == "commander-session" {
                        "commander"
                    } else {
                        "worker"
                    }
                    .into(),
                ),
                tool: Some("claude".into()),
            })
        }
        fn host_status(&self, _: &str) -> Result<HostStatus> {
            Ok(HostStatus::Unknown)
        }
        fn send(&self, to: &str, _: &str) -> Result<()> {
            assert!(["worker-session", "commander-session"].contains(&to));
            Ok(())
        }
    }
    let home = std::env::temp_dir().join(format!(
        "gaebu-mcp-flow-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let _cleanup = common::RemoveOnDrop(home.clone());
    let store = gaebu::store::Store::open(&home).unwrap();
    store
        .transact(std::time::Duration::from_secs(1), |st| {
            ops::dev_seed(
                st,
                now_ms(),
                &SeedSpec {
                    team: "mcp-flow".into(),
                    project: "fixture".into(),
                    commander_alias: "commander".into(),
                    commander_session: Some("commander-session".into()),
                    commander_tool: Some(Tool::Claude),
                    members: vec![(
                        "worker".into(),
                        Some("worker-session".into()),
                        Some(Tool::Codex),
                    )],
                },
            )
        })
        .unwrap();
    store
        .session("worker-session")
        .update_sighting(|s| {
            s.tool = Some(Tool::Codex);
            s.last_seen = Some(now_ms());
        })
        .unwrap();
    let state = store.read().unwrap();
    let worker = state
        .members
        .values()
        .find(|m| m.binding.alias == "worker")
        .unwrap()
        .id
        .clone();
    let reply = exchange_on(
        &store,
        &SessionPager,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"me","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"duty_issue","arguments":{"assignee":worker,"spec":{"intent":"verify MCP","outcome":"working adapter","scope":["src/**"],"decision_rights":"internal implementation","escalate_on":[],"done_criteria":"tests pass","plan_ref":null,"exec_root":null}}}}),
        ],
    );
    assert_eq!(reply[2]["result"]["isError"], false);
    assert_eq!(reply[3]["result"]["isError"], false, "{}", reply[3]);
    let result: Value =
        serde_json::from_str(reply[3]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let duty = result["ids"][0].as_str().unwrap();
    let detail = exchange_on(
        &store,
        &SessionPager,
        &[
            json!({"jsonrpc":"2.0","id":"detail","method":"tools/call","params":{"name":"duty_get","arguments":{"duty":duty}}}),
        ],
    );
    assert_eq!(detail[0]["result"]["isError"], false, "{}", detail[0]);
    let brief: Value =
        serde_json::from_str(detail[0]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(brief["intent"], "verify MCP");
    assert_eq!(brief["version"], 1);
    assert_eq!(brief["escalate_on"], json!([]));
    let caller = gaebu::identity::Caller {
        session_id: "commander-session".into(),
        tool: Some(Tool::Claude),
        source: gaebu::identity::Source::PagerHost,
    };
    gaebu::schedule::register(&store, &caller, &home.join("schedule.md"), true).unwrap();
    let root_bu = store
        .read()
        .unwrap()
        .teams
        .values()
        .next()
        .unwrap()
        .root_bu
        .clone();
    let mapped = exchange_on(
        &store,
        &SessionPager,
        &[
            json!({"jsonrpc":"2.0","id":"mapped","method":"tools/call","params":{"name":"schedule_add","arguments":{"title":"mapped work","scope":["src/**"],"owner_bu":root_bu,"priority":0,"pm_ref":{"task":"FLOW","item":"flow-item","plan":".agents/plans/flow.md"}}}}),
        ],
    );
    assert_eq!(mapped[0]["result"]["isError"], false, "{}", mapped[0]);
    let mapped: Value =
        serde_json::from_str(mapped[0]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(mapped["item"]["pm_ref"]["item"], "flow-item");
    let reply = exchange_on(
        &store,
        &SessionPager,
        &[
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"duty_accept","_meta":{"threadId":"worker-session"},"arguments":{"duty":duty,"version":1}}}),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"lease_acquire","_meta":{"threadId":"worker-session"},"arguments":{"key":"src/mcp.rs","mode":"exclusive","ttl_ms":60000,"reason":"implement MCP"}}}),
        ],
    );
    assert_eq!(reply[0]["result"]["isError"], false, "{}", reply[0]);
    assert_eq!(reply[1]["result"]["isError"], false, "{}", reply[1]);
    let st = store.read().unwrap();
    assert_eq!(st.duties[duty].status, gaebu::model::DutyStatus::Accepted);
    assert!(
        st.leases
            .values()
            .any(|l| l.key == "src/mcp.rs" && l.holder == worker)
    );
    // Cancellation must be rechecked in the next MCP mutation transaction.
    store
        .transact(std::time::Duration::from_secs(1), |st| {
            for t in st.teams.values_mut() {
                t.status = gaebu::model::TeamStatus::Cancelled;
            }
            Ok(())
        })
        .unwrap();
    let reply = exchange_on(
        &store,
        &SessionPager,
        &[
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"lease_acquire","_meta":{"threadId":"worker-session"},"arguments":{"key":"src/hook.rs"}}}),
        ],
    );
    assert_eq!(reply[0]["result"]["isError"], true);
    let refusal: Value =
        serde_json::from_str(reply[0]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(refusal["reason"], "cancelled");
}

#[test]
fn escalation_withdraw_tool() {
    use gaebu::model::{EscalationStatus, Tool, now_ms};
    use gaebu::ops::{self, SeedSpec};
    struct TwoSessions;
    impl Pager for TwoSessions {
        fn whoami(&self, session: Option<&str>) -> Result<WhoAmI> {
            let id = session.unwrap_or("boss-session");
            Ok(WhoAmI {
                session_id: id.into(),
                via: "host".into(),
                alias: Some(if id == "boss-session" { "boss" } else { "aide" }.into()),
                tool: Some("claude".into()),
            })
        }
        fn host_status(&self, _: &str) -> Result<HostStatus> {
            Ok(HostStatus::Unknown)
        }
        fn send(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }
    let home = std::env::temp_dir().join(format!(
        "gaebu-mcp-withdraw-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let _cleanup = common::RemoveOnDrop(home.clone());
    let store = gaebu::store::Store::open(&home).unwrap();
    store
        .transact(std::time::Duration::from_secs(1), |st| {
            ops::dev_seed(
                st,
                now_ms(),
                &SeedSpec {
                    team: "withdraw".into(),
                    project: "fixture".into(),
                    commander_alias: "boss".into(),
                    commander_session: Some("boss-session".into()),
                    commander_tool: Some(Tool::Claude),
                    members: vec![(
                        "aide".into(),
                        Some("aide-session".into()),
                        Some(Tool::Claude),
                    )],
                },
            )
        })
        .unwrap();
    for session in ["aide-session", "boss-session"] {
        store
            .session(session)
            .update_sighting(|s| {
                s.tool = Some(Tool::Codex);
                s.last_seen = Some(now_ms());
            })
            .unwrap();
    }
    let call = |id: u64, thread: &str, name: &str, args: Value| {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"_meta":{"threadId":thread},"arguments":args}})
    };
    let text = |v: &Value| {
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let reply = exchange_on(
        &store,
        &TwoSessions,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            call(
                2,
                "aide-session",
                "escalate",
                json!({"question":"may I  rename it?"}),
            ),
            call(
                3,
                "aide-session",
                "escalate",
                json!({"question":"may I rename it?"}),
            ),
        ],
    );
    let names: Vec<_> = reply[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 14 + gaebu::schedule::tools().len());
    assert!(names.iter().any(|n| n == "duty_get"));
    assert!(names.iter().any(|n| n == "bu_form"));
    assert!(names.iter().any(|n| n == "escalation_withdraw"));
    assert!(!names.iter().any(|n| n == "hold" || n == "release"));
    assert_eq!(reply[1]["result"]["isError"], false, "{}", reply[1]);
    assert!(
        text(&reply[2]).contains("escalation reused"),
        "{}",
        reply[2]
    );
    let esc = store
        .read()
        .unwrap()
        .escalations
        .keys()
        .next()
        .unwrap()
        .clone();
    let reply = exchange_on(
        &store,
        &TwoSessions,
        &[
            call(
                4,
                "boss-session",
                "escalation_withdraw",
                json!({"escalation":esc}),
            ),
            call(
                5,
                "aide-session",
                "escalation_withdraw",
                json!({"escalation":esc,"note":"solved"}),
            ),
            call(
                6,
                "aide-session",
                "escalation_withdraw",
                json!({"escalation":esc}),
            ),
        ],
    );
    assert_eq!(
        reply[0]["result"]["isError"], true,
        "the recipient cannot withdraw"
    );
    assert!(text(&reply[0]).contains("no_grant"));
    assert_eq!(reply[1]["result"]["isError"], false, "{}", reply[1]);
    assert_eq!(reply[2]["result"]["isError"], true);
    assert!(text(&reply[2]).contains("conflict"));
    assert_eq!(
        store.read().unwrap().escalations[&esc].status,
        EscalationStatus::Withdrawn
    );
}
