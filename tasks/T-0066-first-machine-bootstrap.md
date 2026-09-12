---
id: T-0066
title: A stranger cannot bootstrap the first machine — the path exists but nothing names it
phase: 2
priority: 1
status: done
depends_on: []
scope:
  - docs/tour.md
  - docs/machines.md
  - README.md
  - crates/arreo-server/src/relay_client.rs
  - .loop/evidence/T-0066/**
---

## Goal

Phase 2's exit criterion is **"a stranger pairs a second machine in < 5 min without docs help"**
(ROADMAP §6). Exercising it (2026-09-12, timed, from a clean state) shows the criterion fails at
step zero, before any second machine is involved: **nothing tells a stranger how to get the first
machine into its own account**, and the daemon's own error message points at a command that cannot
work on its own.

## Evidence (`.loop/evidence/T-0066/`)

Following only the docs, on a clean box: create the relay, register an account with this machine's
root key, write a `[relay]` config, start the daemon. The daemon says:

```
arreo-server: device authority ready (root 28302dead964f71c…, 0 device(s))
arreo-server: relay enabled but this machine has no identity: cannot read
  /tmp/boot/a/identity/devices/f3085ed3a1bf426835569a5068df6fdf.cert: cert io (…): No such file
arreo-server: pair this machine first (arreo pair), or set enabled = false
```

`arreo pair` is the **admitting** side: it prints a four-word code for a *joining* machine. Run it
on this machine and it waits 300 s for a joiner that will never come (the timed run took 303 s and
ended `{"paired":false,"reason":"the pairing window closed while waiting for slot B"}`).
`docs/machines.md` §"What happens, in order" starts from *"Only a machine that holds that root can
issue one the relay will accept, which is why the admitting side must be a machine that already
belongs"* — i.e. it assumes the reader already has one, and never says how.

**The mechanism exists and works.** The machine that holds the account root can admit *itself*: run
`arreo pair`, then `arreo pair --join "<its own code>" --uri "<its own invite>"`. Verified:

```
code=[button scroll silver quilt]
paired with this server as viewer (dev_2f0e21f000a4bf7752559742dcf57649)
certificate: /tmp/boot2/a/identity/devices/2f0e21f000a4bf7752559742dcf57649.cert
```

So this is a **discoverability** defect, not a missing capability — which makes the fix cheap and
the current state inexcusable: the one thing a stranger must do first is the one thing no document
and no error message names.

## Acceptance criteria

- [x] The daemon's message names the actual next step, in order, for a machine holding the account
      root (print a code with `arreo pair`, then join with it), and keeps the "a machine that
      already belongs admits you" alternative. Landed in `ec4034a`.
- [x] `docs/machines.md` gained **§ The first machine** — placed before § Joining — with the four
      steps, the exact commands, and a table saying which half of the key goes where (the secret
      seed vs the public key). `docs/tour.md` now opens the pairing paragraph from the *first*
      machine and links to that section. **Every command in the new section was executed verbatim**
      (transcript in `.loop/evidence/T-0066/`), including the `arreo pair --role owner` variant it
      documents — a machine that holds the account root can give itself any role, and the doc says
      so rather than leaving the `viewer` default unexplained.
- [x] The README's pairing paragraph now states where the account root comes from, that the first
      machine admits itself, and links to the machines.md section — so a reader who follows only the
      README cannot land in the circular state.
- [x] **The exit criterion is re-run, timed, and passes: 1–3 s** against the 300 s budget, two
      machines in one account, both `online`. The script is committed at
      `.loop/evidence/T-0066/exit-criterion.sh` (pid-scoped ports, `trap` cleanup, polled readiness)
      and is now a durable artifact rather than a throwaway.
- [x] The `--role` question is answered, and the answer is "it is the admitting side's choice, and
      the default is documented": a self-admitted machine gets `viewer` unless it passes
      `arreo pair --role owner`, which the docs now show. Verified: self-admission with `--role
      owner` produces `paired with this server as owner` and `devices list` records `owner`.

## Update (2026-09-12, final): the criterion PASSES

Exercised with `.loop/evidence/T-0066/exit-criterion.sh` — pid-scoped ports, `trap`-killed relay,
poll-for-readiness, account registered from `devices list --json`:

```
--- 3. machine A's daemon registers with the account's relay
directory: this machine is machine-a
--- 5. machine B joins — it has NO config and NO identity of its own
joined as dev_9970bf3c4ec096903811f3cb4a1e718c (machine-b)
--- 6. the account lists both machines
machine-a                online              now
machine-b                online              now
=== TOTAL: 1s (budget 300) ===
```

**1 second against the 300 s budget**, both machines listed and online. The self-admission
mechanism works; the two earlier "failures" were my harness (a leaked relay on a fixed port, and
registering the *secret* seed as the account root). See T-0067 for that correction.

What remains from this task is therefore the **discoverability** half — and the evidence for it is
now precise rather than guessed:

1. The daemon message (fixed this turn) names the two-step self-admission.
2. `devices list` prints the root **truncated**, so a stranger cannot get the account root from the
   human output; `--json` carries it, and no document says so. **This is the remaining gap.**
3. The docs describe adding a *second* machine before ever establishing a first — `docs/machines.md`
   starts from "the admitting side must already belong".

## Earlier update (2026-09-12, mid-investigation)

The timed run was completed and it found more than a documentation gap. Recording it here because
this task owns the exit criterion:

1. **The account root public key is effectively undiscoverable.** `arreo devices list` prints
   `root 17d0a47fbfc70f63…` — truncated. `identity/root.key` holds the **secret** seed, and the relay
   rejects it as `--root-key` (or worse, accepts the wrong thing and fails later). The full value
   exists only in `devices list --json` (`{"root": …}`), which no document names for this purpose.
2. **Self-admission produces a certificate nothing can verify** — including the issuing machine's own
   authority and the relay, *with the account registered from this machine's own root public key*
   (verified byte-identical in the relay's database). Filed as **T-0067** with the reproduction; it
   is the actual blocker, and it is a product bug rather than a doc bug.
3. **The relay's handshake budget masks it**: after a few retries, `127.0.0.1` exceeds 3 handshakes
   per 10 s and the message becomes "the server refused to accept a new connection" — a network-shaped
   error for a certificate problem.

So the criterion's fix is **T-0067 first**, then this task's documentation half, then the timed re-run.
The docs work here is still worth doing (a stranger should not have to discover the root-public-key
route), but it cannot make the criterion pass on its own.

## Notes

- **Why p1:** it is the Phase 2 exit gate, it is cheap to fix, and the repo is public — a stranger
  who tries the two-machine flow today hits a circular instruction and has nowhere to look.
- The `viewer` default in the evidence above is worth a second look rather than a silent fix: a
  machine admitting itself as a viewer may be exactly right (a machine's role is *its* grant from
  the account, and the account root holder can grant more) or an accident of the default. Decide it,
  then write it down.
- Not in scope: changing the trust model or the pairing protocol. This task makes the existing path
  findable, and only touches the protocol if the role question forces it.
- Found while exercising ROADMAP §6's exit criterion, which is what that criterion is for — the
  loop is the first stranger.

## Verification

```console
# The timed stranger run, from empty directories:
bash .loop/evidence/T-0066/stranger-pair.sh   # script committed with the evidence
```

## Outcome

Done. `arreo devices list` now prints the **full** root rather than a 16-character prefix with an
ellipsis: that value has exactly one job — it is pasted into `account add --root-key`, where it must
be 64 hex characters — and a truncated identifier that *looks* complete is a trap. The docs
(`docs/machines.md` § The first machine, `docs/tour.md`, `README.md`) now start from the first
machine instead of assuming one exists, and distinguish the two files that are easy to confuse
(`identity/root.key` = the secret; the printed `root …` = the public key).

**The "without docs help" half is what the extra pass found.** With the docs fixed, I asked the
sharper question the criterion actually poses — can a stranger get there from the *binaries alone*? —
and the relay's refusal was the weak link: `unknown account X`, with nothing about how an account
comes to exist. Both sides of that failure now name the fix:

```
arreo-server: relay registration failed: … unknown account never-registered: this relay has no such
  account. Register it on the relay's host with `arreo-relay account add --account never-registered
  --root-key <the account's root PUBLIC key>` — a machine prints its own with `arreo devices list`

arreo-relay: refused 127.0.0.1:48569: unknown account never-registered — register it on this host
  with `arreo-relay account add --state-dir <the dir this relay serves from> …`
```

That is the difference between "the relay said no" and "run this command here". The disclosure is
unchanged — the old message already said the account did not exist, pre-crypto, by design.
