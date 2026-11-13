use super::commands::Input;
use super::*;
use crate::{
    authz,
    identity::{self, Caller},
    model::ScheduleRegistration,
    ops,
    store::{self, CLI_DEADLINE, Store},
};
use serde_json::{Value, json};
use std::{fs::File, path::Path, time::Instant};

pub fn register(store: &Store, caller: &Caller, path: &Path, create: bool) -> Result<Value> {
    let snapshot = store.read_current()?;
    let actor = identity::actor(&snapshot, caller)?;
    let team = snapshot.members[&actor.member].team.clone();
    commander(&snapshot, &actor, &team)?;
    let path = if path.exists() {
        fs::canonicalize(path).map_err(io_error)?
    } else {
        let absolute = std::path::absolute(path).map_err(io_error)?;
        let parent = fs::canonicalize(
            absolute
                .parent()
                .ok_or_else(|| invalid("schedule has no parent"))?,
        )
        .map_err(io_error)?;
        parent.join(
            absolute
                .file_name()
                .ok_or_else(|| invalid("schedule has no file name"))?,
        )
    };
    archive::refuse_sidecar_name(&path)?;
    let _file = lock_path(&path)?;
    let mut document = if path.exists() {
        read_document(&path)?
    } else if create {
        Document::empty(format!(
            "schedule-{}",
            crate::evidence::sha256_hex(&format!(
                "{}:{}:{}",
                snapshot.runtime_id,
                path.display(),
                now_ms()
            ))
        ))
    } else {
        return Err(Error::new(Reason::NotFound, "schedule file does not exist"));
    };
    if !path.exists() {
        document.save(&path, Instant::now() + CLI_DEADLINE)?;
    }
    store.transact(CLI_DEADLINE,|st| {
        let actor=identity::actor(st,caller)?;
        commander(st,&actor,&team)?;
        if let Some(reg)=st.schedules.get(&team)
            && (reg.path != path.to_string_lossy() || reg.schedule_id!=document.schedule.id) {
                return Err(conflict("team already has a canonical schedule; preserve its registration"));
        }
        if st.schedules.iter().any(|(id,reg)|id!=&team && (reg.schedule_id==document.schedule.id || reg.path==path.to_string_lossy())) {
            return Err(conflict("schedule is already registered to another team"));
        }
        st.schedules.entry(team.clone()).or_insert(ScheduleRegistration {schedule_id:document.schedule.id.clone(),path:path.to_string_lossy().into_owned(),manager:None});
        Ok(json!({"registered":true,"schedule_id":document.schedule.id,"path":path,"revision":document.schedule.revision}))
    })
}
pub fn set_manager(
    store: &Store,
    caller: &Caller,
    member: &str,
    scope: &[String],
) -> Result<Value> {
    store.transact(CLI_DEADLINE, |st| {
        let actor = identity::actor(st, caller)?;
        let team = st.members[&actor.member].team.clone();
        commander(st, &actor, &team)?;
        if !st.schedules.contains_key(&team) {
            return Err(Error::new(Reason::NotFound, "team has no schedule"));
        }
        let selected = if member == "commander" {
            None
        } else {
            let id = crate::authz::resolve_member(st, Some(&team), member)?;
            let target = &st.members[&id];
            if !authz::is_below(st, &actor.member, &id)
                || target.binding.state != BindingState::Bound
                || target.status != MemberStatus::Active
            {
                return Err(Error::new(
                    Reason::NoGrant,
                    "schedule manager must be a bound subordinate",
                ));
            }
            if !scope.is_empty() {
                ops::grant(st, now_ms(), &actor, &id, &[Verb::Assign], scope)?;
            }
            if !authz::effective_grants(st, &id)
                .iter()
                .any(|g| g.verbs.contains(&Verb::Assign) && !g.scope.is_empty())
            {
                return Err(Error::new(
                    Reason::NoGrant,
                    "schedule manager needs an existing scoped Assign grant or --scope",
                ));
            }
            Some(id)
        };
        st.schedules.get_mut(&team).unwrap().manager = selected.clone();
        Ok(json!({"manager":selected,"mode":if selected.is_some(){"delegated"}else{"commander"}}))
    })
}

