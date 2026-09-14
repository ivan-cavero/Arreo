# Notifications — which agent transitions are worth telling you about (T-0093)

> Operator's reference for the `[notify]` policy: what a notification is in this
> product, which transitions produce one, the four reasons one is withheld, the
> commands that answer the questions the feature creates — *why was I not told?*
> (`arreo notify --why`), *is my configuration doing what I think?*
> (`arreo notify --policy`) — and the quick actions that answer a notification
> where it is read (`arreo notify act`, §9). The rule itself is
> `crates/arreo-core/src/notify/`; the rows are the audit log
> ([docs/audit.md](audit.md)).

**One sentence: a notification here is an audit row, not a push** — the daemon
decides, per state transition, whether to tell you, and writes down *both*
answers (`notify.sent` when it tells you, `notify.suppressed` when it does not),
so "why was I not told?" has an answer that outlives the daemon.

```console
arreo notify --why <pane> [--json] [--socket PATH] [--config PATH]
arreo notify --policy [--config PATH]
arreo notify act <pane> <reply|skip|kill> [--text ...] [--socket PATH]
```

## 1. What a notification is

The state engine already knows when an agent becomes `blocked`, asks a
`question`, or finishes. The `[notify]` policy is the operator's answer to *which
of those are news*, and the daemon applies it on **every state transition**:

1. the daemon pumps each pane's state engine — including panes nobody is
   attached to, which is the point: an agent that blocks while you are at lunch
   must be classified, not just one you happen to be watching;
2. for each transition it asks the policy (`on`-rules → quiet hours →
   coalescing → same episode, §3) with the little history the rule needs;
3. it writes **one row per decision**, under one of two actions:

| Action | Meaning |
| --- | --- |
| `notify.sent` | the policy said to tell you; the row carries the sentence |
| `notify.suppressed` | the policy withheld it; the row carries **why** |
| `notify.act` | a quick action was taken on a notification (reply / skip / kill) — §9 |

A row's shape (the same columns every audit row has — see
[docs/audit.md](audit.md) §2):

| Column | Value for a notification row |
| --- | --- |
| `device` | `daemon` — the background tick, not a human |
| `agent` | the **pane id**, so "what have I been told about this pane" is one filter |
| `prompt` | the human sentence: `blocked (inferred:silence)`, `question: Proceed?` |
| `detail` | `state=blocked; blocked (inferred:silence) actions=skip,kill` for a sent row, `state=blocked; quiet-hours — inside quiet hours 22:00-07:00 actions=skip,kill` for a suppressed one — the `actions=` tail is the bounded list of quick actions that answer the notification (§9), and an older reader that never heard of it reads the row exactly as before |
| `action` | `notify.sent` / `notify.suppressed` / `notify.act` |

**The log is the memory.** "Have I already told them about this pane, and when?"
is answered by reading the newest `notify.sent` row for that pane — there is no
second table to fall out of step with the log, and a daemon restart mid-episode
does not re-notify. `arreo audit export --action notify.suppressed` is the whole
withheld history; `arreo notify --why <pane>` is the same log asked about one
pane.

**Nothing rings.** There is no desktop pop-up and no phone push: the sentence is
written to the log, and `arreo audit` is where you read it. This is deliberate —
it is what makes the feature auditable, and it is why "was I told?" is a question
about rows rather than about a delivery mechanism.

## 2. The policy, in full

```toml
[notify]
# The three modifiers. They decide *delivery*, not relevance — and they go
# FIRST: in TOML a bare key after a [[notify.rules]] block belongs to that
# block, not to [notify] (see "Where the keys go" below).
once_per_episode = true    # default true
coalesce_secs = 300        # default 0 (off); at most a week (604800)
quiet_hours = "22:00-07:00"  # default: none
utc_offset_minutes = -300    # default 0; the offset quiet hours are read in

# What the policy is *about*: `[[notify.rules]]` entries are OR'ed, so any match
# makes a transition relevant. A field left out does not constrain.
[[notify.rules]]
on = ["blocked"]
panes = "build-*"          # pane-id pattern; `*` is the only wildcard
machine = "workbox"        # the machine the pane runs on

[[notify.rules]]
on = ["blocked", "question"]   # a second rule: any pane, any machine
```

