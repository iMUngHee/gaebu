use super::commands::Input;
use super::operations::ensure_ready;
use super::runtime::{authorize_master, lock_document, operate, runtime_duty};
use super::*;
use crate::{
    identity::{self, Caller},
    ops,
    store::{CLI_DEADLINE, Store},
};
use serde_json::{Value, json};

pub(super) fn reserve_input(store: &Store, caller: &Caller, args: &Input) -> Result<Value> {
    operate(store, caller, args, |st, actor, team, document| {
        let index = item_index(&document.schedule, args.text("item")?)?;
        let item = &document.schedule.items[index];
        authorize_master(st, actor, team, item)?;
        ensure_ready(&document.schedule, item)?;
        let key = args.text("key")?.to_owned();
        nonempty(&key, "dispatch key")?;
        let assignee = crate::authz::resolve_member(st, Some(team), args.text("assignee")?)?;
        let mut spec = args
            .spec
            .clone()
            .ok_or_else(|| invalid("missing dispatch spec"))?;
        nonempty(&spec.outcome, "outcome")?;
        nonempty(&spec.done_criteria, "done criteria")?;
        spec.scope = scope::normalize_list(&spec.scope)?;
        if !scope::within(&spec.scope, &item.scope)? {
            return Err(Error::new(
                Reason::OutOfScope,
                "worker duty exceeds schedule item scope",
            ));
        }
        let previous = item.dispatches.iter().rev().find(|d| d.key == key);
        if let Some(old) = previous.filter(|d| d.retired.is_none()) {
            if old.assignee.id != assignee || old.spec != spec {
                return Err(conflict(
                    "dispatch key already has a different immutable manifest; retire it before replacement",
                ));
            }
            return Ok(json!({"item":item.id,"token":old.token,"reserved":true}));
        }
        if previous.is_some() && args.predecessor.as_deref() != previous.map(|d| d.token.as_str()) {
            return Err(conflict("replacement must name its retired predecessor"));
        }
        let mut duty_spec = spec.duty_spec(None);
        crate::evidence::attach_source(store, &actor.session_id, &mut duty_spec)?;
        let mut probe = st.clone();
        ops::duty_issue(&mut probe, now_ms(), actor, &assignee, duty_spec.clone())?;
        let token = format!("x-{}", document.schedule.next_dispatch);
        let mut dispatch = Dispatch {
            key,
            token: token.clone(),
            assignee: origin(st, team, &assignee),
            issuer: origin(st, team, &actor.member),
            source: duty_spec.source.clone(),
            spec,
            manifest_hash: String::new(),
            predecessor: previous.map(|d| d.token.clone()),
            retired: None,
        };
        dispatch.manifest_hash = manifest_hash(&dispatch)?;
        document.schedule.next_dispatch = document
            .schedule
            .next_dispatch
            .checked_add(1)
            .ok_or_else(|| invalid("dispatch ids exhausted"))?;
        document.schedule.items[index].dispatches.push(dispatch);
        Ok(json!({"item":document.schedule.items[index].id,"token":token,"reserved":true}))
    })
}

