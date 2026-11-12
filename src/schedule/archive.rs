use super::*;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    schedule_id: String,
    digest: String,
    item: Item,
}

pub(super) fn path(schedule: &Path) -> PathBuf {
    let mut name = schedule.as_os_str().to_owned();
    name.push(".archive.jsonl");
    name.into()
}

fn digest(item: &Item) -> Result<String> {
    let bytes = serde_json::to_string(item).map_err(io_error)?;
    Ok(crate::evidence::sha256_hex(&bytes)[..16].to_owned())
}

pub(super) fn sweep(
    st: &State,
    team: &str,
    document: &mut Document,
    schedule_path: &Path,
    strict: bool,
) -> Result<Value> {
    let archive = path(schedule_path);
    let mut retired = vec![];
    let mut deferred = vec![];
    let candidates: Vec<_> = document
        .schedule
        .items
        .iter()
        .filter(|item| item.completion.is_some() || item.cancellation.is_some())
        .cloned()
        .collect();
    for item in candidates {
        let eligible = if item.hold.is_some() {
            Err(Error::new(Reason::Held, "held items cannot be archived"))
        } else {
            operations::ensure_terminated(st, team, &document.schedule, &item)
        };
        if let Err(error) = eligible {
            deferred.push(json!({"item":item.id,"reason":error.message}));
            continue;
        }
        let item_digest = digest(&item)?;
        let write = (|| {
            let mut exists = false;
            crate::archive::scan(&archive, |entry: Entry| {
                if entry.schedule_id == document.schedule.id
                    && entry.digest == item_digest
                    && entry.item == item
                {
                    exists = true;
                }
            })?;
            let bytes = if exists {
                None
            } else {
                Some(
                    serde_json::to_vec(&Entry {
                        schedule_id: document.schedule.id.clone(),
                        digest: item_digest.clone(),
                        item: item.clone(),
                    })
                    .map_err(io_error)?,
                )
            };
            crate::archive::append(&archive, bytes.as_deref())
        })();
        if let Err(error) = write {
            if strict {
                return Err(error);
            }
            deferred.push(json!({"item":item.id,"reason":error.message}));
            continue;
        }
        let last_dispatch = item
            .dispatches
            .iter()
            .filter_map(|dispatch| dispatch.token.strip_prefix("x-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        document.schedule.retired.push(Retired {
            id: item.id.clone(),
            state: if item.cancellation.is_some() {
                RetiredState::Cancelled
            } else {
                RetiredState::Complete
            },
            digest: item_digest,
            last_dispatch,
            depends_on: if item.cancellation.is_some() {
                item.depends_on.clone()
            } else {
                vec![]
            },
        });
        document
            .schedule
            .items
            .retain(|active| active.id != item.id);
        retired.push(item.id);
    }
    Ok(
        json!({"retired":retired,"deferred":deferred,"retired_total":document.schedule.retired.len()}),
    )
}

pub(super) fn load(schedule_path: &Path, schedule: &Schedule, retired: &Retired) -> Result<Item> {
    let mut found = None;
    crate::archive::scan(&path(schedule_path), |entry: Entry| {
        if entry.schedule_id == schedule.id
            && entry.item.id == retired.id
            && entry.digest == retired.digest
            && digest(&entry.item).is_ok_and(|d| d == retired.digest)
        {
            found = Some(entry.item);
        }
    })?;
    let item = found.ok_or_else(|| {
        Error::new(
            Reason::StoreCorrupt,
            format!("archive entry missing or corrupt for {}", retired.id),
        )
    })?;
    if (retired.state == RetiredState::Complete
        && (item.completion.is_none() || item.cancellation.is_some()))
        || (retired.state == RetiredState::Cancelled && item.cancellation.is_none())
        || (retired.state == RetiredState::Cancelled && retired.depends_on != item.depends_on)
        || item
            .dispatches
            .iter()
            .filter_map(|d| d.token.strip_prefix("x-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            != retired.last_dispatch
    {
        return Err(Error::new(
            Reason::StoreCorrupt,
            "archive item disagrees with tombstone",
        ));
    }
    let mut restored = schedule.clone();
    restored.retired.retain(|entry| entry.id != item.id);
    restored.items.push(item.clone());
    validate_contents(&restored, MAX_ITEMS + 1)
        .map_err(|error| Error::new(Reason::StoreCorrupt, error.message))?;
    Ok(item)
}