When one unscoped rule is all you need, `on` is the shorthand for it:

```toml
[notify]
on = ["blocked", "question"]
```

Use **one or the other**: a section that sets both `on` and `[[notify.rules]]` is
refused rather than resolved — two spellings of one rule is a configuration the
operator did not mean.

**Where the keys go.** `[[notify.rules]]` opens an array of tables, and every
bare key written after it belongs to the last rule table, not to `[notify]`. A
`quiet_hours` line placed below the rules is therefore not a quiet hour — and
because a rule has no such key, it is dropped without complaint. Put the scalars
first, as above, and **run `arreo notify --policy` after editing the section**: it
prints what the loader actually got, which is the only way to see a key that
landed in the wrong table.

| Field | Default | What changing it does |
| --- | --- | --- |
| `on` | `["blocked", "question"]` | The states this policy is *about*. `unknown`, `working`, `idle`, `done` are valid names but are noise as defaults: `working`/`idle` are where a pane sits all day, and `done` fires on every short command. |
| `rules` | one unscoped rule for the two states above | More than one rule = *any* match makes a transition relevant (relevance is a yes/no). A rule with an empty `on` matches nothing and is refused — delete the rule instead. |
| `rules[].panes` | any pane | A pane-id pattern with one wildcard: `build-*` covers `build-1` and `build-`. Unset does not constrain. |
| `rules[].machine` | any machine | Scopes a rule to one machine's panes; unset does not constrain. |
| `once_per_episode` | `true` | `true` tells you when a run *starts*, not on every event inside it (§4). `false` notifies on every matching transition. |
| `coalesce_secs` | `0` (off) | After a notification for a pane, hold the next one for this many seconds. `0` disables it. Capped at a week: a stray digit (`600000000`) would be a window that never reopens, and refusing is kinder than a silence nobody can explain. |
| `quiet_hours` | none | A daily local wall-clock window in which nothing is delivered (§5). |
| `utc_offset_minutes` | `0` | Minutes from UTC to *your* local time, used only for `quiet_hours`. |

Every wrong *value* is loud rather than silently defaulted — an unknown state
name, an empty rule, both `on` and `rules`, a malformed window, or a coalescing
window over a week all refuse to load and name the field. An operator who asked
for notifications and silently did not get them has a bug they cannot see, which
is the failure this whole feature exists to make impossible. The one mistake this
does *not* catch is a key in the wrong table ("Where the keys go" above), which is
why `arreo notify --policy` is worth running after an edit.

**The file is the daemon's file.** The daemon reads `--config PATH`, else
`$ARREO_CONFIG`, and nothing else — there is no default config path. `arreo
notify --policy` reads exactly the same file, so point it at the same path (or
run it in the same environment). The policy is resolved when the daemon starts,
so a change takes effect on the next start.

## 3. The four reasons a notification is withheld, in the order they are asked

| # | Reason word | The question it answers | What the row says |
| --- | --- | --- | --- |
| 1 | `no-rule` | *Is this transition any of the policy's business?* | `no rule matches this transition` |
| 2 | `quiet-hours` | *Are we inside a window where nothing is delivered?* | `inside quiet hours 22:00-07:00` |
| 3 | `coalesced` | *Did I tell them something for this pane too recently?* | `a notification for this pane went out 120s ago, inside the 300s coalescing window` |
| 4 | `same-episode` | *Is this the run I already told them about?* | `already notified for this episode of blocked` |

The **order is the design**, not an accident of the code, because it decides
which reason the operator reads and what the counts mean:

- **`no-rule` first, always.** A transition no rule claims is not the policy's
  business; calling that "quiet hours" would be a lie in the one place an
  operator goes to find the truth. This is the row that tells you your config has
  a hole in it.
- **`quiet-hours` before `coalesced`.** Both are delivery-window answers, but if
  the duplicate check came first, a pane that flaps all night would be counted as
  duplicates and the quiet-hours rows would under-report the noise the window is
  actually holding back. Asking quiet hours first makes that count mean what it
  says: *everything the window suppressed, however many times it fired.*
