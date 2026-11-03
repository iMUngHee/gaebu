//! Position cards (Decision 13): what a member re-reads after compaction.

use crate::model::{Ms, State};
use serde::{Deserialize, Serialize};

pub const MAX_LINES: usize = 12;

/// JSON form of `gaebu me --json`; `docs/model.md` fixes these fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub rev: u64,
    pub member: String,
    pub alias: String,
    pub title: String,
    pub team: String,
    pub bu: String,
    pub binding: String,
    /// 대협 → commander → ... → me, as display strings.
    pub chain: Vec<String>,
    pub superior: Option<String>,
    pub duties: Vec<CardDuty>,
    pub peers: Vec<String>,
    pub leases: Vec<String>,
    pub escalations: Vec<String>,
    pub cancels: Vec<String>,
    pub norms: Vec<String>,
    /// sha256 of the text form; used to decide full vs one-line injection.
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardDuty {
    pub id: String,
    pub status: String,
    pub outcome: String,
    pub scope: Vec<String>,
    pub done_criteria: String,
    pub decision_rights: String,
}

/// Build the card for `member`; `None` when it is not an active member.
pub fn build(st: &State, member: &str, now: Ms) -> Option<Card> {
    let _ = (st, member, now);
    todo!("gapu: card")
}

/// At most `MAX_LINES` lines of text.
pub fn render(card: &Card) -> String {
    let _ = card;
    todo!("gapu: card")
}

/// The one-line form used when the card hash is unchanged.
pub fn one_line(card: &Card) -> String {
    let _ = card;
    todo!("gapu: card")
}
