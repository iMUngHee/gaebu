use super::commands::Input;
use super::runtime::{
    authorize_manager, authorize_master, dependencies_ready, operate, runtime_duty,
};
use super::*;
use crate::{authz, identity::Caller, store::Store};
use serde_json::{Value, json};

pub(super) fn mutate(store: &Store, caller: &Caller, name: &str, args: &Input) -> Result<Value> {
    let auto = name != "schedule_archive";
    operate(store, caller, args, auto, |st, actor, team, document| {
        if name == "schedule_archive" {
            authorize_manager(st, actor, team, &[])?;
            let path = Path::new(&st.schedules[team].path);
            return archive::sweep(st, team, document, path, true);
        }
        if name == "schedule_add" {
            let title = args.text("title")?.to_owned();
            nonempty(&title, "title")?;
            let item_scope = args.scope.clone().unwrap_or_default();
            let item_scope = scope::normalize_list(&item_scope)?;
            authorize_manager(st, actor, team, &item_scope)?;
            let owner_bu = owner_ref(st, team, args.owner_bu.as_deref())?;
            let pm_ref = args.pm_ref.clone();
            let mut item = Item {
                id: format!("w-{}", document.schedule.next_item),
                title,
                pm_ref,
                priority: args.priority.unwrap_or(100),
                depends_on: args.depends_on.clone().unwrap_or_default(),
                scope: item_scope,
                owner_bu,
                hold: None,
                cancellation: None,
                dispatches: vec![],
                completion: None,
            };
            let satisfied =
                resolve_dependencies(st, team, &document.schedule, &item.id, &mut item.depends_on)?;
            document.schedule.next_item = document
                .schedule
                .next_item
                .checked_add(1)
                .ok_or_else(|| invalid("item ids exhausted"))?;
            document.schedule.items.push(item.clone());
            validate(&document.schedule)?;
            return Ok(json!({"item":item,"satisfied":satisfied}));
        }
        let index = item_index(&document.schedule, args.text("item")?)?;
        let item = document.schedule.items[index].clone();
        match name {
            "schedule_update" => {
                authorize_manager(st, actor, team, &item.scope)?;
                let semantic = args.title.is_some()
                    || args.scope.is_some()
                    || args.depends_on.is_some()
                    || args.owner_bu_set
                    || args.pm_ref_set;
                if item.completion.is_some() && semantic {
                    return Err(conflict("completed item contract is immutable"));
                }
                if !item.dispatches.is_empty()
                    && (args.title.is_some()
                        || args.scope.is_some()
                        || args.depends_on.is_some()
                        || args.pm_ref_set)
                {
                    return Err(conflict("dispatched item contract is immutable"));
                }
                if args.owner_bu_set {
                    ensure_terminated(st, team, &document.schedule, &item)?;
                }
                let mut dependencies = args.depends_on.clone();
                let satisfied = if let Some(deps) = &mut dependencies {
                    resolve_dependencies(st, team, &document.schedule, &item.id, deps)?
                } else {
                    vec![]
                };
                let changed = &mut document.schedule.items[index];
                if let Some(title) = &args.title {
                    changed.title = title.clone();
                }
                if let Some(priority) = args.priority {
                    changed.priority = priority;
                }
                if let Some(scope) = args.scope.clone() {
                    changed.scope = scope::normalize_list(&scope)?;
                    authorize_manager(st, actor, team, &changed.scope)?;
                }
                if let Some(deps) = dependencies {
                    changed.depends_on = deps;
                }
                if args.owner_bu_set {
                    changed.owner_bu = owner_ref(st, team, args.owner_bu.as_deref())?;
                }
                if args.pm_ref_set {
                    changed.pm_ref = args.pm_ref.clone();
                }
                let value = json!({"item":changed,"satisfied":satisfied});
                validate(&document.schedule)?;
                Ok(value)
            }
            "schedule_complete" => {
                authorize_master(st, actor, team, &item)?;
                if item.completion.is_some() {
                    return Err(conflict("item already completed"));
                }
                ensure_ready(&document.schedule, &item)?;
                for dispatch in latest_dispatches(&item)
                    .into_iter()
                    .filter(|d| d.retired.is_none())
                {
                    if !matching_origin(st, team, &dispatch.assignee) {
                        return Err(conflict(
                            "unknown runtime origin cannot establish completion",
                        ));
                    }
                    if runtime_duty(st, &document.schedule, &item, dispatch)?
                        .is_none_or(|d| d.status != DutyStatus::Done)
                    {
                        return Err(conflict(
                            "all required worker attempts must be Done or explicitly retired",
                        ));
                    }
                }
                let evidence = args.text("evidence")?.to_owned();
                nonempty(&evidence, "completion evidence")?;
                document.schedule.items[index].completion = Some(Completion {
                    by: origin(st, team, &actor.member),
                    at: now_ms(),
                    evidence,
                });
                Ok(json!({"item":document.schedule.items[index]}))
            }
            "schedule_cancel" => {
                authorize_recovery(st, actor, team, &item)?;
                if item.completion.is_some() {
                    return Err(conflict("completed item cannot be cancelled"));
                }
                let reason = args.text("reason")?.to_owned();
                nonempty(&reason, "cancellation reason")?;
                document.schedule.items[index].cancellation = Some(Decision {
                    by: origin(st, team, &actor.member),
                    at: now_ms(),
                    reason,
                });
                Ok(json!({"item":document.schedule.items[index]}))
            }
            "schedule_retire" | "schedule_abandon" => {
                authorize_recovery(st, actor, team, &item)?;
                if item.completion.is_some() {
                    return Err(conflict("completed item history cannot be retired"));
                }
                let token = args.text("token")?;
                let pos = item
                    .dispatches
                    .iter()
                    .position(|d| d.token == token)
                    .ok_or_else(|| Error::new(Reason::NotFound, "dispatch token not found"))?;
                let dispatch = &item.dispatches[pos];
                if !matching_origin(st, team, &dispatch.assignee)
                    || !matching_origin(st, team, &dispatch.issuer)
                {
                    return Err(conflict(
                        "foreign runtime cannot prove an attempt terminated or absent",
                    ));
                }
                if dispatch.retired.is_some() {
                    return Ok(json!({"token":token,"retired":true}));
                }
                if let Some(duty) = runtime_duty(st, &document.schedule, &item, dispatch)? {
                    if name == "schedule_abandon" {
                        return Err(conflict("duty exists; retire only after known termination"));
                    }
                    terminated(st, duty, dispatch)?;
                }
                let reason = args.text("reason")?.to_owned();
                nonempty(&reason, "retirement reason")?;
                document.schedule.items[index].dispatches[pos].retired = Some(Decision {
                    by: origin(st, team, &actor.member),
                    at: now_ms(),
                    reason,
                });
                Ok(json!({"token":token,"retired":true}))
            }
            _ => Err(invalid("unknown schedule mutation")),
        }
    })
}