- **`same-episode` last.** It is the only reason that says "this is not news"
  rather than "we are not delivering right now", and an operator reading a row
  wants the delivery reason when there is one.

A suppression is still a decision, so it is still a row: a quiet night is visible
in `arreo audit export --action notify.suppressed`, with the window that held it.

## 4. Episodes: one notification per run, not per event

An **episode** is a maximal run of one pane staying in one state.
`once_per_episode = true` (the default) means *tell me when the run starts, not
on every event inside it*. Both halves matter:

- **A repeat inside one run notifies once.** The engine can and does emit the
  same state twice — a second bell while already `blocked`, an inference that
  re-matches the same prompt — so `blocked → blocked` is a real event that must
  **not** be a second notification. That second event is withheld as
  `same-episode`.
- **A run that ends and starts again notifies twice.** `blocked → working →
  blocked` is news the second time: the operator who answered the first question
  needs to know the agent is asking something else now. Only the *destination*
  state has to match for the episode test; leaving the state ends the episode.

The test is the transition's own `from`/`to` pair plus the last state you were
told about, so it needs no per-pane memory beyond the log: a `from == to` event
is the one that "did not leave the state".

## 5. Quiet hours, and the DST caveat

```toml
[notify]
quiet_hours = "22:00-07:00"
utc_offset_minutes = -300     # US Eastern standard time
```

- The window is **local wall-clock** `HH:MM-HH:MM`, **half-open**: `22:00-07:00`
  includes 22:00 and excludes 07:00, so `00:00-07:00` and `07:00-12:00` tile the
  day with no minute in both. A window that wraps midnight is the common case and
  is handled as the two ranges it is.
- `utc_offset_minutes` is **minutes from UTC**, applied to the transition's
  timestamp: `-300` at 02:00 UTC is 21:00 the previous local day. Timestamps in
  the log stay UTC (`ts_ms`), so nothing is lost — the offset only decides which
  window a transition falls in.
- **Equal bounds mean an empty window**, not a full day: `22:00-22:00` holds
  nothing. An operator who wants no notifications at all turns the feature off,
  which is one decision rather than a clever reading of a window.

**The caveat, plainly: `utc_offset_minutes` is a fixed offset, not a time zone.**
Arreo has no time-zone database (a local-time library in a multi-threaded daemon
is a trap, and a tzdata dependency is not worth it for a policy window), so the
offset you write is the offset used all year. **A machine that observes daylight
saving will be an hour out for half the year** — your quiet hours will start an
hour early or an hour late relative to the wall clock, silently. Two honest
workarounds, and nothing else:

1. **Edit the offset twice a year.** `-300` in winter, `-240` in summer (for US
   Eastern; the same shift for any DST zone), and restart the daemon so it reads
   the new value. The window itself does not move — only the clock it is read in.
2. **Do not rely on the window for correctness.** Leave notifications off during
   your working day and let them fire; when one arrives at a bad time,
   `arreo notify --why <pane>` names the decision, and `arreo audit` shows the
   window that was in force.

This is written down here rather than hidden because a quiet hour that is quietly
an hour wrong is exactly the sort of thing an operator blames on the agent.

## 6. Off by default, on purpose

**No `[notify]` section means nothing is notified.** No rules, no rows, no
change in behaviour for a machine that upgrades.

That is a deliberate default, not an unfinished one: this feature is a background
writer, and turning it on by default would start appending rows for every pane
transition on every existing machine — a behaviour change nobody asked for, and
the sort of trail that makes an audit log unreadable ([docs/audit.md](audit.md)
§1: "an audit trail that records every poll is a trail nobody reads"). You turn
it on by writing the section:

```toml
[notify]
```

That alone is the useful default policy: `blocked` and `question`, once per
episode, no coalescing, no quiet hours.

## 7. "Why was I not told?" — `arreo notify --why`

