//! Command line. `run` returns the process exit code: 0 done, 1 refused
//! (a typed reason is printed), 2 usage error.

use crate::card;
use crate::error::{Error, Reason, Result};
use crate::evidence;
use crate::identity::{self, Caller};
use crate::model::{Actor, DutyStatus, LeaseMode, RecordKind, State, Tool, Verb, now_ms};
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
        /// Print nothing but `unchanged` when the board still has this token.
        #[arg(long)]
        since: Option<String>,
    },
    /// Root directive: give 가절월 to a pager alias (quote the user's words).
    /// With --explicit on a commander appointed from inferred words, confirms
    /// the appointment instead.
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
        /// The user's words name both this action and its target.
        #[arg(long)]
        explicit: bool,
    },
    /// Root directive: put aliases under a commander.
    Assign {
        aliases: Vec<String>,
        #[arg(long)]
        to: String,
        #[arg(long)]
        evidence: String,
        /// The user's words name both this action and its target.
        #[arg(long)]
        explicit: bool,
    },
    /// Root directive: dismiss a member (revokes everything below it).
    Dismiss {
        alias: String,
        #[arg(long)]
        evidence: String,
        /// The user's words name both this action and its target.
        #[arg(long)]
        explicit: bool,
    },
    /// Root directive: cancel a whole team.
    CancelTeam {
        team: String,
        #[arg(long)]
        evidence: String,
        /// The user's words name both this action and its target.
        #[arg(long)]
        explicit: bool,
    },
    /// Re-point a member at a new alias. With --evidence it is a root
    /// directive; without, it needs `reassign` over the member.
    Rebind {
        member: String,
        #[arg(long)]
        alias: String,
        #[arg(long)]
        evidence: Option<String>,
        /// The user's words name both this action and its target.
        #[arg(long)]
        explicit: bool,
    },
    /// Root directive: hold a duty (`d-…`) or a bu (`b-…`) until the human
    /// lifts it. Questions and automated feedback never lift a hold.
    Hold {
        target: String,
        /// The release condition, in the user's terms (1-120 characters).
        #[arg(long)]
        until: String,
        #[arg(long)]
        evidence: String,
        /// The user's words name both this action and its target.
        #[arg(long)]
        explicit: bool,
    },
    /// Root directive: lift the human hold on a duty or bu.
    Release {
        target: String,
        #[arg(long)]
        evidence: String,
        /// The user's words name both this action and its target.
        #[arg(long)]
        explicit: bool,
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
    /// Canonical project schedules and operational work distribution.
    #[command(subcommand)]
    Schedule(crate::schedule::ScheduleCmd),
    /// Archive eligible terminal runtime history.
    Compact {
        /// Permanently delete older history to repair missing-segment holes (commander only).
        #[arg(long)]
        heal: bool,
    },
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
    /// Manage my own escalations.
    #[command(subcommand)]
    Escalation(EscalationCmd),
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
    /// Atomically form a subordinate unit with its existing workers.
    Form {
        #[arg(long)]
        head: String,
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_delimiter = ',')]
        verbs: Vec<VerbArg>,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
    },
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

#[derive(Subcommand)]
enum EscalationCmd {
    /// Clear an open question I raised.
    Withdraw {
        escalation: String,
        #[arg(long)]
        note: Option<String>,
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
    /// The user's words this duty answers, quoted from a prompt in your own
    /// session.
    #[arg(long)]
    source: Option<String>,
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
            source_quote: self.source,
            source: None,
        }
    }
}

pub use crate::evidence::attach_source;

