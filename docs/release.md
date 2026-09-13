# Release process

> **Status: pre-launch, and honest about which half is proven.** There are no
> releases, no tags, no published crates, and the `arreo.dev` URLs below are
> **reserved names, not working endpoints** — the domain has no DNS record.
>
> What *is* real, as of T-0036: a minisign public key is committed at
> [`supply-chain/arreo.pub`](../supply-chain/arreo.pub) and compiled into every
> binary; `arreo_core::update::verify` refuses an artifact whose signature is
> missing, foreign, or wrong; `arreo update verify` is the user door onto it; and
> [`.github/workflows/release.yml`](../.github/workflows/release.yml) builds the
> three targets, signs every artifact and the manifest, and fails hard if any of
> that does not hold. The **tag job has never run with a real key** — the secret
> `MINISIGN_SECRET_KEY` exists in the repository's secret store, but no tag has
> been pushed through it. That is the one line of this document that CI cannot
> vouch for, and it is written here rather than implied.
>
> Machine-checked today: `cargo xtask package --dry-run` (in CI on every PR)
> validates the cargo-dist skeleton *and* the signed-release structure — that the
> pinned key parses and its comment names its own key id, that the tag job exists,
> that it takes the key from the environment, and that it carries no key material.
> `cargo test -p arreo-core --test update_verify` proves the refusals against a
> throwaway keypair generated inside the test.

## What a release does

1. **Tag.** `git tag v0.1.0 && git push origin v0.1.0`. The tag starts
   [`.github/workflows/release.yml`](../.github/workflows/release.yml): one native
   runner per target builds `arreo` and `arreo-server` (`--release --locked`), and
   the artifacts are collected into a single `dist/`.
2. **Manifest.** `SHA256SUMS` is written over every artifact. It is the **only
   trusted digest source** — never the web page, never a file name, never a
   filename's shape.
