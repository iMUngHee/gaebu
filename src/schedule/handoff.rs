use super::commands::Input;
use super::runtime;
use super::*;
use crate::{
    identity::{self, Caller},
    ops,
    store::{self, CLI_DEADLINE, Store},
};
use serde_json::{Value, json};

fn team<'a>(st: &'a State, value: &str) -> Result<&'a Team> {
    st.teams
        .get(value)
        .or_else(|| st.teams.values().find(|team| team.name == value))
        .ok_or_else(|| Error::new(Reason::NotFound, "team not found"))
}

pub(super) fn offer(store: &Store, caller: &Caller, args: &Input) -> Result<Value> {
    if args.withdraw == args.to.is_some() {
        return Err(invalid("choose exactly one of to or withdraw"));
    }
    store.transact(CLI_DEADLINE, |st| {
        let actor = identity::actor(st, caller)?;
        let owner = st.members[&actor.member].team.clone();
        runtime::commander(st, &actor, &owner)?;
        let target = if args.withdraw {
            None
        } else {
            let target = team(st, args.to.as_deref().unwrap())?;
            if target.id == owner || target.status != TeamStatus::Active {
                return Err(conflict("offer target must be another active team"));
            }
            Some(target.id.clone())
        };
        let registration = st
            .schedules
            .get_mut(&owner)
            .ok_or_else(|| Error::new(Reason::NotFound, "team has no schedule"))?;
        registration.offer = target.clone();
        Ok(json!({"offered":target.is_some(),"to":target,"schedule_id":registration.schedule_id}))
    })
}

#[derive(Serialize)]
struct SourceHold {
    target: String,
    until: String,
    cleared: bool,
}

fn source_holds(st: &State, source: &str, schedule_id: &str) -> Vec<SourceHold> {
    st.bus
        .values()
        .filter(|bu| bu.team == source)
        .filter_map(|bu| {
            bu.hold.as_ref().map(|hold| SourceHold {
                target: bu.id.clone(),
                until: hold.until.clone(),
                cleared: false,
            })
        })
        .chain(
            st.duties
                .values()
                .filter(|duty| duty.team == source)
                .filter_map(|duty| {
                    duty.hold.as_ref().map(|hold| SourceHold {
                        target: duty.id.clone(),
                        until: hold.until.clone(),
                        cleared: matches!(
                            duty.status,
                            DutyStatus::Proposed | DutyStatus::Accepted | DutyStatus::Blocked
                        ) && duty
                            .schedule_ref
                            .as_ref()
                            .is_some_and(|reference| reference.schedule_id == schedule_id),
                    })
                }),
        )
        .collect()
}

