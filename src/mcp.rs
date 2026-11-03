//! stdio MCP server (Decision 16): JSON-RPC 2.0, protocol 2025-06-18.
//! stdout carries JSON-RPC only; logs go to stderr. Root directives are not
//! exposed here.

use crate::pager::Pager;
use crate::store::Store;
use crate::{Error, Reason, Result, card, identity, model, ops};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn serve(
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
    store: &Store,
    pager: &dyn Pager,
) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                writeln!(
                    stdout,
                    "{}",
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
                )?;
                stdout.flush()?;
                continue;
            }
        };
        let raw_id = request.get("id").cloned().unwrap_or(Value::Null);
        let valid_id = raw_id.is_null() || raw_id.is_string() || raw_id.is_number();
        let id = if valid_id { raw_id } else { Value::Null };
        let response = if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !request["method"].is_string()
            || !valid_id
        {
            json!({"error":{"code":-32600,"message":"Invalid Request"}})
        } else if request.get("params").is_some_and(|p| !p.is_object()) {
            if request.get("id").is_none() {
                continue;
            }
            json!({"error":{"code":-32602,"message":"Parameters must be an object"}})
        } else {
            let method = request["method"].as_str().unwrap_or("");
            if request.get("id").is_none() {
                continue;
            }
            match method {
                "initialize" => {
                    json!({"result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"gaebu","version":env!("CARGO_PKG_VERSION")}}})
                }
                "ping" => json!({"result":{}}),
                "tools/list" => json!({"result":{"tools":tools()}}),
                "tools/call" => {
                    let params = &request["params"];
                    let name = params["name"].as_str().unwrap_or("");
                    let args = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let thread = params.pointer("/_meta/threadId").and_then(Value::as_str);
                    let malformed_meta = params.get("_meta").is_some_and(|meta| {
                        !meta.is_object() || meta.get("threadId").is_some_and(|id| !id.is_string())
                    });
                    let result = if malformed_meta {
                        Err(Error::new(
                            Reason::UnresolvedCaller,
                            "invalid MCP _meta.threadId",
                        ))
                    } else {
                        call(name, &args, thread, store, pager)
                    };
                    match result {
                        Ok(value) => {
                            json!({"result":{"content":[{"type":"text","text":value.to_string()}],"isError":false}})
                        }
                        Err(error) => {
                            json!({"result":{"content":[{"type":"text","text":json!({"reason":error.reason,"message":error.message}).to_string()}],"isError":true}})
                        }
                    }
                }
                _ => json!({"error":{"code":-32601,"message":"Method not found"}}),
            }
        };
        let mut response = response;
        response["jsonrpc"] = json!("2.0");
        response["id"] = id;
        serde_json::to_writer(&mut *stdout, &response)?;
        writeln!(stdout)?;
        stdout.flush()?;
    }
}

fn tools() -> Vec<Value> {
    let string = json!({"type":"string"});
    let strings = json!({"type":"array","items":{"type":"string"}});
    let version = json!({"type":"integer","minimum":1});
    [
        ("me", "Read your current position card", json!({}), vec![]),
        ("board", "Read your team's command tree", json!({}), vec![]),
        ("duty_issue", "Issue an outcome brief to a subordinate", json!({"assignee":string,"spec":{"type":"object","properties":{"intent":string,"outcome":string,"scope":strings,"decision_rights":string,"escalate_on":strings,"done_criteria":string,"plan_ref":{"type":["string","null"]},"exec_root":{"type":["string","null"]}},"required":["intent","outcome","scope","decision_rights","escalate_on","done_criteria"],"additionalProperties":false}}), vec!["assignee","spec"]),
        ("duty_accept", "Accept your duty", json!({"duty":string,"version":version}), vec!["duty"]),
        ("duty_decline", "Decline your duty with a reason", json!({"duty":string,"version":version,"note":string}), vec!["duty","note"]),
        ("duty_done", "Declare your duty complete", json!({"duty":string,"note":string}), vec!["duty"]),
        ("lease_acquire", "Claim a resource inside your accepted duty scope", json!({"key":string,"mode":{"type":"string","enum":["exclusive","shared"]},"ttl_ms":{"type":"integer","minimum":1},"reason":string}), vec!["key"]),
        ("lease_release", "Release your resource claim", json!({"lease":string}), vec!["lease"]),
        ("escalate", "Ask your superior to resolve a decision", json!({"duty":string,"question":string}), vec!["question"]),
        ("escalation_answer", "Answer an escalation addressed to you", json!({"escalation":string,"answer":string}), vec!["escalation","answer"]),
        ("record_add", "Record a team decision, change or handoff", json!({"kind":{"type":"string","enum":["decision","handoff","change","note"]},"refs":strings,"text":string}), vec!["text"]),
    ].into_iter().map(|(name, description, properties, required)| json!({
        "name":name,"description":description,
        "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}
    })).collect()
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            Error::new(
                Reason::InvalidArgument,
                format!("{key} must be a nonempty string"),
            )
        })
}

