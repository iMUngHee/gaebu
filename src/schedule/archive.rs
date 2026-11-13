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

/// Explicit maintenance deletes unlisted files with sidecar names, so a
/// Schedule document must never carry one.
pub(super) fn refuse_sidecar_name(schedule: &Path) -> Result<()> {
    let name = schedule
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let stem = name.strip_suffix(".jsonl").unwrap_or_default();
    let numbered = stem.rsplit_once('.').is_some_and(|(head, number)| {
        head.ends_with(".archive")
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
    });
    if stem.ends_with(".archive") || numbered {
        return Err(invalid(
            "a schedule file cannot use an archive segment name (*.archive.jsonl, *.archive.<n>.jsonl)",
        ));
    }
    Ok(())
}

fn digest(item: &Item) -> Result<String> {
    let bytes = serde_json::to_string(item).map_err(io_error)?;
    Ok(crate::evidence::sha256_hex(&bytes)[..16].to_owned())
}

pub(super) fn empty_head(id: &str) -> ArchiveHead {
    ArchiveHead {
        base: crate::evidence::sha256_hex(id),
        expired_rows: 0,
        next: 1,
        last_item: 0,
        last_dispatch: 0,
        segments: vec![],
    }
}

pub(super) fn validate_head(head: &ArchiveHead, id: &str) -> Result<()> {
    let valid_hash =
        |value: &str| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit());
    if !valid_hash(&head.base) || head.next == 0 {
        return Err(invalid("invalid schedule archive head"));
    }
    let (mut n, mut rows, mut bytes) = (0, head.expired_rows, 0u64);
    for segment in &head.segments {
        if segment.n <= n
            || segment.n >= head.next
            || segment.bytes == 0
            || segment.rows == 0
            || !valid_hash(&segment.chain)
            || segment.lo == 0
            || segment.lo > segment.hi
            || segment.hi > head.last_item
        {
            return Err(invalid("invalid schedule archive segment"));
        }
        n = segment.n;
        rows = rows
            .checked_add(segment.rows)
            .ok_or_else(|| invalid("archive rows exhausted"))?;
        bytes = bytes
            .checked_add(segment.bytes)
            .ok_or_else(|| invalid("archive bytes exhausted"))?;
    }
    if (rows == 0 && head != &empty_head(id))
        || (rows > 0 && (head.last_item == 0 || head.next == 1))
    {
        return Err(invalid("invalid schedule archive head"));
    }
    Ok(())
}

fn advance(segment: &mut ArchiveSegment, line: &[u8], item: &Item) -> Result<()> {
    let mut previous = [0u8; 32];
    for (index, byte) in previous.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&segment.chain[index * 2..index * 2 + 2], 16).map_err(io_error)?;
    }
    let mut hash = Sha256::new();
    hash.update(previous);
    hash.update(line);
    segment.chain = format!("{:x}", hash.finalize());
    segment.bytes = segment
        .bytes
        .checked_add(line.len() as u64)
        .ok_or_else(|| invalid("archive bytes exhausted"))?;
    segment.rows = segment
        .rows
        .checked_add(1)
        .ok_or_else(|| invalid("archive rows exhausted"))?;
    let number = item_number(&item.id)?;
    segment.lo = segment.lo.min(number);
    segment.hi = segment.hi.max(number);
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

fn fold_expired(
    head: &mut ArchiveHead,
    expired: impl IntoIterator<Item = ArchiveSegment>,
) -> Result<(u64, u64)> {
    let (mut segments, mut rows) = (0, 0u64);
    for segment in expired {
        rows = rows
            .checked_add(segment.rows)
            .ok_or_else(|| invalid("archive rows exhausted"))?;
        segments += 1;
        head.base = segment.chain;
    }
    head.expired_rows = head
        .expired_rows
        .checked_add(rows)
        .ok_or_else(|| invalid("archive rows exhausted"))?;
    Ok((segments, rows))
}

fn append_rows(
    head: &mut ArchiveHead,
    schedule_path: &Path,
    rows: &[(Vec<u8>, Item)],
    bytes: &[u8],
    limit: u64,
) -> Result<()> {
    let appended = crate::archive::append_segment(
        &path(schedule_path),
        &head.segments,
        &mut head.next,
        limit / 8,
        bytes,
    )?;
    if appended.created {
        let chain = head
            .segments
            .last()
            .map_or(&head.base, |s| &s.chain)
            .clone();
        head.segments.push(ArchiveSegment {
            n: appended.number,
            bytes: 0,
            rows: 0,
            chain,
            lo: u64::MAX,
            hi: 0,
        });
    }
    let segment = head
        .segments
        .last_mut()
        .ok_or_else(|| invalid("missing appended segment"))?;
    for (line, item) in rows {
        advance(segment, line, item)?;
        let (number, dispatch) = validate_item(item, &mut BTreeSet::new())?;
        head.last_item = head.last_item.max(number);
        head.last_dispatch = head.last_dispatch.max(dispatch);
    }
    if segment.bytes != appended.bytes {
        return Err(invalid("archive append length disagrees"));
    }
    Ok(())
}

