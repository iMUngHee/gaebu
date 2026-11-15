use super::*;
use crate::{identity::Caller, pager::Pager, store::Store};
use clap::Subcommand;
use serde_json::{Value, json};

/// Decode transport values once; scheduling rules operate on typed fields.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    pub item: Option<String>,
    pub team: Option<String>,
    pub to: Option<String>,
    #[serde(default)]
    pub withdraw: bool,
    pub title: Option<String>,
    pub key: Option<String>,
    pub token: Option<String>,
    pub assignee: Option<String>,
    pub spec: Option<DispatchSpec>,
    pub predecessor: Option<String>,
    pub scope: Option<Vec<String>>,
    pub owner_bu: Option<String>,
    pub pm_ref: Option<PmRef>,
    pub priority: Option<u16>,
    pub depends_on: Option<Vec<String>>,
    pub expected_revision: Option<u64>,
    pub reason: Option<String>,
    pub evidence: Option<String>,
    pub until: Option<String>,
    pub path: Option<std::path::PathBuf>,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub heal: bool,
    #[serde(default)]
    pub explicit: bool,
    #[serde(skip)]
    pub owner_bu_set: bool,
    #[serde(skip)]
    pub pm_ref_set: bool,
}

impl Input {
    pub(super) fn parse(value: &Value) -> Result<Self> {
        if let Some(spec) = value.get("spec") {
            let allowed = [
                "intent",
                "outcome",
                "scope",
                "decision_rights",
                "escalate_on",
                "done_criteria",
                "plan_ref",
                "exec_root",
                "source_quote",
            ];
            if spec
                .as_object()
                .is_none_or(|object| object.keys().any(|key| !allowed.contains(&key.as_str())))
            {
                return Err(invalid("unknown dispatch spec field or non-object spec"));
            }
        }
        let mut input: Self =
            serde_json::from_value(value.clone()).map_err(|e| invalid(e.to_string()))?;
        input.owner_bu_set = value.get("owner_bu").is_some();
        input.pm_ref_set = value.get("pm_ref").is_some();
        Ok(input)
    }
    pub(super) fn text(&self, key: &str) -> Result<&str> {
        let value = match key {
            "item" => &self.item,
            "title" => &self.title,
            "key" => &self.key,
            "token" => &self.token,
            "assignee" => &self.assignee,
            "reason" => &self.reason,
            "evidence" => &self.evidence,
            "until" => &self.until,
            _ => return Err(invalid("unknown request field")),
        };
        value
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| invalid(format!("{key} must be nonempty")))
    }
}

/// Commit an assignment without materializing it. Recovery callers can resume
/// with the returned token after a runtime write failure.
pub fn reserve(store: &Store, caller: &Caller, args: &Value) -> Result<Value> {
    dispatch::reserve_input(store, caller, &Input::parse(args)?)
}

pub fn human_hold(store: &Store, caller: &Caller, args: &Value, release: bool) -> Result<Value> {
    dispatch::human_hold(store, caller, &Input::parse(args)?, release)
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCmd {
    /// Offer this team's Schedule to another team or withdraw its offer.
    Offer {
        #[arg(
            long,
            required_unless_present = "withdraw",
            conflicts_with = "withdraw"
        )]
        to: Option<String>,
        #[arg(long)]
        withdraw: bool,
    },
    /// Adopt a team's Schedule, or an unregistered file with human evidence.
    Adopt {
        #[arg(required_unless_present = "path", conflicts_with = "path")]
        team: Option<String>,
        #[arg(long)]
        path: Option<std::path::PathBuf>,
        #[arg(long)]
        evidence: Option<String>,
        #[arg(long)]
        explicit: bool,
    },
    Init {
        path: std::path::PathBuf,
    },
    Attach {
        path: std::path::PathBuf,
    },
    /// Root directive: seat, replace or remove (`commander`) the separate
    /// Schedule manager, quoting the human's current-turn words.
    Manager {
        member: String,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        #[arg(long)]
        evidence: String,
        /// The user's words name both this action and its target.
        #[arg(long)]
        explicit: bool,
    },
    Show {
        #[arg(conflicts_with = "all")]
        item: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        session: Option<String>,
    },
    Archive {
        /// Delete older history to turn a missing-segment hole into an expired prefix.
        #[arg(long)]
        heal: bool,
        #[arg(long)]
        expected_revision: Option<u64>,
    },
    Move {
        path: std::path::PathBuf,
    },
    Add {
        #[arg(long)]
        title: String,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        #[arg(long)]
        owner_bu: Option<String>,
        #[arg(long, default_value_t = 100)]
        priority: u16,
        #[arg(long, value_delimiter = ',')]
        depends_on: Vec<String>,
        /// Structured PM reference JSON: {"task":"KEY","item":"id","plan":"relative/path"}.
        #[arg(long)]
        pm_ref: Option<String>,
    },
    Update {
        item: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        priority: Option<u16>,
        #[arg(long, value_delimiter = ',')]
        scope: Option<Vec<String>>,
        #[arg(long)]
        owner_bu: Option<String>,
        #[arg(long, value_delimiter = ',')]
        depends_on: Option<Vec<String>>,
        #[arg(long)]
        expected_revision: Option<u64>,
        #[arg(long)]
        pm_ref: Option<String>,
        #[arg(long, conflicts_with = "owner_bu")]
        clear_owner: bool,
    },
    Dispatch {
        item: String,
        #[arg(long)]
        key: String,
        #[arg(long = "to")]
        assignee: String,
        #[arg(long)]
        spec: String,
        #[arg(long)]
        predecessor: Option<String>,
    },
    Reconcile {
        item: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    Retire {
        item: String,
        token: String,
        #[arg(long)]
        reason: String,
    },
    Abandon {
        item: String,
        token: String,
        #[arg(long)]
        reason: String,
    },
    Complete {
        item: String,
        #[arg(long)]
        evidence: String,
    },
    Cancel {
        item: String,
        #[arg(long)]
        reason: String,
    },
    Hold {
        item: String,
        #[arg(long)]
        until: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        explicit: bool,
    },
    Release {
        item: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        explicit: bool,
    },
}

