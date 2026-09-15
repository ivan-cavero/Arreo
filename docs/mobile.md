# Mobile (Phase 3): the UniFFI surface, and what a phone still has to bring

One sentence: `crates/arreo-core-ffi` exposes the client-relevant core —
pairing, device identity, the relay session, the machine directory, the protocol
codec and the theme engine's tokens — as **one typed UniFFI definition**, from
which `uniffi-bindgen` generates the Swift and the Kotlin, so the two mobile UIs
are written against a generated API rather than against a Rust ABI.

This is the *bindings* half of ROADMAP §3.5 ("Rust core via UniFFI; iOS and
Android built in parallel — shared core means duplicated UI only"). The UI half
needs Xcode and the Android SDK, neither of which exists on the Linux box this
was built on; the bindings do not, which is why this is the one Phase 3 task
that can be finished here.

## What is exported

Every item below is an `#[uniffi::export]` in `crates/arreo-core-ffi`, so it
appears in both generated languages. Names are Rust's; each generator renames
them to its own convention (`pairing_server_begin` → `pairingServerBegin` in
Swift and Kotlin alike).

### Pairing — both sides

| Rust | Foreign | Notes |
|---|---|---|
| `pairing_server_begin(server_identity, mailbox, ttl_secs, account, relay)` | `pairingServerBegin` | The admitting half. **Blocking** (opens the mailbox session, writes flight A). |
| `PairingServerHandle::invite()` / `::code()` | `invite` / `code` | What to draw as a QR, and the four words to read out. |
| `PairingServerHandle::receive()` | `receive` | Waits for the phone's hello. **Blocking, up to the TTL.** A wrong code burns the session and returns `CodeMismatch`. |
| `PairingServerHandle::complete(cert)` | `complete` | Sends the certificate back. Consumes the server. |
| `PairingServerHandle::abandon()` | `abandon` | Cancelled scan: burns the session. |
| `pairing_invite_parse(uri)` / `pairing_invite_uri(invite)` | `pairingInviteParse` / `pairingInviteUri` | The QR's contents, typed. The **code is never in the URI**. |
| `pairing_code_random()` / `pairing_code_phrase(text)` | `pairingCodeRandom` / `pairingCodePhrase` | A fresh code, and canonicalization of a typed one (with the core's "did you mean …?" hint). |
| `pairing_phone_join(invite_uri, code, seed, name)` | `pairingPhoneJoin` | The joining half. **Blocking** (waits for flight A). |
| `PairingPhoneHandle::await_cert()` | `awaitCert` | **Blocking, up to the TTL.** Verifies the certificate against the pinned server key. |

### Identity and its fingerprint

| Rust | Foreign | Notes |
|---|---|---|
| `device_key_from_seed(seed)` | `deviceKeyFromSeed` | 32 bytes in, a keypair out. The seed comes *in* — see below. |
| `DeviceKeyHandle::public_hex()` / `::fingerprint()` / `::display_id()` / `::sign(payload)` | `publicHex` / `fingerprint` / `displayId` / `sign` | The fingerprint is `sha256(pubkey)` truncated to 128 bits, hex; `display_id` is the `dev_<hex>` spelling the CLI prints. |
| `root_key_from_seed(seed)` / `RootKeyHandle::public_hex()` | `rootKeyFromSeed` / `publicHex` | The admitting machine's key. **No `sign` export**: an arbitrary-bytes signing oracle for the account's trust anchor is a capability the CLI has no verb for, nothing here called it, and certificates are signed by `device_cert_issue` instead. |
| `device_cert_issue(root, device_public_hex, name, role, issued_at_ms, serial)` | `deviceCertIssue` | Encoding, not policy — it signs whatever key it is given, **except** a small-order ed25519 point, which it refuses with the core's own `WeakKey` sentence: this is the only issuing door a phone has and the key comes from the peer. Revocation (`revocation::may_pin`) is *not* applied here — it needs a store this crate has none of, so that check is the admitting machine's. |
| `device_cert_decode(bytes)` | `deviceCertDecode` | Structural validity only; `verify` is the trust decision. |
| `DeviceCertHandle::device_id()` / `::fingerprint()` / `::name()` / `::role()` / `::serial()` / `::encode()` / `::verify(root_hex, presented_hex)` | `deviceId` / … / `verify` | `verify` needs **both** keys: a certificate that verifies for a different key must never authorize the key that presented it. |
| `fingerprint_of_public_key(public_hex)` | `fingerprintOfPublicKey` | What an admitting machine derives to know which device id to issue for. |
| `identity_verify(public_hex, payload, signature)` | `identityVerify` | A malformed key is a typed refusal; a signature that does not verify is `false`. |
| `role_word(role)` / `role_parse(text)` | `roleWord` / `roleParse` | `owner` / `viewer`; `operator` also parses to `owner`, as the core does. |

### The relay session

| Rust | Foreign | Notes |
|---|---|---|
| `relay_session_dial(addr, account, device, cert)` | `relaySessionDial` | **Async.** `addr` is `IP:PORT`. One attempt; a refusal comes back with the relay's own reason. |
| `RelaySessionHandle::device_id()` / `::account()` / `::nonce()` | `deviceId` / `account` / `nonce` | `device_id` is **this device's own**, derived from the key the session dialed with (`DeviceId::from_key`) rather than from the relay's `AuthReply::Welcome`; `dial` checks the relay's confirmation against that derivation and refuses the session when they disagree (`RelayIdentityMismatch`). Plus this session's 32-byte challenge. |
| `RelaySessionHandle::drain(from_seq)` / `::ack(seq)` / `::heartbeat()` | `drain` / `ack` / `heartbeat` | **Async.** The durable inbox cursor, and the presence beat. |
| `RelaySessionHandle::machines(all)` | `machines` | **Async.** The account's machine directory. `refused` is an *answer*, not an error. |
| `RelaySessionHandle::metrics_history(peer, server_key, pane, since_ms, until_ms, step_ms)` | `metricsHistory` | **Async.** One pane's durable series (T-0040), from the machine's **daemon** over a peer stream, on a conversation reused per peer. `server_key` is the machine's pinned key, hex. `until_ms = u64::MAX` means "to now". See "A RAM meter" below. |
| `RelaySessionHandle::next_peer()` | `nextPeer` | **Async.** The accept door: who has written to you and has no stream yet. |
| `RelaySessionHandle::stream_to(peer)` | `streamTo` | Opens (or reuses) the byte stream to a peer. Lock-free. |
| `RelaySessionHandle::closed()` | `closed` | **Async.** Wait until the session ends. Lock-free, so a UI can always notice. |
| `relay_peer_parse(device_id)` | `relayPeerParse` | `dev_<hex>` or bare hex. |
| `RelayPeerHandle::device_id()` / `::fingerprint()` | `deviceId` / `fingerprint` | |
| `RelayStreamHandle::peer()` / `::read(max)` / `::write(bytes)` / `::close()` | `peer` / `read` / `write` / `close` | **Async** (except `peer`). An empty `read` means the peer closed; a broken stream fails with the reason. |

### A RAM meter: the metrics reads

Two `Message` verbs, both T-0040's, and the boundary adds no third: the client
asks **`MetricsHistory`** (pane, window, tier) and the daemon answers
**`MetricsSeries`**. `RelaySessionHandle::metrics_history` is that pair, and it is
the CLI's question field for field — so `arreo metrics history` and a phone's
graph cannot disagree about what was asked. The reply crosses as
`MetricsSeriesInfo` (a version, the tier served, the downshift flag) plus one
`WireMetricsPoint` per row: `ts_ms`, `rss_avg`, `rss_peak`, `cpu_avg`,
`cpu_peak`, `pids`.

**What a meter must do about the step: draw the tier the reply names, not the
tier it asked for.** `step_ms` is what the machine actually served and
`downshifted` says whether that differs from the ask — a pane with 10 s rows
cannot answer a 1 s ask, and the daemon answers with the nearest real tier rather
than an empty graph. A meter that labelled its axis with the ask would draw a
graph that is wrong in the one direction nobody re-checks.

**An empty `rows` is a state, not a failure.** A pane that just started has no
history, and the read returns an empty list — the CLI prints "no history for …
in this window" and exits 0. Only a session that could not be reached, or a
machine that hung up mid-answer, is an `Err`.

Two facts the read needs, and the UI supplies:

- **The peer and the pinned key.** The machine's *pinned* key, never the one the
  relay's directory reports: the relay routes by device id and is not trusted for
  identity, so the handshake proves the machine holds the key this phone pinned
  at pairing. A phone has both by the time it draws a meter — the pinned key is
  what it stored at pairing, and **the peer is that key's device id**:
  `relay_peer_parse(fingerprint_of_public_key(pinned_key_hex))`.
  *Not* the row's `machine_id`, which is the machine's directory identity (its
  root key, T-0043 — the thing that outlives re-pairing): the relay routes by the
  id a device dialed with, so a stream opened to `machine_id` reaches no device
  ("the relay does not know that device"). The core's own resolver draws the same
  line (`DeviceId::from_key(&server_key)`).
- **The cadence.** A read reuses the peer's conversation (below), so a poll costs
  one round trip rather than a handshake — but the tier it draws is what makes a
  faster poll useful: do not poll faster than the tier is worth.

**The conversation is per peer, and every read reuses it.** One Noise handshake,
then as many verbs as the meter asks for: a machine's daemon keeps its session
open after a verb (that is `serve_session`, the same loop the local socket and the
direct transport run), so a client that handshakes per call writes its next
handshake into the conversation the daemon is still holding and the read never
arrives — the failure T-0114's first implementation shipped. `RelaySessionHandle`
keeps one conversation per peer and drops it when a verb fails, so the next read
opens a fresh one: recovery without a retry loop inside a call.

Two consequences a UI must know:

- **The pinned key is checked when the conversation is opened, and only then.** A
  read naming a *different* key for a peer already being read is a `Peer` refusal,
  not a served read: the conversation carries the key it was proven with, and
  changing the pin means dialing a new session.
- **Two concurrent reads are serialized.** The conversations sit behind one lock,
  because two of them for one peer would write into the same wire channel while
  only the newer could ever read a reply. Two meters polling at once queue; they
  do not interleave. A poll still does not queue behind a caller parked in
  `next_peer`.

Nothing here charts, smooths or formats: that is the UI's, and this crate carries
data, not presentation.

### The machine directory

| Rust | Foreign | Notes |
|---|---|---|
| `directory_cache_new()` | `directoryCacheNew` | A client's read-only mirror. Pure: no store, no file, no socket. |
| `DirectoryCacheHandle::mirror(rows, as_of_ms)` | `mirror` | **Replaces**, never merges — a merge would let a stale name survive a rename. |
| `DirectoryCacheHandle::as_of_ms()` / `::len()` / `::is_empty()` / `::rows()` | `asOfMs` / `len` / `isEmpty` / `rows` | `as_of_ms == 0` means the relay was never contacted. |
| `DirectoryCacheHandle::lookup(name)` / `::resolve_known(machine_id)` | `lookup` / `resolveKnown` | Both refuse a malformed argument with the directory's own sentence. |
| `presence_word(presence)` | `presenceWord` | `online` / `offline` / `stale`. |

### The protocol codec

`WireMessage` mirrors `arreo_core::proto::Message` **variant for variant** (all
30), so a phone can build a request *and* read the reply. `codec_encode`,
`codec_decode`, `codec_encode_frame`, `codec_decode_frame`, `codec_frame_body_len`,
`codec_classify_op`, `codec_op_name`, `codec_negotiate`,
`codec_protocol_version`, `codec_min_version`, `codec_max_frame_bytes`,
`codec_client_versions`. Two shapes are not the core's: a `(usize, usize)`
cursor becomes the `WireCursor` record, and a `usize` field becomes `u64`
(a value that will not fit is refused, never truncated).

### The theme engine's tokens

`theme_builtin(variant, depth)`, `theme_from_tokens(name, variant, depth, tokens)`,
`ThemeHandle::tokens()` / `::color(token)` / `::state_color(state)` /
`::state_label_color(state)` / `::with_depth(depth)` / `::name()` / `::variant()` /
`::depth()`, and the color primitives `color_parse`, `color_quantize`,
`color_fg_sequence`, `color_contrast_ratio`.

`theme_builtin` reads the `arreo` theme from the binary (`include_str!`), so
**nothing in this crate touches a filesystem**. A user-authored theme arrives as
a token table the UI passes in — the UI has the platform's file APIs, this crate
deliberately does not. `depth` is a **parameter**, not a detection: `TERM` and
`COLORTERM` are terminal questions and a phone has no terminal.

## Errors: typed, and carrying the CLI's sentences

Every fallible call returns one of eight `#[derive(uniffi::Error)]` enums —
`PairingFfiError`, `KeyFfiError`, `CertFfiError`, `RoleFfiError`,
`SessionFfiError`, `DirectoryFfiError`, `CodecFfiError`, `ColorFfiError` — with
one variant per core variant and **the core error's own `Display` as the
payload**. So the sentence a phone shows and the sentence the CLI prints are the
same string by construction rather than by two format strings kept in step
(T-0074's "no silent divergences", applied to the boundary). A
`Result<_, String>` anywhere in this surface would be a finding; there is none.

The enums are **flat** errors (`#[uniffi(flat_error)]`) on purpose, and the
reason is measurable rather than stylistic: UniFFI renders a *rich* error
variant's foreign `message` from its **fields** (`"field=${field}"`, see
`bindings/kotlin/templates/ErrorTemplate.kt`), so a rich variant would replace
the core's sentence with `got=2, want=4` — the sentence would be gone from the
only place a UI reads it. A flat error crosses as a variant tag plus the Rust
`Display`, which is exactly "typed, and carrying the sentence".

Each enum's `From` impl is exhaustive over its core enum, so a new core variant
is a **compile error here** rather than a silently-unmapped state.

Six variants are the boundary's own, and each is marked as such in the source
and here — they exist because a phone can supply something the CLI cannot:

| Variant | Why the CLI has no equivalent |
|---|---|
| `KeyFfiError::BadSeed` / `PairingFfiError::BadSeed` | "a device seed is exactly 32 bytes, got N". The CLI never takes a seed from outside; a phone supplies its own. |
| `SessionFfiError::BadAddress` | The relay address is not `IP:PORT`. The CLI's equivalent sentence is the CLI's own (`machines: cannot reach the relay at …`); this names the *parse*, before any socket exists. |
| `SessionFfiError::RelayIdentityMismatch` | The relay confirmed an identity that is not the one this device's key names. The CLI's relay session reports the same echo without checking it; a phone asserts its identity over a peer stream, so a lie about *it* is refused rather than carried. |
| `SessionFfiError::BadPeerKey` | A pinned machine key that is not a hex public key. Same shape as `CertFfiError::BadPublicKey`: the CLI parses those at its own argument layer, with its own words, and the *sentence* here is the core's `KeyError` `Display`. |
| `CertFfiError::BadPublicKey` | A hex public key that is not 64 hex characters. The CLI parses those at its own argument layer, with its own words. |
| `CodecFfiError::OutOfRange` | A wire `u64` this platform cannot address. Unreachable on the 64-bit targets the product ships. |

Two more session variants carry *core* sentences rather than preconditions, so
they are not in that table: `SessionFfiError::Peer` is talking to a machine's
daemon that failed — the channel could not be established, it broke mid-answer,
the frame could not be built (the transport's own `TransportError` /
`mesh::ClientError` wording), or the read named a pinned key the open conversation
was not proven with (this crate's own sentence, and the one `Peer` payload that is
not a core error's) — and `SessionFfiError::Daemon` is a machine's daemon
refusing a request in its own words, the sentence the CLI prints after its verb
name (`metrics history: {message}`).

`SessionFfiError::Refused` is split out of `Client` deliberately: it is the one
distinction the product itself acts on. The daemon's reconnect loop retries a
transport that is absent and does **not** retry a refusal
(`arreo-server/src/relay_client.rs` matches `ClientError::Refused` for exactly
this), and a phone that retried a bad certificate in a tight loop would look
hung.

## The two SKIPs, and what would lift them

`cargo xtask ffi --check` builds the cdylib, generates both languages, asserts
the generated surface against a golden symbol list, and then compiles what a
local toolchain can. Two steps skip here, and neither is a fabricated PASS:

**Kotlin compile — SKIP: no `kotlinc` on PATH.** This box has a JDK (Temurin 25)
and *a JDK alone cannot compile Kotlin*. The task's original parenthetical
claiming otherwise was wrong, and it is corrected here. **What lifts it:** install
the Kotlin compiler (`sdk install kotlin`, or the `kotlin` package), plus JNA
(the generated Kotlin binds through it — `net.java.dev.jna:jna`) and the Kotlin
stdlib jar. The gate probes for all three by path and names whichever is missing;
`$ARREO_JNA_JAR` and `$ARREO_KOTLIN_STDLIB` point it at them explicitly. Once
`kotlinc` is present, a `kotlinc` that *runs and complains* is a FAIL, not a skip.

**Swift compile — SKIP: needs macOS + Xcode.** `swiftc` is Apple-only, and
`AGENTS.md` forbids faking a macOS result. **What lifts it:** the GitHub macOS
runner, which is the same class of gate as T-0090 (Windows ConPTY). This is not
a gap that a Linux box can close by trying harder; it is a different machine.

`--enforce` turns a SKIP into a failure, exactly as `check-targets` does. **Nothing
runs this gate automatically yet**: `.github/workflows/ci.yml` is the user's file during
T-0063 and has no `xtask ffi` step, so today it is a developer's shell command and a
ledger line. Wiring it into the check job beside `check-targets` (without `--enforce`,
while the toolchains are absent) is part of the CI work, and the job that *has* the
toolchains is the authority for the SKIPs.

### What the gate does *not* prove

It does not prove the API works. That is
`crates/arreo-core-ffi/tests/contract.rs` — fourteen tests that drive pairing
(both sides), a wrong code, the codec's round trip and its garbage handling, the
theme tokens, the directory cache, a live relay session (dial → drain → ack →
heartbeat → directory read), a relay that confirms someone else's identity, bytes
between two devices, and the metrics reads answered by a daemon standing behind
the relay: two reads on one conversation, a refusal that keeps it, an empty
window, a key the machine does not hold, a frame the codec refuses, and the
reconnect after a failed read. They call the
**exported surface**, never `arreo-core`'s client API underneath: the only
`arreo-core` imports in that file are the *server-side* types a test-local
mailbox and relay need to speak the shipped protocol. A boundary that cannot be
crossed fails there, on this box, rather than in an Xcode build.

## What a mobile UI still has to bring

The honest split between this crate and the toolchains it does not have.

1. **A store.** This crate has none, by design: a phone is a client, and every
   function here is a pure function of what it is handed. The device keypair is
   therefore supplied as a **32-byte seed** that the platform keystore owns
   (Keychain, Android Keystore), and the certificate crosses as bytes
   (`DeviceCertHandle::encode`) for the same store. Nothing secret crosses the
   boundary in the outbound direction, so there is nothing to leak into a crash
   report. The UI owns the file layout; `arreo_core::identity::identity_root()`
   is a desktop convention this crate never calls.
2. **Push.** The relay's durable inbox is drained by an explicit
   `drain(from_seq)`; a phone that must be woken when a machine's agent asks a
   question needs APNs/FCM — a registration, a token, and a relay-side notify
   path that do not exist yet. Until then a mobile UI polls while foregrounded.
3. **A QR camera.** `pairing_invite_parse` takes the URI string; reading it off
   a camera is AVFoundation on iOS and CameraX/ML Kit on Android. The code is
   typed by a human on purpose — the invite URI deliberately does not carry it —
   so the UI needs both a scanner and a text field.
4. **A thread policy.** `pairing_server_receive`, `pairing_server_begin`,
   `pairing_phone_join` and `pairing_phone_await_cert` are **blocking** (the
   core's pairing flow is synchronous `std::net`/`std::os::unix::net`, the CLI
   drives it from a sync function, and the TUI wraps it in `spawn_blocking`).
   UniFFI cannot say "this parks the calling thread", so a UI that calls them on
   its main thread will freeze for up to the pairing TTL. The relay session's
   calls are the opposite — genuinely `async`, driven by the tokio runtime
   UniFFI's async-compat layer provides.
5. **A call-ordering rule, from the core.** A session's `stream_to` and
   `next_peer` are *not* interchangeable. To receive, the UI waits in
   `next_peer()` and only then opens `stream_to(peer)` — the peer's first bytes
   are already queued on the stream the accept door names. Opening a stream
   first is a different protocol, not a race the binding tolerates: it replaces
   the live peer entry, and the core's read pump announces only a peer it
   created itself. To send, `stream_to` works immediately, which is the door a
   phone uses when it knows which machine it is talking to (a directory row's
   `daemon_key`). `tests/contract.rs::bytes_cross_the_boundary_between_two_devices`
   is where this is executed.
6. **A "one operation at a time" discipline.** `next_peer` needs `&mut` on the
   session, so the handle holds it behind a lock. Everything that *can* avoid
   that lock does — `stream_to`, `closed`, `heartbeat`, the identity accessors
   (cached at dial time) and `metrics_history` (it opens its stream through the
   same cached factory) — but a directory read issued while another caller sits
   in `next_peer` will queue behind it. A UI should run its accept loop and its
   directory refreshes as separate tasks and accept that serialization, rather
   than expecting the two to interleave. Metrics reads are a *second* serial
   point, on purpose: they share the per-peer conversation (see "A RAM meter"),
   so two polls of the same session queue behind each other rather than opening
   two conversations on one peer's channel.
7. **The UI itself**, which is the whole point: both platforms render the same
   `ThemeHandle` token table the TUI does, read the same `WireMessage` enum, and
   pair with the same mailbox the CLI pairs with — so the duplicated work is
   genuinely only the UI.

## Building it

```console
cargo xtask ffi --check          # build + generate + assert + compile where possible
```

The generated artifacts land in `target/test-scratch/T-0104/ffi/{swift,kotlin}`:
`ArreoCore.swift` (+ `ArreoCoreFFI.h` and a modulemap) and
`dev/arreo/core/arreo_core_ffi.kt`. The names come from
`crates/arreo-core-ffi/uniffi.toml`, so an app writes `import dev.arreo.core` /
`import ArreoCore` rather than the crate's own identifier.

The generator is this crate's own binary behind its `cli` feature
(`cargo run -p arreo-core-ffi --features cli --bin uniffi-bindgen -- generate
--library target/debug/libarreo_core_ffi.so --language swift|kotlin --out-dir
<dir>`), so it is version-locked to the library it reads: the generator and the
metadata cannot be a version apart. The `cli` feature is off by default, so the
shipped cdylib and staticlib carry neither the generator nor its dependencies.

The surface is defined in **proc-macro mode** — `#[uniffi::export]` plus
`uniffi::setup_scaffolding!()` — with no `build.rs`. That is a deliberate
departure from the Design's sketch, and it is a measured one: `uniffi`'s `build`
feature exists for `.udl` files and their build scripts, and taking it pulls
`uniffi_build` → `uniffi_bindgen`, whose own tree is where `askama` (the template
engine), `goblin`, `clap` and `cargo_metadata` live — all **absent** from
`cargo tree -p arreo-core-ffi -e normal`, because they are reached only through
the `cli`-gated generator. The one edge that *is* in the normal tree is
`toml 1.1.6` / `fs-err` / `camino` / `winnow 1.0.4`, and it arrives through
`uniffi_macros` — a **proc-macro**, so it is host-side build machinery and never
linked into the phone's library. The generated Swift and Kotlin are byte-identical
either way. The generated Swift
and Kotlin are byte-identical either way. The Design's *crate shape*, its
`cdylib`+`staticlib`+`lib` types, its leaf position, its `uniffi-bindgen`-in-crate
invocation and its honest SKIPs all hold unchanged.

## What the shipped library still carries, and the honest number

A phone links `libarreo_core_ffi.{so,a}` and calls only what is exported above. Two
dependencies of `arreo-core` ride along that **no export can reach**, because
`arreo-core` is not separable along those lines today:

| Weight | Measured | Why it is there |
|---|---|---|
| SQLite | **53** defined symbols matching `sqlite` (`nm --defined-only … | grep -ci sqlite`); `rusqlite → libsqlite3-sys` in `cargo tree -p arreo-core-ffi -e normal` | `arreo-core`'s defaults are `sqlite` + `transport`, and `mesh::resolve` calls `store::rfc3339_ms` unconditionally, so `default-features = false` does not compile today |
| PTY | **197** defined symbols matching `pty` | `portable-pty` is not optional in `arreo-core` |

Both are filed as **T-0113** (make the core separable), with the rule that the fix is
measured by symbol count rather than asserted. This matters because ROADMAP §3.5 sets a
size discipline for the mobile artifact ("the Rust .a adds ~2–6 MB", `opt-level="z"`,
`strip`), and the crate already prunes uniffi's own features for exactly this reason —
the intent is established; only the core's separability is missing.

Re-measure with:

```console
cargo build -p arreo-core-ffi
nm --defined-only target/debug/libarreo_core_ffi.so | grep -ci sqlite
cargo tree -p arreo-core-ffi -e normal | grep -E "rusqlite|portable-pty"
```
