---
id: T-0066
title: A stranger cannot bootstrap the first machine — the path exists but nothing names it
phase: 2
priority: 1
status: proposed
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

- [ ] The daemon's message for "relay enabled but this machine has no identity" names the actual
      next step, in order, for a machine that holds the account root: print a code with `arreo
      pair`, then join with it. It must not say "pair this machine first" alone — that reads as
      "run the admitting command", which is what the timed run did, and it waits forever.
- [ ] `docs/tour.md` and `docs/machines.md` each carry the bootstrap as its own short section
      ("the first machine"), placed *before* the existing "adding a second machine" material,
      because that is the order a reader performs it.
- [ ] The README's quickstart states where the account root comes from and that the first machine
      admits itself; a reader who follows only the README must not be able to reach the circular
      state above.
- [ ] **The exit criterion is re-run, timed, from a clean state, and passes**: a scripted stranger
      path from empty directories to two machines in one account, in **< 5 minutes**, using only
      commands the docs name. Transcript in `.loop/evidence/T-0066/`. If it still fails, the
      remaining step becomes its own task with the transcript as the repro.
- [ ] The `--role` for a self-admitted machine is considered explicitly: the self-join above granted
      `viewer` by default, and a machine that owns the account should almost certainly be an
      operator of itself. Whatever the rule is, it is written down.

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