pub(super) fn sweep(
    st: &State,
    team: &str,
    document: &mut Document,
    schedule_path: &Path,
    strict: bool,
    limit: u64,
    heal: Option<&mut crate::archive::Healed>,
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
            if !items.is_empty() {
                let mut segment = ArchiveSegment {
                    n: 1,
                    bytes: 0,
                    rows: 0,
                    chain: head.base.clone(),
                    lo: u64::MAX,
                    hi: 0,
                };
                let mut bytes = vec![];
                for item in items {
                    let line = encode(&document.schedule.id, &item)?;
                    advance(&mut segment, &line, &item)?;
                    let (number, dispatch) = validate_item(&item, &mut BTreeSet::new())?;
                    head.last_item = head.last_item.max(number);
                    head.last_dispatch = head.last_dispatch.max(dispatch);
                    bytes.extend(line);
                }
                crate::store::replace_until(
                    &path(schedule_path),
                    &bytes,
                    Instant::now() + crate::store::CLI_DEADLINE,
                )?;
                head.segments.push(segment);
                head.next = 2;
            }
            document.schedule.archive = Some(head);
            document.schedule.schema_version = 3;
            Ok(())
        })();
        if let Err(error) = migration {
            if strict {
                return Err(error);
            }
            return Ok(
                json!({"retired":[],"pruned":[],"deferred":[{"reason":error.message}],"retired_total":retired_total(&document.schedule)}),
            );
        }
    }
    let mut head = document
        .schedule
        .archive
        .clone()
        .ok_or_else(|| invalid("missing archive head"))?;
    let healed = if let Some(report) = heal {
        // Without a hole heal has nothing to do, so the full verification that
        // must precede a deletion is not paid for either.
        if !migrated && crate::archive::has_hole(&path(schedule_path), &head.segments)? {
            precheck_heal(schedule_path, &document.schedule)?;
            *report = crate::archive::heal_holes(&path(schedule_path), &head.segments)?;
        }
        Some(report)
    } else {
        None
    };
    if let Some(report) = &healed
        && let Some(reason) = &report.deferred
    {
        return Ok(
            json!({"retired":[],"pruned":[],"deferred":[],"retired_total":retired_total(&document.schedule),
            "expired":{"segments":0,"rows":0},"retained_bytes":head.retained_bytes(),"limit_bytes":limit,
            "healed":heal_counts(&head,report),"heal_deferred":reason,"hole_remaining":report.hole_remaining}),
        );
    }
    let healed_counts = healed.as_ref().map(|report| heal_counts(&head, report));
    let mut retired = vec![];
    let mut deferred = vec![];
    let mut bytes = vec![];
    let mut rows = vec![];
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
        rows.push((line.clone(), item.clone()));
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

    let (mut expired_segments, mut expired_rows) = (0, 0);
    let mut expiry_deferred = None;
    if strict || !bytes.is_empty() {
        let write = (|| -> Result<()> {
            let prefix = crate::archive::prepare_segments(&path(schedule_path), &head.segments)?;
            let expired: Vec<_> = head.segments.drain(..prefix).collect();
            let count = fold_expired(&mut head, expired)?;
            expired_segments += count.0;
            expired_rows += count.1;
            if !bytes.is_empty() {
                append_rows(&mut head, schedule_path, &rows, &bytes, limit)?;
            }
            if !migrated {
                let result =
                    crate::archive::enforce_limit(&path(schedule_path), &mut head.segments, limit)?;
                let count = fold_expired(&mut head, result.expired)?;
                expired_segments += count.0;
                expired_rows += count.1;
                expiry_deferred = result.deferred;
                if strict {
                    let cleanup =
                        crate::archive::remove_unlisted(&path(schedule_path), &head.segments)?;
                    if expiry_deferred.is_none() {
                        expiry_deferred = cleanup;
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = write {
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
    }
    let retained_bytes = head.retained_bytes();
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

    let mut result = json!({"retired":ids,"pruned":pruned,"deferred":deferred,"retired_total":retired_total(&document.schedule),"expired":{"segments":expired_segments,"rows":expired_rows},"retained_bytes":retained_bytes,"limit_bytes":limit});
    if let Some(counts) = healed_counts {
        result["expired"]["segments"] = json!(expired_segments - counts.segments);
        result["expired"]["rows"] = json!(expired_rows - counts.rows);
        result["healed"] = json!(counts);
    }
    if let Some(reason) = expiry_deferred {
        result["expiry_deferred"] = json!(reason);
    }
    Ok(result)
}

#[derive(Serialize)]
struct HealCounts {
    segments: u64,
    bytes: u64,
    rows: u64,
    unlisted_files: u64,
    unlisted_bytes: u64,
}

fn heal_counts(head: &ArchiveHead, healed: &crate::archive::Healed) -> HealCounts {
    let mut counts = HealCounts {
        segments: 0,
        bytes: 0,
        rows: 0,
        unlisted_files: healed.unlisted_files,
        unlisted_bytes: healed.unlisted_bytes,
    };
    for segment in head
        .segments
        .iter()
        .filter(|segment| healed.segments.contains(&segment.n))
    {
        counts.segments += 1;
        counts.bytes += segment.bytes;
        counts.rows += segment.rows;
    }
    counts
}

fn precheck_heal(schedule_path: &Path, schedule: &Schedule) -> Result<()> {
    let head = schedule
        .archive
        .as_ref()
        .ok_or_else(|| invalid("missing archive head"))?;
    validate_head(head, &schedule.id)?;
    let retained = crate::archive::precheck_heal(&path(schedule_path), &head.segments)?;
    let (mut ids, mut tokens) = (BTreeSet::new(), BTreeSet::new());
    let mut maxima = (0, 0);
    for index in retained..head.segments.len() {
        for item in verify_segment(schedule_path, schedule, index)?
            .into_iter()
            .flatten()
        {
            check_unique_item(&item, &mut ids, &mut tokens)?;
            let (number, dispatch) = validate_item(&item, &mut BTreeSet::new())?;
            maxima.0 = maxima.0.max(number);
            maxima.1 = maxima.1.max(dispatch);
        }
    }
    if retained == 0 && head.expired_rows == 0 && maxima != (head.last_item, head.last_dispatch) {
        return Err(Error::new(
            Reason::StoreCorrupt,
            "archive allocator commitment does not match",
        ));
    }
    Ok(())
}

fn check_unique_item(
    item: &Item,
    ids: &mut BTreeSet<String>,
    tokens: &mut BTreeSet<String>,
) -> Result<()> {
    if !ids.insert(item.id.clone()) {
        return Err(Error::new(
            Reason::StoreCorrupt,
            "duplicate archive item id",
        ));
    }
    for dispatch in &item.dispatches {
        if !tokens.insert(dispatch.token.clone()) {
            return Err(Error::new(Reason::StoreCorrupt, "duplicate dispatch token"));
        }
    }
    Ok(())
}

pub(super) fn heal_error(mut error: Error, healed: &crate::archive::Healed) -> Error {
    let removed = healed.segments.len() as u64 + healed.unlisted_files;
    if removed > 0 {
        error.message.push_str(&format!(
            "; heal had removed {removed} segment(s) of the Schedule archive"
        ));
    }
    if let Some(reason) = &healed.deferred {
        error.message.push_str(&format!(
            "; heal of the Schedule archive is incomplete ({reason}), run --heal again"
        ));
    }
    error
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

pub(super) struct Loaded {
    pub items: Vec<Item>,
    pub expired_rows: u64,
}

fn verify_segment(
    schedule_path: &Path,
    schedule: &Schedule,
    index: usize,
) -> Result<Option<Vec<Item>>> {
    let head = schedule
        .archive
        .as_ref()
        .ok_or_else(|| invalid("missing archive head"))?;
    let expected = &head.segments[index];
    let mut actual = ArchiveSegment {
        n: expected.n,
        bytes: 0,
        rows: 0,
        chain: if index == 0 {
            head.base.clone()
        } else {
            head.segments[index - 1].chain.clone()
        },
        lo: u64::MAX,
        hi: 0,
    };
    let mut items = vec![];
    let mut ids = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    let mut max_dispatch = 0;
    let live: BTreeSet<_> = schedule.items.iter().map(|item| item.id.as_str()).collect();
    let result =
        crate::archive::scan_segment(&path(schedule_path), &head.segments, index, |line| {
            let entry: Entry = serde_json::from_slice(line)
                .map_err(|error| Error::new(Reason::StoreCorrupt, error.to_string()))?;
            if entry.schedule_id != schedule.id
                || entry.digest != digest(&entry.item)?
                || !ids.insert(entry.item.id.clone())
                || live.contains(entry.item.id.as_str())
            {
                return Err(invalid("invalid archive identity or digest"));
            }
            let (number, dispatch) = validate_item(&entry.item, &mut BTreeSet::new())?;
            max_dispatch = max_dispatch.max(dispatch);
            if number < expected.lo
                || number > expected.hi
                || number > head.last_item
                || dispatch > head.last_dispatch
            {
                return Err(invalid(
                    "archive bounds or allocator commitment does not match",
                ));
            }
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
        });
    let present = result.map_err(|error| {
        if error.reason == Reason::InvalidArgument {
            Error::new(Reason::StoreCorrupt, error.message)
        } else {
            error
        }
    })?;
    if !present {
        return Ok(None);
    }
    if actual.bytes != expected.bytes
        || actual.rows != expected.rows
        || actual.chain != expected.chain
        || (head.expired_rows == 0
            && head.segments.len() == 1
            && (actual.hi != head.last_item || max_dispatch != head.last_dispatch))
    {
        return Err(Error::new(
            Reason::StoreCorrupt,
            "archive commitment does not match",
        ));
    }
    Ok(Some(items))
}

pub(super) fn load(schedule_path: &Path, schedule: &Schedule) -> Result<Loaded> {
    load_observed(schedule_path, schedule, |_| Ok(()))
}

fn load_observed(
    schedule_path: &Path,
    schedule: &Schedule,
    mut after_segment: impl FnMut(usize) -> Result<()>,
) -> Result<Loaded> {
    if schedule.schema_version == 1 {
        return Ok(Loaded {
            items: legacy_load(schedule_path, schedule, &schedule.retired)?,
            expired_rows: 0,
        });
    }
    let head = schedule
        .archive
        .as_ref()
        .ok_or_else(|| invalid("missing archive head"))?;
    validate_head(head, &schedule.id)?;
    crate::archive::expired_prefix(&path(schedule_path), &head.segments)?;
    let mut scanned = vec![];
    for index in 0..head.segments.len() {
        scanned.push(verify_segment(schedule_path, schedule, index)?);
        after_segment(index)?;
    }
    // An expiry can remove an already scanned file before the next open.
    let expired = crate::archive::expired_prefix(&path(schedule_path), &head.segments)?;
    let mut loaded = Loaded {
        items: vec![],
        expired_rows: head.expired_rows,
    };
    let mut ids = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    let mut maxima = (0, 0);
    for (index, items) in scanned.into_iter().enumerate() {
        if index < expired || items.is_none() {
            loaded.expired_rows = loaded
                .expired_rows
                .checked_add(head.segments[index].rows)
                .ok_or_else(|| invalid("archive rows exhausted"))?;
            continue;
        }
        for item in items.into_iter().flatten() {
            check_unique_item(&item, &mut ids, &mut tokens)?;
            let (number, dispatch) = validate_item(&item, &mut BTreeSet::new())?;
            maxima.0 = maxima.0.max(number);
            maxima.1 = maxima.1.max(dispatch);
            loaded.items.push(item);
        }
    }
    if loaded.expired_rows == 0 && maxima != (head.last_item, head.last_dispatch) {
        return Err(Error::new(
            Reason::StoreCorrupt,
            "archive allocator commitment does not match",
        ));
    }
    Ok(loaded)
}

pub(super) fn lookup(schedule_path: &Path, schedule: &Schedule, id: &str) -> Result<Item> {
    if schedule.schema_version == 1 {
        let retired = schedule
            .retired
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| Error::new(Reason::NotFound, "schedule item not found"))?;
        return legacy_load(schedule_path, schedule, std::slice::from_ref(retired))?
            .into_iter()
            .next()
            .ok_or_else(|| Error::new(Reason::NotFound, "schedule item not found"));
    }
    let number = item_number(id)?;
    let head = schedule
        .archive
        .as_ref()
        .ok_or_else(|| invalid("missing archive head"))?;
    validate_head(head, &schedule.id)?;
    crate::archive::expired_prefix(&path(schedule_path), &head.segments)?;
    for (index, segment) in head.segments.iter().enumerate().rev() {
        if number < segment.lo || number > segment.hi {
            continue;
        }
        if let Some(items) = verify_segment(schedule_path, schedule, index)?
            && let Some(item) = items.into_iter().rev().find(|item| item.id == id)
        {
            return Ok(item);
        }
    }
    Err(Error::new(
        Reason::NotFound,
        if number < schedule.next_item {
            format!("schedule item {id} expired from the archive")
        } else {
            "schedule item not found".into()
        },
    ))
}

pub(super) fn require_all_segments(schedule_path: &Path, schedule: &Schedule) -> Result<()> {
    if let Some(head) = &schedule.archive {
        for segment in &head.segments {
            fs::metadata(crate::archive::segment_path(
                &path(schedule_path),
                segment.n,
            ))
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Error::new(
                        Reason::StoreCorrupt,
                        "every listed archive segment must be copied before moving a schedule",
                    )
                } else {
                    io_error(error)
                }
            })?;
        }
    }
    load(schedule_path, schedule)?;
    Ok(())
}

pub(super) fn read_with_migration_retry<T>(
    schedule_path: &Path,
    schedule: &Schedule,
    mut read: impl FnMut(&Schedule) -> Result<T>,
) -> Result<T> {
    match read(schedule) {
        Err(original) if schedule.schema_version == 1 => {
            if let Ok(document) = read_document(schedule_path)
                && document.schedule.schema_version != 1
            {
                read(&document.schedule)
            } else {
                Err(original)
            }
        }
        result => result,
    }
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
    fn schedule_files_cannot_use_sidecar_names() {
        for (name, refused) in [
            ("schedule.md", false),
            ("notes.jsonl", false),
            ("plan2.jsonl", false),
            ("plan.archive5.jsonl", false),
            ("plan.md.archive.jsonl", true),
            ("plan.md.archive.2.jsonl", true),
            ("plan.md.archive.02.jsonl", true),
        ] {
            assert_eq!(
                refuse_sidecar_name(Path::new("dir").join(name).as_path()).is_err(),
                refused,
                "{name}"
            );
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
        sweep(
            &st,
            "team",
            &mut document,
            &schedule_path,
            true,
            64 * 1024 * 1024,
            None,
        )
        .unwrap();
        let snapshot = document.clone();
        let expected = load(&schedule_path, &snapshot.schedule).unwrap().items;
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
                    sweep(
                        &st,
                        "team",
                        &mut document,
                        schedule_path,
                        true,
                        64 * 1024 * 1024,
                        None,
                    )
                    .unwrap();
                    done_tx.send(()).unwrap();
                }
            });
            for _ in 3..=5 {
                let mut lines = vec![];
                // Force truncate+append after the reader opened its prefix, with
                // its file handle still open and its document snapshot unchanged.
                crate::archive::scan_prefix(
                    &path(schedule_path),
                    snapshot.schedule.archive.as_ref().unwrap().retained_bytes(),
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
                    assert_eq!(
                        load(schedule_path, &snapshot.schedule).unwrap().items,
                        expected
                    );
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
            sweep(
                &st,
                "team",
                &mut document,
                &schedule_path,
                true,
                64 * 1024 * 1024,
                None,
            )
            .unwrap();
            document
                .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
                .unwrap();
        }
        let start = Instant::now();
        assert_eq!(
            load(&schedule_path, &document.schedule)
                .unwrap()
                .items
                .len(),
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
            document.schedule.archive.as_ref().unwrap().retained_bytes()
        );
        fs::remove_dir_all(dir).unwrap();
    }
    fn segmented_fixture(count: u64, limit: u64) -> (crate::store::Store, PathBuf, Document) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let home = std::env::temp_dir().join(format!(
            "gaebu-segmented-schedule-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let store = crate::store::Store::open_with_archive_limit(home, limit).unwrap();
        let schedule_path = store.home().join("schedule.md");
        let mut document = Document::empty("segmented-unit".into());
        for n in 1..=count {
            let mut item = terminal(n);
            item.title = "x".repeat(1000);
            document.schedule.items.push(item);
            document.schedule.next_item = n + 1;
            sweep(
                &State::default(),
                "team",
                &mut document,
                &schedule_path,
                true,
                limit,
                None,
            )
            .unwrap();
        }
        document
            .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
            .unwrap();
        (store, schedule_path, document)
    }

    fn legacy_snapshot(schedule_path: &Path, document: &Document) -> Document {
        let mut legacy = document.clone();
        legacy.schedule.retired = load(schedule_path, &document.schedule)
            .unwrap()
            .items
            .into_iter()
            .map(|item| Retired {
                id: item.id.clone(),
                state: RetiredState::Complete,
                digest: digest(&item).unwrap(),
                last_dispatch: 0,
                depends_on: vec![],
            })
            .collect();
        legacy.schedule.schema_version = 1;
        legacy.schedule.archive = None;
        legacy
    }

    #[test]
    fn archive_metadata_rejects_invalid_legacy_and_current_heads() {
        let (store, _, document) = segmented_fixture(1, 64 * 1024);
        let original = serde_json::to_value(&document.schedule).unwrap();
        let base = crate::evidence::sha256_hex(&document.schedule.id);
        let empty = json!({"bytes":0,"rows":0,"chain":base,"last_item":0,"last_dispatch":0});
        let nonempty = json!({"bytes":100,"rows":1,"chain":base,"last_item":1,"last_dispatch":0});
        for (head, field, value) in [
            (empty.clone(), "bytes", json!(1)),
            (empty.clone(), "last_item", json!(1)),
            (empty.clone(), "last_dispatch", json!(1)),
            (empty.clone(), "chain", json!("a".repeat(64))),
            (empty.clone(), "chain", json!("f")),
            (empty, "chain", json!("g".repeat(64))),
            (nonempty.clone(), "chain", json!("f")),
            (nonempty.clone(), "chain", json!("g".repeat(64))),
            (nonempty.clone(), "bytes", json!(0)),
            (nonempty, "last_item", json!(0)),
        ] {
            let mut bad = original.clone();
            bad["schema_version"] = json!(2);
            bad["archive"] = head;
            bad["archive"][field] = value;
            assert!(serde_json::from_value::<Schedule>(bad).is_err(), "{field}");
        }
        let valid = document.schedule.archive.as_ref().unwrap();
        for field in ["base", "next", "last_item"] {
            let mut bad = serde_json::to_value(valid).unwrap();
            bad[field] = if field == "base" {
                json!("bad")
            } else {
                json!(0)
            };
            let bad: ArchiveHead = serde_json::from_value(bad).unwrap();
            assert!(
                validate_head(&bad, &document.schedule.id).is_err(),
                "{field}"
            );
        }
        let mut all_expired = empty_head(&document.schedule.id);
        all_expired.expired_rows = 1;
        all_expired.last_item = 1;
        assert!(validate_head(&all_expired, &document.schedule.id).is_err());
        fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn expiry_results_count_missing_and_deleted_prefixes() {
        let (store, schedule_path, mut document) = segmented_fixture(4, 4096);
        let before = document.schedule.archive.as_ref().unwrap().clone();
        fs::remove_file(crate::archive::segment_path(
            &path(&schedule_path),
            before.segments[0].n,
        ))
        .unwrap();
        let result = sweep(
            &State::default(),
            "team",
            &mut document,
            &schedule_path,
            true,
            8,
            None,
        )
        .unwrap();
        let head = document.schedule.archive.as_ref().unwrap();
        assert_eq!(
            result["expired"]["segments"],
            (before.segments.len() - 1) as u64
        );
        assert_eq!(
            result["expired"]["rows"],
            before.retained_rows() - head.retained_rows()
        );
        fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn segment_rows_must_fit_each_committed_bound() {
        let (store, schedule_path, document) = segmented_fixture(3, 4096);
        for (index, lo, hi) in [(0, 2, 3), (1, 1, 1)] {
            let mut bad = document.schedule.clone();
            let head = bad.archive.as_mut().unwrap();
            head.segments[index].lo = lo;
            head.segments[index].hi = hi;
            assert_eq!(
                verify_segment(&schedule_path, &bad, index)
                    .unwrap_err()
                    .reason,
                Reason::StoreCorrupt
            );
        }
        assert_eq!(
            lookup(&schedule_path, &document.schedule, "w-4")
                .unwrap_err()
                .message,
            "schedule item not found"
        );
        let newest = crate::archive::segment_path(
            &path(&schedule_path),
            document
                .schedule
                .archive
                .as_ref()
                .unwrap()
                .segments
                .last()
                .unwrap()
                .n,
        );
        let mut bytes = fs::read(&newest).unwrap();
        bytes[0] = b'!';
        fs::write(newest, bytes).unwrap();
        assert_eq!(
            lookup(&schedule_path, &document.schedule, "w-1")
                .unwrap()
                .id,
            "w-1"
        );
        assert_eq!(
            lookup(&schedule_path, &document.schedule, "w-4")
                .unwrap_err()
                .message,
            "schedule item not found"
        );
        fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn current_schema_read_errors_are_not_retried() {
        let (store, schedule_path, document) = segmented_fixture(1, 4096);
        let mut calls = 0;
        let error =
            read_with_migration_retry(&schedule_path, &document.schedule, |_| -> Result<()> {
                calls += 1;
                Err(Error::new(Reason::StoreCorrupt, "original"))
            })
            .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(error.message, "original");
        fs::remove_dir_all(store.home()).unwrap();
    }

    fn registered_heal_fixture(
        count: u64,
    ) -> (crate::store::Store, PathBuf, crate::identity::Caller) {
        let (store, schedule_path, _) = segmented_fixture(count, 8192);
        store
            .transact(crate::store::CLI_DEADLINE, |st| {
                crate::ops::dev_seed(
                    st,
                    now_ms(),
                    &crate::ops::SeedSpec {
                        team: "heal-test".into(),
                        project: "project".into(),
                        commander_alias: "root".into(),
                        commander_session: Some("root-session".into()),
                        commander_tool: Some(Tool::Codex),
                        members: vec![],
                    },
                )?;
                Ok(())
            })
            .unwrap();
        let caller = crate::identity::Caller {
            session_id: "root-session".into(),
            tool: Some(Tool::Codex),
            source: crate::identity::Source::PagerNative,
        };
        runtime::register(&store, &caller, &schedule_path, false).unwrap();
        (store, schedule_path, caller)
    }

    #[test]
    fn partial_heal_reports_and_resumes() {
        let (store, schedule_path, caller) = registered_heal_fixture(4);
        let mut document = read_document(&schedule_path).unwrap();
        let head = document.schedule.archive.as_ref().unwrap().clone();
        assert_eq!(head.segments.len(), 4);
        fs::remove_file(crate::archive::segment_path(
            &path(&schedule_path),
            head.segments[2].n,
        ))
        .unwrap();
        document.schedule.items.push(terminal(5));
        document.schedule.next_item = 6;
        document
            .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
            .unwrap();
        let original = fs::read(&schedule_path).unwrap();
        let failed = crate::archive::segment_path(&path(&schedule_path), head.segments[1].n);
        crate::archive::FAILED_UNLINK.with_borrow_mut(|value| *value = Some(failed));
        let args = commands::Input::parse(&json!({"heal":true})).unwrap();
        let result = operations::mutate(&store, &caller, "schedule_archive", &args).unwrap();
        crate::archive::FAILED_UNLINK.with_borrow_mut(|value| *value = None);
        assert_eq!(result["healed"]["segments"], 1);
        assert_eq!(result["expired"]["segments"], 0);
        assert_eq!(result["hole_remaining"], true);
        assert!(result["heal_deferred"].is_string());
        assert_eq!(result["retired"], json!([]));
        assert_eq!(fs::read(&schedule_path).unwrap(), original);
        let retry = operations::mutate(&store, &caller, "schedule_archive", &args).unwrap();
        assert_eq!(retry["healed"]["segments"], 1);
        assert_eq!(retry["expired"]["segments"], 2);
        assert_eq!(retry["retired"], json!(["w-5"]));
        let final_ = read_document(&schedule_path).unwrap();
        assert_eq!(
            load(&schedule_path, &final_.schedule).unwrap().items.len(),
            2
        );
        fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn heal_reports_listed_and_unlisted_separately() {
        let (store, schedule_path, caller) = registered_heal_fixture(4);
        let mut document = read_document(&schedule_path).unwrap();
        let head = document.schedule.archive.as_mut().unwrap();
        let expired: Vec<_> = head.segments.drain(..2).collect();
        head.base = expired[1].chain.clone();
        head.expired_rows = 2;
        fs::remove_file(crate::archive::segment_path(&path(&schedule_path), 1)).unwrap();
        fs::remove_file(crate::archive::segment_path(&path(&schedule_path), 4)).unwrap();
        let bytes = fs::metadata(crate::archive::segment_path(&path(&schedule_path), 2))
            .unwrap()
            .len();
        document
            .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
            .unwrap();
        let original = fs::read(&schedule_path).unwrap();
        crate::archive::FAILED_UNLINK.with_borrow_mut(|value| {
            *value = Some(crate::archive::segment_path(&path(&schedule_path), 3))
        });
        let args = commands::Input::parse(&json!({"heal":true})).unwrap();
        let result = operations::mutate(&store, &caller, "schedule_archive", &args).unwrap();
        crate::archive::FAILED_UNLINK.with_borrow_mut(|value| *value = None);
        assert_eq!(
            result["healed"],
            json!({"segments":0,"bytes":0,"rows":0,"unlisted_files":1,"unlisted_bytes":bytes})
        );
        assert!(result["heal_deferred"].is_string());
        assert_eq!(fs::read(&schedule_path).unwrap(), original);
        fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn heal_then_failed_publish_is_consistent() {
        for unlisted in [false, true] {
            let (store, schedule_path, caller) = registered_heal_fixture(4);
            let mut document = read_document(&schedule_path).unwrap();
            if unlisted {
                let head = document.schedule.archive.as_mut().unwrap();
                let expired: Vec<_> = head.segments.drain(..2).collect();
                head.base = expired[1].chain.clone();
                head.expired_rows = 2;
                fs::remove_file(crate::archive::segment_path(&path(&schedule_path), 1)).unwrap();
            }
            let head = document.schedule.archive.as_ref().unwrap().clone();
            let missing = if unlisted { 4 } else { 3 };
            fs::remove_file(crate::archive::segment_path(&path(&schedule_path), missing)).unwrap();
            document.schedule.revision = u64::MAX;
            let json = serde_json::to_string(&document.schedule).unwrap();
            fs::write(&schedule_path, format!("{MARKER}\n```json\n{json}\n```\n")).unwrap();
            let args = commands::Input::parse(&json!({"heal":true})).unwrap();
            let error = operations::mutate(&store, &caller, "schedule_archive", &args).unwrap_err();
            assert!(
                error
                    .message
                    .contains("heal had removed 2 segment(s) of the Schedule archive"),
                "{error}"
            );
            let mut recovered = read_document(&schedule_path).unwrap();
            assert_eq!(recovered.schedule.archive.as_ref().unwrap(), &head);
            assert_eq!(
                load(&schedule_path, &recovered.schedule)
                    .unwrap()
                    .items
                    .len(),
                if unlisted { 0 } else { 1 }
            );
            recovered.schedule.revision = 0;
            recovered
                .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
                .unwrap();
            let retry = operations::mutate(&store, &caller, "schedule_archive", &args).unwrap();
            assert_eq!(retry["healed"]["segments"], 0);
            assert_eq!(retry["expired"]["segments"], if unlisted { 2 } else { 3 });
            fs::remove_dir_all(store.home()).unwrap();
        }
    }

    fn failed_publish_case(limit: u64) {
        let (store, schedule_path, mut document) =
            segmented_fixture(if limit == 8 { 1 } else { 3 }, 4096);
        let snapshot = document.clone();
        let number = document.schedule.next_item;
        document.schedule.items.push(terminal(number));
        document.schedule.next_item += 1;
        let result = sweep(
            &State::default(),
            "team",
            &mut document,
            &schedule_path,
            true,
            limit,
            None,
        )
        .unwrap();
        assert!(result["expired"]["segments"].as_u64().unwrap() > 0);
        assert!(document.save(&schedule_path, Instant::now()).is_err());
        let mut recovered = read_document(&schedule_path).unwrap();
        assert_eq!(recovered.schedule, snapshot.schedule);
        let retained = load(&schedule_path, &recovered.schedule).unwrap();
        let retained_ids: Vec<_> = retained.items.iter().map(|i| i.id.clone()).collect();
        let prefix: Vec<_> = recovered
            .schedule
            .archive
            .as_ref()
            .unwrap()
            .segments
            .iter()
            .filter_map(|segment| {
                fs::read(crate::archive::segment_path(
                    &path(&schedule_path),
                    segment.n,
                ))
                .ok()
                .map(|bytes| (segment.n, bytes))
            })
            .collect();
        recovered.schedule.items.push(terminal(number));
        recovered.schedule.next_item += 1;
        sweep(
            &State::default(),
            "team",
            &mut recovered,
            &schedule_path,
            true,
            limit,
            None,
        )
        .unwrap();
        recovered
            .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
            .unwrap();
        let retried = load(
            &schedule_path,
            &read_document(&schedule_path).unwrap().schedule,
        )
        .unwrap();
        assert_eq!(
            retried
                .items
                .iter()
                .filter(|i| i.id == format!("w-{number}"))
                .count(),
            1
        );
        for id in retained_ids {
            assert!(retried.items.iter().any(|i| i.id == id));
        }
        for (n, bytes) in prefix {
            if let Ok(after) = fs::read(crate::archive::segment_path(&path(&schedule_path), n)) {
                assert_eq!(&after[..bytes.len()], bytes.as_slice());
            }
        }
        fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn failed_publish_after_expiry_is_consistent() {
        for limit in [8, 4096] {
            failed_publish_case(limit);
        }
    }

    #[test]
    fn bound_exceptions_are_reported() {
        let (store, schedule_path, mut document) = segmented_fixture(1, 64 * 1024);
        let head = document.schedule.archive.as_ref().unwrap();
        let mut old = serde_json::to_value(&document.schedule).unwrap();
        old["schema_version"] = json!(2);
        old["archive"] = json!({"bytes":head.retained_bytes(),"rows":head.retained_rows(),"chain":head.segments[0].chain,"last_item":head.last_item,"last_dispatch":head.last_dispatch});
        fs::write(
            &schedule_path,
            format!("<!-- gaebu-schedule -->\n```json\n{old}\n```\n"),
        )
        .unwrap();
        document = read_document(&schedule_path).unwrap();
        let prefix = fs::read(path(&schedule_path)).unwrap();
        let result = sweep(
            &State::default(),
            "team",
            &mut document,
            &schedule_path,
            true,
            8,
            None,
        )
        .unwrap();
        assert_eq!(result["expired"]["segments"], 0);
        assert!(
            result["retained_bytes"].as_u64().unwrap() > result["limit_bytes"].as_u64().unwrap()
        );
        assert!(result.get("expiry_deferred").is_none());
        assert_eq!(fs::read(path(&schedule_path)).unwrap(), prefix);
        document.schedule.items.push(terminal(2));
        document.schedule.next_item = 3;
        let result = sweep(
            &State::default(),
            "team",
            &mut document,
            &schedule_path,
            true,
            8,
            None,
        )
        .unwrap();
        assert_eq!(result["expired"]["segments"], 1);
        assert!(!path(&schedule_path).exists());
        fs::remove_dir_all(store.home()).unwrap();
        let (store, schedule_path, mut document) = segmented_fixture(3, 8192);
        let oldest = crate::archive::segment_path(
            &path(&schedule_path),
            document.schedule.archive.as_ref().unwrap().segments[0].n,
        );
        crate::archive::FAILED_UNLINK.with_borrow_mut(|failed| *failed = Some(oldest.clone()));
        let result = sweep(
            &State::default(),
            "team",
            &mut document,
            &schedule_path,
            true,
            2000,
            None,
        )
        .unwrap();
        assert!(result["expiry_deferred"].is_string());
        assert_eq!(result["expired"]["segments"], 0);
        assert!(oldest.exists());
        assert_eq!(
            load(&schedule_path, &document.schedule)
                .unwrap()
                .items
                .len(),
            3
        );
        crate::archive::FAILED_UNLINK.with_borrow_mut(|failed| *failed = None);
        let result = sweep(
            &State::default(),
            "team",
            &mut document,
            &schedule_path,
            true,
            2000,
            None,
        )
        .unwrap();
        assert!(result.get("expiry_deferred").is_none());
        assert!(result["expired"]["segments"].as_u64().unwrap() > 0);
        fs::remove_dir_all(store.home()).unwrap();
        failed_publish_case(8);
    }

    #[test]
    fn schema1_migration_over_limit_keeps_sidecar() {
        let (store, schedule_path, document) = segmented_fixture(3, 64 * 1024);
        let mut legacy = legacy_snapshot(&schedule_path, &document);
        legacy
            .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
            .unwrap();
        let snapshot = legacy.clone();
        let before = fs::read(path(&schedule_path)).unwrap();
        let result = sweep(
            &State::default(),
            "team",
            &mut legacy,
            &schedule_path,
            true,
            8,
            None,
        )
        .unwrap();
        assert_eq!(result["expired"]["segments"], 0);
        assert!(legacy.save(&schedule_path, Instant::now()).is_err());
        assert_eq!(fs::read(path(&schedule_path)).unwrap(), before);
        assert_eq!(
            load(&schedule_path, &snapshot.schedule)
                .unwrap()
                .items
                .len(),
            3
        );
        let mut retry = read_document(&schedule_path).unwrap();
        assert_eq!(retry.schedule.schema_version, 1);
        sweep(
            &State::default(),
            "team",
            &mut retry,
            &schedule_path,
            true,
            8,
            None,
        )
        .unwrap();
        retry
            .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
            .unwrap();
        assert_eq!(
            read_document(&schedule_path)
                .unwrap()
                .schedule
                .schema_version,
            3
        );
        retry.schedule.items.push(terminal(4));
        retry.schedule.next_item = 5;
        sweep(
            &State::default(),
            "team",
            &mut retry,
            &schedule_path,
            true,
            8,
            None,
        )
        .unwrap();
        assert!(!path(&schedule_path).exists());
        assert_eq!(
            load(&schedule_path, &retry.schedule).unwrap().items[0].id,
            "w-4"
        );
        fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn schema1_reader_restarts_after_migration() {
        let (store, schedule_path, document) = segmented_fixture(2, 64 * 1024);
        let mut legacy = legacy_snapshot(&schedule_path, &document);
        legacy
            .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
            .unwrap();
        let snapshot = legacy.schedule.clone();
        let mut calls = 0;
        let read = read_with_migration_retry(&schedule_path, &snapshot, |schedule| {
            calls += 1;
            if calls == 1 {
                let mut writer = read_document(&schedule_path)?;
                sweep(
                    &State::default(),
                    "team",
                    &mut writer,
                    &schedule_path,
                    true,
                    8,
                    None,
                )?;
                writer.save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)?;
                writer.schedule.items.push(terminal(3));
                writer.schedule.next_item = 4;
                sweep(
                    &State::default(),
                    "team",
                    &mut writer,
                    &schedule_path,
                    true,
                    8,
                    None,
                )?;
                writer.save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)?;
            }
            let loaded = load(&schedule_path, schedule)?;
            Ok((schedule.schema_version, schedule.revision, loaded.items))
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(read.0, 3);
        assert!(read.1 > snapshot.revision);
        assert_eq!(
            read.2.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["w-3"]
        );
        legacy
            .save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)
            .unwrap();
        let error = read_with_migration_retry(&schedule_path, &snapshot, |_| -> Result<()> {
            Err(Error::new(Reason::StoreCorrupt, "original"))
        })
        .unwrap_err();
        assert_eq!(error.message, "original");
        fs::remove_dir_all(store.home()).unwrap();
    }

    #[test]
    fn reader_survives_concurrent_expiry() {
        let (store, schedule_path, mut writer) = segmented_fixture(3, 8192);
        let snapshot = writer.schedule.clone();
        assert_eq!(snapshot.archive.as_ref().unwrap().segments.len(), 3);
        let loaded = load_observed(&schedule_path, &snapshot, |index| {
            if index == 0 {
                sweep(
                    &State::default(),
                    "team",
                    &mut writer,
                    &schedule_path,
                    true,
                    2000,
                    None,
                )?;
                writer.save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            loaded
                .items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["w-3"]
        );
        assert_eq!(loaded.expired_rows, 2);
        let loaded = load_observed(&schedule_path, &snapshot, |index| {
            if index == 0 {
                writer.schedule.items.push(terminal(4));
                writer.schedule.next_item = 5;
                sweep(
                    &State::default(),
                    "team",
                    &mut writer,
                    &schedule_path,
                    true,
                    8,
                    None,
                )?;
                writer.save(&schedule_path, Instant::now() + crate::store::CLI_DEADLINE)?;
            }
            Ok(())
        })
        .unwrap();
        assert!(loaded.items.is_empty());
        assert_eq!(loaded.expired_rows, 3);
        fs::remove_dir_all(store.home()).unwrap();
    }
}