3. **Sign.** Every artifact *and* the manifest are signed with minisign. The key
   comes from the `MINISIGN_SECRET_KEY` repository secret, through the
   environment; the workflow contains no key material, and no code in this
   workspace can *produce* a signature (see [Supply chain](#supply-chain)).
4. **Verify, and refuse.** The job then runs the artifact it just built —
   `arreo update verify` — against the key compiled into that binary, for every
   artifact and for the manifest, and finally flips one byte of a copy and
   requires the verifier to reject it. Any failure fails the job before anything
   is published. A release job that cannot verify its own output is a release job
   that would ship something nobody can check.
5. **Publish.** `gh release create` attaches `dist/*`: the binaries, their
   `.minisig` signatures, `SHA256SUMS` and `SHA256SUMS.minisig`.

What this job does **not** do: it does not build the cargo-dist archives or the
installers the skeleton declares (`[workspace.metadata.dist]`), and it does not
notarize or Authenticode-sign anything. Both are recorded below as gaps rather
than implied away.

## The trust decision: minisign, not keyless Sigstore

**minisign**, one offline ed25519 key, verified in-process, with zero egress at
update time. The public half is `supply-chain/arreo.pub` — a trust set that
today holds exactly one key — and that key's id is `076F2F7CEBE0AF51`, the
spelling `minisign` prints and the spelling every refusal quotes.

The rejected alternative was keyless Sigstore (Fulcio + Rekor, `cosign` /
`cargo dist sign`): it is the fashionable answer, and it is the wrong one *here*.
Fulcio needs an OIDC identity at signing time and Rekor needs a network round trip
at **verification** time. Arreo's self-hosted tier verifies on hosts behind zero
inbound ports, sometimes with no route to the internet at all — a verification
path that requires reaching a transparency log is a verification path that cannot
run where this product is deployed. Sigstore also proves *who was at the
keyboard*, which is not the claim being made: the claim is "this artifact came
from Arreo's release key", and a key we hold offline makes that claim directly.

Also rejected, and recorded because both look reasonable:

* the `minisign` crate — it signs as well as verifies, and this workspace
  deliberately contains no signing code. The signing key exists only as a CI
  secret, and the moment the tree can produce a signature is the moment the key
  can end up next to it.
* hand-rolling minisign's format over `ed25519-dalek` — it re-implements a
  signature format (where crypto dies), and it loses compatibility with
  `cargo dist sign` / `minisign -S`, which is what the release job and every
  operator's own tooling uses.

## Where signatures live, and how to check one

Each artifact ships beside its signature: `<artifact>.minisig`. The manifest is
signed the same way, `SHA256SUMS.minisig`.

```console
$ arreo update verify arreo-0.1.0-x86_64-unknown-linux-gnu
verified arreo-0.1.0-x86_64-unknown-linux-gnu
  key    076F2F7CEBE0AF51 (pinned in supply-chain/arreo.pub, compiled into this binary)
  sha256 1f0c…e4

$ arreo update verify arreo-0.1.0-x86_64-unknown-linux-gnu --manifest SHA256SUMS
# the manifest's own signature is checked first, then the artifact's digest
# against its entry — the order the install scripts use
```

* `--sig <path>` checks a signature kept somewhere else; without it the sibling
  `<path>.minisig` is used.
* `--manifest <path>` verifies the manifest **first** and then compares the
  artifact's digest with the entry for it. A manifest that has not been verified
  is a list of digests from nowhere, so nothing in it is consulted until its own
  signature holds.
* Exit codes: **0** verified, **1** refused, **2** usage. There is **no bypass
  flag** — not `--force`, not `--insecure`, not an environment variable. A flag
  that skips the check would make every other line of this document decorative,
  so it does not exist; the recourse after a refusal is to fetch the artifact
  again, which is the correct answer to every failure mode.

Each refusal names what failed and which file:

| Refusal | What it means |
| --- | --- |
| `no signature at <path>` | the artifact has no `.minisig` — a broken release job, not an attack |
| `signed by key X, this build trusts Y` | a valid signature from a key that is not ours |
| `signature does not authenticate this file` | the bytes changed after signing, or a signature carried over from another file |
| `sha256 is …, the trusted manifest says …` | the file is not the one the manifest lists |
| `<path>: <io detail>` | the file could not be read |

## What a signature proves, and what it does not

It proves **our authorship of exactly these bytes**, offline, forever, with no
service in the loop. That is the whole claim, and it is worth being precise about
its edges:

* It is **not Apple notarization and not Windows Authenticode.** A minisign
  signature does not stop Gatekeeper or SmartScreen from warning a user who
  downloads the binary with a browser. Those are a separate **paid-identity line
  item** (an Apple Developer ID and a Windows code-signing certificate, plus the
  CI plumbing to use them) and they are not claimed anywhere in this repository.
* It says nothing about the machine that runs the update. An operator whose host
  is already compromised does not get that host back from a signature.
* It is **not reproducibility.** Two builds of the same commit are not proven
  bit-identical; the signature covers whatever the release job produced.

## Rotation: a two-key trust set

A single pinned key has an ugly property: leaking it means either signing with a
key you no longer trust or an emergency release, and the second is how projects
ship mistakes. So `supply-chain/arreo.pub` is a **trust set** — a list of keys,
one or two, and `arreo_core::update::verify` accepts a signature from any of them.
Today it holds exactly one; the second is generated and held by the same
custodian, and is added to the file *before* it is ever used.

Because the file is a list, rotation is four ordinary releases and **no code
change** — which is the whole reason it is a list:

1. **Publish the next key.** Append its public half to `supply-chain/arreo.pub`
   in a normal PR, with a comment naming it (the key id in the comment is checked
   against the key itself by `cargo xtask package --dry-run`, on every PR). Every
   client built from that commit onwards now accepts signatures from both keys.
2. **Wait out the compat window.** N−1 clients (see CONTRIBUTING §6) do not know
   the next key yet, so the *current* key keeps signing until the window has
   passed. Nothing is rushed.
3. **Switch.** Releases start being signed by the next key — a change to which
   secret the workflow holds, not to this repository. Old clients still accept
   them, because they trust the set.
4. **Retire.** The old key's line is deleted in a later release, once no supported
   client predates step 1.

If a key is **leaked** rather than rotated on schedule, the same machinery answers
the question in order of urgency: publish the next key (step 1) and switch signing
to it (step 3) — the leaked key can be dropped in a release whose *only* change is
that deletion, and the users who matter most (the ones updating from a build that
already knows the next key) are protected immediately. Users on older builds are
the ones the compat window exists for; for them the honest answer is a re-install
from the release page, not a signature chain that pretends the leak did not
happen.

Two keys is the whole design. A third adds a state machine to a procedure whose
value is that an operator can hold it in their head at 3am.

## Supply chain

The verifier is [`minisign-verify`](https://crates.io/crates/minisign-verify)
0.2.5: pure Rust, **zero dependencies**, no build script, no network — which is
the point, since it runs on hosts that may have no route out. It is
*verification only*, so it cannot sign even if someone tried.

`cargo vet` has no upstream review to import for it, so it carries an exemption in
[`supply-chain/config.toml`](../supply-chain/config.toml) — the day-one floor this
repository uses for every crate, and the goal CONTRIBUTING §4 names is a real
`cargo vet certify` rather than the exemption. The exemption was earned by
reading the crate, not by its download count:

* zero dependencies and no build script, so the supply chain does not grow;
* the only `unsafe` is a vendored copy of the old stdlib byte-order helpers in
  `src/crypto/cryptoutil.rs`, where every pointer copy is preceded by a length
  assertion; `src/base64.rs` carries `#![forbid(unsafe_code)]`;
* `cargo audit` reports nothing, `cargo deny check` passes (licenses, bans,
  duplicates, sources), and `cargo vet --locked` is green — the three gates CI
  runs on every PR.

The signing half is not a dependency at all: it is the `minisign` binary, invoked
by the release workflow with a key that exists only as a secret. Nothing in this
workspace can produce a signature, and the workflow is checked for embedded key
material on every PR.

## Updating a client in place

While there is no release channel, a client can still be updated from a binary you
already have (T-0070):

```console
$ arreo update --from /path/to/a/newer/arreo
installed /home/you/.cargo/bin/arreo
version: arreo 0.1.0
previous kept at /home/you/.cargo/bin/arreo.prev
resumed pane build from /run/user/1000/arreo.sock (4 line(s) after 0)

$ arreo update --rollback          # put the previous binary back
$ arreo update --check             # refused: this build has no channel (see below)
```

### The invariant

**The client update path never signals, reaps, restarts or stops a PTY-bearing
process, and never stops the daemon.** It touches exactly three things: the binary
path, the `.prev` sibling, and the resume token. The daemon owns the agents; this
verb owns the client binary. That is what makes a client restart cost seconds and
touch nobody else, and it is asserted — not promised — by `cargo xtask e2e --slice
update`, which holds a real daemon and eight live panes across a real update and
checks that no pid moved and no pane's output stream restarted.

### What happens, in order

```text
1. write the resume token      (before the swap: a swapped client that cannot say
                                where it was has lost the operator's place)
2. copy the candidate to       (same directory ⇒ same filesystem ⇒ the rename
   <binary>.staged              below is atomic)
3. hard_link the current       (a second name for the old inode; nothing moves)
   binary to <binary>.prev
4. rename(<binary>.staged,     (atomic replace, one syscall)
   <binary>)
5. hand over to the new        (exec with the same arguments; the second run finds
   binary                       the binary already installed and resumes)
```

There is no instant at which the binary path is missing or non-executable, and a
crash between any two steps leaves a runnable binary there — the old one before
step 4, the new one after. The reasoning, and the rejected two-rename design, are
in [ADR 0020](../specs/adr/0020-client-update-swap.md).

### What `--check` refuses

`arreo update` with no `--from` is the **anonymous** path: fetch a release and
verify its signature before staging it. The verifier for it exists as of T-0036 —
`arreo update verify` checks an artifact against the key compiled into this
binary — but the *channel* that fetches a signed index and its artifacts is
T-0037, and this build has none. So `--check` says exactly that and exits 2 rather
than pretending. Installing an artifact nobody verified is the one thing an
updater must never do quietly.

### When the path belongs to a package manager

Homebrew and `cargo install` own their binary paths. The updater does not fight
them: a path it cannot write produces the command that does the job
(`brew upgrade arreo`, `cargo install --force arreo`) instead of a partial write or
a `sudo` over the package manager's files.

## Install URLs (reserved — none of these resolves today)

| Channel | URL | State |
| --- | --- | --- |
| Shell | `https://arreo.dev/install.sh` | Reserved; domain has no DNS record |
| PowerShell | `https://arreo.dev/install.ps1` | Reserved; domain has no DNS record |
| Homebrew | `brew install arreo/tap/arreo` | The `arreo/homebrew-arreo` tap named in `[workspace.metadata.dist]` does not exist |

Until a release exists, the only install path is building this checkout:
`cargo build --workspace` ([docs/tour.md](tour.md) has the walkthrough). An
install script, when one is written, verifies `SHA256SUMS` and its signature
first and only then verifies what it is about to install — the manifest is the
trusted digest source, and nothing else is.