pub(super) fn adopt(store: &Store, caller: &Caller, args: &Input) -> Result<Value> {
    if args.team.is_some() == args.path.is_some() {
        return Err(invalid("choose exactly one of team or path"));
    }
    let before = store.read_current()?;
    let actor = identity::actor(&before, caller)?;
    let target = before.members[&actor.member].team.clone();
    runtime::commander(&before, &actor, &target)?;
    let source = args
        .team
        .as_deref()
        .map(|name| team(&before, name).map(|team| team.id.clone()))
        .transpose()?;
    if source.as_deref() == Some(&target) {
        return Err(invalid("cannot adopt the caller team's own schedule"));
    }
    let registration = source
        .as_ref()
        .and_then(|source| before.schedules.get(source))
        .cloned();
    let existing = before.schedules.get(&target);
    let path = if let Some(path) = &args.path {
        let path = store::canonical_path(path)?;
        runtime::refuse_runtime_home(store, &path)?;
        archive::refuse_sidecar_name(&path)?;
        path
    } else if let Some(registration) = &registration {
        Path::new(&registration.path).to_owned()
    } else if let Some(existing) = existing.filter(|registration| {
        source
            .as_ref()
            .is_some_and(|source| registration.predecessors.contains(source))
    }) {
        Path::new(&existing.path).to_owned()
    } else {
        return Err(Error::new(Reason::NotFound, "source team has no schedule"));
    };
    let (_lock, mut document) = runtime::lock_document(&path)?;
    let original = document.schedule.clone();
    let first = if existing.is_some_and(|reg| reg.schedule_id == document.schedule.id) {
        None
    } else {
        args.evidence
            .as_deref()
            .map(|quote| human_evidence(store, caller, quote, args.explicit))
            .transpose()?
    };
    store.transact(CLI_DEADLINE,|st| {
        let actor=identity::actor(st,caller)?;
        let current=st.members[&actor.member].team.clone();
        if current!=target {return Err(conflict("adopting team changed"));}
        runtime::commander(st,&actor,&target)?;
        if let Some(existing)=st.schedules.get(&target) {
            if existing.schedule_id==document.schedule.id {
                return Ok(json!({"adopted":true,"existing":true,"schedule_id":existing.schedule_id,"path":existing.path}));
            }
            return Err(conflict("team already has a canonical schedule"));
        }
        if let Some(source)=&source {
            let owner=st.teams.get(source).ok_or_else(||conflict("source team changed"))?;
            if st.schedules.get(source)!=registration.as_ref() {
                return Err(conflict("source schedule registration changed"));
            }
            let registered=registration.as_ref().ok_or_else(||conflict("source has no schedule"))?;
            if registered.schedule_id!=document.schedule.id || Path::new(&registered.path)!=path {
                return Err(conflict("schedule identity changed"));
            }
            if owner.status==TeamStatus::Active && registered.offer.as_deref()!=Some(&target) {
                return Err(Error::new(Reason::NoGrant,"active source team must offer its schedule to this team"));
            }
        } else if st.schedules.values().any(|reg|reg.schedule_id==document.schedule.id || Path::new(&reg.path)==path) {
            return Err(conflict("schedule is already registered to another team"));
        }
        let evidence=if let Some(quote)=args.evidence.as_deref() {
            let verified=human_evidence(store,caller,quote,args.explicit)?;
            if first.as_ref().is_none_or(|first|first.sha256!=verified.sha256 || first.prompt_seq!=verified.prompt_seq) {
                return Err(Error::new(Reason::NoUserEvidence,"current prompt changed"));
            }
            Some(verified)
        } else {None};
        if source.is_none() && evidence.is_none() {
            return Err(Error::new(Reason::NoUserEvidence,"adopt --path requires current-turn evidence"));
        }
        let holds=source.as_ref().map(|source|source_holds(st,source,&document.schedule.id)).unwrap_or_default();
        if evidence.is_none() && !holds.is_empty() {
            let conditions=holds.iter().map(|hold|format!("{}: {}",hold.target,hold.until)).collect::<Vec<_>>().join("; ");
            return Err(Error::new(Reason::Held,format!("source team has human holds: {conditions}; provide --evidence to hand off")));
        }
        let at=now_ms();
        let by=origin(st,&target,&actor.member);
        let reason=source.as_ref().map_or_else(||"adopted from an unregistered schedule".to_owned(),|source|format!("handed off from team {source}"));
        let mut retired_dispatches=0;
        for item in &mut document.schedule.items {
            for dispatch in &mut item.dispatches {
                if dispatch.retired.is_none() {
                    dispatch.retired=Some(Decision{by:by.clone(),at,reason:reason.clone()});
                    retired_dispatches+=1;
                }
            }
            if item.completion.is_none() && item.cancellation.is_none() {item.owner_bu=None;}
        }
        let mut affected=BTreeSet::new();
        let mut handed_off_duties=0;
        let mut lifted_holds=0;
        if let Some(source)=&source {
            for duty in st.duties.values_mut().filter(|duty|duty.team==*source && duty.schedule_ref.as_ref().is_some_and(|reference|reference.schedule_id==document.schedule.id)
                && matches!(duty.status,DutyStatus::Proposed|DutyStatus::Accepted|DutyStatus::Blocked)) {
                duty.version=duty.version.checked_add(1).ok_or_else(||invalid("duty version exhausted"))?;
                duty.status=DutyStatus::HandedOff;duty.updated_at=at;duty.note=Some(format!("schedule handed off to team {target}"));
                if duty.hold.take().is_some(){lifted_holds+=1;}
                affected.insert(duty.assignee.clone());handed_off_duties+=1;
            }
            affected.insert(st.teams[source].commander.clone());
        }
        let mut released_leases=0;
        if let Some(source)=&source {
            let accepted:BTreeSet<_>=st.duties.values().filter(|d|d.status==DutyStatus::Accepted).map(|d|d.assignee.as_str()).collect();
            st.leases.retain(|_,lease| {
                let release=st.members.get(&lease.holder).is_some_and(|member|member.team==*source) && !accepted.contains(lease.holder.as_str());
                if release {released_leases+=1;}!release
            });
        }
        let mut predecessors=registration.as_ref().map(|reg|reg.predecessors.clone()).unwrap_or_default();
        if let Some(source)=&source {predecessors.insert(source.clone());st.schedules.remove(source);}
        predecessors.remove(&target);
        st.schedules.insert(target.clone(),ScheduleRegistration{schedule_id:document.schedule.id.clone(),path:path.to_string_lossy().into_owned(),manager:None,offer:None,predecessors});
        let text=json!({"schedule_id":document.schedule.id,"from":source,"to":target,"by":by,"holds":holds,"evidence":evidence,"former_runtime_holds":if source.is_none(){"not recoverable"}else{"recorded"}}).to_string();
        let affected=affected.into_iter().collect::<Vec<_>>();
        if let Some(source)=&source {
            ops::record_change(st,at,&actor.member,&affected,&text,ops::Notify::Act,(RecordKind::Handoff,source));
        }
        ops::record_change(st,at,&actor.member,&[],&text,ops::Notify::Record,(RecordKind::Handoff,&target));
        // A document-only commit retires the old tokens even if state publication fails.
        if document.schedule!=original {
            document.save(&path,Instant::now()+CLI_DEADLINE)?;
        }
        Ok(json!({"adopted":true,"schedule_id":document.schedule.id,"path":path,"from":source,"retired_dispatches":retired_dispatches,
            "handed_off_duties":handed_off_duties,"released_leases":released_leases,"lifted_holds":lifted_holds}))
    })
}

pub(crate) fn predecessor_duty(st: &State, team: &str, duty: &Duty) -> bool {
    st.schedules.get(team).is_some_and(|registration| {
        registration.predecessors.contains(&duty.team)
            && duty
                .schedule_ref
                .as_ref()
                .is_some_and(|reference| reference.schedule_id == registration.schedule_id)
    })
}
