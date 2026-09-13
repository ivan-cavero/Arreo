# Security policy

Arreo runs your coding agents in real terminals, on a machine you own, and promises
that nothing on the network — including the relay — can read what happens inside
them. That promise is the product, so security reports are the most valuable kind
of report this project can receive. They also need a private channel, and this file
is honest about the one it has.

## Status: read this before you report

**Arreo is pre-launch and pre-1.0.** There are no releases, no installers, no
`arreo.dev`, and no email address at that domain — mail to `security@arreo.dev`
bounces, so do not use it. There has been **no external security audit** (see
"Posture on audits" below).

**This file is incomplete until one repository setting is flipped.** The private
reporting channel is GitHub's private vulnerability reporting, and as of
2026-09-12 it is **disabled** on this repository — GitHub's API reports
`{"enabled": false}` for `private-vulnerability-reporting`, which means the
"Report a vulnerability" button does not exist yet.

**Setting to flip before launch:** repository **Settings → Security and quality →
Advanced Security → Private vulnerability reporting → Enable**. The API equivalent,
requiring admin access, is:

```console
curl -X PUT -H "Authorization: Bearer $TOKEN" \
  https://api.github.com/repos/ivan-cavero/Arreo/private-vulnerability-reporting
# 204 No Content — then verify:
curl -s https://api.github.com/repos/ivan-cavero/Arreo/private-vulnerability-reporting
# {"enabled": true}
```

Until that is done, there is **no working private channel**. In that case: do not
open a public issue, do not paste a repro in a discussion — write only that you have
a security report and need a private channel, and a maintainer will arrange one. The
repository owner (`@ivan-cavero`, the only maintainer today) is who to reach; there is
no security email address to fall back on.

## How to report (once the setting is enabled)

Use the repository's **Security** tab → **Report a vulnerability** —
`https://github.com/ivan-cavero/Arreo/security/advisories/new`, which is this
repository's URL today (its `origin` remote). The form is private between you and the
maintainers, and it becomes the advisory draft. If you are reading this file on a
repository with a different URL, use that repository's own `/security/advisories/new`
path: the launch handoff in `tasks/T-0048-oss-launch-readiness.md` includes creating the
project org and moving the repository, and every URL here moves with it.

Include:

