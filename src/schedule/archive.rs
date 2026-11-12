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
    let mut retired = vec![];
    let mut deferred = vec![];
    let mut bytes = vec![];
    for item in document
        .schedule
        .items
        .iter()
        .filter(|item| item.completion.is_some() || item.cancellation.is_some())
    {
        let eligible = if item.hold.is_some() {
            Err(Error::new(Reason::Held, "held items cannot be archived"))
        } else {
            operations::ensure_terminated(st, team, &document.schedule, item)
        };
        if let Err(error) = eligible {
            deferred.push(json!({"item":item.id,"reason":error.message}));
            continue;
        }
        let item_digest = digest(item)?;
        serde_json::to_writer(
            &mut bytes,
            &Entry {
                schedule_id: document.schedule.id.clone(),
                digest: item_digest.clone(),
                item: item.clone(),
            },
        )
        .map_err(io_error)?;
        bytes.push(b'\n');
        retired.push(Retired {
            id: item.id.clone(),
            state: if item.cancellation.is_some() {
                RetiredState::Cancelled
            } else {
                RetiredState::Complete
            },
            digest: item_digest,
            last_dispatch: item
                .dispatches
                .iter()
                .filter_map(|dispatch| dispatch.token.strip_prefix("x-")?.parse::<u64>().ok())
                .max()
                .unwrap_or(0),
            depends_on: if item.cancellation.is_some() {
                item.depends_on.clone()
            } else {
                vec![]
            },
        });
    }
    if !retired.is_empty() {
        // Duplicates left by an interrupted publish are harmless: tombstones
        // name the exact digest, so appending never scans historical data.
        if let Err(error) = crate::archive::append(&path(schedule_path), &bytes) {
            if strict {
                return Err(error);
            }
            for item in retired.drain(..) {
                deferred.push(json!({"item":item.id,"reason":error.message}));
            }
        }
    }
    let ids: BTreeSet<_> = retired.iter().map(|item| item.id.as_str()).collect();
    document
        .schedule
        .items
        .retain(|item| !ids.contains(item.id.as_str()));
    let ids: Vec<_> = retired.iter().map(|item| item.id.clone()).collect();
    document.schedule.retired.extend(retired);
    Ok(json!({"retired":ids,"deferred":deferred,"retired_total":document.schedule.retired.len()}))
}

pub(super) fn load(
    schedule_path: &Path,
    schedule: &Schedule,
    requested: &[Retired],
) -> Result<Vec<Item>> {
    if requested.is_empty() {
        return Ok(vec![]);
    }
    let wanted: BTreeMap<_, _> = requested
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let ids: BTreeSet<_> = schedule
        .items
        .iter()
        .map(|item| item.id.as_str())
        .chain(schedule.retired.iter().map(|item| item.id.as_str()))
        .collect();
    let mut found = BTreeMap::new();
    crate::archive::scan(&path(schedule_path), |entry: Entry| {
        if entry.schedule_id == schedule.id
            && wanted.get(entry.item.id.as_str()).is_some_and(|retired| {
                entry.digest == retired.digest
                    && digest(&entry.item).is_ok_and(|digest| digest == retired.digest)
            })
        {
            found.insert(entry.item.id.clone(), entry.item);
        }
    })?;
    let mut result = Vec::with_capacity(requested.len());
    for retired in requested {
        let mut check = || -> Result<Item> {
            let item = found
                .remove(&retired.id)
                .ok_or_else(|| invalid("archive entry missing or corrupt"))?;
            let (_, last_dispatch) = validate_item(&item, &mut BTreeSet::new())?;
            validate_dependencies(&item.id, &item.depends_on, &ids)?;
            let state = if item.cancellation.is_some() {
                RetiredState::Cancelled
            } else if item.completion.is_some() {
                RetiredState::Complete
            } else {
                return Err(invalid("archived item is not terminal"));
            };
            if state != retired.state
                || last_dispatch != retired.last_dispatch
                || state == RetiredState::Cancelled && item.depends_on != retired.depends_on
            {
                return Err(invalid("archive item disagrees with tombstone"));
            }
            Ok(item)
        };
        result.push(check().map_err(|error| {
            Error::new(
                Reason::StoreCorrupt,
                format!("{}: {}", retired.id, error.message),
            )
        })?);
    }
    Ok(result)
}
