use super::runtime::{dependencies_ready, master, runtime_duty};
use super::*;
use crate::{authz, store::Store};
use serde_json::{Value, json};

pub fn view(store: &Store, st: &State, team: &str) -> Result<Value> {
    view_mode(store, st, team, false)
}

pub(super) fn show(store: &Store, st: &State, team: &str, args: &commands::Input) -> Result<Value> {
    if let Some(id) = &args.item {
        let reg = st
            .schedules
            .get(team)
            .ok_or_else(|| Error::new(Reason::NotFound, "team has no schedule"))?;
        let document = read_document(Path::new(&reg.path))?;
        if document.schedule.id != reg.schedule_id {
            return Err(conflict("schedule identity changed"));
        }
        if let Some(item) = document.schedule.items.iter().find(|item| &item.id == id) {
            return Ok(item_view(
                store,
                st,
                team,
                &document.schedule,
                item,
                true,
                false,
            ));
        }
        let retired = document
            .schedule
            .retired
            .iter()
            .find(|item| &item.id == id)
            .ok_or_else(|| Error::new(Reason::NotFound, "schedule item not found"))?;
        let item = archive::load(Path::new(&reg.path), &document.schedule, retired)?;
        let mut restored = document.schedule.clone();
        restored.items.push(item.clone());
        return Ok(item_view(store, st, team, &restored, &item, true, true));
    }
    view_mode(store, st, team, args.all)
}

fn view_mode(store: &Store, st: &State, team: &str, all: bool) -> Result<Value> {
    let Some(reg) = st.schedules.get(team) else {
        return Ok(json!({"registered":false,"items":[]}));
    };
    if let Err(error) = store.read_current() {
        return Ok(
            json!({"registered":true,"schedule_id":reg.schedule_id,"path":reg.path,"items":[],"error":{"reason":error.reason,"message":error.message}}),
        );
    }
    let document = match read_document(Path::new(&reg.path)) {
        Ok(document) if document.schedule.id == reg.schedule_id => document,
        Ok(_) => {
            return Ok(
                json!({"registered":true,"schedule_id":reg.schedule_id,"path":reg.path,"items":[],"error":{"reason":"conflict","message":"schedule identity changed"}}),
            );
        }
        Err(error) => {
            return Ok(
                json!({"registered":true,"schedule_id":reg.schedule_id,"path":reg.path,"items":[],"error":{"reason":error.reason,"message":error.message}}),
            );
        }
    };
    let manager_id = reg
        .manager
        .as_ref()
        .or_else(|| st.teams.get(team).map(|t| &t.commander));
    let manager=manager_id.and_then(|id|st.members.get(id)).map(|m| {
        let state=if m.status!=MemberStatus::Active || authz::bu_cancelled(st,&m.bu)
            || !authz::effective_grants(st,&m.id).iter().any(|g|g.verbs.contains(&Verb::Assign)&&!g.scope.is_empty()) {"unknown"}
            else if m.binding.state==BindingState::Bound && m.binding.session_id.as_deref().and_then(|id|store.last_seen(id)).is_some_and(|at|now_ms().saturating_sub(at)>7_200_000) {"quiet"}
            else {match m.binding.state {BindingState::Bound=>"bound",BindingState::Pending=>"pending",BindingState::Vacant=>"vacant"}};
        json!({"member":m.id,"alias":m.binding.alias,"mode":if reg.manager.is_some(){"delegated"}else{"commander"},"state":state})
    });
    let mut items: Vec<Value> = document
        .schedule
        .items
        .iter()
        .map(|item| item_view(store, st, team, &document.schedule, item, false, false))
        .collect();
    items.sort_by_key(|item| {
        (
            item["priority"].as_u64().unwrap_or(100),
            item["id"].as_str().unwrap_or("").to_owned(),
        )
    });
    let stages = [
        "unassigned",
        "dependency_wait",
        "ready",
        "dispatching",
        "awaiting_acceptance",
        "working",
        "blocked",
        "vacant",
        "review",
        "complete",
        "cancelled",
        "held",
        "unknown",
    ];
    let mut counts: BTreeMap<&str, usize> = stages.iter().map(|stage| (*stage, 0)).collect();
    for item in &items {
        if let Some(count) = item["stage"]
            .as_str()
            .and_then(|stage| counts.get_mut(stage))
        {
            *count += 1;
        }
    }
    counts.insert("queued", counts["unassigned"] + counts["dependency_wait"]);
    counts.insert("total", items.len());
    counts.insert("retired", document.schedule.retired.len());
    if all {
        for retired in &document.schedule.retired {
            let item = archive::load(Path::new(&reg.path), &document.schedule, retired)?;
            let mut restored = document.schedule.clone();
            restored.items.push(item.clone());
            items.push(item_view(store, st, team, &restored, &item, false, true));
        }
    } else {
        items.retain(|item| {
            item["stage"] != "complete"
                && (item["stage"] != "cancelled"
                    || item["dispatches"].as_array().is_some_and(|dispatches| {
                        dispatches.iter().any(|dispatch| {
                            matches!(
                                dispatch["status"].as_str(),
                                Some("proposed" | "accepted" | "blocked")
                            )
                        })
                    }))
        });
    }
    let next_action = [
        "unknown",
        "held",
        "blocked",
        "vacant",
        "dispatching",
        "review",
        "ready",
        "unassigned",
        "dependency_wait",
        "awaiting_acceptance",
        "working",
    ]
    .iter()
    .find_map(|stage| {
        items
            .iter()
            .find(|item| item["stage"] == *stage)
            .and_then(|item| item["next_action"].as_str())
    })
    .unwrap_or("지금 필요한 일정 판단이 없습니다");
    Ok(
        json!({"registered":true,"schedule_id":reg.schedule_id,"path":reg.path,"revision":document.schedule.revision,"manager":manager,"items":items,"counts":counts,"next_action":next_action}),
    )
}

