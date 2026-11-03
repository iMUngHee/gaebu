//! Command line. `run` returns the process exit code: 0 done, 1 refused
//! (a typed reason is printed), 2 usage error.

use crate::card;
use crate::error::{Error, Reason, Result};
use crate::evidence;
use crate::identity::{self, Caller};
use crate::model::{
    Actor, DutyStatus, LeaseMode, MemberStatus, RecordKind, State, Tool, Verb, now_ms,
};
use crate::ops::{self, DutySpec, Outcome, SeedSpec};
use crate::pager::{CliPager, Pager};
use crate::store::{self, ComposerRecord, EventKind, Store};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::ffi::OsString;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "gaebu",
    version,
    about = "Command tree, duties, leases and position cards for agent sessions"
)]
struct Cli {
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show my position card.
    Me {
        /// Print only the card text (what hooks inject).
        #[arg(long)]
        card: bool,
        /// Read the card of this session id instead of resolving through pager
        /// (read-only; used by the Claude mod).
        #[arg(long)]
        session: Option<String>,
    },
    /// Show the whole roster (명부).
    Board {
        /// Print nothing but `unchanged` when the store rev is not newer.
        #[arg(long)]
        since: Option<u64>,
    },
    /// Root directive: give 가절월 to a pager alias (quote the user's words).
    Appoint {
        alias: String,
        #[arg(long)]
        team: Option<String>,
        #[arg(long, default_value = "")]
        project: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        evidence: String,
    },
    /// Root directive: put aliases under a commander.
    Assign {
        aliases: Vec<String>,
        #[arg(long)]
        to: String,
        #[arg(long)]
        evidence: String,
    },
    /// Root directive: dismiss a member (revokes everything below it).
    Dismiss {
        alias: String,
        #[arg(long)]
        evidence: String,
    },
    /// Root directive: cancel a whole team.
    CancelTeam {
        team: String,
        #[arg(long)]
        evidence: String,
    },
    /// Re-point a member at a new alias. With --evidence it is a root
    /// directive; without, it needs `reassign` over the member.
    Rebind {
        member: String,
        #[arg(long)]
        alias: String,
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Open, close or cancel a bu (부).
    #[command(subcommand)]
    Bu(BuCmd),
    /// Recruit or move members.
    #[command(subcommand)]
    Member(MemberCmd),
    /// Delegate verbs and scope to a member below.
    Grant {
        member: String,
        #[arg(long, value_delimiter = ',')]
        verbs: Vec<VerbArg>,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
    },
    /// Revoke a grant.
    Revoke { grant: String },
    /// Change a display title.
    Title { member: String, title: String },
    /// Duties (소임).
    #[command(subcommand)]
    Duty(DutyCmd),
    /// Leases.
    #[command(subcommand)]
    Lease(LeaseCmd),
    /// Raise a question beyond my decision rights.
    Escalate {
        question: String,
        #[arg(long)]
        duty: Option<String>,
    },
    /// Answer an escalation addressed to me.
    Answer { escalation: String, answer: String },
    /// Add a team record (팀기록).
    Record {
        text: String,
        #[arg(long, value_enum, default_value = "note")]
        kind: KindArg,
        #[arg(long = "ref")]
        refs: Vec<String>,
    },
    /// Hook adapter (registered in Claude Code / Codex settings).
    Hook {
        #[arg(value_enum)]
        tool: ToolArg,
    },
    /// stdio MCP server.
    Mcp,
    /// Evidence plumbing used by the Claude mod.
    #[command(subcommand)]
    Evidence(EvidenceCmd),
    /// Session plumbing used by the Claude mod.
    #[command(subcommand)]
    Session(SessionCmd),
    /// Development helpers (refused on the default home).
    #[command(subcommand)]
    Dev(DevCmd),
    /// Restore state.json from state.prev.json.
    Repair,
}

#[derive(Subcommand)]
enum BuCmd {
    /// 개부: open a bu headed by a member below you.
    Open {
        #[arg(long)]
        head: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_delimiter = ',')]
        verbs: Vec<VerbArg>,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
    },
    Close {
        bu: String,
    },
    Cancel {
        bu: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
}