pub(super) fn move_registration(store: &Store, caller: &Caller, path: &Path) -> Result<Value> {
    let snapshot = store.read_current()?;
    let actor = identity::actor(&snapshot, caller)?;
    let team = snapshot.members[&actor.member].team.clone();
    commander(&snapshot, &actor, &team)?;
    let registration = snapshot
        .schedules
        .get(&team)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "team has no schedule"))?;
    let path = fs::canonicalize(path).map_err(io_error)?;
    archive::refuse_sidecar_name(&path)?;
    let (_file, document) = lock_document(&path)?;
    if document.schedule.id != registration.schedule_id {
        return Err(conflict("schedule identity changed"));
    }
    archive::require_all_segments(&path, &document.schedule)?;
    store.transact(CLI_DEADLINE, |st| {
        let actor = identity::actor(st, caller)?;
        commander(st, &actor, &team)?;
        if st.schedules.get(&team) != Some(&registration) {
            return Err(conflict("schedule registration changed"));
        }
        match fs::metadata(&registration.path) {
            Ok(_) => return Err(conflict("old schedule still exists; move the file first")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(io_error(error)),
        }
        if st
            .schedules
            .iter()
            .any(|(id, reg)| id != &team && reg.path == path.to_string_lossy())
        {
            return Err(conflict("schedule is already registered to another team"));
        }
        st.schedules.get_mut(&team).unwrap().path = path.to_string_lossy().into_owned();
        Ok(json!({"schedule_id":registration.schedule_id,"path":path}))
    })
}

pub fn transaction<T>(
    store: &Store,
    caller: &Caller,
    f: impl FnOnce(&mut State, &Actor) -> Result<T>,
) -> Result<T> {
    let before = store.read()?;
    let actor = identity::actor(&before, caller)?;
    let team = before.members[&actor.member].team.clone();
    let registration = before.schedules.get(&team).cloned();
    let locked = registration
        .as_ref()
        .map(|reg| lock_path(Path::new(&reg.path)));
    let cached = std::cell::OnceCell::new();
    let document = || -> Result<&Document> {
        match &locked {
            Some(Ok(_)) => cached
                .get_or_init(|| {
                    let registration = registration.as_ref().unwrap();
                    let document = read_document(Path::new(&registration.path))?;
                    if document.schedule.id != registration.schedule_id {
                        return Err(conflict("schedule identity changed"));
                    }
                    Ok(document)
                })
                .as_ref()
                .map_err(Clone::clone),
            Some(Err(error)) => Err(error.clone()),
            None => Err(conflict("linked duty has no schedule registration")),
        }
    };
    let result = store.transact(CLI_DEADLINE, |st| {
        let actor = identity::actor(st, caller)?;
        if st.schedules.get(&team) != registration.as_ref() {
            return Err(conflict("schedule registration changed while waiting"));
        }
        let old = st.clone();
        let result = f(st, &actor)?;
        for duty in st.duties.values() {
            let prior = old.duties.get(&duty.id);
            if prior == Some(duty) {
                continue;
            }
            if prior.is_some_and(|old| {
                old.schedule_ref.is_some() && old.schedule_ref != duty.schedule_ref
            }) {
                return Err(conflict("a linked duty's schedule reference is immutable"));
            }
            if duty.schedule_ref.is_none() {
                continue;
            }
            if matches!(duty.status, DutyStatus::Declined | DutyStatus::Cancelled)
                && prior.is_some_and(|old| {
                    old.assignee == duty.assignee
                        && old.issuer == duty.issuer
                        && old.bu == duty.bu
                        && old.intent == duty.intent
                        && old.outcome == duty.outcome
                        && old.scope == duty.scope
                        && old.decision_rights == duty.decision_rights
                        && old.escalate_on == duty.escalate_on
                        && old.done_criteria == duty.done_criteria
                        && old.plan_ref == duty.plan_ref
                        && old.exec_root == duty.exec_root
                        && old.source == duty.source
                })
            {
                continue;
            }
            guard_duty(st, &team, document()?, duty)?;
        }
        for lease in st.leases.values() {
            if old
                .leases
                .get(&lease.id)
                .is_some_and(|old| old.expires_at == lease.expires_at)
            {
                continue;
            }
            let mut refusal = None;
            let mut allowed = false;
            for duty in st.duties.values().filter(|d| {
                d.assignee == lease.holder
                    && d.status == DutyStatus::Accepted
                    && !authz::bu_cancelled(st, &d.bu)
            }) {
                if !scope::within(std::slice::from_ref(&lease.key), &duty.scope)?
                    || authz::not_held(st, duty).is_err()
                {
                    continue;
                }
                if duty.schedule_ref.is_none() {
                    allowed = true;
                    break;
                }
                let check = document().and_then(|document| guard_duty(st, &team, document, duty));
                match check {
                    Ok(()) => {
                        allowed = true;
                        break;
                    }
                    Err(error) => refusal = Some(error),
                }
            }
            if !allowed && let Some(error) = refusal {
                return Err(error);
            }
        }
        if registration.is_some() {
            for bu in st.bus.values().filter(|bu| bu.status == BuStatus::Closed
                && old.bus.get(&bu.id).is_some_and(|before| before.status == BuStatus::Open)) {
                let open: Vec<_> = document()?.schedule.items.iter().filter(|item| {
                    item.completion.is_none() && item.cancellation.is_none()
                        && item.owner_bu.as_ref().is_some_and(|owner|
                            matching_origin(st, &bu.team, owner) && owner.id == bu.id)
                }).map(|item| item.id.as_str()).collect();
                if !open.is_empty() {
                    return Err(conflict(format!("bu owns open schedule items {}; transfer them with gaebu schedule update <item> --owner-bu <bu> or cancel them", open.join(", "))));
                }
            }
        }
        Ok(result)
    })?;
    match store.read_current() {
        Ok(after) if after.schedules.get(&team) == registration.as_ref() => {
            let retired = if crate::compact::needs_schedule(&after, &team) {
                document().ok().map(|document| crate::compact::Retired {
                    schedule_id: document.schedule.id.clone(),
                    next_item: document.schedule.next_item,
                    items: document
                        .schedule
                        .items
                        .iter()
                        .map(|item| item.id.clone())
                        .collect(),
                })
            } else {
                None
            };
            crate::compact::auto(store, &team, retired.as_ref());
        }
        Ok(_) => (),
        Err(error) => eprintln!("gaebu: runtime compaction deferred: {error}"),
    }
    Ok(result)
}