pub fn tools() -> Vec<Value> {
    let string = json!({"type":"string"});
    let strings = json!({"type":"array","items":{"type":"string"}});
    let revision = json!({"type":"integer","minimum":0});
    let pm = json!({"type":["object","null"],"properties":{"task":string,"item":string,"plan":{"type":["string","null"]}},"required":["task","item"],"additionalProperties":false});
    let spec = json!({"type":"object","properties":{"intent":string,"outcome":string,"scope":strings,"decision_rights":string,"escalate_on":strings,"done_criteria":string,"plan_ref":{"type":["string","null"]},"exec_root":{"type":["string","null"]},"source_quote":string},"required":["intent","outcome","scope","decision_rights","escalate_on","done_criteria"],"additionalProperties":false});
    [
        ("schedule_offer","Offer the commander's registered Schedule to another active team, or withdraw the offer",json!({"to":string,"withdraw":{"type":"boolean"}}),vec![]),
        ("schedule_adopt","Adopt a team's whole Schedule. Active sources must offer it; source runtime holds require current-turn evidence. An unregistered path always requires human evidence. Existing item holds remain.",json!({"team":string,"path":string,"evidence":string,"explicit":{"type":"boolean"}}),vec![]),
        ("schedule_show","Read compact open work, all history, or one full Schedule item",json!({"item":string,"all":{"type":"boolean"}}),vec![]),
        ("schedule_archive","Archive eligible terminal Schedule items and enforce the byte limit, permanently deleting old segments. expired reports removed segments/rows; retained_bytes and limit_bytes show bounds; expiry_deferred reports deletion failures. heal=true explicitly deletes older history to repair holes; healed separates listed and unlisted removals, heal_deferred/hole_remaining report partial recovery.",json!({"expected_revision":revision,"heal":{"type":"boolean"}}),vec![]),
        ("schedule_move","Move the commander team's missing Schedule registration to an existing file",json!({"path":string}),vec!["path"]),
        ("schedule_add","Add an operational work item without changing PM",json!({"title":string,"scope":strings,"owner_bu":{"type":["string","null"]},"priority":{"type":"integer","minimum":0,"maximum":65535},"depends_on":strings,"pm_ref":pm,"expected_revision":revision}),vec!["title","scope"]),
        ("schedule_update","Update operational priority or an undispatched item contract",json!({"item":string,"title":string,"scope":strings,"owner_bu":{"type":["string","null"]},"priority":{"type":"integer","minimum":0,"maximum":65535},"depends_on":strings,"pm_ref":pm,"expected_revision":revision}),vec!["item"]),
        ("schedule_dispatch","Task master commits an assignment and materializes its worker duty idempotently",json!({"item":string,"key":string,"assignee":string,"spec":spec,"predecessor":string,"expected_revision":revision}),vec!["item","key","assignee","spec"]),
        ("schedule_reconcile","Recover committed assignment tokens without creating new work",json!({"item":string,"token":string}),vec![]),
        ("schedule_retire","Retire a terminated attempt or prove an unmaterialized token absent",json!({"item":string,"token":string,"reason":string,"expected_revision":revision}),vec!["item","token","reason"]),
        ("schedule_abandon","Tombstone a token only after proving the current runtime never materialized it",json!({"item":string,"token":string,"reason":string,"expected_revision":revision}),vec!["item","token","reason"]),
        ("schedule_complete","Current task master records explicit review acceptance evidence",json!({"item":string,"evidence":string,"expected_revision":revision}),vec!["item","evidence"]),
        ("schedule_cancel","Cancel a schedule item while preserving duty cleanup paths",json!({"item":string,"reason":string,"expected_revision":revision}),vec!["item","reason"]),
    ].into_iter().map(|(name,description,properties,required)|json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}

pub fn call(
    store: &Store,
    pager: &dyn Pager,
    caller: &Caller,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let schema = tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| invalid("unknown schedule tool"))?;
    let object = args
        .as_object()
        .ok_or_else(|| invalid("schedule arguments must be an object"))?;
    let properties = schema["inputSchema"]["properties"].as_object().unwrap();
    if object.keys().any(|key| !properties.contains_key(key)) {
        return Err(invalid("unknown schedule argument"));
    }
    for key in schema["inputSchema"]["required"].as_array().unwrap() {
        if !object.contains_key(key.as_str().unwrap()) {
            return Err(invalid(format!("missing {}", key.as_str().unwrap())));
        }
    }
    if name == "schedule_show" {
        let input = Input::parse(args)?;
        if input.all && input.item.is_some() {
            return Err(invalid("item and all are mutually exclusive"));
        }
        let st = store.read_current()?;
        let member = crate::authz::member_for_session(&st, &caller.session_id)
            .ok_or_else(|| Error::new(Reason::NotMember, "session has no member"))?;
        return presentation::show(store, &st, &member.team, &input);
    }
    let input = Input::parse(args)?;
    let result = match name {
        "schedule_offer" => handoff::offer(store, caller, &input),
        "schedule_adopt" => handoff::adopt(store, caller, &input),
        "schedule_move" => runtime::move_registration(
            store,
            caller,
            input
                .path
                .as_deref()
                .ok_or_else(|| invalid("missing schedule path"))?,
        ),
        "schedule_dispatch" => {
            let reserved = dispatch::reserve_input(store, caller, &input)?;
            dispatch::materialize(
                store,
                caller,
                reserved["item"].as_str().unwrap(),
                reserved["token"].as_str().unwrap(),
            )
        }
        "schedule_reconcile" => dispatch::reconcile(store, caller, &input),
        _ => operations::mutate(store, caller, name, &input),
    };
    if result.is_ok() {
        crate::pager::flush_outbox(store, pager);
    }
    result
}

