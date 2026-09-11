# ADR 0017 — Protocol N−1 window: refuse loudly, downgrade silently, never guess

Status: accepted (T-0028)
Context: ROADMAP §3.13 (updates must never strand an attached client), ADR 0006/0007
(one protocol), T-0013 (append-only schema discipline)

## The problem

An update ships a new daemon while an old client is still attached — or a new
client talks to a daemon that has not updated yet. Without stated rules, each
mismatch becomes an ad-hoc decision: sometimes a silent downgrade (the client
thinks it asked for one thing and got another), sometimes a hang (a frame nobody
decodes), sometimes a panic. The task is to write down, then enforce, exactly
what is refused versus what is downgraded — deterministically, so both directions
behave the same way every time.

## The decision

**Compatibility is a range check plus five rules, not a translation layer.**
`negotiate(server, wants)` accepts the highest version common to
`[server-1, server]` and echoes it in `Welcome.v`; anything outside is a typed
`Error` naming the offered versions and our range. A downgrade is therefore never
silent — the agreed version is on the wire — and a gap wider than one is a
deferred update (§3.13), not a guess.

The five rules, enforced in code:

- **(a) Unknown client→server *request* → typed `Error`, connection stays open.**
  The client asked for work the server does not know; it is told so, loudly, and
  can report it. Never a hang, never a silent discard of a state-mutating
  message. The session loop answers the synthesized `Error` and continues.
- **(b) Unknown server→client *event* → ignored and counted, never fatal.**
  Killing a session over news it does not understand would make every server
  addition a breaking change. The reader drops the frame and bumps
  `UNKNOWN_EVENTS` — ignored *and counted*, because an ignore nobody can observe
  is a silent discard with better manners.
- **(c) New `#[serde(default)]` field → silent downgrade.** Old readers simply do
  not see the field; new readers see the default. This is the append-only
  discipline T-0013 stated as a comment, now machine-checked by the compat
  suite: a message with an unknown *field* still decodes, because serde skips
  what the struct does not name.
- **(d) Renamed/removed field or renumbered variant → refuse.** The decode fails,
  the frame is classified as a request (unknown ops default to request — see
  below), and rule (a) answers. Renaming is a break, and breaks are refused.
- **(e) Gap > 1 → refuse.** A v2 server does not speak v0. The window is one
  version wide because the test corpus is two versions deep; a wider window is
  more code paths, not more compatibility.

**Classification without a full decode.** A `{op, v}` probe reads the MessagePack
map head for the `op` tag, so an unknown variant is classified request-or-event
*before* the typed decoder sees it. Unknown variants never panic and never
silently discard a state-mutating message. Unknown ops default to *request*:
a client that sent something the server does not know asked for work, and work
is refused rather than ignored. (The reverse default would discard a `send` from
the future as if it were news.)

**Per-verb versions are checked, not just the handshake.** `check_version(v)` on
every verb refuses a frame whose `v` is not ours — the handshake agrees on a
version, and a frame that claims another is either corrupt or hostile.

## What this rules out

- **A hand-rolled schema registry.** It pays no rent for two versions: the probe
  reuses `rmp-serde`'s encoding plus ~150 lines of map-head walking, and a
  registry would be a second source of truth for what the enum already says.
- **Dual readers (one decoder per version).** T-0013's append-only +
  `serde(default)` discipline keeps N−1 cheap; a change that cannot honor it is
  refused and scheduled as a deferred update. Two decoders would be two answers
  to "what did the client say".
- **Silent downgrade on empty `wants`.** An empty offer list is a refusal, not a
  default: guessing a version for a client that named none is how a downgrade
  goes silent.
- **A new dependency for classification** (`rmpv` for one function). The map-head
  walk is hand-rolled over raw bytes — fixmap/map16/map32, fixstr/str8/16/32,
  and a `skip_value` covering what the encoder emits. No scaffolding dependencies
  (things implementable in an afternoon) is the project rule, and this was an
  afternoon.

## Consequences

- `MIN_VERSION` (currently 0) is the floor the window computes from; bumping
  `VERSION` is a deliberate act with a compat corpus to prove it.
- Refusals are recorded, never partial: a refused connection writes an audit row
  and leaves no session (T-0033's `auth.reject` path already does this; the
  version refusal uses the same door).
- The 1 MB frame budget is enforced in `frame_body_len`, before any allocation —
  a length prefix naming more is corruption, not a large message, and no caller
  decides "too big" for itself anymore.
- The window is proven for v0→v1 only. The first major break is out of scope,
  though the rules name it refused. Sibling protocol work (relay v0, mesh,
  handoff) must land its messages as v1 appends under these rules.