```console
$ arreo notify --why worker-1
pane      worker-1
when      2026-09-14T15:04:32.891Z (1789398272891)
decision  suppressed (notify.suppressed)
state     blocked
actions   skip, kill
reason    quiet-hours — inside quiet hours 22:00-07:00
sentence  blocked (inferred:silence)
```

It reads the **audit log** the daemon wrote — no daemon needs to be running, and
the answer survives the pane, the daemon and the reboot — and reports the newest
`notify.sent` **or** `notify.suppressed` row for that pane: when it was, which
decision it was, the sentence, and for a suppression the reason word and its
text. The state comes from the row's own `state=` prefix, read with the same
function the daemon reads its history back with, so a writer and this reader
cannot drift — and the `actions` line is the bounded quick-action list the row
itself carries (§9), so the operator reads the notification and its answers
together. A row written before quick actions existed simply has no `actions`
line (and `null` in `--json`): absent, never an empty list.

**Three outcomes, and they stay three:**

| Situation | Output | Exit |
| --- | --- | --- |
| A decision was found | the block above | 0 |
| The pane has no rows | `no notification history for <pane> — either nothing has happened, or the daemon has no [notify] section` | 4 |
| The log cannot be read | the store's own error, naming the file | 2 |

The second message names **both** possibilities on purpose: the daemon's
`[notify]` section lives in *its* configuration file, read at *its* start-up, and
this command cannot see it from the log. Give it the file (`--config PATH`, else
`$ARREO_CONFIG`) and it sharpens the answer with what that file says — as a fact
about that file, never as a claim about the daemon's, which may be another one:

```console
$ arreo notify --why p3 --config /etc/arreo/arreo.toml
notify: no notification history for p3 — either nothing has happened, or the daemon has no [notify] section
notify: /etc/arreo/arreo.toml has no [notify] section, so a daemon started with it notifies nothing
```

Neither "notifications are off" nor "the log is empty" is printed as if it were
the answer: exit 4 is about *this pane*, and when the log itself does not exist
yet, one extra line says so before the same sentence.

### The script contract: `--json`

```json
{
  "schema": 1,
  "pane": "worker-1",
  "found": true,
  "action": "notify.suppressed",
  "at_ms": 1789398272891,
  "state": "blocked",
  "decision": "suppressed",
  "reason": "quiet-hours",
  "detail": "inside quiet hours 22:00-07:00",
  "prompt": "blocked (inferred:silence)",
  "actions": ["skip", "kill"]
}
```

- Keys are exactly those eleven; the shape is **additive-only** (a renamed or
  removed key is a wire break for a script).
- `decision` is the closed pair `sent` / `suppressed`. `reason` is the reason
  **word** (`no-rule`, `quiet-hours`, `coalesced`, `same-episode`) and `detail`
  is the text after it, so a script never has to take the row's format apart
  itself. `prompt` is the sentence the row recorded.
- `actions` is the bounded quick-action list the notification carries (§9), as
  the operator's words — `["reply", "skip", "kill"]` on a question, `["skip",
  "kill"]` on anything else. `null` for a row written before the feature, never
  an empty list.
- **A value that cannot be known is `null`, never invented** — the rule
  `arreo machines` and `arreo worktrees` follow. With `"found": false` everything
  but `schema`, `pane` and `found` is `null`; a **sent** row has no `reason` and
  no `detail` (nothing was withheld); a row that recorded no sentence has no
  `prompt`.
- `--json` prints one object and nothing else on stdout, so
  `arreo notify --why p1 --json | jq -r .decision` works with or without a
  daemon. A store that cannot be read prints **no** object at all (exit 2): an
  object saying `"found": false` about a log nobody could open would be a lie.
- The scan is bounded: `--why` looks at the newest 200 rows of each action. A
  pane whose last notification is older than that reads as "no history" — a
  stated gap, never a stale row reported as the last decision.

## 8. "Is my config doing what I think?" — `arreo notify --policy`

```console
$ arreo notify --policy --config /etc/arreo/arreo.toml
config /etc/arreo/arreo.toml
[notify] present — notifications are on
rules
  1  on blocked   panes build-*   machine workbox
  2  on blocked, question   panes *   machine *
