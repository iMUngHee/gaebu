use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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

pub(super) fn empty_head(id: &str) -> ArchiveHead {
    ArchiveHead {
        bytes: 0,
        rows: 0,
        chain: crate::evidence::sha256_hex(id),
        last_item: 0,
        last_dispatch: 0,
    }
}

fn advance(head: &mut ArchiveHead, line: &[u8], item: &Item) -> Result<()> {
    let mut previous = [0u8; 32];
    for (index, byte) in previous.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&head.chain[index * 2..index * 2 + 2], 16).map_err(io_error)?;
    }
    let mut hash = Sha256::new();
    hash.update(previous);
    hash.update(line);
    head.chain = format!("{:x}", hash.finalize());
    head.bytes = head
        .bytes
        .checked_add(line.len() as u64)
        .ok_or_else(|| invalid("archive bytes exhausted"))?;
    head.rows = head
        .rows
        .checked_add(1)
        .ok_or_else(|| invalid("archive rows exhausted"))?;
    head.last_item = head.last_item.max(item_number(&item.id)?);
    let (_, dispatch) = validate_item(item, &mut BTreeSet::new())?;
    head.last_dispatch = head.last_dispatch.max(dispatch);
    Ok(())
}

fn encode(id: &str, item: &Item) -> Result<Vec<u8>> {
    let mut line = serde_json::to_vec(&Entry {
        schedule_id: id.into(),
        digest: digest(item)?,
        item: item.clone(),
    })
    .map_err(io_error)?;
    line.push(b'\n');
    Ok(line)
}

pub(super) fn sweep(
    st: &State,
    team: &str,
    document: &mut Document,
    schedule_path: &Path,
    strict: bool,
) -> Result<Value> {
    let migrated = document.schedule.schema_version == 1;
    if migrated {
        let migration = (|| -> Result<()> {
            let items = legacy_load(
                schedule_path,
                &document.schedule,
                &document.schedule.retired,
            )?;
            let mut head = empty_head(&document.schedule.id);
            let mut bytes = vec![];
            for item in items {
                let line = encode(&document.schedule.id, &item)?;
                advance(&mut head, &line, &item)?;
                bytes.extend(line);
            }
            crate::store::replace_until(
                &path(schedule_path),
                &bytes,
                Instant::now() + crate::store::CLI_DEADLINE,
            )?;
            document.schedule.archive = Some(head);
            document.schedule.schema_version = 2;
            Ok(())
        })();
        if let Err(error) = migration {
            if strict {
                return Err(error);
            }
            return Ok(
                json!({"retired":[], "pruned":[], "deferred":[{"reason":error.message}], "retired_total":retired_total(&document.schedule)}),
            );
        }
    }
    let mut head = document
        .schedule
        .archive
        .clone()
        .ok_or_else(|| invalid("missing archive head"))?;
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
        let line = encode(&document.schedule.id, item)?;
        advance(&mut head, &line, item)?;
        bytes.extend(line);
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
    // A sweep that retires nothing leaves the sidecar alone so ordinary Schedule
    // writes pay no archive fsync; the next append drops any unpublished tail.
    let committed = document
        .schedule
        .archive
        .as_ref()
        .ok_or_else(|| invalid("missing archive head"))?
        .bytes;
    if (strict || !bytes.is_empty())
        && let Err(error) = crate::archive::append_prefix(&path(schedule_path), committed, &bytes)
    {
        if strict {
            return Err(error);
        }
        for item in retired.drain(..) {
            deferred.push(json!({"item":item.id,"reason":error.message}));
        }
        return Ok(
            json!({"retired":[],"pruned":[],"deferred":deferred,"retired_total":retired_total(&document.schedule)}),
        );
    }
    document.schedule.archive = Some(head);
    let ids: BTreeSet<_> = retired.iter().map(|item| item.id.as_str()).collect();
    document
        .schedule
        .items
        .retain(|item| !ids.contains(item.id.as_str()));
    let ids: Vec<_> = retired.iter().map(|item| item.id.clone()).collect();
    document.schedule.retired.extend(retired);
    let mut pruned = vec![];
    if !migrated {
        let referenced: BTreeSet<_> = document
            .schedule
            .items
            .iter()
            .flat_map(|item| item.depends_on.iter().map(String::as_str))
            .chain(
                st.duties
                    .values()
                    .filter_map(|duty| duty.schedule_ref.as_ref())
                    .filter(|r| r.schedule_id == document.schedule.id)
                    .map(|r| r.item_id.as_str()),
            )
            .collect();
        document.schedule.retired.retain(|item| {
            if referenced.contains(item.id.as_str()) {
                true
            } else {
                pruned.push(item.id.clone());
                false
            }
        });
    }
    Ok(
        json!({"retired":ids,"pruned":pruned,"deferred":deferred,"retired_total":retired_total(&document.schedule)}),
    )
}