#[derive(Subcommand)]
enum MemberCmd {
    /// Recruit a pager alias into a bu (pending until its session confirms).
    Add {
        alias: String,
        #[arg(long)]
        bu: String,
        #[arg(long)]
        title: Option<String>,
    },
    /// Move a member to another bu.
    Move {
        member: String,
        #[arg(long)]
        bu: String,
    },
}

#[derive(Args)]
struct SpecArgs {
    #[arg(long, default_value = "")]
    intent: String,
    #[arg(long)]
    outcome: String,
    #[arg(long, value_delimiter = ',')]
    scope: Vec<String>,
    #[arg(long = "rights", default_value = "")]
    decision_rights: String,
    #[arg(long = "escalate-on")]
    escalate_on: Vec<String>,
    #[arg(long = "done")]
    done_criteria: String,
    #[arg(long)]
    plan: Option<String>,
    #[arg(long = "exec-root")]
    exec_root: Option<String>,
}

impl SpecArgs {
    fn spec(self) -> DutySpec {
        DutySpec {
            intent: self.intent,
            outcome: self.outcome,
            scope: self.scope,
            decision_rights: self.decision_rights,
            escalate_on: self.escalate_on,
            done_criteria: self.done_criteria,
            plan_ref: self.plan,
            exec_root: self.exec_root,
        }
    }
}