- **What breaks**: the promise you believe is violated (for example: "the relay can
  read envelope payloads", "a revoked device still authenticates", "a peer can reach
  panes it was not granted").
- **Where**: component and path — `crates/arreo-relay`, `crates/arreo-server`,
  `crates/arreo-core` (pairing / identity / transport / enforce), `arreo-cli`,
  `arreo-tui` — plus the ADR or doc you tested against.
- **The build**: commit (`git rev-parse HEAD`) and `arreo --version`. There are no
  releases yet, so "from source" is the only install path.
- **Reproduction**: exact commands and setup, or a proof of concept. If it needs
  several machines or a relay, say what the relay was (self-hosted `arreo-relay`
  binary, which commit) — a hostile relay is a shape we explicitly design for.
- **Impact assessment**: your best read on what an attacker gains, and whether it
  needs local access, a paired device, or a network position.
- **Disclosure state**: whether anyone else knows, and whether you plan to publish.
- **Credit**: how you want to be named in the advisory (or that you want to stay
  anonymous).

## What to expect from us

These are commitments, not measurements:

- **Acknowledgement within 7 days.** If a week passes with no reply, assume the
  channel failed somewhere and use the fallback above.
- **Assessment within 30 days**: in scope, out of scope, or need-more-information,
  with reasoning.
- **Coordinated disclosure on a 90-day window** from the report, matching
  CONTRIBUTING §8. It will be shorter when a fix is quick; it can be extended only
  by agreement with you. We publish advisories and credit reporters.
- **A fix is a code change plus a test.** This repo's rule is that no production
  change lands without a test that fails without it, and security fixes are not an
  exception.

## In scope

Anything that breaks a promise the code makes. Concretely, today:

| Area | What we want to hear about | Where it lives |
| --- | --- | --- |
| Pairing | Guessing or replaying a pairing code, substituting a machine in the middle of the SPAKE2 exchange, turning a burned session into impersonation, or a failed pairing leaving state behind | `arreo pair`, `crates/arreo-core/src/pairing`, `crates/arreo-relay/src/pairing.rs`; ADR 0010 |
| Device identity and trust | Pinning a device without the code, using a certificate across accounts, a revoked device or cut machine grant still working, a device reaching panes it does not own | `crates/arreo-core/src/identity`, `crates/arreo-server/src/devices.rs`, `arreo devices`; ADR 0009, 0019 |
| Relay auth and routing | Forging the `Hello → Challenge → Auth → Welcome` proof, routing an envelope to the wrong account or device, or making the relay read/retain envelope payloads it is designed to be unable to read | `crates/arreo-relay`; ADR 0013, 0014 |
| Remote transport | Authenticating without the pinned ed25519 key, replaying a handshake (the flight guard), or forcing a downgrade | `crates/arreo-core/src/transport` (Noise-KK inside QUIC); ADR 0011 |
| Daemon / socket boundary | An unprivileged local peer driving panes it was not granted, unauthenticated verbs, or an action that happens without its audit row (or with the wrong actor) | socket API v1; `crates/arreo-server`, `docs/agent-skill.md`, `docs/audit.md` |
| Secret handling | Secret-shaped content reaching the audit log, a fixture, or an `arreo audit export` despite the redaction and scan paths | `crates/arreo-core/src/store.rs` (`redact`, `[REDACTED:<label>]` rows), `crates/arreo-core/src/fixtures.rs` (`scan_secrets`, the `arreo record` refusal without `--allow-secrets`) |
| Resource guard | A pane escaping its cgroup budget, or the kill switch firing against the wrong pane's tree | `crates/arreo-core/src/enforce` (Linux cgroup v2, `memory.max` + `pids.max`); ROADMAP §4, `tasks/T-0019-resource-enforcement.md` |

The threat model that defines these boundaries — hostile devices on a home or office
network, a rented VPS, a stolen phone, a compromised relay, a curious cloud provider —
is in `ROADMAP.md` §4. Read it before reporting: it also names what v1 deliberately
does **not** defend against.

## Out of scope

- **Anything that requires root, a compromised kernel, or physical access to the
  machine.** ROADMAP §4 excludes these explicitly; the daemon is a same-user tool.
- **The non-Linux resource guard.** Windows Job Objects and macOS rlimits are not
  implemented (`EnforceError::Unimplemented`). An agent exceeding a budget there is a
  known gap, not a vulnerability — file it as a bug.
- **The Landlock/seccomp profile.** It does not exist in the code; ROADMAP §4 lists it
  as planned. Today an agent runs as you and can touch your files: that is the
  documented design, not a finding.
- **Software that is not shipped.** The managed relay service, the mobile clients, the
  web dashboard, the plugin runtime (`arreo-plugin-api`) and the theme gallery have no
  implementation here, so there is nothing to test.
- **Upstream harness vulnerabilities** in the agents Arreo runs ([CC], opencode, pi and
  friends). Report those to their projects. An Arreo adapter mis-detecting a harness's
  state is an ordinary bug — use the issue tracker — unless it leaks pane content, in
  which case it is in scope above.
- **Denial of service by someone who can already run code as your user**, including
  spawning many agents in your own daemon.
- **Missing hardening with no exploit** (unsigned development builds, no audit log
  hash chain — `docs/audit.md` §1 states plainly that the log is append-only by API but
  not tamper-evident). Tell us anyway if you think one is a real risk; it is a design
  discussion, not a report.

## File permissions

Who may read the daemon's state and connect to its socket (T-0078):

**Owner-only by default: 0600 for files, 0700 for directories.** Every file the
daemon creates — `<socket>`, `<socket>.db` (pane scrollback — *agent output*: the
operator's prompts and whatever a tool printed — plus the audit log and the device
records), its `-wal`/`-shm` sidecars, `<socket>.lock` and `<socket>.handoff` — is the
operator's private state, and nothing in it is group- or world-reachable by default.

Why not a group-shared default: the store's contents decide the answer. Pane
scrollback is agent output, and a group-readable mode would leak prompts and tool
output the moment someone forgets to unshare. An operator who genuinely wants a shared
runtime can set their umask and share the directory deliberately — the default must
not leak pane content because someone forgot to unshare.

Where `XDG_RUNTIME_DIR` is unset, the default socket lands in `/tmp` (world-writable),
so the **file mode is the only control there** — the daemon never relies on a private
directory that was never asked for.

One asymmetry, stated because it is designed: the **socket's** mode is enforced
fail-closed (the daemon refuses to serve a group-connectable socket), while the
**store's** is best-effort with a loud warning — on a filesystem where `chmod`
fails (some network mounts), the daemon serves the store at the ambient umask
rather than refuse to start. The socket is the door that hands out the pane API;
the store is the file, and a daemon that refuses to start because of an exotic
mount is worse than one that warns. The warning is on stderr at every open that
fails to tighten.

Upgrade path for existing installs: the policy applies at creation and re-applies at
every open. The store chmods its files (including re-created `-wal`/`-shm` sidecars)
on each open, and the lock and socket chmod an existing file on acquire/bind — so a
store created before this policy is tightened by the first open of the fixed build.

Honest window: SQLite creates its store files itself and cannot be told a creation
mode, so the chmod follows the open. The window is bounded to a brand-new store's
first empty pages — before any pane content or audit row exists — and an existing
store is chmodded by the very open that first touches it.

## Posture on audits

**No external audit has been performed.** ROADMAP §4 makes "third-party security audit
plus a public threat model" the gate before the project charges anyone, and ROADMAP §6
places that audit in Phase 5 — so it is a commitment for the paid launch, not a claim
about today. Until an audit exists and is published, treat any wording implying Arreo
has been audited as a defect, and please report it: this repo's rule is that every
public claim must point at a shipped artifact, and a false security claim is the most
expensive kind of bug here.

## Safe harbor

Testing that follows this policy is welcome and we will not pursue legal action for it:
work against your own machines and accounts, or ones you have explicit permission to
test; no denial of service against shared infrastructure; no social engineering; no
access to other people's data beyond the minimum needed to demonstrate the issue; give
us the 90 days above before publishing, and we will keep you informed as we go.
