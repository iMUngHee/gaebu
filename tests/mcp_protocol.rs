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
    let store = gaebu::store::Store::open(&home).unwrap();
    let input = messages
        .iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>();
    let mut out = Vec::new();
    gaebu::mcp::serve(&mut Cursor::new(input), &mut out, &store, &FakePager).unwrap();
    let result = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    std::fs::remove_dir_all(&home).unwrap();
    result
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
    assert_eq!(out[1]["result"]["tools"].as_array().unwrap().len(), 11);
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
    assert!(
        !names
            .iter()
            .any(|s| ["appoint", "assign", "rebind", "cancel_team", "evidence"].contains(s))
    );
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
    ]);
    assert_eq!(out[0]["error"]["code"], -32600);
    assert_eq!(out[1]["error"]["code"], -32601);
}