once_per_episode true — one notification per episode: a second event inside the same run is not news
coalesce_secs 300 — at most one notification per pane per 5m
quiet_hours 22:00-07:00 — local wall-clock, half-open [start, end); utc_offset_minutes -300
(what the daemon gets from --config/$ARREO_CONFIG; a daemon started without one notifies nothing)
```

It needs no daemon, no socket and no pane, and it uses the daemon's own loader,
so what it prints is what a tick would decide — one parser, not two. It answers
the question the feature otherwise creates ("is my config even being read?").

| Situation | Output | Exit |
| --- | --- | --- |
| The file yields a policy | the block above | 0 |
| The file has no `[notify]` section (or does not exist) | `[notify] absent — nothing is notified` | 0 |
| No configuration named | a refusal: pass `--config PATH`, or set `$ARREO_CONFIG` | 2 |
| The file is named but does not parse | the loader's own message, naming the field | 2 |

"Off" is a complete answer and exits 0 — it is the default, and it is the most
likely reason nothing has been notified. **No configuration named is exit 2, not
"off"**: the daemon reads `--config`/`$ARREO_CONFIG` and nothing else, so a
daemon this command was not told about may well have a `[notify]` section, and
answering "off" from the absence of a flag would be a second answer to a question
this project only ever answers once.

## 9. Quick actions — answering where you read it

A blocked agent is a question, and a notification for a `question` pane carries
the actions that answer it. The vocabulary is **exactly three**, and it is
bounded on purpose — a fourth action is a product decision, not a plug-in:

| Action | What it does | Audited as |
| --- | --- | --- |
| `reply <text>` | sends `text` plus a newline through the pane's **existing send path** — the very path a direct `arreo send` takes, so it is gated by the same per-verb trust rule, audited as the acting device, and subject to the same secret scan | `notify.act` / `action=reply`, outcome `ok`, `prompt` = the text (redacted on the way in) |
| `skip` | dismisses the notification; **writes no pane bytes at all** | `notify.act` / `action=skip`, outcome `ok`, no prompt |
| `kill` | ends the pane through the pane-kill path (the same one `arreo kill` uses) | `notify.act` / `action=kill`, outcome `ok` |

The text a `reply` sends is **at most 4096 bytes** — longer is a refusal the
operator sees named with both numbers, never a truncation. And it is never a
keystroke replay: the bytes are constructed from the operator's own input, not
read back from any recording of the pane.

Two doors, one implementation: `arreo notify act <pane> <action> [--text …]`
(the CLI) and the TUI's notification-panel keys both send the same `NotifyAct`
message to the daemon, which has exactly one act path.

```console
$ arreo notify --why q1          # the notification and its answers, together
state     question
actions   reply, skip, kill
$ arreo notify act q1 reply --text "use the staging branch"
$ arreo notify act q1 skip
$ arreo notify act other kill
```

**What a pane may be acted on by is its own state, and the refusals are its
own.** A `question` pane offers all three actions; any other state offers
`skip` and `kill` only — you cannot `reply` to a pane that is not asking, which
is refused with `cannot reply: the pane is not asking (state=…)`. A pane whose
process has since exited is refused with the exact sentence
`the pane has exited` — the pane's state is the authority, and the CLI keys its
exit code on that sentence. A viewer-role device gets the trust refusal naming
the role — the same sentence the CLI prints for a direct `send` — because a
`reply` is a send. A refused act is still a row: `outcome = refused` with the
action and the reason in `detail`, never a silence.

| Situation | Exit |
| --- | --- |
| the action was taken | 0 |
| the pane has exited | 2 |
| usage (unknown action, `reply` without `--text`, `--text` on `skip`/`kill`, over the byte bound) | 2 |
| any other refusal (state gate, unknown pane, role, an older daemon's "unknown request") | 1 |

## See also

- [docs/audit.md](audit.md) — the log these rows live in, its columns, redaction
  and the explicit prune.
- `arreo notify` in the top-level `arreo` usage text (with its exit codes).
- `crates/arreo-core/src/notify/` — the rule itself: pure, no clock, no disk,
  with the decision order documented at the top of the module.
