//! Display titles (Decision 1a). Wei-dynasty general ranks, adapted — not a
//! historical reconstruction. Titles never carry authority.

/// Titles for the 가절월 holder.
pub const COMMANDER: &[&str] = &["대장군", "표기장군", "거기장군", "위장군"];

/// Titles for the head of a bu below the commander's.
pub const HEAD: &[&str] = &[
    "정동장군",
    "정서장군",
    "정남장군",
    "정북장군",
    "진동장군",
    "진서장군",
    "진남장군",
    "진북장군",
    "전장군",
    "후장군",
    "좌장군",
    "우장군",
];

/// Titles for 속관 of the commander's own bu.
pub const ROOT_STAFF: &[&str] = &["장사", "사마", "종사중랑", "주부", "기실독", "참군"];

/// Titles for 속관 of a bu below the commander's.
pub const SUB_STAFF: &[&str] = &["장사", "사마", "주부", "공조사", "문하독", "녹사"];

/// Title of a separate Schedule manager.
pub const LIAISON: &str = "황문시랑";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    Commander,
    Head,
    RootStaff,
    SubStaff,
}

fn pool(seat: Seat) -> &'static [&'static str] {
    match seat {
        Seat::Commander => COMMANDER,
        Seat::Head => HEAD,
        Seat::RootStaff => ROOT_STAFF,
        Seat::SubStaff => SUB_STAFF,
    }
}

/// FNV-1a. The standard hasher's output is not stable across Rust releases,
/// and a seat must draw the same title wherever it is computed.
fn fnv1a64(key: &str) -> u64 {
    key.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A title from the seat's pool that `taken` does not hold, chosen by `key`;
/// when the pool is exhausted, a pool title with a numeric suffix (`녹사2`).
pub fn pick(seat: Seat, taken: &[&str], key: &str) -> String {
    let pool = pool(seat);
    let hash = fnv1a64(key);
    let free: Vec<&str> = pool
        .iter()
        .copied()
        .filter(|title| !taken.contains(title))
        .collect();
    if !free.is_empty() {
        return free[(hash % free.len() as u64) as usize].into();
    }
    let base = pool[(hash % pool.len() as u64) as usize];
    for suffix in 2_u64.. {
        let title = format!("{base}{suffix}");
        if !taken.contains(&title.as_str()) {
            return title;
        }
    }
    unreachable!("the title pool cannot exhaust all u64 suffixes")
}

/// Whether `title` is one this module hands out, with or without a numeric
/// suffix. Such a title belongs to the seat and is drawn again when the
/// member's seat changes; any other title is kept.
pub fn follows_seat(title: &str) -> bool {
    let base = title.trim_end_matches(|c: char| c.is_ascii_digit());
    base == LIAISON
        || [COMMANDER, HEAD, ROOT_STAFF, SUB_STAFF]
            .iter()
            .any(|pool| pool.contains(&base))
}

/// Default bu name from its head's title: 좌장군 → 좌장군부.
pub fn bu_name(head_title: &str) -> String {
    format!("{head_title}부")
}

#[cfg(test)]
mod tests {
    use super::fnv1a64;

    /// Published FNV-1a 64-bit test vectors: titles already drawn must not
    /// change because the hash did.
    #[test]
    fn fnv1a64_matches_reference_vectors() {
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
