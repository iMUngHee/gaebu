//! Display titles (Decision 1a). A Three Kingdoms motif, adapted — not a
//! historical reconstruction. Titles never carry authority.

/// Titles for the 가절월 holder.
pub const COMMANDER: &[&str] = &[
    "승상",
    "대도독",
    "대장군",
    "대사마",
    "태위",
    "도독중외제군사",
];

/// Titles for the head of a bu.
pub const HEAD: &[&str] = &[
    "좌장군",
    "우장군",
    "전장군",
    "후장군",
    "정동장군",
    "정서장군",
    "정남장군",
    "정북장군",
    "진동장군",
    "진서장군",
    "진남장군",
    "진북장군",
    "안국장군",
    "소열장군",
    "편장군",
];

/// Titles for 속관 (members of a bu).
pub const STAFF: &[&str] = &[
    "장사",
    "사마",
    "주부",
    "참군",
    "종사중랑",
    "기실",
    "공조",
    "연",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    Commander,
    Head,
    Staff,
}

/// First title from the seat's pool not already used in the team; when the
/// pool is exhausted, the base title with a numeric suffix (`연2`).
pub fn pick(seat: Seat, taken: &[&str]) -> String {
    let pool = match seat {
        Seat::Commander => COMMANDER,
        Seat::Head => HEAD,
        Seat::Staff => STAFF,
    };
    if let Some(title) = pool.iter().find(|title| !taken.contains(title)) {
        return (*title).into();
    }
    let base = pool[pool.len() - 1];
    for suffix in 2_u64.. {
        let title = format!("{base}{suffix}");
        if !taken.contains(&title.as_str()) {
            return title;
        }
    }
    unreachable!("the title pool cannot exhaust all u64 suffixes")
}

/// Default bu name from its head's title: 좌장군 → 좌장군부, 승상 → 승상부.
pub fn bu_name(head_title: &str) -> String {
    format!("{head_title}부")
}