/// Explicit history maintenance follows the Schedule -> state lock order.
pub fn compact(store: &Store, caller: &Caller, heal: bool) -> Result<Value> {
    let snapshot = store.read_current()?;
    let actor = identity::actor(&snapshot, caller)?;
    let team = snapshot.members[&actor.member].team.clone();
    if heal
        && snapshot
            .teams
            .get(&team)
            .is_none_or(|t| t.commander != actor.member)
    {
        return Err(Error::new(
            Reason::NoGrant,
            "compact --heal requires the team commander",
        ));
    }
    let registration = snapshot.schedules.get(&team);
    let _file = registration
        .map(|reg| lock_path(Path::new(&reg.path)))
        .transpose()?;
    let current = store.read_current()?;
    identity::actor(&current, caller)?;
    if current.schedules.get(&team) != registration {
        return Err(conflict("schedule registration changed"));
    }
    let retired = registration.and_then(|reg| {
        read_document(Path::new(&reg.path))
            .ok()
            .filter(|document| document.schedule.id == reg.schedule_id)
            .map(|document| crate::compact::Retired {
                schedule_id: document.schedule.id,
                next_item: document.schedule.next_item,
                items: document
                    .schedule
                    .items
                    .into_iter()
                    .map(|item| item.id)
                    .collect(),
            })
    });
    if heal {
        crate::compact::run_heal(store, caller, &team, retired.as_ref())
    } else {
        crate::compact::run(store, &team, retired.as_ref())
    }
}

pub(super) fn lock_document(path: &Path) -> Result<(File, Document)> {
    let lock = lock_path(path)?;
    let document = read_document(path)?;
    Ok((lock, document))
}