pub(super) fn materialize(
    store: &Store,
    caller: &Caller,
    item_id: &str,
    token: &str,
) -> Result<Value> {
    let snapshot = store.read_current()?;
    let actor = identity::actor(&snapshot, caller)?;
    let team = snapshot.members[&actor.member].team.clone();
    let reg = snapshot
        .schedules
        .get(&team)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "team has no schedule"))?;
    let (_file, document) = lock_document(Path::new(&reg.path))?;
    store.transact(CLI_DEADLINE, |st| {
        let actor = identity::actor(st, caller)?;
        if st.schedules.get(&team) != Some(&reg) || document.schedule.id != reg.schedule_id {
            return Err(conflict("schedule registration changed"));
        }
        let item = &document.schedule.items[item_index(&document.schedule, item_id)?];
        let dispatch = item
            .dispatches
            .iter()
            .find(|d| d.token == token)
            .ok_or_else(|| Error::new(Reason::NotFound, "dispatch token missing"))?;
        if dispatch.retired.is_some() {
            return Err(conflict("dispatch token has a retirement tombstone"));
        }
        if !matching_origin(st, &team, &dispatch.assignee)
            || !matching_origin(st, &team, &dispatch.issuer)
        {
            return Err(conflict("dispatch runtime origin does not match"));
        }
        if let Some(duty) = runtime_duty(st, &document.schedule, dispatch)? {
            return Ok(json!({"item":item_id,"token":token,"duty":duty.id,"existing":true}));
        }
        authorize_master(st, &actor, &team, item)?;
        ensure_ready(&document.schedule, item)?;
        if dispatch.issuer.id != actor.member {
            return Err(Error::new(
                Reason::NoGrant,
                "only original current task master materializes this assignment",
            ));
        }
        let spec = dispatch.spec.duty_spec(dispatch.source.clone());
        let outcome = ops::duty_issue(st, now_ms(), &actor, &dispatch.assignee.id, spec)?;
        let duty = st.duties.get_mut(&outcome.ids[0]).unwrap();
        duty.schedule_ref = Some(ScheduleRef {
            schedule_id: document.schedule.id.clone(),
            item_id: item.id.clone(),
            dispatch_token: token.into(),
            manifest_hash: dispatch.manifest_hash.clone(),
        });
        Ok(json!({"item":item_id,"token":token,"duty":duty.id,"existing":false}))
    })
}

pub(super) fn reconcile(store: &Store, caller: &Caller, args: &Input) -> Result<Value> {
    let st = store.read_current()?;
    let actor = identity::actor(&st, caller)?;
    let team = &st.members[&actor.member].team;
    let reg = st
        .schedules
        .get(team)
        .ok_or_else(|| Error::new(Reason::NotFound, "team has no schedule"))?;
    let document = read_document(Path::new(&reg.path))?;
    if let Some(item) = &args.item {
        item_index(&document.schedule, item)?;
    }
    let mut results = vec![];
    for item in &document.schedule.items {
        if args.item.as_deref().is_some_and(|id| id != item.id) {
            continue;
        }
        for dispatch in item.dispatches.iter().filter(|d| d.retired.is_none()) {
            if args
                .token
                .as_deref()
                .is_some_and(|token| token != dispatch.token)
            {
                continue;
            }
            let result = materialize(store, caller, &item.id, &dispatch.token);
            results.push(match result {Ok(value)=>value,Err(error)=>json!({"item":item.id,"token":dispatch.token,"error":{"reason":error.reason,"message":error.message}})});
        }
    }
    if args.token.is_some() && results.is_empty() {
        return Err(conflict("token is missing or retired"));
    }
    Ok(json!({"results":results}))
}

pub(super) fn human_hold(
    store: &Store,
    caller: &Caller,
    args: &Input,
    release: bool,
) -> Result<Value> {
    let quote = args.text("evidence")?;
    let verify = || -> Result<Evidence> {
        let log = store.session(&caller.session_id);
        let mut ev = crate::evidence::current_turn_evidence(
            &caller.session_id,
            &log.events()?,
            &log.composers()?,
            quote,
            now_ms(),
        )?;
        if log.unrecorded_after(ev.at)?
            || store
                .session(crate::store::GLOBAL_MARKERS)
                .unrecorded_after(ev.at)?
        {
            return Err(Error::new(
                Reason::NoUserEvidence,
                "a later prompt was unrecorded",
            ));
        }
        if args.explicit {
            ev.strength = Strength::Explicit;
        }
        Ok(ev)
    };
    let first = verify()?;
    operate(store, caller, args, |_st, _actor, _team, document| {
        let ev = verify()?;
        if ev.sha256 != first.sha256 || ev.prompt_seq != first.prompt_seq {
            return Err(Error::new(Reason::NoUserEvidence, "current prompt changed"));
        }
        let index = item_index(&document.schedule, args.text("item")?)?;
        let item = &mut document.schedule.items[index];
        if release {
            item.hold = None;
        } else {
            if item.hold.is_some() {
                return Err(conflict("item already held; release it first"));
            }
            let until = args
                .text("until")?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if until.is_empty()
                || until.chars().count() > 120
                || until.chars().any(char::is_control)
            {
                return Err(invalid("until must be 1-120 characters on one line"));
            }
            item.hold = Some(Hold {
                evidence: ev,
                until,
                at: now_ms(),
            });
        }
        Ok(json!({"item":item}))
    })
}