fn item_view(
    store: &Store,
    st: &State,
    team: &str,
    schedule: &Schedule,
    item: &Item,
    full: bool,
    archived: bool,
) -> Value {
    let mut reasons = vec![];
    let mut stage = "ready";
    let owner = item
        .owner_bu
        .as_ref()
        .filter(|r| matching_origin(st, team, r))
        .and_then(|r| st.bus.get(&r.id));
    let master_result = master(st, team, item);
    let dispatches:Vec<_>=item.dispatches.iter().map(|dispatch| {
        let joined=if matching_origin(st,team,&dispatch.assignee) {runtime_duty(st,schedule,dispatch)} else {Err(conflict("foreign runtime origin"))};
        let (duty,status)=match joined {
            Ok(Some(duty))=>(Some(duty.id.clone()),match duty.status {DutyStatus::Proposed=>"proposed",DutyStatus::Accepted=>"accepted",DutyStatus::Blocked=>"blocked",DutyStatus::Done=>"done",DutyStatus::Cancelled=>"cancelled",DutyStatus::Declined=>"declined",DutyStatus::HandedOff=>"handed_off"}),
            Ok(None)=>(None,if archived {"archived"} else {"dispatching"}),Err(_)=>(None,"unknown"),
        };
        let mut value = json!({"key":dispatch.key,"token":dispatch.token,"assignee":dispatch.assignee,"duty":duty,"status":status,"retired":dispatch.retired,"manifest_hash":dispatch.manifest_hash});
        if full {
            value["spec"] = json!(dispatch.spec);
            value["source"] = json!(dispatch.source);
        }
        value
    }).collect();
    if item.cancellation.is_some() {
        stage = "cancelled";
    } else if item.completion.is_some() {
        stage = "complete";
    } else if let Some(hold) = &item.hold {
        stage = "held";
        reasons.push(hold.until.clone());
    } else if item.owner_bu.is_none() {
        stage = "unassigned";
        reasons.push("choose an owner unit".into());
    } else if let Err(error) = &master_result {
        stage = "unknown";
        reasons.push(error.message.clone());
    } else if !dependencies_ready(schedule, item) {
        stage = "dependency_wait";
        reasons.extend(
            item.depends_on
                .iter()
                .filter(|id| !super::runtime::dependency_complete(schedule, id))
                .map(|id| format!("waiting for {id} acceptance")),
        );
    } else {
        let mut states = vec![];
        for dispatch in latest_dispatches(item)
            .into_iter()
            .filter(|d| d.retired.is_none())
        {
            let state = match runtime_duty(st, schedule, dispatch) {
                Ok(None) => "dispatching",
                Err(_) => "unknown",
                Ok(Some(duty)) => match duty.status {
                    DutyStatus::Proposed => "awaiting_acceptance",
                    DutyStatus::Blocked => "blocked",
                    DutyStatus::Accepted => match crate::card::stage(
                        st,
                        duty,
                        now_ms(),
                        st.members
                            .get(&duty.assignee)
                            .and_then(|m| m.binding.session_id.as_deref())
                            .and_then(|id| store.last_seen(id)),
                    ) {
                        crate::card::Stage::Held => "held",
                        crate::card::Stage::AwaitingAnswer => "blocked",
                        crate::card::Stage::Vacant => "vacant",
                        _ => "working",
                    },
                    DutyStatus::Done => "review",
                    _ => "blocked",
                },
            };
            states.push(state);
        }
        for candidate in [
            "unknown",
            "held",
            "blocked",
            "vacant",
            "dispatching",
            "awaiting_acceptance",
            "working",
            "review",
        ] {
            if states.contains(&candidate) {
                stage = candidate;
                break;
            }
        }
        if stage == "blocked" {
            reasons.push("a worker attempt needs a decision or replacement".into());
        }
        if stage == "dispatching" {
            reasons.push("assignment is committed; reconcile its runtime duty".into());
        }
        if stage == "unknown" {
            reasons.push("runtime join could not be established".into());
        }
    }
    let (label, next) = match stage {
        "unassigned" => ("미배정", "일정 담당자가 책임 부를 정한다"),
        "dependency_wait" => ("선행 대기", "선행 과업 검수 완료를 기다린다"),
        "ready" => ("실행 가능", "과업 책임자가 실행하거나 워커에게 배분한다"),
        "dispatching" => ("배정 복구", "동일 token으로 reconcile한다"),
        "awaiting_acceptance" => ("수락 대기", "배정된 워커가 소임을 수락한다"),
        "working" => ("진행", "담당자가 결과를 수행한다"),
        "blocked" => ("막힘", "과업 책임자가 막힌 결정을 해결한다"),
        "vacant" => ("자리 비움", "책임자가 세션 바인딩을 확인한다"),
        "review" => ("검수 대기", "과업 책임자가 근거를 검토하고 complete한다"),
        "complete" => ("완료", "후속 실행 가능 항목을 확인한다"),
        "cancelled" => ("취소", "남은 소임과 자원을 정리한다"),
        "held" => ("보류", "사람의 명시적인 재개를 기다린다"),
        _ => ("확인 필요", "불명확한 연결을 확인한다"),
    };
    json!({"id":item.id,"title":item.title,"stage":stage,"stage_label":label,"reasons":reasons,"priority":item.priority,"scope":item.scope,"depends_on":item.depends_on,"owner_bu":item.owner_bu,"owner_display":owner.map(|b|b.name.clone()),"master":master_result.ok().map(|m|json!({"member":m.id,"alias":m.binding.alias})),"pm_ref":item.pm_ref,"pm_ref_status":if item.pm_ref.is_some(){"unresolved"}else{"unlinked"},"hold":item.hold,"cancellation":item.cancellation,"completion":item.completion,"dispatches":dispatches,"next_action":next})
}