fn authorize_recovery(st: &State, actor: &Actor, team: &str, item: &Item) -> Result<()> {
    if st.teams[team].commander == actor.member {
        return authz::require(st, &actor.member, Verb::Assign, &item.scope);
    }
    authorize_manager(st, actor, team, &item.scope)
        .or_else(|_| authorize_master(st, actor, team, item))
}

fn owner_ref(st: &State, team: &str, value: Option<&str>) -> Result<Option<OriginRef>> {
    match value {
        None => Ok(None),
        Some(reference) => {
            let id = crate::authz::resolve_bu(st, Some(team), reference)?;
            if st
                .bus
                .get(&id)
                .is_none_or(|b| b.team != team || b.status != BuStatus::Open)
                || authz::bu_cancelled(st, &id)
            {
                return Err(conflict("owner unit must be open in this team"));
            }
            Ok(Some(origin(st, team, &id)))
        }
    }
}

pub(super) fn ensure_ready(schedule: &Schedule, item: &Item) -> Result<()> {
    if item.cancellation.is_some() {
        return Err(Error::new(Reason::Cancelled, "schedule item cancelled"));
    }
    if item.completion.is_some() {
        return Err(conflict("schedule item completed"));
    }
    if let Some(hold) = &item.hold {
        return Err(Error::new(
            Reason::Held,
            format!("schedule item held until {}", hold.until),
        ));
    }
    if !dependencies_ready(schedule, item) {
        return Err(conflict("schedule dependencies are not accepted"));
    }
    Ok(())
}

fn terminated(st: &State, duty: &Duty, dispatch: &Dispatch) -> Result<()> {
    if matches!(
        duty.status,
        DutyStatus::Proposed | DutyStatus::Accepted | DutyStatus::Blocked
    ) {
        return Err(conflict("worker attempt is still live"));
    }
    for lease in st
        .leases
        .values()
        .filter(|l| l.holder == duty.assignee && l.expires_at > now_ms())
    {
        let key = scope::parse(&lease.key)?;
        if dispatch
            .spec
            .scope
            .iter()
            .any(|s| scope::parse(s).is_ok_and(|s| s.overlaps(&key)))
        {
            return Err(conflict(
                "worker still holds resources in the attempt scope",
            ));
        }
    }
    Ok(())
}

pub(super) fn ensure_terminated(
    st: &State,
    team: &str,
    schedule: &Schedule,
    item: &Item,
) -> Result<()> {
    for dispatch in &item.dispatches {
        if dispatch.retired.is_some() {
            continue;
        }
        if !matching_origin(st, team, &dispatch.assignee) {
            return Err(conflict("foreign runtime attempt is unresolved"));
        }
        let duty = runtime_duty(st, schedule, item, dispatch)?.ok_or_else(|| {
            conflict("unmaterialized attempt needs explicit abandonment before reassignment")
        })?;
        terminated(st, duty, dispatch)?;
    }
    Ok(())
}

fn resolve_dependencies(
    st: &State,
    team: &str,
    schedule: &Schedule,
    id: &str,
    deps: &mut Vec<String>,
) -> Result<Vec<String>> {
    let mut allowed: BTreeSet<_> = schedule
        .items
        .iter()
        .map(|item| item.id.as_str())
        .chain(schedule.retired.iter().map(|item| item.id.as_str()))
        .collect();
    for dependency in deps.iter() {
        if absent_retired(schedule, dependency) {
            allowed.insert(dependency);
        }
    }
    validate_dependencies(id, deps, &allowed)?;
    let archived = if deps
        .iter()
        .any(|dependency| absent_retired(schedule, dependency))
    {
        archive::load(Path::new(&st.schedules[team].path), schedule)?
    } else {
        vec![]
    };
    let mut satisfied = vec![];
    for dependency in deps.iter() {
        if schedule
            .items
            .iter()
            .any(|item| item.id == *dependency && item.cancellation.is_some())
            || schedule
                .retired
                .iter()
                .any(|item| item.id == *dependency && item.state == RetiredState::Cancelled)
            || archived
                .iter()
                .any(|item| item.id == *dependency && item.cancellation.is_some())
        {
            return Err(conflict(format!(
                "schedule dependency {dependency} is cancelled"
            )));
        }
        if absent_retired(schedule, dependency) {
            if archived
                .iter()
                .any(|item| item.id == *dependency && item.completion.is_some())
            {
                satisfied.push(dependency.clone());
            } else {
                return Err(invalid("missing, repeated, or self dependency"));
            }
        }
    }
    deps.retain(|dependency| !satisfied.contains(dependency));
    Ok(satisfied)
}