fn guard_duty(st: &State, team: &str, document: &Document, duty: &Duty) -> Result<()> {
    let reference = duty
        .schedule_ref
        .as_ref()
        .ok_or_else(|| conflict("missing schedule link"))?;
    if reference.schedule_id != document.schedule.id {
        return Err(conflict("schedule origin mismatch"));
    }
    if let Some(retired) = document
        .schedule
        .retired
        .iter()
        .find(|r| r.id == reference.item_id)
    {
        return Err(match retired.state {
            RetiredState::Complete => conflict("schedule item is already complete"),
            RetiredState::Cancelled => Error::new(Reason::Cancelled, "schedule item cancelled"),
        });
    }
    if absent_retired(&document.schedule, &reference.item_id) {
        let item = archive::lookup(
            Path::new(&st.schedules[team].path),
            &document.schedule,
            &reference.item_id,
        )
        .map_err(|error| {
            if error.reason == Reason::NotFound {
                conflict("schedule item is retired")
            } else {
                error
            }
        })?;
        return Err(if item.cancellation.is_some() {
            Error::new(Reason::Cancelled, "schedule item cancelled")
        } else {
            conflict("schedule item is already complete")
        });
    }
    let item = &document.schedule.items[item_index(&document.schedule, &reference.item_id)?];
    let dispatch = item
        .dispatches
        .iter()
        .find(|d| d.token == reference.dispatch_token)
        .ok_or_else(|| conflict("dispatch token is not in schedule"))?;
    if dispatch.retired.is_some() {
        return Err(conflict("dispatch token was retired"));
    }
    if item.cancellation.is_some() {
        return Err(Error::new(Reason::Cancelled, "schedule item cancelled"));
    }
    if let Some(hold) = &item.hold {
        return Err(Error::new(
            Reason::Held,
            format!("schedule item held until {}", hold.until),
        ));
    }
    if item.completion.is_some() {
        return Err(conflict("schedule item is already complete"));
    }
    if !matching_origin(st, team, &dispatch.assignee)
        || !matching_origin(st, team, &dispatch.issuer)
    {
        return Err(conflict("dispatch belongs to another runtime origin"));
    }
    if dispatch.manifest_hash != reference.manifest_hash
        || dispatch.manifest_hash != manifest_hash(dispatch)?
        || duty.assignee != dispatch.assignee.id
        || duty.issuer != dispatch.issuer.id
        || !duty_matches(duty, dispatch)
    {
        return Err(conflict(
            "linked duty differs from immutable dispatch manifest",
        ));
    }
    let master = master(st, team, item)?;
    if master.id != dispatch.issuer.id {
        return Err(conflict("task master changed"));
    }
    if !dependencies_ready(&document.schedule, item) {
        return Err(conflict("schedule dependencies are incomplete"));
    }
    Ok(())
}

fn duty_matches(duty: &Duty, dispatch: &Dispatch) -> bool {
    let spec = &dispatch.spec;
    duty.intent == spec.intent
        && duty.outcome == spec.outcome
        && duty.scope == spec.scope
        && duty.decision_rights == spec.decision_rights
        && duty.escalate_on == spec.escalate_on
        && duty.done_criteria == spec.done_criteria
        && duty.plan_ref == spec.plan_ref
        && duty.exec_root == spec.exec_root
        && duty.source == dispatch.source
}

pub(super) fn dependencies_ready(schedule: &Schedule, item: &Item) -> bool {
    item.depends_on
        .iter()
        .all(|id| dependency_complete(schedule, id))
}

pub(super) fn dependency_complete(schedule: &Schedule, id: &str) -> bool {
    schedule
        .items
        .iter()
        .any(|d| d.id == id && d.completion.is_some() && d.cancellation.is_none())
        || schedule
            .retired
            .iter()
            .any(|d| d.id == id && d.state == RetiredState::Complete)
}

pub(super) fn master<'a>(st: &'a State, team: &str, item: &Item) -> Result<&'a Member> {
    let reference = item
        .owner_bu
        .as_ref()
        .ok_or_else(|| conflict("schedule item is unassigned"))?;
    if !matching_origin(st, team, reference) {
        return Err(conflict("owner unit belongs to another runtime origin"));
    }
    let bu = st
        .bus
        .get(&reference.id)
        .filter(|b| b.team == team && b.status == BuStatus::Open)
        .ok_or_else(|| conflict("owner unit is not open"))?;
    if authz::bu_cancelled(st, &bu.id) {
        return Err(Error::new(Reason::Cancelled, "owner unit is cancelled"));
    }
    let member = st
        .members
        .get(&bu.head)
        .ok_or_else(|| conflict("task master missing"))?;
    if member.status != MemberStatus::Active || member.binding.state != BindingState::Bound {
        return Err(conflict("task master is not bound"));
    }
    Ok(member)
}