pub fn run_cli(cmd: ScheduleCmd, json_output: bool, pager: &dyn Pager) -> Result<()> {
    let store = Store::from_env()?;
    let mut caller = crate::identity::from_pager(pager)?;
    if let ScheduleCmd::Show {
        session: Some(session),
        ..
    } = &cmd
    {
        caller.session_id = session.clone();
        caller.source = crate::identity::Source::Other("read-only --session".into());
    }
    if !matches!(cmd, ScheduleCmd::Show { .. }) {
        if !caller.may_mutate() {
            return Err(Error::new(
                Reason::UnresolvedCaller,
                "flag/env caller is read-only",
            ));
        }
        crate::identity::refresh_bindings(&store, pager)?;
    }
    let result = match cmd {
        ScheduleCmd::Offer { to, withdraw } => {
            let mut args = json!({"withdraw":withdraw});
            if let Some(to) = to {
                args["to"] = json!(to);
            }
            call(&store, pager, &caller, "schedule_offer", &args)
        }
        ScheduleCmd::Adopt {
            team,
            path,
            evidence,
            explicit,
        } => {
            let mut args = json!({"explicit":explicit});
            if let Some(team) = team {
                args["team"] = json!(team);
            }
            if let Some(path) = path {
                args["path"] = json!(path);
            }
            if let Some(evidence) = evidence {
                args["evidence"] = json!(evidence);
            }
            call(&store, pager, &caller, "schedule_adopt", &args)
        }
        ScheduleCmd::Init { path } => runtime::register(&store, &caller, &path, true),
        ScheduleCmd::Attach { path } => runtime::register(&store, &caller, &path, false),
        ScheduleCmd::Manager {
            member,
            scope,
            evidence,
            explicit,
        } => runtime::set_manager(&store, &caller, &member, &scope, &evidence, explicit),
        ScheduleCmd::Show { item, all, .. } => {
            let mut args = json!({"all":all});
            if let Some(item) = item {
                args["item"] = json!(item);
            }
            call(&store, pager, &caller, "schedule_show", &args)
        }
        ScheduleCmd::Archive {
            expected_revision,
            heal,
        } => {
            let mut args = json!({"heal":heal});
            if let Some(revision) = expected_revision {
                args["expected_revision"] = json!(revision);
            }
            call(&store, pager, &caller, "schedule_archive", &args)
        }
        ScheduleCmd::Move { path } => call(
            &store,
            pager,
            &caller,
            "schedule_move",
            &json!({"path":path}),
        ),
        ScheduleCmd::Add {
            title,
            scope,
            owner_bu,
            priority,
            depends_on,
            pm_ref,
        } => call(
            &store,
            pager,
            &caller,
            "schedule_add",
            &json!({"title":title,"scope":scope,"owner_bu":owner_bu,"priority":priority,"depends_on":depends_on,"pm_ref":pm_ref.map(|text|serde_json::from_str::<Value>(&text).map_err(|e|invalid(e.to_string()))).transpose()?}),
        ),
        ScheduleCmd::Update {
            item,
            title,
            priority,
            scope,
            owner_bu,
            depends_on,
            expected_revision,
            pm_ref,
            clear_owner,
        } => {
            let mut args = json!({"item":item});
            for (key, value) in [
                ("title", json!(title)),
                ("priority", json!(priority)),
                ("scope", json!(scope)),
                ("owner_bu", json!(owner_bu)),
                ("depends_on", json!(depends_on)),
                ("expected_revision", json!(expected_revision)),
            ] {
                if !value.is_null() {
                    args[key] = value;
                }
            }
            if let Some(text) = pm_ref {
                args["pm_ref"] = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
            }
            if clear_owner {
                args["owner_bu"] = Value::Null;
            }
            call(&store, pager, &caller, "schedule_update", &args)
        }
        ScheduleCmd::Dispatch {
            item,
            key,
            assignee,
            spec,
            predecessor,
        } => {
            let spec: Value =
                serde_json::from_str(&spec).map_err(|error| invalid(error.to_string()))?;
            let mut args = json!({"item":item,"key":key,"assignee":assignee,"spec":spec});
            if let Some(predecessor) = predecessor {
                args["predecessor"] = json!(predecessor);
            }
            call(&store, pager, &caller, "schedule_dispatch", &args)
        }
        ScheduleCmd::Reconcile { item, token } => {
            let mut args = json!({});
            if let Some(item) = item {
                args["item"] = json!(item);
            }
            if let Some(token) = token {
                args["token"] = json!(token);
            }
            call(&store, pager, &caller, "schedule_reconcile", &args)
        }
        ScheduleCmd::Retire {
            item,
            token,
            reason,
        } => call(
            &store,
            pager,
            &caller,
            "schedule_retire",
            &json!({"item":item,"token":token,"reason":reason}),
        ),
        ScheduleCmd::Abandon {
            item,
            token,
            reason,
        } => call(
            &store,
            pager,
            &caller,
            "schedule_abandon",
            &json!({"item":item,"token":token,"reason":reason}),
        ),
        ScheduleCmd::Complete { item, evidence } => call(
            &store,
            pager,
            &caller,
            "schedule_complete",
            &json!({"item":item,"evidence":evidence}),
        ),
        ScheduleCmd::Cancel { item, reason } => call(
            &store,
            pager,
            &caller,
            "schedule_cancel",
            &json!({"item":item,"reason":reason}),
        ),
        ScheduleCmd::Hold {
            item,
            until,
            evidence,
            explicit,
        } => human_hold(
            &store,
            &caller,
            &json!({"item":item,"until":until,"evidence":evidence,"explicit":explicit}),
            false,
        ),
        ScheduleCmd::Release {
            item,
            evidence,
            explicit,
        } => human_hold(
            &store,
            &caller,
            &json!({"item":item,"evidence":evidence,"explicit":explicit}),
            true,
        ),
    }?;
    if json_output {
        println!("{}", serde_json::to_string(&result).map_err(io_error)?);
    } else if let Some(items) = result.get("items").and_then(Value::as_array) {
        if !result["registered"].as_bool().unwrap_or(false) {
            println!("일정 미등록 — gaebu schedule init <schedule.md>");
        } else if let Some(error) = result.get("error") {
            println!(
                "일정 확인 필요: {}",
                error["message"].as_str().unwrap_or("unknown")
            );
        } else {
            println!(
                "일정 {} · rev {}",
                result["schedule_id"].as_str().unwrap_or(""),
                result["revision"]
            );
            for item in items {
                println!(
                    "{} [{}] {} · {}",
                    item["id"].as_str().unwrap_or(""),
                    item["stage_label"].as_str().unwrap_or(""),
                    item["title"].as_str().unwrap_or(""),
                    item["next_action"].as_str().unwrap_or("")
                );
            }
        }
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(io_error)?
        );
    }
    Ok(())
}