#[derive(Subcommand)]
enum DutyCmd {
    /// Read the complete contract, even after completion, without changing state.
    Show {
        duty: String,
        #[arg(long)]
        session: Option<String>,
    },
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
        /// The mod's prompt.compose hook added its card section, so the hook
        /// can shorten its own card.
        #[arg(long)]
        compose: bool,
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
    let args: Vec<OsString> = args.into_iter().collect();
    // A hook must never fail its host, even when it is registered wrongly:
    // exit 2 from a prompt hook can block the prompt.
    let is_hook = args
        .iter()
        .skip(1)
        .find(|a| !a.to_string_lossy().starts_with('-'))
        .is_some_and(|a| a == "hook");
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if is_hook || !e.use_stderr() { 0 } else { 2 };
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
    // A state change must not proceed on bindings it could not refresh.
    identity::refresh_bindings(store, pager)?;
    let now = now_ms();
    let out = crate::schedule::transaction(store, &caller, |st, actor| {
        ops::expire_leases(st, now);
        f(st, now, actor)
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
    explicit: bool,
    f: impl FnOnce(&mut State, u64, &crate::model::Evidence) -> Result<Outcome>,
) -> Result<()> {
    let caller = mutating_caller(pager)?;
    let now = now_ms();
    let log = store.session(&caller.session_id);
    let global = store.session(GLOBAL_MARKERS);
    // Checked twice: once to fail fast, and again under the state lock so a
    // Stop, peer prompt or missed prompt recorded meanwhile still wins.
    let verify = || -> Result<crate::model::Evidence> {
        let mut ev = evidence::current_turn_evidence(
            &caller.session_id,
            &log.events()?,
            &log.composers()?,
            quote,
            now_ms(),
        )?;
        ev.strength = if explicit {
            crate::model::Strength::Explicit
        } else {
            crate::model::Strength::Inferred
        };
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
pub use crate::store::GLOBAL_MARKERS;

pub use crate::pager::flush_outbox;

/// Lease minutes to ms, capped at the 8 h maximum ops enforces anyway.
fn ttl_ms(minutes: Option<u64>) -> Option<u64> {
    minutes.map(|m| m.min(8 * 60).saturating_mul(60_000))
}

pub use crate::authz::{resolve_bu, resolve_member};

fn team_of(st: &State, actor: &Actor) -> Option<String> {
    st.members.get(&actor.member).map(|m| m.team.clone())
}

fn print_board(v: &card::BoardView) {
    println!("[개부 명부] rev {}", v.rev);
    for t in &v.teams {
        println!("팀 {} ({}) — {}", t.name, t.status, t.commander);
        for b in &t.bus {
            let indent = "  ".repeat(b.depth + 1);
            match &b.hold {
                Some(until) => println!(
                    "{indent}{} [{}] 장: {} · 보류 — {until}",
                    b.name, b.status, b.head
                ),
                None => println!("{indent}{} [{}] 장: {}", b.name, b.status, b.head),
            }
            for m in &b.members {
                println!("{indent}  {} · {}", m.display, m.binding);
                for d in &m.duties {
                    match &d.hold {
                        Some(until) => println!(
                            "{indent}    소임 {} [{}] {} — {until}",
                            d.id, d.stage_label, d.outcome
                        ),
                        None => println!(
                            "{indent}    소임 {} [{}] {}",
                            d.id, d.stage_label, d.outcome
                        ),
                    }
                }
                for l in &m.leases {
                    println!("{indent}    점유 {l}");
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
            let st = store.read_view()?;
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
            let mut c = card::build(&st, &member, now_ms(), last_seen)
                .ok_or_else(|| Error::new(Reason::NotMember, "member is not active"))?;
            card::enrich(&store, &st, &mut c);
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
            let st = store.read_view()?;
            let mut v = card::board(&st, now_ms(), None, &|sid| store.last_seen(sid));
            card::enrich_board(&store, &st, &mut v);
            if since.as_deref() == Some(v.token.as_str()) {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"rev": v.rev, "token": v.token, "unchanged": true})
                    );
                } else {
                    println!("unchanged (rev {})", v.rev);
                }
                return Ok(());
            }
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
            explicit,
        } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, explicit, |st, now, ev| {
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
            explicit,
        } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, explicit, |st, now, ev| {
                ops::root_assign(st, now, ev, &aliases, &to)
            })
        }
        Cmd::Dismiss {
            alias,
            evidence,
            explicit,
        } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, explicit, |st, now, ev| {
                ops::root_dismiss(st, now, ev, &alias)
            })
        }
        Cmd::CancelTeam {
            team,
            evidence,
            explicit,
        } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, explicit, |st, now, ev| {
                ops::root_cancel_team(st, now, ev, &team)
            })
        }
        Cmd::Hold {
            target,
            until,
            evidence,
            explicit,
        } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, explicit, |st, now, ev| {
                ops::root_hold(st, now, ev, &target, &until)
            })
        }
        Cmd::Release {
            target,
            evidence,
            explicit,
        } => {
            let store = open_store()?;
            root_op(&store, pager, json, &evidence, explicit, |st, now, ev| {
                ops::root_release(st, now, ev, &target)
            })
        }
        Cmd::Rebind {
            member,
            alias,
            evidence,
            explicit,
        } => {
            let store = open_store()?;
            match evidence {
                Some(q) => root_op(&store, pager, json, &q, explicit, |st, now, ev| {
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
                    BuCmd::Form {
                        head,
                        members,
                        name,
                        verbs,
                        scope,
                    } => {
                        let verbs = verbs.into_iter().map(Into::into).collect();
                        ops::bu_form(
                            st,
                            now,
                            actor,
                            &ops::BuFormSpec {
                                head,
                                members,
                                name,
                                verbs,
                                scope,
                            },
                        )
                    }
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
        Cmd::Schedule(cmd) => crate::schedule::run_cli(cmd, json, pager),
        Cmd::Compact { heal } => {
            let store = open_store()?;
            let caller = mutating_caller(pager)?;
            identity::refresh_bindings(&store, pager)?;
            let result = crate::schedule::compact(&store, &caller, heal)?;
            if json {
                print_json(&result);
            } else {
                println!(
                    "archived duties: {}, escalations: {}, records: {}",
                    result["duties"], result["escalations"], result["records"]
                );
                println!("expired history: {}", result["expired"]);
                for field in ["healed", "heal_deferred", "hole_remaining"] {
                    if let Some(value) = result.get(field) {
                        println!("{field}: {value}");
                    }
                }
                println!(
                    "retained bytes: {} (limit per team/stream: {})",
                    result["retained_bytes"], result["limit_bytes"]
                );
                if let Some(deferred) = result.get("expiry_deferred") {
                    println!("expiry deferred: {deferred}");
                }
                if result["over_limit"]
                    .as_array()
                    .is_some_and(|entries| !entries.is_empty())
                {
                    println!("archives still over limit: {}", result["over_limit"]);
                }
            }
            Ok(())
        }
        Cmd::Duty(DutyCmd::Show { duty, session }) => {
            let store = open_store()?;
            let session = match session {
                Some(s) => s,
                None => identity::from_pager(pager)?.session_id,
            };
            let state = store.read_view()?;
            let duty = crate::compact::full_duty(&store, &state, &session, &duty)?;
            if json {
                print_json(&duty);
            } else {
                println!(
                    "{} [{}] {}\n의도: {}\n범위: {}\n결정권: {}\n완료 기준: {}\n계획: {}\n작업 위치: {}\n버전: {}\n메모: {}",
                    duty.id,
                    card::status_word(duty.status),
                    duty.outcome,
                    duty.intent,
                    duty.scope.join(", "),
                    duty.decision_rights,
                    duty.done_criteria,
                    duty.plan_ref.as_deref().unwrap_or("—"),
                    duty.exec_root.as_deref().unwrap_or("—"),
                    duty.version,
                    duty.note.as_deref().unwrap_or("—")
                );
            }
            Ok(())
        }
        Cmd::Duty(d) => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                let team = team_of(st, actor);
                match d {
                    DutyCmd::Show { .. } => Err(Error::new(
                        Reason::InvalidArgument,
                        "duty show is read-only",
                    )),
                    DutyCmd::Issue { to, spec } => {
                        let m = resolve_member(st, team.as_deref(), &to)?;
                        let mut spec = spec.spec();
                        attach_source(&store, &actor.session_id, &mut spec)?;
                        ops::duty_issue(st, now, actor, &m, spec)
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
                    } => {
                        let mut spec = spec.spec();
                        attach_source(&store, &actor.session_id, &mut spec)?;
                        ops::duty_amend(st, now, actor, &duty, version, spec)
                    }
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
                    ops::lease_acquire(st, now, actor, &key, mode, ttl_ms(ttl), &reason)
                }
                LeaseCmd::Renew { lease, ttl } => {
                    ops::lease_renew(st, now, actor, &lease, ttl_ms(ttl))
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
        Cmd::Escalation(EscalationCmd::Withdraw { escalation, note }) => {
            let store = open_store()?;
            member_op(&store, pager, json, |st, now, actor| {
                ops::escalation_withdraw(st, now, actor, &escalation, note.as_deref())
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
            compose,
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
                if compose {
                    s.compose_heartbeat = Some(now);
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