fn commander(st: &State, actor: &Actor, team: &str) -> Result<()> {
    authz::check_actor(st, actor)?;
    if st
        .teams
        .get(team)
        .is_some_and(|t| t.commander == actor.member)
    {
        Ok(())
    } else {
        Err(Error::new(
            Reason::NoGrant,
            "only commander changes schedule registration or manager",
        ))
    }
}

pub(super) fn authorize_manager(
    st: &State,
    actor: &Actor,
    team: &str,
    item_scope: &[String],
) -> Result<()> {
    let member = authz::check_actor(st, actor)?;
    let reg = st
        .schedules
        .get(team)
        .ok_or_else(|| Error::new(Reason::NotFound, "team has no schedule"))?;
    let manager = reg.manager.as_deref().unwrap_or(&st.teams[team].commander);
    if member.team != team || manager != actor.member {
        return Err(Error::new(
            Reason::NoGrant,
            "not the current schedule manager",
        ));
    }
    authz::require(st, &actor.member, Verb::Assign, item_scope)
}

pub(super) fn authorize_master(st: &State, actor: &Actor, team: &str, item: &Item) -> Result<()> {
    authz::check_actor(st, actor)?;
    if master(st, team, item)?.id != actor.member {
        return Err(Error::new(
            Reason::NoGrant,
            "only the current task master may do this",
        ));
    }
    authz::require(st, &actor.member, Verb::Assign, &item.scope)
}

pub(super) fn operate<T>(
    store: &Store,
    caller: &Caller,
    args: &Input,
    auto_archive: bool,
    f: impl FnOnce(&State, &Actor, &str, &mut Document) -> Result<T>,
) -> Result<T> {
    let snapshot = store.read_current()?;
    let actor = identity::actor(&snapshot, caller)?;
    let team = snapshot.members[&actor.member].team.clone();
    let reg = snapshot
        .schedules
        .get(&team)
        .cloned()
        .ok_or_else(|| Error::new(Reason::NotFound, "team has no registered schedule"))?;
    let (_file, mut document) = lock_document(Path::new(&reg.path))?;
    let _state_lock = store::lock(store.home(), "state.lock", CLI_DEADLINE)?;
    let st = store.read_current()?;
    let actor = identity::actor(&st, caller)?;
    if st.schedules.get(&team) != Some(&reg) || document.schedule.id != reg.schedule_id {
        return Err(conflict("schedule registration changed"));
    }
    if args
        .expected_revision
        .is_some_and(|revision| revision != document.schedule.revision)
    {
        return Err(conflict("schedule revision changed"));
    }
    let before = document.schedule.clone();
    let result = f(&st, &actor, &team, &mut document)?;
    if before != document.schedule {
        if auto_archive {
            archive::sweep(
                &st,
                &team,
                &mut document,
                Path::new(&reg.path),
                false,
                store.archive_limit(),
                None,
            )?;
        }
        document.save(Path::new(&reg.path), Instant::now() + CLI_DEADLINE)?;
    }
    Ok(result)
}

fn lock_path(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("schedule path has no parent"))?;
    let name = format!(
        "{}.lock",
        path.file_name()
            .ok_or_else(|| invalid("invalid schedule path"))?
            .to_string_lossy()
    );
    store::lock(parent, &name, CLI_DEADLINE)
}

pub(super) fn runtime_duty<'a>(
    st: &'a State,
    schedule: &Schedule,
    item: &Item,
    dispatch: &Dispatch,
) -> Result<Option<&'a Duty>> {
    let matches: Vec<_> = st
        .duties
        .values()
        .filter(|d| {
            d.schedule_ref
                .as_ref()
                .is_some_and(|r| r.schedule_id == schedule.id && r.dispatch_token == dispatch.token)
        })
        .collect();
    if matches.len() > 1 {
        return Err(conflict("multiple runtime duties share one dispatch token"));
    }
    if let Some(duty) = matches.first()
        && (!duty_matches(duty, dispatch)
            || duty.assignee != dispatch.assignee.id
            || duty.issuer != dispatch.issuer.id
            || duty.schedule_ref.as_ref().unwrap().manifest_hash != dispatch.manifest_hash
            || item.id != duty.schedule_ref.as_ref().unwrap().item_id)
    {
        return Err(conflict("runtime duty manifest differs from schedule"));
    }
    Ok(matches.first().copied())
}