#[derive(Subcommand)]
enum DutyCmd {
    /// Issue a duty to a member below you.
    Issue {
        #[arg(long)]
        to: String,
        #[command(flatten)]
        spec: SpecArgs,
    },
    Accept {
        duty: String,
        #[arg(long)]
        version: Option<u64>,
    },
    Decline {
        duty: String,
        #[arg(long)]
        note: String,
    },
    Block {
        duty: String,
        #[arg(long)]
        note: String,
    },
    Unblock {
        duty: String,
    },
    Done {
        duty: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    Amend {
        duty: String,
        #[arg(long)]
        version: u64,
        #[command(flatten)]
        spec: SpecArgs,
    },
    Cancel {
        duty: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
}

#[derive(Subcommand)]
enum LeaseCmd {
    Acquire {
        key: String,
        #[arg(long)]
        shared: bool,
        /// Minutes (default 30, max 480).
        #[arg(long)]
        ttl: Option<u64>,
        #[arg(long, default_value = "")]
        reason: String,
    },
    Renew {
        lease: String,
        #[arg(long)]
        ttl: Option<u64>,
    },
    Release {
        lease: String,
    },
    ForceRelease {
        lease: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
}

#[derive(Subcommand)]
enum EvidenceCmd {
    /// Record a composer-attested prompt hash (Claude mod `prompt.submit`).
    Composer {
        #[arg(long)]
        session: String,
        /// Hash of the normalized prompt; when absent the prompt text is read
        /// from stdin and hashed here.
        #[arg(long)]
        sha256: Option<String>,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Mark the Claude mod alive in this session.
    Heartbeat {
        #[arg(long)]
        session: String,
        #[arg(long = "mod")]
        mod_active: bool,
    },
}

#[derive(Subcommand)]
enum DevCmd {
    /// Build a fixture team in a non-default GAEBU_HOME.
    Seed {
        #[arg(long)]
        team: String,
        #[arg(long, default_value = "fixture")]
        project: String,
        /// alias[:session[:claude|codex]]
        #[arg(long)]
        commander: String,
        /// alias[:session[:claude|codex]], repeatable
        #[arg(long = "member")]
        members: Vec<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ToolArg {
    Claude,
    Codex,
}

impl From<ToolArg> for Tool {
    fn from(t: ToolArg) -> Tool {
        match t {
            ToolArg::Claude => Tool::Claude,
            ToolArg::Codex => Tool::Codex,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum VerbArg {
    Assign,
    Amend,
    Cancel,
    Reassign,
    Open,
    Arbitrate,
}

impl From<VerbArg> for Verb {
    fn from(v: VerbArg) -> Verb {
        match v {
            VerbArg::Assign => Verb::Assign,
            VerbArg::Amend => Verb::Amend,
            VerbArg::Cancel => Verb::Cancel,
            VerbArg::Reassign => Verb::Reassign,
            VerbArg::Open => Verb::Open,
            VerbArg::Arbitrate => Verb::Arbitrate,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Decision,
    Handoff,
    Change,
    Note,
}

impl From<KindArg> for RecordKind {
    fn from(k: KindArg) -> RecordKind {
        match k {
            KindArg::Decision => RecordKind::Decision,
            KindArg::Handoff => RecordKind::Handoff,
            KindArg::Change => RecordKind::Change,
            KindArg::Note => RecordKind::Note,
        }
    }
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> i32 {
    let pager = CliPager {
        timeout: Duration::from_secs(3),
    };
    run_with(args, &pager)
}

/// `run` with an injected pager (tests use a fake).
pub fn run_with(args: impl IntoIterator<Item = OsString>, pager: &dyn Pager) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    let json = cli.json;
    match dispatch(cli.cmd, json, pager) {
        Ok(()) => 0,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"ok": false, "reason": e.reason.as_str(), "message": e.message})
                );
            } else {
                eprintln!("gaebu: {e}");
            }
            1
        }
    }
}

fn open_store() -> Result<Store> {
    Store::from_env()
}

fn print_outcome(json: bool, o: &Outcome) {
    if json {
        println!(
            "{}",
            serde_json::json!({"ok": true, "summary": o.summary, "ids": o.ids})
        );
    } else {
        println!("{}", o.summary);
    }
}

fn print_json<T: Serialize>(v: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "null".into())
    );
}

/// Resolve the CLI caller and require a mutating source.
fn mutating_caller(pager: &dyn Pager) -> Result<Caller> {
    let caller = identity::from_pager(pager)?;
    if !caller.may_mutate() {
        return Err(Error::new(
            Reason::UnresolvedCaller,
            "this session's identity came from a flag or environment variable; state changes need pager's host/native resolution",
        ));
    }
    Ok(caller)
}

/// Run a member operation: resolve caller, then act inside one transaction.
fn member_op(
    store: &Store,
    pager: &dyn Pager,
    json: bool,
    f: impl FnOnce(&mut State, u64, &Actor) -> Result<Outcome>,
) -> Result<()> {
    let caller = mutating_caller(pager)?;
    // Observe vacancies first so a gone session's seat and leases are freed.
    if let Err(e) = identity::refresh_bindings(store, pager) {
        eprintln!("gaebu: binding refresh skipped: {e}");
    }
    let now = now_ms();
    let out = store.transact(store::CLI_DEADLINE, |st| {
        ops::expire_leases(st, now);
        let actor = identity::actor(st, &caller)?;
        f(st, now, &actor)
    })?;
    print_outcome(json, &out);
    flush_outbox(store, pager);
    Ok(())
}

/// Run a root directive: the quote must come from this session's current turn.
fn root_op(
    store: &Store,
    pager: &dyn Pager,
    json: bool,
    quote: &str,
    f: impl FnOnce(&mut State, u64, &crate::model::Evidence) -> Result<Outcome>,
) -> Result<()> {
    let caller = mutating_caller(pager)?;
    let now = now_ms();
    let log = store.session(&caller.session_id);
    let global = store.session(GLOBAL_MARKERS);
    // Checked twice: once to fail fast, and again under the state lock so a
    // Stop, peer prompt or missed prompt recorded meanwhile still wins.
    let verify = || -> Result<crate::model::Evidence> {
        let ev = evidence::current_turn_evidence(
            &caller.session_id,
            &log.events()?,
            &log.composers()?,
            quote,
            now_ms(),
        )?;
        // A prompt the hook could not record (in this session, or in a hook
        // that could not even tell which session it was) may have arrived
        // after the quoted one; then the quoted prompt is not the current turn.
        if log.unrecorded_after(ev.at)? || global.unrecorded_after(ev.at)? {
            return Err(Error::new(
                Reason::NoUserEvidence,
                "a later prompt could not be recorded; restate the instruction",
            ));
        }
        Ok(ev)
    };
    let first = verify()?;
    let out = store.transact(store::CLI_DEADLINE, |st| {
        let ev = verify()?;
        if ev.sha256 != first.sha256 || ev.prompt_seq != first.prompt_seq {
            return Err(Error::new(
                Reason::NoUserEvidence,
                "the current prompt changed while waiting; restate the instruction",
            ));
        }
        f(st, now, &ev)
    })?;
    print_outcome(json, &out);
    flush_outbox(store, pager);
    Ok(())
}

/// Session-log name for markers left by hooks that could not identify their
/// session; they invalidate pending evidence everywhere.
pub const GLOBAL_MARKERS: &str = "_global";

/// Deliver pending notices through pager and drop the delivered ones.
/// Failures stay queued for the next call.
pub fn flush_outbox(store: &Store, pager: &dyn Pager) {
    let Ok(st) = store.read() else { return };
    if st.outbox.is_empty() {
        return;
    }
    let mut delivered = Vec::new();
    let mut failed = Vec::new();
    for n in &st.outbox {
        match pager.send(&n.to_session, &n.body) {
            Ok(()) => delivered.push(n.id.clone()),
            Err(e) => {
                eprintln!("gaebu: notice {} not delivered: {e}", n.id);
                failed.push(n.id.clone());
            }
        }
    }
    let _ = store.transact(store::CLI_DEADLINE, |st| {
        st.outbox.retain(|n| !delivered.contains(&n.id));
        for n in st.outbox.iter_mut() {
            if failed.contains(&n.id) {
                n.attempts += 1;
            }
        }
        Ok(())
    });
}

/// Resolve a member reference: id (`m-3`), pager alias, or title, among
/// active members (the caller's team first).
pub fn resolve_member(st: &State, team: Option<&str>, reference: &str) -> Result<String> {
    if let Some(m) = st.members.get(reference) {
        return Ok(m.id.clone());
    }
    let active = || {
        st.members
            .values()
            .filter(|m| m.status == MemberStatus::Active)
    };
    let pick = |hits: Vec<&crate::model::Member>| -> Option<String> {
        let in_team: Vec<_> = hits
            .iter()
            .filter(|m| team.map(|t| m.team == t).unwrap_or(true))
            .collect();
        if in_team.len() == 1 {
            Some(in_team[0].id.clone())
        } else if hits.len() == 1 {
            Some(hits[0].id.clone())
        } else {
            None
        }
    };
    if let Some(id) = pick(active().filter(|m| m.binding.alias == reference).collect()) {
        return Ok(id);
    }
    if let Some(id) = pick(active().filter(|m| m.title == reference).collect()) {
        return Ok(id);
    }
    Err(Error::new(
        Reason::NotFound,
        format!("no single active member matches '{reference}'"),
    ))
}

/// Resolve a bu reference: id (`b-2`) or name.
pub fn resolve_bu(st: &State, team: Option<&str>, reference: &str) -> Result<String> {
    if st.bus.contains_key(reference) {
        return Ok(reference.to_string());
    }
    let hits: Vec<_> = st
        .bus
        .values()
        .filter(|b| b.name == reference && team.map(|t| b.team == t).unwrap_or(true))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.id.clone()),
        _ => Err(Error::new(
            Reason::NotFound,
            format!("no single bu matches '{reference}'"),
        )),
    }
}

fn team_of(st: &State, actor: &Actor) -> Option<String> {
    st.members.get(&actor.member).map(|m| m.team.clone())
}

#[derive(Serialize)]
struct BoardView {
    rev: u64,
    teams: Vec<BoardTeam>,
    user_queue: Vec<String>,
}

#[derive(Serialize)]
struct BoardTeam {
    id: String,
    name: String,
    status: String,
    commander: String,
    bus: Vec<BoardBu>,
}

#[derive(Serialize)]
struct BoardBu {
    id: String,
    name: String,
    status: String,
    head: String,
    parent: Option<String>,
    members: Vec<BoardMember>,
}

#[derive(Serialize)]
struct BoardMember {
    id: String,
    display: String,
    alias: String,
    title: String,
    binding: String,
    duties: Vec<String>,
    leases: Vec<String>,
}

fn board_view(st: &State, now: u64) -> BoardView {
    let teams = st
        .teams
        .values()
        .map(|t| {
            let bus = st
                .bus
                .values()
                .filter(|b| b.team == t.id)
                .map(|b| BoardBu {
                    id: b.id.clone(),
                    name: b.name.clone(),
                    status: format!("{:?}", b.status).to_lowercase(),
                    head: st
                        .members
                        .get(&b.head)
                        .map(|m| card::display(st, m))
                        .unwrap_or_default(),
                    parent: b.parent.clone(),
                    members: st
                        .members
                        .values()
                        .filter(|m| m.bu == b.id && m.status == MemberStatus::Active)
                        .map(|m| BoardMember {
                            id: m.id.clone(),
                            display: card::display(st, m),
                            alias: m.binding.alias.clone(),
                            title: m.title.clone(),
                            binding: format!("{:?}", m.binding.state).to_lowercase(),
                            duties: st
                                .duties
                                .values()
                                .filter(|d| {
                                    d.assignee == m.id
                                        && !matches!(
                                            d.status,
                                            DutyStatus::Done
                                                | DutyStatus::Cancelled
                                                | DutyStatus::Declined
                                        )
                                })
                                .map(|d| {
                                    format!(
                                        "{} [{}] {}",
                                        d.id,
                                        format!("{:?}", d.status).to_lowercase(),
                                        d.outcome
                                    )
                                })
                                .collect(),
                            leases: st
                                .leases
                                .values()
                                .filter(|l| l.holder == m.id && l.expires_at > now)
                                .map(|l| l.key.clone())
                                .collect(),
                        })
                        .collect(),
                })
                .collect();
            BoardTeam {
                id: t.id.clone(),
                name: t.name.clone(),
                status: format!("{:?}", t.status).to_lowercase(),
                commander: st
                    .members
                    .get(&t.commander)
                    .map(|m| format!("가절월을 받은 {}", card::display(st, m)))
                    .unwrap_or_default(),
                bus,
            }
        })
        .collect();
    let user_queue = st
        .escalations
        .values()
        .filter(|e| e.to.is_none() && e.status == crate::model::EscalationStatus::Open)
        .map(|e| format!("{}: {}", e.id, e.question))
        .collect();
    BoardView {
        rev: st.rev,
        teams,
        user_queue,
    }
}

fn print_board(v: &BoardView) {
    println!("[개부 명부] rev {}", v.rev);
    for t in &v.teams {
        println!("팀 {} ({}) — {}", t.name, t.status, t.commander);
        for b in &t.bus {
            println!("  {} [{}] 장: {}", b.name, b.status, b.head);
            for m in &b.members {
                println!("    {} · {}", m.display, m.binding);
                for d in &m.duties {
                    println!("      소임 {d}");
                }
                for l in &m.leases {
                    println!("      점유 {l}");
                }
            }
        }
    }
    if !v.user_queue.is_empty() {
        println!("대협 대기: {}", v.user_queue.join(" / "));
    }
}

fn parse_member_spec(s: &str) -> Result<(String, Option<String>, Option<Tool>)> {
    let mut parts = s.splitn(3, ':');
    let alias = parts.next().unwrap_or_default().to_string();
    let session = parts.next().filter(|x| !x.is_empty()).map(str::to_string);
    let tool = match parts.next() {
        None | Some("") => None,
        Some("claude") => Some(Tool::Claude),
        Some("codex") => Some(Tool::Codex),
        Some(other) => {
            return Err(Error::new(
                Reason::InvalidArgument,
                format!("unknown tool '{other}'"),
            ));
        }
    };
    if alias.is_empty() {
        return Err(Error::new(Reason::InvalidArgument, "empty alias"));
    }
    Ok((alias, session, tool))
}

fn dispatch(cmd: Cmd, json: bool, pager: &dyn Pager) -> Result<()> {
    match cmd {
        Cmd::Hook { tool } => {
            let store = match open_store() {
                Ok(store) => store,
                Err(e) => {
                    eprintln!("gaebu: hook: {e}");
                    return Ok(());
                }
            };
            let pager = CliPager {
                timeout: Duration::from_millis(1_500),
            };
            crate::hook::run_stdio(tool.into(), &store, &pager);
            Ok(())
        }
        Cmd::Mcp => {
            let store = open_store()?;
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            let mut stdout = std::io::stdout();
            crate::mcp::serve(&mut lock, &mut stdout, &store, pager)
                .map_err(|e| Error::new(Reason::InvalidArgument, e.to_string()))
        }
        Cmd::Me {
            card: text_only,
            session,
        } => {
            let store = open_store()?;
            let session_id = match session {
                Some(s) => s,
                None => identity::from_pager(pager)?.session_id,
            };
            let st = store.read()?;
            let member = crate::authz::member_for_session(&st, &session_id)
                .ok_or_else(|| {
                    Error::new(Reason::NotMember, "this session holds no seat in any team")
                })?
                .id
                .clone();
            let last_seen = store
                .session(&session_id)
                .sighting()
                .ok()
                .and_then(|s| s.last_seen);
            let c = card::build(&st, &member, now_ms(), last_seen)
                .ok_or_else(|| Error::new(Reason::NotMember, "member is not active"))?;
            if json {
                print_json(&c);
            } else if text_only {
                println!("{}", card::render(&c));
            } else {
                println!("{}", card::render(&c));
                for d in &c.duties {
                    println!(
                        "- {} [{}] {}\n  범위 {}\n  완료 {}\n  결정권 {}",
                        d.id,
                        d.status,
                        d.outcome,
                        d.scope.join(", "),
                        d.done_criteria,
                        d.decision_rights
                    );
                }
            }
            Ok(())
        }
        Cmd::Board { since } => {
            let store = open_store()?;
            if let Err(e) = identity::refresh_bindings(&store, pager) {
                eprintln!("gaebu: binding refresh skipped: {e}");
            }
            let st = store.read()?;
            if let Some(s) = since
                && st.rev <= s
            {
                if json {
                    println!("{}", serde_json::json!({"rev": st.rev, "unchanged": true}));
                } else {
                    println!("unchanged (rev {})", st.rev);
                }
                return Ok(());
            }
            let v = board_view(&st, now_ms());
            if json {
                print_json(&v);
            } else {
                print_board(&v);
            }
            Ok(())
        }
        Cmd::Appoint {
            alias,
            team,
            project,
            title,
            evidence,
        } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, |st, now, ev| {
                ops::root_appoint(
                    st,
                    now,
                    ev,
                    &alias,
                    team.as_deref(),
                    &project,
                    title.as_deref(),
                )
            })
        }
        Cmd::Assign {
            aliases,
            to,
            evidence,
        } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, |st, now, ev| {
                ops::root_assign(st, now, ev, &aliases, &to)
            })
        }
        Cmd::Dismiss { alias, evidence } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, |st, now, ev| {
                ops::root_dismiss(st, now, ev, &alias)
            })
        }
        Cmd::CancelTeam { team, evidence } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, |st, now, ev| {
                ops::root_cancel_team(st, now, ev, &team)
            })
        }
        Cmd::Rebind {
            member,
            alias,
            evidence,
        } => {
            let store = open_store()?;
            match evidence {
                Some(q) => root_op(&store, pager, json, &q, |st, now, ev| {
                    let m = resolve_member(st, None, &member)?;
                    ops::root_rebind(st, now, ev, &m, &alias)
                }),
                None => member_op(&store, pager, json, |st, now, actor| {
                    let team = team_of(st, actor);
                    let m = resolve_member(st, team.as_deref(), &member)?;
                    ops::member_rebind(st, now, actor, &m, &alias)
                }),
            }
        }
        Cmd::Bu(b) => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                let team = team_of(st, actor);
                match b {
                    BuCmd::Open {
                        head,
                        name,
                        verbs,
                        scope,
                    } => {
                        let h = resolve_member(st, team.as_deref(), &head)?;
                        let verbs: Vec<Verb> = verbs.into_iter().map(Into::into).collect();
                        ops::bu_open(st, now, actor, &h, name.as_deref(), &verbs, &scope)
                    }
                    BuCmd::Close { bu } => {
                        let id = resolve_bu(st, team.as_deref(), &bu)?;
                        ops::bu_close(st, now, actor, &id)
                    }
                    BuCmd::Cancel { bu, reason } => {
                        let id = resolve_bu(st, team.as_deref(), &bu)?;
                        ops::bu_cancel(st, now, actor, &id, &reason)
                    }
                }
            })
        }
        Cmd::Member(m) => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                let team = team_of(st, actor);
                match m {
                    MemberCmd::Add { alias, bu, title } => {
                        let id = resolve_bu(st, team.as_deref(), &bu)?;
                        ops::member_add(st, now, actor, &id, &alias, title.as_deref())
                    }
                    MemberCmd::Move { member, bu } => {
                        let mid = resolve_member(st, team.as_deref(), &member)?;
                        let id = resolve_bu(st, team.as_deref(), &bu)?;
                        ops::member_move(st, now, actor, &mid, &id)
                    }
                }
            })
        }
        Cmd::Grant {
            member,
            verbs,
            scope,
        } => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                let team = team_of(st, actor);
                let m = resolve_member(st, team.as_deref(), &member)?;
                let verbs: Vec<Verb> = verbs.into_iter().map(Into::into).collect();
                ops::grant(st, now, actor, &m, &verbs, &scope)
            })
        }
        Cmd::Revoke { grant } => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                ops::revoke(st, now, actor, &grant)
            })
        }
        Cmd::Title { member, title } => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                let team = team_of(st, actor);
                let m = resolve_member(st, team.as_deref(), &member)?;
                ops::title_set(st, now, actor, &m, &title)
            })
        }
        Cmd::Duty(d) => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                let team = team_of(st, actor);
                match d {
                    DutyCmd::Issue { to, spec } => {
                        let m = resolve_member(st, team.as_deref(), &to)?;
                        ops::duty_issue(st, now, actor, &m, spec.spec())
                    }
                    DutyCmd::Accept { duty, version } => {
                        ops::duty_respond(st, now, actor, &duty, version, true, None)
                    }
                    DutyCmd::Decline { duty, note } => {
                        ops::duty_respond(st, now, actor, &duty, None, false, Some(&note))
                    }
                    DutyCmd::Block { duty, note } => {
                        ops::duty_progress(st, now, actor, &duty, DutyStatus::Blocked, Some(&note))
                    }
                    DutyCmd::Unblock { duty } => {
                        ops::duty_progress(st, now, actor, &duty, DutyStatus::Accepted, None)
                    }
                    DutyCmd::Done { duty, note } => {
                        ops::duty_progress(st, now, actor, &duty, DutyStatus::Done, Some(&note))
                    }
                    DutyCmd::Amend {
                        duty,
                        version,
                        spec,
                    } => ops::duty_amend(st, now, actor, &duty, version, spec.spec()),
                    DutyCmd::Cancel { duty, reason } => {
                        ops::duty_cancel(st, now, actor, &duty, &reason)
                    }
                }
            })
        }
        Cmd::Lease(l) => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| match l {
                LeaseCmd::Acquire {
                    key,
                    shared,
                    ttl,
                    reason,
                } => {
                    let mode = if shared {
                        LeaseMode::Shared
                    } else {
                        LeaseMode::Exclusive
                    };
                    ops::lease_acquire(st, now, actor, &key, mode, ttl.map(|m| m * 60_000), &reason)
                }
                LeaseCmd::Renew { lease, ttl } => {
                    ops::lease_renew(st, now, actor, &lease, ttl.map(|m| m * 60_000))
                }
                LeaseCmd::Release { lease } => ops::lease_release(st, now, actor, &lease),
                LeaseCmd::ForceRelease { lease, reason } => {
                    ops::lease_force_release(st, now, actor, &lease, &reason)
                }
            })
        }
        Cmd::Escalate { question, duty } => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                ops::escalate(st, now, actor, duty.as_deref(), &question)
            })
        }
        Cmd::Answer { escalation, answer } => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                ops::escalation_answer(st, now, actor, &escalation, &answer)
            })
        }
        Cmd::Record { text, kind, refs } => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                ops::record_add(st, now, actor, kind.into(), &refs, &text)
            })
        }
        Cmd::Evidence(EvidenceCmd::Composer { session, sha256 }) => {
            let store = open_store()?;
            let sha256 = match sha256 {
                Some(h) => h,
                None => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                        .map_err(|e| Error::new(Reason::InvalidArgument, e.to_string()))?;
                    evidence::sha256_hex(&evidence::normalize(&text))
                }
            };
            store.session(&session).append_composer(&ComposerRecord {
                at: now_ms(),
                sha256,
            })?;
            if json {
                println!("{}", serde_json::json!({"ok": true}));
            }
            Ok(())
        }
        Cmd::Session(SessionCmd::Heartbeat {
            session,
            mod_active,
        }) => {
            let store = open_store()?;
            let log = store.session(&session);
            let now = now_ms();
            if mod_active {
                log.append(now, EventKind::ModHeartbeat, None, None, None)?;
            }
            log.update_sighting(|s| {
                s.last_seen = Some(now);
                if mod_active {
                    s.mod_heartbeat = Some(now);
                }
            })?;
            if json {
                println!("{}", serde_json::json!({"ok": true}));
            }
            Ok(())
        }
        Cmd::Dev(DevCmd::Seed {
            team,
            project,
            commander,
            members,
        }) => {
            let store = open_store()?;
            if store.is_default_home() {
                return Err(Error::new(
                    Reason::InvalidArgument,
                    "dev seed is refused on the default home; set GAEBU_HOME to a scratch directory",
                ));
            }
            let (c_alias, c_session, c_tool) = parse_member_spec(&commander)?;
            let members = members
                .iter()
                .map(|m| parse_member_spec(m))
                .collect::<Result<Vec<_>>>()?;
            let spec = SeedSpec {
                team,
                project,
                commander_alias: c_alias,
                commander_session: c_session,
                commander_tool: c_tool,
                members,
            };
            let now = now_ms();
            let out = store.transact(store::CLI_DEADLINE, |st| ops::dev_seed(st, now, &spec))?;
            print_outcome(json, &out);
            Ok(())
        }
        Cmd::Repair => {
            let store = open_store()?;
            store.repair()?;
            println!("restored state.json from state.prev.json");
            Ok(())
        }
    }
}