fn legacy_load(
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
            validate_dependencies(&item.id, &item.depends_on, &ids)?;
            check_tombstone(&item, retired)?;
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

fn check_tombstone(item: &Item, retired: &Retired) -> Result<()> {
    if item.cancellation.is_none() && item.completion.is_none() {
        return Err(invalid("archived item is not terminal"));
    }
    let (_, last_dispatch) = validate_item(item, &mut BTreeSet::new())?;
    if digest(item)? != retired.digest
        || last_dispatch != retired.last_dispatch
        || (item.cancellation.is_some()) != (retired.state == RetiredState::Cancelled)
        || retired.state == RetiredState::Cancelled && item.depends_on != retired.depends_on
    {
        return Err(invalid("archive item disagrees with tombstone"));
    }
    Ok(())
}

/// Validate the complete committed prefix, including rows other than the requested id.
pub(super) fn load(schedule_path: &Path, schedule: &Schedule) -> Result<Vec<Item>> {
    if schedule.schema_version == 1 {
        return legacy_load(schedule_path, schedule, &schedule.retired);
    }
    let head = schedule
        .archive
        .as_ref()
        .ok_or_else(|| invalid("missing archive head"))?;
    let mut actual = empty_head(&schedule.id);
    let mut items = vec![];
    let mut ids = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    let live: BTreeSet<_> = schedule.items.iter().map(|item| item.id.as_str()).collect();
    let result = crate::archive::scan_prefix(&path(schedule_path), head.bytes, |line| {
        let entry: Entry = serde_json::from_slice(line).map_err(io_error)?;
        if entry.schedule_id != schedule.id
            || entry.digest != digest(&entry.item)?
            || !ids.insert(entry.item.id.clone())
            || live.contains(entry.item.id.as_str())
        {
            return Err(invalid("invalid archive identity or digest"));
        }
        validate_item(&entry.item, &mut BTreeSet::new())?;
        for dispatch in &entry.item.dispatches {
            if !tokens.insert(dispatch.token.clone()) {
                return Err(invalid("duplicate dispatch token"));
            }
        }
        if entry.item.cancellation.is_none() && entry.item.completion.is_none() {
            return Err(invalid("archived item is not terminal"));
        }
        let allowed: BTreeSet<_> = entry
            .item
            .depends_on
            .iter()
            .filter(|id| item_number(id).is_ok_and(|n| n < schedule.next_item))
            .map(String::as_str)
            .collect();
        validate_dependencies(&entry.item.id, &entry.item.depends_on, &allowed)?;
        if let Some(retired) = schedule.retired.iter().find(|r| r.id == entry.item.id) {
            check_tombstone(&entry.item, retired)?;
        }
        advance(&mut actual, line, &entry.item)?;
        items.push(entry.item);
        Ok(())
    })
    .and_then(|()| {
        if actual != *head || schedule.retired.iter().any(|r| !ids.contains(&r.id)) {
            return Err(invalid("archive commitment does not match"));
        }
        Ok(())
    });
    result.map_err(|error| Error::new(Reason::StoreCorrupt, error.message))?;
    Ok(items)
}

pub(super) fn lookup(schedule_path: &Path, schedule: &Schedule, id: &str) -> Result<Item> {
    let items = if schedule.schema_version == 1 {
        let retired = schedule
            .retired
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| Error::new(Reason::NotFound, "schedule item not found"))?;
        legacy_load(schedule_path, schedule, std::slice::from_ref(retired))?
    } else {
        load(schedule_path, schedule)?
    };
    items
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| Error::new(Reason::NotFound, "schedule item not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn terminal(number: u64) -> Item {
        let by = OriginRef {
            runtime_id: "runtime".into(),
            team: "team".into(),
            id: "member".into(),
        };
        Item {
            id: format!("w-{number}"),
            title: "archived contract".into(),
            pm_ref: None,
            priority: 100,
            depends_on: vec![],
            scope: vec![],
            owner_bu: None,
            hold: None,
            cancellation: None,
            dispatches: vec![],
            completion: Some(Completion {
                by,
                at: 1,
                evidence: "reviewed".into(),
            }),
        }
    }

    #[test]
    fn old_snapshot_reads_survive_later_sweeps() {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-schedule-snapshot-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        let schedule_path = dir.join("schedule.md");
        let st = State::default();
        let mut document = Document::empty("snapshot-test".into());
        document.schedule.items = vec![terminal(1), terminal(2)];
        document.schedule.next_item = 3;
        sweep(&st, "team", &mut document, &schedule_path, true).unwrap();
        let snapshot = document.clone();
        let expected = load(&schedule_path, &snapshot.schedule).unwrap();
        let prefix = fs::read(path(&schedule_path)).unwrap();
        let (go_tx, go_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let schedule_path = &schedule_path;
            scope.spawn(move || {
                for number in 3..=5 {
                    go_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    use std::io::Write;
                    let mut file = fs::OpenOptions::new()
                        .append(true)
                        .open(path(schedule_path))
                        .unwrap();
                    file.write_all(b"{unpublished tail").unwrap();
                    drop(file);
                    document.schedule.items.push(terminal(number));
                    document.schedule.next_item = number + 1;
                    sweep(&st, "team", &mut document, schedule_path, true).unwrap();
                    done_tx.send(()).unwrap();
                }
            });
            for _ in 3..=5 {
                let mut lines = vec![];
                // Force truncate+append after the reader opened its prefix, with
                // its file handle still open and its document snapshot unchanged.
                crate::archive::scan_prefix(
                    &path(schedule_path),
                    snapshot.schedule.archive.as_ref().unwrap().bytes,
                    |line| {
                        if lines.is_empty() {
                            go_tx.send(()).unwrap();
                            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        }
                        lines.extend_from_slice(line);
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(lines, prefix);
                for _ in 0..3 {
                    assert_eq!(load(schedule_path, &snapshot.schedule).unwrap(), expected);
                }
            }
        });
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schedule_archive_10000_row_cost() {
        let dir = std::env::temp_dir().join(format!(
            "gaebu-schedule-cost-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        let schedule_path = dir.join("schedule.md");
        let st = State::default();
        let mut document = Document::empty("cost-test".into());
        // Each published document respects the 2,000-live-item limit.
        for batch in 0..5 {
            document.schedule.items = (batch * 2000 + 1..=(batch + 1) * 2000)
                .map(terminal)
                .collect();
            document.schedule.next_item = (batch + 1) * 2000 + 1;
            sweep(&st, "team", &mut document, &schedule_path, true).unwrap();
            document
                .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
                .unwrap();
        }
        let start = Instant::now();
        assert_eq!(
            load(&schedule_path, &document.schedule).unwrap().len(),
            10_000
        );
        let scan = start.elapsed();
        // Match the boundary/recovery lookup's Schedule -> state lock order;
        // measure from successful Schedule acquisition through both releases.
        let schedule_lock =
            crate::store::lock(&dir, "schedule.md.lock", crate::store::CLI_DEADLINE).unwrap();
        let acquired = Instant::now();
        let state_lock =
            crate::store::lock(&dir, "state.lock", crate::store::CLI_DEADLINE).unwrap();
        let snapshot = read_document(&schedule_path).unwrap();
        assert_eq!(
            lookup(&schedule_path, &snapshot.schedule, "w-10000")
                .unwrap()
                .id,
            "w-10000"
        );
        drop(state_lock);
        drop(schedule_lock);
        let lock_hold = acquired.elapsed();
        println!(
            "10,000 Schedule rows: bytes={}, verified_scan={scan:?}, Schedule+state_lock_hold={lock_hold:?}",
            document.schedule.archive.as_ref().unwrap().bytes
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