fn call(
    name: &str,
    args: &Value,
    thread: Option<&str>,
    store: &Store,
    pager: &dyn Pager,
) -> Result<Value> {
    let schema = tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| Error::new(Reason::InvalidArgument, "unknown MCP tool"))?;
    let object = args
        .as_object()
        .ok_or_else(|| Error::new(Reason::InvalidArgument, "arguments must be an object"))?;
    for (key, value) in object {
        let property = &schema["inputSchema"]["properties"][key];
        let valid = match property["type"].as_str() {
            Some("string") => value.is_string(),
            Some("integer") => value.as_u64().is_some_and(|n| n > 0),
            Some("object") => value.is_object(),
            Some("array") => value
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string)),
            _ => false,
        };
        if !valid {
            return Err(Error::new(
                Reason::InvalidArgument,
                format!("invalid argument {key}"),
            ));
        }
    }
    for key in schema["inputSchema"]["required"]
        .as_array()
        .into_iter()
        .flatten()
    {
        required(args, key.as_str().unwrap_or("")).or_else(|e| {
            if key == "spec" && args["spec"].is_object() {
                Ok("")
            } else {
                Err(e)
            }
        })?;
    }
    let caller = identity::for_mcp(pager, store, thread)?;
    if let Err(e) = identity::refresh_bindings(store, pager) {
        if name != "me" && name != "board" {
            return Err(Error::new(
                Reason::Busy,
                format!("binding refresh failed: {e}"),
            ));
        }
        eprintln!("gaebu: MCP binding refresh: {e}");
    }
    let now = model::now_ms();
    if name == "me" || name == "board" {
        let st = store.read()?;
        let member = st
            .members
            .values()
            .find(|m| {
                m.status == model::MemberStatus::Active
                    && m.binding.state == model::BindingState::Bound
                    && m.binding.session_id.as_deref() == Some(&caller.session_id)
            })
            .ok_or_else(|| Error::new(Reason::NotMember, "session has no member"))?;
        if name == "me" {
            let seen = store.session(&caller.session_id).sighting()?.last_seen;
            let card = card::build(&st, &member.id, now, seen)
                .ok_or_else(|| Error::new(Reason::NotMember, "no card"))?;
            return serde_json::to_value(card)
                .map_err(|e| Error::new(Reason::InvalidArgument, e.to_string()));
        }
        return Ok(json!({"rev":st.rev,"team":st.teams.get(&member.team),
            "bus":st.bus.values().filter(|b| b.team == member.team).collect::<Vec<_>>(),
            "members":st.members.values().filter(|m| m.team == member.team).collect::<Vec<_>>(),
            "duties":st.duties.values().filter(|d| d.team == member.team).collect::<Vec<_>>() }));
    }
    let outcome = store.transact(crate::store::CLI_DEADLINE, |st| {
        ops::expire_leases(st, now);
        let actor = identity::actor(st, &caller)?;
        match name {
            "duty_issue" => {
                let spec = serde_json::from_value(args["spec"].clone())
                    .map_err(|e| Error::new(Reason::InvalidArgument, e.to_string()))?;
                ops::duty_issue(st, now, &actor, required(args, "assignee")?, spec)
            }
            "duty_accept" | "duty_decline" => ops::duty_respond(
                st,
                now,
                &actor,
                required(args, "duty")?,
                args["version"].as_u64(),
                name == "duty_accept",
                args["note"].as_str(),
            ),
            "duty_done" => ops::duty_progress(
                st,
                now,
                &actor,
                required(args, "duty")?,
                model::DutyStatus::Done,
                args["note"].as_str(),
            ),
            "lease_acquire" => {
                let mode = match args["mode"].as_str().unwrap_or("exclusive") {
                    "exclusive" => model::LeaseMode::Exclusive,
                    "shared" => model::LeaseMode::Shared,
                    _ => return Err(Error::new(Reason::InvalidArgument, "invalid lease mode")),
                };
                ops::lease_acquire(
                    st,
                    now,
                    &actor,
                    required(args, "key")?,
                    mode,
                    args["ttl_ms"].as_u64(),
                    args["reason"].as_str().unwrap_or(""),
                )
            }
            "lease_release" => ops::lease_release(st, now, &actor, required(args, "lease")?),
            "escalate" => ops::escalate(
                st,
                now,
                &actor,
                args["duty"].as_str(),
                required(args, "question")?,
            ),
            "escalation_answer" => ops::escalation_answer(
                st,
                now,
                &actor,
                required(args, "escalation")?,
                required(args, "answer")?,
            ),
            "record_add" => {
                let kind =
                    serde_json::from_value(args.get("kind").cloned().unwrap_or(json!("note")))
                        .map_err(|e| Error::new(Reason::InvalidArgument, e.to_string()))?;
                let refs: Vec<String> =
                    serde_json::from_value(args.get("refs").cloned().unwrap_or(json!([])))
                        .map_err(|e| Error::new(Reason::InvalidArgument, e.to_string()))?;
                ops::record_add(st, now, &actor, kind, &refs, required(args, "text")?)
            }
            _ => Err(Error::new(Reason::InvalidArgument, "unknown tool")),
        }
    })?;
    crate::cli::flush_outbox(store, pager);
    Ok(json!(outcome))
}
