# Threat model and operational boundaries

Gaebu v0 assumes cooperative agent processes running under one local OS user.
It preserves organization and checks coordination rules despite context resets
and separate sessions. It is not a security boundary between those processes.

## Accidents the engine checks

- Peer mail, queued pager pokes and recognized host notification wrappers being
  mistaken for the human's current root instruction.
- Root quotes reused after a later prompt or an observed Stop.
- Delegation outside valid verbs or declared scope.
- Actions by pending, replaced or mismatched session bindings.
- Conflicting leases and state transitions after cancellation.

Agent interpretation remains necessary: a quote occurring in the current prompt
does not establish that the human intended the particular operation or target.
The engine checks provenance and boundaries, not natural-language meaning.

## Root evidence window

UserPromptSubmit events determine prompt sequence. Only the latest user or
excluded prompt can supply evidence, and an observed Stop closes the window.
A quote must occur in that prompt after whitespace normalization and contain at
least six Unicode scalar values. Prompt hashing trims outer whitespace and
converts CRLF to LF. Prompt text is bounded, so a quote outside the retained
prefix cannot be checked.

The most recent 50 user-prompt texts per session, each at most 4 KB, are retained
under `~/.gaebu/sessions/`. These records can contain private instructions and
project information; treat that directory as local session data.

`~/.gaebu/capture.on` enables developer capture of raw hook stdin from every
session into `~/.gaebu/capture/`. Capture may include private prompt and host
metadata. Remove `capture.on` after collecting the needed fixtures and redact
personal content and session identifiers before publishing them.

For a mod-active Claude prompt, a matching composer record is required. Heartbeat
activity is evaluated at prompt time and expires after one hour. Hook-only
prompts still invalidate an older prompt even when they cannot supply evidence.
Without an active mod, a prompt passing the exclusion filter can supply evidence.

Composer and hook records may arrive in either order. Matching uses the same
session's digest, consumes records once in prompt sequence order and is bounded
by Stop events rather than a fixed ten-second timeout. A record on the exact
millisecond of a Stop is conservatively excluded from the next turn. Composer
provenance overrides a string exclusion, allowing a human to discuss those
marker strings. Records are meaningful only within the caller's session log;
the event structure does not independently identify its session.

A session counts as mod-active for a prompt when a mod heartbeat falls within
one hour before the prompt or up to ten seconds after it. The ten seconds absorb
the mod's heartbeat arriving just after the shell hook records the prompt, since
the order of the two is not guaranteed. Disabling the mod mid-session can leave
hook-only evidence unavailable until the last heartbeat expires, for at most one
hour. An observed Stop closes evidence
even if another Stop hook keeps the host turn going. These are conservative
failures. A peer delivery that does not trigger UserPromptSubmit may not close
the evidence window; that residual risk is accepted for cooperative processes.

## Deliberate local forgery is outside the model

A process with the same user's filesystem and execution access can forge hook
stdin, write or alter Gaebu files, invoke the composer-record CLI itself, inject
host messaging socket content, or use terminal send-keys to imitate typed input.
The CLI and MCP do not prevent those actions. A digest detects matching text; it
does not establish who typed it. Host metadata and composer provenance support
accident prevention under the cooperative assumption.

The deployed `Bash(gaebu*)` permission also lets an agent invoke the composer
recording command directly. Deliberately doing so is local forgery outside this
threat model; the permission is not proof of a human composer event.

Filtering recognizes a finite set of observed or explicitly documented wrapper
markers. New delivery formats can be classified as user prompts until captured
and covered by a narrow rule. A bare `From ` prefix is not an exclusion because
it also occurs in ordinary user prose. A new delivery path or changed host
wording can bypass those heuristics.
Synthetic wrapper tests prove the implemented string rules only. Real host
capture and replay, when performed, must be reported separately with host version
and event type; synthetic fixtures must never be labeled as actual captures.

## Identity and pending appointments

An alias designation initially creates a pending member. The target's hook must
confirm its representative Pager alias against its own session id before the
member can act. The engine uses Pager's public interface rather than its DB.

Pager cannot publicly resolve another alias to a session id at designation time.
If an alias moves before the first confirmation, the new session may take that
pending binding. The designation is therefore not a session pin at appointment
time. Report confirmation separately, including its session id and timestamp.
After binding, a session mismatch is stale and requires explicit rebind.

Lack of recent hook sightings means quiet, not proven death. Unknown Pager
liveness is not proof that a session is vacant. Pending members cannot exercise
authority, accept duties or acquire leases. A confirmed binding still does not
make untrusted message contents safe.

## Scope, storage and quality boundaries

Scope uses project-relative paths, directory prefixes or explicit resources;
arbitrary glob containment is refused. `**` covers paths and `res:**` resources.
Comparisons lowercase names on every OS, so case-sensitive filesystems can have
broader scope matches than their native path identity. Teams spanning multiple
repository roots are outside v0.

State mutation uses an OS file lock and durable replacement; reads can use the
previous generation with a warning after corruption. Writes refuse corrupt
current state. A state written by a newer schema is refused as unsupported,
with no fallback and no repair over it. The schema-2 migration keeps the
schema-1 original in `state.v1.json` and writes the previous generation as v2
first, so a gaebu 0.1 binary fails closed rather than reading a stale v1 state.
Per-session logs use their own locks. Those mechanisms coordinate
cooperative writers and do not prevent a same-user process bypassing the engine.

Leases are advisory reservations, not filesystem edit guards. Duty completion
records the recipient's assertion; review and runtime verification establish
quality. Automated tests and seeded fixture teams do not prove a real human's
appointment workflow, host delivery, or context restoration. Report each tested
evidence class separately and mark unperformed runtime checks as UNVERIFIED.

## Residual races and delivery

A root directive re-checks its evidence under the state lock, but the hook
records prompts and Stop events under a per-session lock. A prompt or Stop that
lands between that re-check and the commit can still lose the race; the window
is the length of one in-memory operation. Notices are delivered at least once:
a notice leaves the outbox only after pager accepted it, so a crash between
sending and bookkeeping can deliver it twice but never drops it silently.


## Holds, sources and strength

A hold, like every root directive, rests on a quote of the caller session's
current user prompt. Any session bound in the target's team may set or lift a
hold on any duty or bu of that team: the model has one human, and where they
spoke does not change who they are. A peer that can forge prompts can forge a
hold too (see the forgery section above).

A hold on a duty does not follow its brief: a superior that cancels a held duty
and issues the same brief again starts an unheld duty. Only the rules ask
agents not to do that. To stop a line of work whatever duties carry it, hold
the bu.

A duty's `source` is matched against the user prompts the hook recorded in the
issuer's own session. The log keeps the last 50 user prompts and up to 4 KB of
each, so an older or later-cut quote is refused. A source records which words
a duty answers; it grants no authority.

`strength` is the calling agent's own judgement of how directly the user's
words named the action and target. Gaebu records it and shows inferred root
authority as 가절월(추론); it does not verify the judgement or refuse inferred
directives.

Notices are an attention budget, not a delivery guarantee for every change:
Record changes reach a member on its next card, not through pager. Dedupe
compares text only while a notice is still queued.
