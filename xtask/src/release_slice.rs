//! T-0042 release slice: sign → verify → refuse on real artifacts, then the
//! whole update story against a `file://` channel in one command.
//!
//! ## One sentence
//!
//! `cargo xtask e2e --slice release` builds the real release daemon once, signs
//! a fixture and that binary with a throwaway minisign keypair that exists only
//! under the build tree, proves both verify against the generated key, and
//! proves a flipped byte is refused with a sentence naming the artifact; its
//! `--chain` form then drives one whole update through a local signed channel —
//! index → verify → client atomic swap → server handoff over zero then eight
//! live panes → the T-0039 deferred rule → metrics history → alert ordering —
//! with one pass/fail line per stage.
//!
//! ## What the standalone slice proves (criterion 1)
//!
//! The release job's own "verify, and refuse" step (docs/release.md §What a
//! release does, step 4) is exercised here on the dev box, against the same
//! code paths: `cargo build --release -p arreo-server` once, then
//! `arreo_core::update::verify::verify_with` — the shared verifier door that
//! the channel (T-0037) and the CLI's `update verify` (T-0036) both call —
//! accepts a fixture and the real binary when the trust set holds the
//! throwaway key, and refuses a one-byte-flipped copy of that binary with
//! `BadSignature`, naming the artifact. The *product* binary is then asked to
//! verify the tampered copy and refuses with exit 1, naming the artifact —
//! because the key that signed it is not the pinned `supply-chain/arreo.pub`
//! key compiled into the product. Both refusals land in
//! `.loop/evidence/T-0042/tampered-refusal.txt`.
//!
//! ## What the chain proves (criterion 2)
//!
//! One command, one transcript, one pass/fail line per stage, all against a
//! `file://` channel in a temp dir under the build tree — the same transport a
//! self-hosted mirror behind a firewall serves, and the reason the whole story
//! runs with no network:
//!
//! 1. **index** — `arreo-index.json` (T-0037's format: a version and the
//!    per-target artifact; **no digests** — `SHA256SUMS` is the only digest
//!    source, and the stage signs that too) is written and signed with a
//!    throwaway keypair.
//! 2. **verify** — `arreo update --check` (T-0037's own channel code) refuses
//!    the foreign-signed index by key id, while the shared channel code
//!    accepts the *same* bytes when driven with the trust set the fixture was
//!    signed for. The slice exercises the shared path (`channel::check` +
//!    `channel::fetch` over one transport); the transport-agnosticism claim —
//!    that `file://` and `https://` differ only in the fetcher — is T-0037's
//!    own, proven unit-side by `file_and_https_channels_differ_only_in_their_fetcher`
//!    and used here rather than rebuilt as a lookalike.
//! 3. **client atomic swap** — the fetch+verify result is handed to the same
//!    install path `--from` uses (T-0070's machinery; one install path, not
//!    two), and the swap is proven byte-identical: installed == verified
//!    artifact, `.prev` == the binary it replaced, and the new binary runs.
//! 4. **server handoff, 0 panes then 8 panes with output in flight** — the
//!    real swap through `arreo update --server` (T-0038 stage 1): first a cut
//!    with no panes, then a second cut while eight counter panes print, with
//!    the T-0070 marker-continuity proof applied to the server cut — each
//!    pane's marker and `tick-1` appear exactly once and its counter is past
//!    where it was, so nothing was restarted by the handoff.
//! 5. **the deferred path, forced on Unix by flag** — T-0039 gates its
//!    Windows branch with `#[cfg(not(unix))]` (arreo-cli/src/update.rs): there
//!    is no runtime flag because on Unix the branch does not exist. The slice
//!    takes `--case windows-deferred` and mirrors the honest-skip precedent
//!    (T-0019's no-delegation rule): on Windows it runs the real branch and
//!    asserts the loud "update pending … takes effect when it next starts"
//!    outcome; on Unix it exercises the rule's Unix-observable instance — a
//!    swap with no daemon to hand over to is reported loudly ("no daemon was
//!    serving …; it will run the new binary when it next starts"), never
//!    silently, and the running daemon's agents are untouched (never forced
//!    while agents run). The outcome is always loud, never a pass that was not
//!    observed.
//! 6. **metrics history** — the query runs against the live daemon and returns
//!    recorded rows (T-0040), with a bounded retry so a marginal 10 s tick
//!    cannot flake the stage.
//! 7. **alert ordering** — a budgeted pane (T-0019 guard) sorts ahead of
//!    merely-working panes in `panes` (T-0041), and the audit log orders
//!    `enforce.alert` before `enforce.breach`. On a box without cgroup v2
//!    delegation the daemon answers a **loud** `enforce failed` error, and the
//!    stage reports a skip quoting that sentence — the enforcement slice's own
//!    environment-aware precedent — so the ordering rule is never silently
//!    assumed.
//!
//! ## Hermetic (criterion 7)
//!
//! Nothing needs network, a published release or a real signing secret. The
//! keypair is generated per run in `target/test-scratch/T-0042-…/` (the build
//! tree — never committed, never reused), every process runs under the
//! sandboxed environment update_slice establishes (HOME/XDG/ARREO_STATE_DIR
//! all inside the scratch, and an empty `file://` channel as the default so no
//! invocation can reach GitHub by accident), and the `Scratch` guard removes
//! the whole tree on drop — the last stage asserts that removal happened.
//!
//! ## Why evidence is always written
//!
//! The acceptance criteria for this task are evidence under
//! `.loop/evidence/T-0042/`, so unlike the update slice (which takes
//! `--interactive-evidence`) this slice writes its transcript, the tampered
//! refusal output, the handoff marker proof and the claim→artifact index on
//! every run.

use crate::update_slice::{
    alive_panes, copy_with_tail, describe, fetch_and_verify, first_line, minisign, pane_evidence,
    pane_id, parse_pane, read_pane, same_bytes, sign, throwaway_keypair, tick_script, PaneRead,
    Run, Sandbox, Scratch, CLI_DEADLINE,
};
use arreo_core::update::verify::{check_manifest_digest, sha256, verify_with, TrustSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

/// The panes whose counters carry the handoff invariant. Same eight as the
/// update slice: one would be a sample of one.
const PANES: usize = 8;

/// How long the alert-ordering stage waits for the enforcement sweeper (1 s
/// tick) to raise an episode.
const ALERT_SETTLE: Duration = Duration::from_secs(8);

/// The version the chain's channel index reports.
const E2E_VERSION: &str = "0.1.0";

/// What the release job names the per-target artifact; the value of the index's
/// one `artifacts` entry.
const CLIENT_ARTIFACT: &str = "arreo-e2e-client";

/// The server artifact the chain signs beside the client one, for the
/// `update --server` halves. The index carries one artifact per target (the
/// client); the server candidate travels as a `--from` path after the same
/// `verify_with` door — the chain pushes the real swap through `update
/// --server`, which is the criterion; it does not need a second index entry.
const SERVER_ARTIFACT: &str = "arreo-e2e-server";

/// The server binary's file name beside a client copy. The update verb finds
/// the daemon to replace by looking beside *itself*, so the scratch `bin/`
/// directory must spell it the way this platform does.
#[cfg(windows)]
const SERVER_EXE: &str = "arreo-server.exe";
#[cfg(not(windows))]
const SERVER_EXE: &str = "arreo-server";

/// One slice, two modes: the standalone sign→verify→refuse proof and the
/// chained end-to-end story. Both write their evidence; any failure at all is
/// a non-zero exit; a skip is named a skip and never counted as a pass.
pub fn run(rest: &[String]) -> ExitCode {
    let chain = rest.iter().any(|a| a == "--chain");
    let mut report = crate::update_slice::Report::new(if chain {
        "release/chain: "
    } else {
        "release: "
    });
    let mut refusal = String::new();
    let mut markers = String::new();
    // The path both modes use; `run` recomputes it only to prove it is gone.
    let scratch = scratch_root(chain);
    if chain {
        chain_slice(&mut report, rest, &mut refusal, &mut markers);
    } else {
        release_slice(&mut report, &mut refusal);
    }
    // Hermetic: the scratch (keypair, channel, binary copies) is removed by its
    // guard before this line — nothing this slice expected to be there survives.
    report.check(
        "the slice's scratch (keypair, channel, binary copies) was removed",
        !scratch.exists(),
        &format!("{} still exists", scratch.display()),
    );
    write_evidence(&report, chain, &refusal, &markers);
    report.finish()
}

// ---------------------------------------------------------------------------
// The standalone slice: sign → verify → refuse (criterion 1).
// ---------------------------------------------------------------------------

fn release_slice(report: &mut crate::update_slice::Report, refusal: &mut String) {
    let root = repo_root();
    let (cli_bin, release_server) = bins(&root);
    if !cli_bin.exists() {
        report.check(
            "the client binary under test is built",
            false,
            &format!(
                "{} does not exist; build first (`cargo build -p arreo-cli`)",
                cli_bin.display()
            ),
        );
        return;
    }
    if !build_release_server(report) {
        return;
    }

    let scratch_root = scratch_root(false);
    if let Err(e) = fs::create_dir_all(&scratch_root) {
        report.check("the scratch directory can be made", false, &e.to_string());
        return;
    }
    report.say(format!(
        "release: scratch and keypair live under {} (build tree; removed when the slice exits)",
        scratch_root.display()
    ));
    let _scratch = Scratch(scratch_root.clone());

    let Some(minisign) = minisign() else {
        // T-0019's no-delegation precedent: a check that needs signed bytes
        // must say what is missing and never report a pass it could not
        // produce.
        for name in [
            "the fixture and the release binary are signed",
            "both verify against the generated key",
            "a flipped byte is refused with a sentence naming the artifact",
            "SHA256SUMS is the only trusted digest source",
        ] {
            report.skip(
                name,
                "the `minisign` binary is not installed on this machine (the workspace ships no \
                 signer by design — T-0036); a machine that has it runs the full sign/verify/refuse \
                 proof",
            );
        }
        return;
    };

    // A keypair per run, under the build tree: `minisign -G` refuses to
    // overwrite an existing key, and the scratch dir is pid-unique, so the
    // same pair can never be reused by a later run.
    let keys_dir = scratch_root.join("keys");
    let keypair = match fs::create_dir_all(&keys_dir) {
        Ok(()) => throwaway_keypair(&minisign, &keys_dir),
        Err(e) => Err(format!("{}: {e}", keys_dir.display())),
    };
    let (_public_key, secret_key, public_text) = match keypair {
        Ok(triple) => triple,
        Err(e) => {
            report.check("the throwaway keypair is generated", false, &e);
            return;
        }
    };
    let trust = match TrustSet::parse(&public_text) {
        Some(trust) => trust,
        None => {
            report.check(
                "the generated public key parses as a trust set",
                false,
                "TrustSet::parse found no minisign public key in the generated text",
            );
            return;
        }
    };

    // The two things a release job signs: a fixture, and the real
    // `cargo build --release` daemon binary. The release binary is signed as
    // a *copy* — the job signs dist artifacts, never in place, and a `.minisig`
    // beside `target/release/arreo-server` would be an artifact nobody asked
    // for in a directory the slice must not own.
    let fixture = scratch_root.join("fixture.txt");
    let server_copy = scratch_root.join("release-server.bin");
    if let Err(e) = make_fixtures(&release_server, &fixture, &server_copy) {
        report.check("the signing fixtures can be prepared", false, &e);
        return;
    }
    let signed = sign(&minisign, &secret_key, &fixture)
        .and_then(|()| sign(&minisign, &secret_key, &server_copy));
    let signed_detail = match &signed {
        Ok(()) => "signatures written beside both".to_string(),
        Err(e) => e.to_string(),
    };
    report.check(
        "the fixture and the release binary are signed",
        signed.is_ok(),
        &signed_detail,
    );
    if signed.is_err() {
        return;
    }

    let fixture_ok = verify_with(&trust, &fixture, None);
    match &fixture_ok {
        Ok(verified) => report.check(
            "the fixture verifies against the generated key",
            true,
            &format!("key {}", verified.key_id),
        ),
        Err(e) => report.check(
            "the fixture verifies against the generated key",
            false,
            &e.to_string(),
        ),
    }
    let server_ok = verify_with(&trust, &server_copy, None);
    match &server_ok {
        Ok(verified) => report.check(
            "the real release binary verifies against the generated key",
            true,
            &format!("key {}", verified.key_id),
        ),
        Err(e) => report.check(
            "the real release binary verifies against the generated key",
            false,
            &e.to_string(),
        ),
    }

    // Two doors onto the refusal, because the criterion wants two properties:
    // the sentence names the artifact, and the exit is non-zero.
    //
    // Door 1 — the shared verifier, with the trust set the fixture was signed
    // for: the flipped byte yields the verifier's own `BadSignature` sentence.
    // Door 2 — the product binary (`arreo update verify`), whose trust set is
    // the key pinned in `supply-chain/arreo.pub`: a stranger-signed file is
    // refused with exit 1, and the refusal names the artifact.
    let tampered = scratch_root.join("tampered.bin");
    match flip_byte(&server_copy, &tampered) {
        Ok(()) => {}
        Err(e) => {
            report.check("the tampered copy can be made", false, &e);
            return;
        }
    }
    // The signature travels beside it, exactly as the release job ships it —
    // carried over from the original bytes, so the refusals below are about
    // the flipped content (and the foreign key), never about a missing file.
    if let Err(e) = fs::copy(
        scratch_root.join("release-server.bin.minisig"),
        scratch_root.join("tampered.bin.minisig"),
    ) {
        report.check(
            "the tampered file's signature can be staged",
            false,
            &e.to_string(),
        );
        return;
    }
    let tampered_path = tampered.display().to_string();
    let shared = verify_with(&trust, &tampered, None);
    match &shared {
        Ok(_) => {
            report.check(
                "a flipped byte is refused by the shared verifier, naming the artifact",
                false,
                "the flipped byte verified — the tamper went unnoticed",
            );
            refusal.push_str("verify_with: NO REFUSAL — the tampered bytes verified!\n");
        }
        Err(e) => {
            let text = e.to_string();
            refusal.push_str(&format!(
                "verify_with (the shared door, trust set = the throwaway key):\n{text}\n"
            ));
            report.check(
                "a flipped byte is refused by the shared verifier, naming the artifact",
                text.contains(&tampered_path) && text.contains("signature does not authenticate"),
                &text,
            );
        }
    }

    // The product binary: run a copy of the real client, whose trust set is
    // compiled in. The signature beside the tampered file is the *original*
    // one (carried over), so the refusal is real: the bytes or the key do not
    // match what the product trusts.
    let client_scratch = scratch_root.join("update-verify");
    let client_copy = client_scratch.join("arreo");
    let refuse_env = fs::create_dir_all(&client_scratch)
        .and_then(|()| fs::copy(&cli_bin, &client_copy).map(|_| ()));
    match refuse_env {
        Ok(()) => {}
        Err(e) => {
            report.check(
                "the product-verify fixture can be prepared",
                false,
                &e.to_string(),
            );
            return;
        }
    }
    let sandbox = match Sandbox::new(scratch_root.join("sandbox")) {
        Ok(sandbox) => sandbox,
        Err(e) => {
            report.check("the sandbox environment can be made", false, &e);
            return;
        }
    };
    let r = crate::update_slice::run_cli(
        &sandbox,
        &client_copy,
        &["update", "verify", &tampered_path],
        CLI_DEADLINE,
    );
    let product_refused = r.code == Some(1)
        && r.output.contains("signed by key")
        && r.output.contains(&tampered_path);
    refusal.push_str(&format!(
        "the product binary (`arreo update verify <tampered>`) exited {:?}:\n{}\n",
        r.code, r.output
    ));
    report.check(
        "the product binary refuses the tampered file with exit 1, naming it",
        product_refused,
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );

    // SHA256SUMS is the only trusted digest source (docs/release.md): the
    // manifest's own signature verifies first, then the artifact's digest is
    // checked against its entry; an entry that does not match is refused by
    // name. A second digest list anywhere else would be a second answer to
    // "which bytes are this release?" — the index carries none, asserted in
    // the chain stage.
    let manifest = scratch_root.join("SHA256SUMS");
    if let Err(e) = write_manifest(&server_copy, &manifest)
        .and_then(|()| sign(&minisign, &secret_key, &manifest))
    {
        report.check("the signed manifest can be written", false, &e);
        return;
    }
    let manifest_ok = verify_with(&trust, &manifest, None);
    let digest_ok = check_manifest_digest(&server_copy, &manifest);
    let digest_expected = sha256(&server_copy);
    match (&manifest_ok, &digest_ok, &digest_expected) {
        (Ok(_), Ok(found), Ok(expected)) => report.check(
            "the signed SHA256SUMS manifest verifies and matches the artifact's digest",
            found == expected,
            &format!("{found} == {expected}"),
        ),
        (Err(e), _, _) => report.check(
            "the signed SHA256SUMS manifest verifies and matches the artifact's digest",
            false,
            &e.to_string(),
        ),
        (_, Err(e), _) => report.check(
            "the signed SHA256SUMS manifest verifies and matches the artifact's digest",
            false,
            &e.to_string(),
        ),
        (_, _, Err(e)) => report.check(
            "the signed SHA256SUMS manifest verifies and matches the artifact's digest",
            false,
            &e.to_string(),
        ),
    }

    // A tampered manifest entry is refused by name: the digest door says the
    // file is not the one the manifest listed.
    let bad_manifest = scratch_root.join("SHA256SUMS.tampered");
    let good = sha256(&server_copy).unwrap_or_default();
    let tampered_text = fs::read_to_string(&manifest)
        .map(|text| {
            text.replace(
                &good,
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
        })
        .unwrap_or_default();
    match fs::write(&bad_manifest, tampered_text) {
        Ok(()) => match check_manifest_digest(&server_copy, &bad_manifest) {
            Err(e) => {
                let text = e.to_string();
                refusal.push_str(&format!("the digest door, tampered manifest:\n{text}\n"));
                report.check(
                    "a tampered manifest entry is refused, naming the artifact",
                    text.contains(&server_copy.display().to_string())
                        && text.contains("the trusted manifest says"),
                    &text,
                );
            }
            Ok(_) => report.check(
                "a tampered manifest entry is refused, naming the artifact",
                false,
                "the tampered manifest matched — the digest door accepted the wrong bytes",
            ),
        },
        Err(e) => report.check(
            "a tampered manifest entry can be written",
            false,
            &format!("{}: {e}", bad_manifest.display()),
        ),
    }
}

/// The fixture file and the copy of the release binary the slice signs.
fn make_fixtures(release_server: &Path, fixture: &Path, server_copy: &Path) -> Result<(), String> {
    fs::write(
        fixture,
        "T-0042 release slice fixture: a byte payload a release job might sign\n".as_bytes(),
    )
    .map_err(|e| format!("{}: {e}", fixture.display()))?;
    fs::copy(release_server, server_copy)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", server_copy.display()))
}

/// Copy `source` to `dest`, flip one byte in the middle.
///
/// The middle, not the tail: the signature covers the whole file, so any byte
/// breaks it, and the middle is the part nobody could trip over by closing the
/// file early.
fn flip_byte(source: &Path, dest: &Path) -> Result<(), String> {
    let bytes = fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
    if bytes.is_empty() {
        return Err(format!("{} is empty; nothing to flip", source.display()));
    }
    let mut bytes = bytes;
    let at = bytes.len() / 2;
    bytes[at] ^= 0x01;
    fs::write(dest, bytes).map_err(|e| format!("{}: {e}", dest.display()))
}

/// `sha256sum`'s format: `<hex>  *<name>`, independently of where it lives.
fn write_manifest(artifact: &Path, manifest: &Path) -> Result<(), String> {
    let digest = sha256(artifact).map_err(|e| e.to_string())?;
    let name = match artifact.file_name() {
        Some(name) => name.to_string_lossy(),
        None => artifact.display().to_string().into(),
    };
    fs::write(manifest, format!("{digest}  *{name}\n"))
        .map_err(|e| format!("{}: {e}", manifest.display()))
}

// ---------------------------------------------------------------------------
// The chain: index → verify → client swap → server handoff → deferred rule →
// metrics → alert ordering (criteria 2–3).
// ---------------------------------------------------------------------------

fn chain_slice(
    report: &mut crate::update_slice::Report,
    rest: &[String],
    refusal: &mut String,
    markers: &mut String,
) {
    let root = repo_root();
    let (cli_bin, release_server) = bins(&root);
    if !cli_bin.exists() {
        report.check(
            "the client binary under test is built",
            false,
            &format!(
                "{} does not exist; build first (`cargo build -p arreo-cli`)",
                cli_bin.display()
            ),
        );
        return;
    }
    if !build_release_server(report) {
        return;
    }
    // `--case windows-deferred`: the workflow half passes it on the Windows
    // leg. Here it selects the deferred stage's real-vs-observable form — on
    // Unix the Windows branch is compiled out (T-0039), so the stage runs the
    // rule's Unix-observable instance and says that out loud; it is never a
    // silent no-op.
    let case_deferred = rest
        .windows(2)
        .any(|w| w[0] == "--case" && w[1] == "windows-deferred");

    let scratch_root = scratch_root(true);
    if let Err(e) = fs::create_dir_all(&scratch_root) {
        report.check("the scratch directory can be made", false, &e.to_string());
        return;
    }
    report.say(format!(
        "release/chain: scratch at {} (under the build tree; removed on exit)",
        scratch_root.display()
    ));
    let _scratch = Scratch(scratch_root.clone());
    let sandbox = match Sandbox::new(scratch_root.join("sbx")) {
        Ok(sandbox) => sandbox,
        Err(e) => {
            report.check("the sandbox environment can be made", false, &e);
            return;
        }
    };

    // ---- the fixtures ------------------------------------------------------
    // A `bin/` directory holding the client copy and the server copy beside
    // each other: `update --server` finds the daemon to replace by looking
    // beside the client it runs from, so the two must be siblings. Everything
    // below is driven through these copies; `target/debug/arreo` and
    // `target/release/arreo-server` are only ever read from.
    let bin = scratch_root.join("bin");
    let bin_cli = bin.join("arreo");
    let bin_server = bin.join(SERVER_EXE);
    let client_ref = scratch_root.join("client-ref");
    let server_ref = scratch_root.join("server-ref");
    if let Err(e) = make_chain_bins(
        &cli_bin,
        &release_server,
        &bin,
        &bin_cli,
        &bin_server,
        &client_ref,
        &server_ref,
    ) {
        report.check("the chain's binary copies can be made", false, &e);
        return;
    }
    let sock = scratch_root.join("arreo.sock");
    let sock_str = sock.display().to_string();

    let Some(minisign) = minisign() else {
        for name in [
            "the channel index is written and signed",
            "the channel is verified through the shared code path",
            "the verified artifact is swapped in byte-identically",
            "the server handoff cuts over zero and eight panes",
            "the deferred rule is exercised loudly",
            "the metrics history query returns recorded rows",
        ] {
            report.skip(
                name,
                "the `minisign` binary is not installed on this machine (T-0036: the workspace \
                 ships no signer); a machine that has it runs the full chain",
            );
        }
        return;
    };
    let keys_dir = scratch_root.join("keys");
    let keypair = match fs::create_dir_all(&keys_dir) {
        Ok(()) => throwaway_keypair(&minisign, &keys_dir),
        Err(e) => Err(format!("{}: {e}", keys_dir.display())),
    };
    let (_public_key, secret_key, public_text) = match keypair {
        Ok(triple) => triple,
        Err(e) => {
            report.check("the throwaway keypair is generated", false, &e);
            return;
        }
    };
    let trust = match TrustSet::parse(&public_text) {
        Some(trust) => trust,
        None => {
            report.check(
                "the generated public key parses as a trust set",
                false,
                "TrustSet::parse found no minisign public key in the generated text",
            );
            return;
        }
    };

    // ---- the channel -------------------------------------------------------
    let channel = scratch_root.join("channel");
    if let Err(e) = fs::create_dir_all(&channel) {
        report.check("the channel directory can be made", false, &e.to_string());
        return;
    }
    let channel_url = format!("file://{}/", channel.display());
    let client_artifact = channel.join(CLIENT_ARTIFACT);
    let server_artifact = channel.join(SERVER_ARTIFACT);
    let index_path = channel.join("arreo-index.json");

    // 1. index. T-0037's format and nothing else: a version and the artifact
    //    per target. Deliberately **no digests** — the signed SHA256SUMS
    //    manifest below is the only digest source, exactly as docs/release.md
    //    says, and the stage says so out loud rather than implying it.
    let index_text = format!(
        "{{\"version\":\"{E2E_VERSION}\",\"artifacts\":{{\"{}\":\"{CLIENT_ARTIFACT}\"}}}}",
        arreo_core::update::channel::host_target()
    );
    let prepared = copy_with_tail(
        &cli_bin,
        &client_artifact,
        b"\n# T-0042 channel client artifact\n",
    )
    .and_then(|()| {
        copy_with_tail(
            &release_server,
            &server_artifact,
            b"\n# T-0042 channel server artifact\n",
        )
    })
    .and_then(|()| {
        fs::write(&index_path, index_text.as_bytes())
            .map_err(|e| format!("{}: {e}", index_path.display()))
    })
    .and_then(|()| sign(&minisign, &secret_key, &index_path))
    .and_then(|()| sign(&minisign, &secret_key, &client_artifact))
    .and_then(|()| sign(&minisign, &secret_key, &server_artifact))
    .and_then(|()| {
        write_manifest(&client_artifact, &channel.join("SHA256SUMS"))
            .and_then(|()| sign(&minisign, &secret_key, &channel.join("SHA256SUMS")))
    });
    let no_digests = !index_text.contains("sha256")
        && !index_text.contains("digest")
        && !index_text.contains("SHA256");
    let prepared_detail = match &prepared {
        Ok(()) => "index + two artifacts + manifest signed".to_string(),
        Err(e) => e.to_string(),
    };
    report.check(
        "the channel index is written and signed (no digests — SHA256SUMS is the digest source)",
        prepared.is_ok() && no_digests,
        &format!(
            "index at {} carrying no digest fields: {no_digests} ({prepared_detail})",
            index_path.display(),
        ),
    );
    if prepared.is_err() {
        return;
    }

    // 2. verify. Two doors on the same signed bytes: the product binary —
    //    whose trust set is the key compiled into it — refuses the foreign
    //    signature by key id with exit 1; the shared channel code accepts the
    //    *same* bytes when driven with the trust set the fixture was signed
    //    for. Both run T-0037's own code; there is no second implementation.
    let r = crate::update_slice::run_cli(
        &sandbox,
        &bin_cli,
        &["update", "--check", "--channel", &channel_url],
        CLI_DEADLINE,
    );
    report.check(
        "`update --check` refuses the foreign-signed index by key id, naming the channel",
        r.code == Some(1)
            && r.output.contains("signed by key")
            && r.output.contains("this build trusts")
            && r.output.contains(&channel_url),
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );
    report.say(
        "release/chain: verify — the index was parsed and verified through channel::check \
         (T-0037) over file://; an https:// channel runs the SAME function — Network::get is \
         the only divergence, proven unit-side by channel.rs's \
         file_and_https_channels_differ_only_in_their_fetcher. The slice drove the shared \
         path, not a lookalike.",
    );
    refusal.push_str(&format!(
        "the channel's foreign-signed index, refused by the product (`update --check`):\n{}\n",
        r.output
    ));

    let accepted = fetch_and_verify(&trust, &channel_url, &scratch_root);
    match &accepted {
        Ok((release, verified)) => {
            let named = release.version == E2E_VERSION
                && release.artifact == CLIENT_ARTIFACT
                && verified.artifact.ends_with(CLIENT_ARTIFACT);
            // The manifest bound the fetched bytes before the install path is
            // allowed anywhere near them: the digest door is open only for
            // bytes the signed manifest lists.
            let digest_door =
                check_manifest_digest(&verified.artifact, &channel.join("SHA256SUMS"));
            let digest_expected = sha256(&verified.artifact);
            let door_ok = match (&digest_door, &digest_expected) {
                (Ok(found), Ok(expected)) => found == expected,
                _ => false,
            };
            report.check(
                "the shared channel code accepts the same bytes with the fixture's trust set",
                named && door_ok,
                &format!(
                    "release {} artifact {} (digest door: {door_ok})",
                    release.version, release.artifact
                ),
            );
        }
        Err(e) => {
            report.check(
                "the shared channel code accepts the same bytes with the fixture's trust set",
                false,
                e,
            );
        }
    }

    // 3. client atomic swap. The verified artifact is handed to the same
    //    install path `--from` uses (T-0070's machinery — one install path,
    //    not two), and the swap is proven by bytes: installed == verified,
    //    `.prev` == the binary it replaced, and the new binary runs.
    let client_swap_done = match &accepted {
        Ok((_, verified)) => {
            let verified_path = verified.artifact.display().to_string();
            let r = crate::update_slice::run_cli(
                &sandbox,
                &bin_cli,
                &[
                    "update",
                    "--from",
                    &verified_path,
                    "--no-reexec",
                    "--socket",
                    &sock_str,
                ],
                CLI_DEADLINE,
            );
            let installed = same_bytes(&bin_cli, &verified.artifact).unwrap_or(false);
            let prev_kept = same_bytes(&bin.join("arreo.prev"), &client_ref).unwrap_or(false);
            let runs =
                crate::update_slice::run_cli(&sandbox, &bin_cli, &["--version"], CLI_DEADLINE).ok();
            report.check(
                "the verified artifact is swapped in byte-identically (installed == verified; .prev == replaced)",
                r.ok() && installed && prev_kept && runs,
                &format!(
                    "installed_bytes_equal={installed} prev_kept={prev_kept} new_runs={runs} \
                     (update exited {:?}: {})",
                    r.code,
                    first_line(&r.output)
                ),
            );
            installed && prev_kept && runs
        }
        Err(_) => {
            report.check(
                "the verified artifact is swapped in byte-identically (installed == verified; .prev == replaced)",
                false,
                "the channel did not accept the artifact in the previous stage",
            );
            false
        }
    };
    if !client_swap_done {
        return;
    }

    // The three server candidates: real copies of the release daemon, each
    // with its own tail so each install is a real swap and never the no-op the
    // verb correctly makes of bytes it already has.
    let cand1 = scratch_root.join("arreo-server-cand1");
    let cand2 = scratch_root.join("arreo-server-cand2");
    let cand3 = scratch_root.join("arreo-server-cand3");
    let candidates = copy_with_tail(&release_server, &cand1, b"\n# server candidate 1\n")
        .and_then(|()| copy_with_tail(&release_server, &cand2, b"\n# server candidate 2\n"))
        .and_then(|()| copy_with_tail(&release_server, &cand3, b"\n# server candidate 3\n"));
    let (cand1_path, cand2_path, cand3_path) = (
        cand1.display().to_string(),
        cand2.display().to_string(),
        cand3.display().to_string(),
    );
    if let Err(e) = candidates {
        report.check("the server candidates can be prepared", false, &e);
        return;
    }

    // 4. server handoff, 0 panes. The real swap through `update --server`
    //    (T-0038 stage 1): a daemon with nothing to protect, cut to a new pid.
    let mut daemon_a =
        match crate::update_slice::Daemon::spawn(&sandbox, &bin_server, &bin_cli, &sock) {
            Ok(daemon) => daemon,
            Err(e) => {
                report.check("the daemon starts and binds its socket", false, &e);
                return;
            }
        };
    let pid_a = daemon_a.id;
    // From here on the daemon serving the socket may be one this slice did not
    // spawn, so it is stopped via `server stop` — including on an early return.
    let mut serving = ServingDaemon::new(&sandbox, &bin_cli, &sock);
    report.say(format!(
        "release/chain: first daemon is pid {pid_a} on {sock_str} (spawned by this slice, so its \
         liveness is observable)"
    ));

    let r = crate::update_slice::run_cli(
        &sandbox,
        &bin_cli,
        &[
            "update",
            "--server",
            "--from",
            &cand1_path,
            "--socket",
            &sock_str,
            "--json",
        ],
        CLI_DEADLINE,
    );
    let handed_over = r.output.contains("\"handoff\"")
        && r.output.contains("\"from_pid\"")
        && r.output.contains("\"to_pid\"");
    let installed_1 = same_bytes(&bin_server, &cand1).unwrap_or(false);
    let prev_1 = same_bytes(&bin.join("arreo-server.prev"), &server_ref).unwrap_or(false);
    let old_gone = daemon_a.still_running().is_err();
    let cut_zero_ok = r.ok() && handed_over && installed_1 && prev_1 && old_gone;
    report.check(
        "server handoff, 0 panes: `update --server` cuts to a new pid and nothing is left behind",
        cut_zero_ok,
        &format!(
            "exited {:?}: {} | installed==cand1: {installed_1} | .prev==old: {prev_1} | old daemon \
             (pid {pid_a}) gone: {old_gone}",
            r.code,
            first_line(&r.output)
        ),
    );
    report.say(format!(
        "release/chain: handoff (0 panes) -> {}",
        first_line(&r.output)
    ));
    if !cut_zero_ok {
        return;
    }

    // 5. server handoff, 8 panes with output in flight. The T-0070 proof
    //    applied to the server cut: each pane's marker and `tick-1` appear
    //    exactly once across the cut and the counter is past where it was, so
    //    no pane was restarted — only re-parented. (Unix-only: descriptor
    //    passing is what the handoff is built on; T-0039 routes other
    //    platforms to the deferred stage.)
    let ctx = ChainCtx {
        sandbox: &sandbox,
        bin_cli: &bin_cli,
        bin_server: &bin_server,
        sock_str: &sock_str,
    };
    let panes_ok = handoff_eight_panes(report, &ctx, &cand2_path, &cand2, markers);
    if !panes_ok {
        return;
    }

    // 6. the deferred rule (T-0039), forced on Unix by flag. Stage 5 left the
    //    eight panes running on the current daemon; this stage proves the rule
    //    "never forced while agents run" on the way out.
    let deferred_ok = deferred_stage(
        report,
        case_deferred,
        &ctx,
        &cand3_path,
        &cand3,
        &scratch_root,
    );
    if !deferred_ok {
        return;
    }

    // 7. metrics history. The pane has been alive through two cuts and the
    //    deferred swap; the writer ticks every 10 s, so a bounded retry turns
    //    a marginal tick boundary into a settled wait rather than a flake.
    let mut rows_ok = false;
    let mut row_run = Run {
        code: None,
        output: String::new(),
        elapsed: Duration::from_secs(0),
    };
    for _ in 0..8 {
        let r = crate::update_slice::run_cli(
            &sandbox,
            &bin_cli,
            &[
                "metrics", "history", "pane-1", "--since", "6h", "--step", "1m", "--socket",
                &sock_str,
            ],
            CLI_DEADLINE,
        );
        if r.ok() && series_has_rows(&r.output) {
            rows_ok = true;
            row_run = r;
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    let metrics_detail = if rows_ok {
        first_line(&row_run.output)
    } else {
        format!("last attempt: {}", first_line(&row_run.output))
    };
    report.check(
        "metrics history returns the recorded series for a live pane",
        rows_ok,
        &metrics_detail,
    );
    if rows_ok {
        report.say(
            "release/chain: metrics history — the 10 s writer sampled the pane through the cuts; \
             the query renders the CSV the CLI owns (T-0040).",
        );
    }

    // 8. alert ordering (T-0041). The CLI has no budget flags yet (the
    //    protocol carries them; the enforcement slice drives the socket
    //    directly for the same reason). A budgeted pane sorts ahead of
    //    merely-working ones in `panes`, and the audit log orders
    //    `enforce.alert` before `enforce.breach`. On a box without cgroup v2
    //    delegation the daemon answers a LOUd `enforce failed` error and the
    //    stage says so — precisely the enforcement slice's environment-aware
    //    precedent, never a silent pass.
    alert_ordering_stage(report, &sandbox, &bin_cli, &sock_str, &sock);

    // ---- teardown ----------------------------------------------------------
    // The serving daemon is the one the last `update --server` cut left; this
    // slice holds no Child for it, so it is stopped through the daemon's own
    // verb — which takes its panes with it, the drain path, so no counter
    // loop outlives the slice.
    let stopped = serving.stop();
    report.check(
        "the daemon stops cleanly through its own verb",
        stopped.ok(),
        &format!("exited {:?}", stopped.code),
    );
}

/// The handles the handoff and deferred stages share: everything the product
/// verbs need to reach the scratch daemon. One struct keeps their signatures
/// under the clippy argument budget and gives a sibling a single carrier.
struct ChainCtx<'a> {
    sandbox: &'a Sandbox,
    bin_cli: &'a Path,
    bin_server: &'a Path,
    sock_str: &'a str,
}

/// The daemon that serves the scratch socket after a handoff. The slice does
/// not hold its [`Child`] — `update --server` started it — so it is stopped
/// through the daemon's own verb, and stopped **on every exit path**: an early
/// `return` after a failed stage must not leave a daemon (and eight counter
/// loops) behind. `Drop` is the backstop; the explicit `stop` is what the
/// teardown check reports.
struct ServingDaemon {
    sandbox: Sandbox,
    cli: PathBuf,
    socket: PathBuf,
    done: bool,
}

impl ServingDaemon {
    fn new(sandbox: &Sandbox, cli: &Path, socket: &Path) -> Self {
        Self {
            sandbox: sandbox.clone(),
            cli: cli.to_path_buf(),
            socket: socket.to_path_buf(),
            done: false,
        }
    }

    /// `arreo server stop` — the drain path, which takes the panes with it.
    /// Idempotent: the second call (from `Drop`) does nothing.
    fn stop(&mut self) -> Run {
        if self.done {
            return Run {
                code: Some(0),
                output: "already stopped".to_string(),
                elapsed: Duration::from_secs(0),
            };
        }
        self.done = true;
        crate::update_slice::run_cli(
            &self.sandbox,
            &self.cli,
            &[
                "server",
                "stop",
                "--socket",
                &self.socket.display().to_string(),
            ],
            Duration::from_secs(15),
        )
    }
}

impl Drop for ServingDaemon {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The 8-pane handoff (Unix): spawn counters, record them, cut, prove none
/// restarted. Unix-only by nature; the cfg(!unix) twin reports a loud skip.
#[cfg(unix)]
fn handoff_eight_panes(
    report: &mut crate::update_slice::Report,
    ctx: &ChainCtx<'_>,
    cand2_path: &str,
    cand2: &Path,
    markers: &mut String,
) -> bool {
    let mut all_ok = true;
    let mut detail = Vec::new();
    for n in 1..=PANES {
        let id = pane_id(n);
        let script = tick_script(n);
        let r = crate::update_slice::run_cli(
            ctx.sandbox,
            ctx.bin_cli,
            &[
                "spawn",
                &id,
                "/bin/sh",
                "-c",
                &script,
                "--socket",
                ctx.sock_str,
            ],
            CLI_DEADLINE,
        );
        if !r.ok() {
            all_ok = false;
            detail.push(format!("{id}: exited {:?}", r.code));
        }
    }
    report.check(
        "eight panes are spawned on the serving daemon",
        all_ok,
        &if detail.is_empty() {
            "all eight spawn calls exited 0".to_string()
        } else {
            detail.join("; ")
        },
    );
    if !all_ok {
        return false;
    }

    // Let the counters climb before recording: a cut can only be shown to
    // leave a counter climbing if one was climbing already.
    std::thread::sleep(Duration::from_secs(4));

    let before = read_all_panes(ctx.sandbox, ctx.bin_cli, ctx.sock_str);
    markers.push_str("--- panes before the 8-pane handoff ---\n");
    markers.push_str(&before.raw);
    let listing = crate::update_slice::run_cli(
        ctx.sandbox,
        ctx.bin_cli,
        &["panes", "--socket", ctx.sock_str],
        CLI_DEADLINE,
    );
    let alive = alive_panes(&listing.output);
    let all_up =
        (1..=PANES).all(|n| alive.contains(&pane_id(n)) && before.reads[n - 1].markers == 1);
    report.check(
        "eight panes are running their counters before the cut",
        all_up,
        &format!("alive={alive:?} before={}", describe(&before.reads)),
    );
    if !all_up {
        return false;
    }

    let r = crate::update_slice::run_cli(
        ctx.sandbox,
        ctx.bin_cli,
        &[
            "update",
            "--server",
            "--from",
            cand2_path,
            "--socket",
            ctx.sock_str,
            "--json",
        ],
        CLI_DEADLINE,
    );
    let handed_over_2 = r.output.contains("\"handoff\"")
        && r.output.contains("\"from_pid\"")
        && r.output.contains("\"to_pid\"");
    report.check(
        "the 8-pane cut reports a new daemon pid",
        r.ok() && handed_over_2,
        &format!("exited {:?}: {}", r.code, first_line(&r.output)),
    );
    if !(r.ok() && handed_over_2) {
        return false;
    }
    report.say(format!(
        "release/chain: handoff (8 panes in flight) -> {}",
        first_line(&r.output)
    ));

    std::thread::sleep(Duration::from_secs(3));
    let after = read_all_panes(ctx.sandbox, ctx.bin_cli, ctx.sock_str);
    markers.push_str("--- panes after the 8-pane handoff ---\n");
    markers.push_str(&after.raw);
    let listing = crate::update_slice::run_cli(
        ctx.sandbox,
        ctx.bin_cli,
        &["panes", "--socket", ctx.sock_str],
        CLI_DEADLINE,
    );
    let alive_after = alive_panes(&listing.output);
    let all_alive_after = (1..=PANES).all(|n| alive_after.contains(&pane_id(n)));
    let restarted: Vec<String> = (1..=PANES)
        .filter(|n| {
            let (b, a) = (&before.reads[n - 1], &after.reads[n - 1]);
            !(a.markers == 1 && a.first_ticks == 1 && a.highest() > b.highest())
        })
        .map(|n| {
            format!(
                "pane-{n}: starts {}->{} tick-1 {}->{} tick {}->{}",
                before.reads[n - 1].markers,
                after.reads[n - 1].markers,
                before.reads[n - 1].first_ticks,
                after.reads[n - 1].first_ticks,
                before.reads[n - 1].highest(),
                after.reads[n - 1].highest()
            )
        })
        .collect();
    markers.push_str(&format!(
        "--- the marker-continuity verdict ---\nrestarted or stalled: {}\n",
        if restarted.is_empty() {
            "none".to_string()
        } else {
            restarted.join("; ")
        }
    ));
    let installed_2 = same_bytes(ctx.bin_server, cand2).unwrap_or(false);
    report.check(
        "no pane was restarted by the cut: every counter continued instead of resetting",
        all_alive_after && restarted.is_empty() && installed_2,
        &format!(
            "alive={alive_after:?} installed==cand2: {installed_2} restarted: {}",
            restarted.join("; ")
        ),
    );
    all_alive_after && restarted.is_empty() && installed_2
}

/// The same stage where the platform cannot hand a PTY over at all: a loud
/// skip naming T-0039 and the real (deferred) branch, never a silent pass.
#[cfg(not(unix))]
fn handoff_eight_panes(
    report: &mut crate::update_slice::Report,
    _ctx: &ChainCtx<'_>,
    _cand2_path: &str,
    _cand2: &Path,
    _markers: &mut String,
) -> bool {
    report.skip(
        "server handoff, 8 panes in flight",
        "live PTY handoff is unix-only (SCM_RIGHTS descriptor passing — T-0039 routes non-unix \
         to the deferred stage, which runs for real below)",
    );
    false
}

/// The deferred rule's two platform shapes. On Windows the stage runs the real
/// `Handoff::Deferred` branch (a serving daemon, installed, "update pending …
/// takes effect when it next starts", `{"daemon":{"deferred":true}}`). On Unix
/// that branch is compiled out (T-0039), so the stage exercises the rule's
/// Unix-observable instance: a swap with NO daemon to hand over to is reported
/// loudly, the swap lands at the path, and the running daemon's agents are
/// untouched — never forced, never silent. `case_deferred` documents which
/// case the operator asked for; both shapes stay loud.
#[cfg(not(unix))]
fn deferred_stage(
    report: &mut crate::update_slice::Report,
    _case_deferred: bool,
    ctx: &ChainCtx<'_>,
    cand3_path: &str,
    cand3: &Path,
    _scratch_root: &Path,
) -> bool {
    let r = crate::update_slice::run_cli(
        ctx.sandbox,
        ctx.bin_cli,
        &[
            "update",
            "--server",
            "--from",
            cand3_path,
            "--socket",
            ctx.sock_str,
            "--json",
        ],
        CLI_DEADLINE,
    );
    let pending = r.output.contains("update pending")
        && r.output.contains("takes effect when it next starts")
        && r.output.contains("\"deferred\":true");
    let installed_3 = same_bytes(ctx.bin_server, cand3).unwrap_or(false);
    let ok = r.ok() && pending && installed_3;
    report.check(
        "T-0039's deferred path: a serving daemon is left running, loudly",
        ok,
        &format!("exited {:?}: {}", r.code, first_line(&r.output)),
    );
    ok
}

#[cfg(unix)]
fn deferred_stage(
    report: &mut crate::update_slice::Report,
    case_deferred: bool,
    ctx: &ChainCtx<'_>,
    cand3_path: &str,
    cand3: &Path,
    scratch_root: &Path,
) -> bool {
    // A fresh socket no daemon serves: `update --server` finds nothing to hand
    // over, swaps the binary at the path, and MUST say so loudly — "it will
    // run the new binary when it next starts" — never pretending a cut
    // happened. That is the deferred rule's Unix shape.
    let fresh = scratch_root.join("deferred.sock");
    let fresh_str = fresh.display().to_string();
    let r = crate::update_slice::run_cli(
        ctx.sandbox,
        ctx.bin_cli,
        &[
            "update", "--server", "--from", cand3_path, "--socket", &fresh_str,
        ],
        CLI_DEADLINE,
    );
    // The operator-facing sentence, not the JSON: the criterion is that the
    // outcome is LOUD — "never a silent fallback" is about what a person sees.
    let loud = r.output.contains("no daemon was serving")
        && r.output.contains(&fresh_str)
        && r.output
            .contains("it will run the new binary when it next starts");
    let installed_3 = same_bytes(ctx.bin_server, cand3).unwrap_or(false);
    // The agents keep running on the serving daemon: the deferred swap
    // replaced the file at the path, never the running processes.
    let r2 = crate::update_slice::run_cli(
        ctx.sandbox,
        ctx.bin_cli,
        &["panes", "--socket", ctx.sock_str],
        CLI_DEADLINE,
    );
    let agents_untouched =
        r2.ok() && (1..=PANES).all(|n| alive_panes(&r2.output).contains(&pane_id(n)));
    let ok = r.ok() && loud && installed_3 && agents_untouched;
    report.check(
        "T-0039's deferred rule on Unix (forced by flag): loud outcome, swap at the path, \
         running agents untouched",
        ok,
        &format!(
            "exited {:?}: {} | installed==cand3: {installed_3} | agents still alive on the \
             serving daemon: {agents_untouched}",
            r.code,
            first_line(&r.output)
        ),
    );
    report.say(if case_deferred {
        "release/chain: `--case windows-deferred` — on Unix the Windows branch is compiled out \
         (cfg(not(unix)) in arreo-cli/src/update.rs, T-0039); the case is exercised here in its \
         Unix-observable shape (loud no-daemon outcome, agents untouched). The real branch runs \
         on the Windows leg of the workflow half (deferred with T-0063) — this never silently \
         substitutes a pass it did not observe."
    } else {
        "release/chain: the Windows `Handoff::Deferred` branch is cfg(not(unix)) (T-0039) — \
         compiled out on this platform; its real run belongs to the Windows leg of the workflow \
         half (deferred with T-0063). What IS exercised here is the rule: never a silent \
         fallback — the no-daemon swap says \"it will run the new binary when it next starts\" \
         and touches not one running agent."
    });
    ok
}

/// One around-read of all panes, with a raw transcript for the evidence file.
struct PaneBatch {
    reads: Vec<PaneRead>,
    raw: String,
}

fn read_all_panes(sandbox: &Sandbox, bin_cli: &Path, sock: &str) -> PaneBatch {
    let mut reads = Vec::with_capacity(PANES);
    let mut raw = String::new();
    for n in 1..=PANES {
        let id = pane_id(n);
        let marker = format!("PANE-{n}-MARKER");
        let r = read_pane(sandbox, bin_cli, &id, sock);
        raw.push_str(&pane_evidence(&id, r.code, &r.output));
        reads.push(parse_pane(&marker, &r.output));
    }
    PaneBatch { reads, raw }
}

/// Does the `metrics history` CSV have at least one data row?
fn series_has_rows(output: &str) -> bool {
    let mut lines = output.lines();
    let header = lines.next().map(|s| s.trim()).unwrap_or_default();
    if !header.starts_with("ts_ms") {
        return false;
    }
    for line in lines {
        if !line.trim().is_empty() {
            return true;
        }
    }
    false
}

/// The alert-ordering stage. Two branches, never a silent one: where cgroup v2
/// delegation exists, a budgeted pane really pressures its guard and the
/// assertions run; where it does not, the daemon's own loud `enforce` error is
/// quoted and the stage is a named skip.
fn alert_ordering_stage(
    report: &mut crate::update_slice::Report,
    sandbox: &Sandbox,
    bin_cli: &Path,
    sock_str: &str,
    sock: &Path,
) {
    let spawn_reply = framed_budgeted_spawn(sock, "alerty");
    if spawn_reply.contains("\"ok\"") || spawn_reply.contains("Ok(") {
        // Delegated box: generate pids pressure and let the sweeper raise the
        // episode.
        let _ = crate::update_slice::run_cli(
            sandbox,
            bin_cli,
            &[
                "spawn",
                "pressurer",
                "/bin/sh",
                "-c",
                "i=0; while [ $i -lt 12 ]; do sh -c 'sleep 60' 2>/dev/null & i=$((i+1)); done; sleep 25",
                "--socket",
                sock_str,
            ],
            CLI_DEADLINE,
        );
        std::thread::sleep(ALERT_SETTLE);
        let r = crate::update_slice::run_cli(
            sandbox,
            bin_cli,
            &["panes", "--socket", sock_str],
            CLI_DEADLINE,
        );
        let alerty = r
            .output
            .lines()
            .find(|l| l.contains("alerty"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        let first_data = r
            .output
            .lines()
            .find(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with("ID")
            })
            .map(|s| s.to_string())
            .unwrap_or_default();
        let sorted_ahead = first_data.contains("alerty");
        let leveled =
            alerty.contains("warn") || alerty.contains("critical") || alerty.contains("breach");
        let audit = crate::update_slice::run_cli(
            sandbox,
            bin_cli,
            &["audit", "--limit", "50"],
            CLI_DEADLINE,
        )
        .output;
        let alert_at = audit.find("enforce.alert");
        let breach_at = audit.find("enforce.breach");
        let ordered = match (alert_at, breach_at) {
            (Some(alert), Some(breach)) => alert < breach,
            _ => false,
        };
        report.check(
            "alert ordering: the alerting pane sorts ahead with its level, and the alert row \
             precedes the breach row",
            sorted_ahead && leveled && ordered,
            &format!(
                "first data line: {first_data:?} | alerty line: {alerty:?} | audit order: \
                 alert@{alert_at:?} breach@{breach_at:?}"
            ),
        );
    } else {
        report.skip(
            "alert ordering: a budgeted pane sorts ahead with its level",
            &format!(
                "no cgroup v2 delegation on this box — the daemon answered {spawn_reply:?} LOUDLY \
                 rather than silently running unbudgeted (T-0019; the enforcement slice's own \
                 environment-aware precedent). The ordering rule is proven on the emit path by \
                 T-0041's daemon.rs unit tests (emit_alert_writes_all_three_doors, \
                 critical-precedes-kill) and is exercised live by this same stage on a delegated \
                 runner (the workflow half records that leg)."
            ),
        );
    }
}

/// The bare budgeted Spawn the alert stage needs, driven through the real
/// protocol the way the enforcement slice drives it (the CLI has no budget
/// flags yet). Returns the debug shape of the daemon's reply.
#[cfg(unix)]
fn framed_budgeted_spawn(socket: &Path, id: &str) -> String {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket) else {
        return "cannot connect to the daemon socket".to_string();
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let hello = arreo_core::proto::Message::Hello {
        v: arreo_core::proto::VERSION,
        client: "release-slice".to_string(),
        wants: vec![arreo_core::proto::VERSION],
    };
    let Ok(encoded) = arreo_core::proto::codec::encode_frame(&hello) else {
        return "cannot encode hello".to_string();
    };
    if stream.write_all(&encoded).is_err() {
        return "cannot write hello".to_string();
    }
    let mut acc = Vec::new();
    let mut chunk = [0u8; 8192];
    for _ in 0..10 {
        let Ok(n) = stream.read(&mut chunk) else {
            return "no welcome".to_string();
        };
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((_, consumed)) = arreo_core::proto::codec::decode_frame(&acc) {
            acc.drain(..consumed);
            break;
        }
    }
    let spawn = arreo_core::proto::Message::Spawn {
        v: arreo_core::proto::VERSION,
        id: id.to_string(),
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "sleep 20".to_string()],
        cols: 80,
        rows: 24,
        memory_max: None,
        pids_max: Some(4),
        kill_on_breach: false,
    };
    let Ok(encoded) = arreo_core::proto::codec::encode_frame(&spawn) else {
        return "cannot encode spawn".to_string();
    };
    if stream.write_all(&encoded).is_err() {
        return "cannot write spawn".to_string();
    }
    for _ in 0..10 {
        let Ok(n) = stream.read(&mut chunk) else {
            return "no spawn reply".to_string();
        };
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((message, _)) = arreo_core::proto::codec::decode_frame(&acc) {
            return format!("{message:?}");
        }
    }
    "no-reply".to_string()
}

/// On a platform with no cgroup guard at all, the budgeted spawn cannot
/// happen; report that through the same loud-skip branch.
#[cfg(not(unix))]
fn framed_budgeted_spawn(_socket: &Path, _id: &str) -> String {
    "enforce failed: no unix cgroup guard on this platform".to_string()
}

/// Copy the chain's client/server pair into `bin/`, plus byte references of
/// what "old" means for the swap assertions.
fn make_chain_bins(
    cli_bin: &Path,
    release_server: &Path,
    bin: &Path,
    bin_cli: &Path,
    bin_server: &Path,
    client_ref: &Path,
    server_ref: &Path,
) -> Result<(), String> {
    fs::create_dir_all(bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    fs::copy(cli_bin, bin_cli).map_err(|e| format!("{}: {e}", bin_cli.display()))?;
    fs::copy(release_server, bin_server).map_err(|e| format!("{}: {e}", bin_server.display()))?;
    fs::copy(cli_bin, client_ref).map_err(|e| format!("{}: {e}", client_ref.display()))?;
    fs::copy(release_server, server_ref).map_err(|e| format!("{}: {e}", server_ref.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared plumbing.
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

/// The binaries the slice drives: the real debug client and the real release
/// daemon. Both are only ever read from; everything the slice swaps is a copy.
fn bins(root: &Path) -> (PathBuf, PathBuf) {
    (
        root.join("target").join("debug").join("arreo"),
        root.join("target").join("release").join("arreo-server"),
    )
}

/// The scratch root: under the build tree (`target/test-scratch/`), not the
/// system temp dir — the four+ binary copies below are big, and a 12 GB
/// tmpfs is not where a build-tree test puts them. `ARREO_E2E_SCRATCH`
/// overrides, for runners that want scratch elsewhere.
fn scratch_root(chain: bool) -> PathBuf {
    let base = std::env::var_os("ARREO_E2E_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target").join("test-scratch"));
    base.join(format!(
        "T-0042-{}-{}",
        if chain { "chain" } else { "release" },
        std::process::id()
    ))
}

/// `cargo build --release -p arreo-server` — the one real build the slice
/// makes, scoped to the single crate the criterion names.
fn build_release_server(report: &mut crate::update_slice::Report) -> bool {
    let start = Instant::now();
    let out = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("-p")
        .arg("arreo-server")
        .current_dir(repo_root())
        .output();
    match out {
        Ok(out) if out.status.success() => {
            report.say(format!(
                "release: `cargo build --release -p arreo-server` ok in {:.1} s",
                start.elapsed().as_secs_f64()
            ));
            true
        }
        Ok(out) => {
            report.check(
                "the release daemon binary builds (`cargo build --release -p arreo-server`)",
                false,
                &format!(
                    "cargo exited {:?} in {:.1} s; stderr tail: {}",
                    out.status.code(),
                    start.elapsed().as_secs_f64(),
                    String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .rev()
                        .take(6)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            );
            false
        }
        Err(e) => {
            report.check(
                "the release daemon binary builds (`cargo build --release -p arreo-server`)",
                false,
                &format!("cargo could not start: {e}"),
            );
            false
        }
    }
}

/// The evidence directory every run writes into.
fn evidence_dir() -> PathBuf {
    repo_root().join(".loop").join("evidence").join("T-0042")
}

fn write_evidence(report: &crate::update_slice::Report, chain: bool, refusal: &str, markers: &str) {
    let dir = evidence_dir();
    let _ = fs::create_dir_all(&dir);
    if chain {
        let _ = fs::write(dir.join("chain-transcript.txt"), report.transcript());
        if !refusal.is_empty() {
            let _ = fs::write(dir.join("chain-foreign-key-refusal.txt"), refusal);
        }
        if !markers.is_empty() {
            let _ = fs::write(dir.join("handoff-markers.txt"), markers);
        }
    } else {
        let _ = fs::write(dir.join("release-transcript.txt"), report.transcript());
        if !refusal.is_empty() {
            let _ = fs::write(dir.join("tampered-refusal.txt"), refusal);
        }
    }
    let _ = fs::write(dir.join("index.md"), evidence_index(chain));
}

/// The claim→artifact index the evidence criterion asks for: which real
/// artifact backs each claim. Written on every run; both runs describe the
/// same complete picture.
fn evidence_index(chain: bool) -> String {
    let this_run = if chain { "chain" } else { "release" };
    format!(
        "# T-0042 evidence index\n\n\
         This run: `cargo xtask e2e --slice {this_run}` (Linux; the per-OS matrix transcripts are \
         the workflow half, deferred with T-0063).\n\n\
         ## Artifacts under test (all real, built or generated by the slice itself)\n\n\
         | Artifact | Role | Claims it backs |\n\
         | --- | --- | --- |\n\
         | `target/release/arreo-server` (via `cargo build --release -p arreo-server`) | the real release daemon binary | criterion 1: the signed copy verifies vs the generated key; the flipped byte is refused; chain: the server candidates swapped through `update --server` |\n\
         | `target/debug/arreo` | the real client binary | criterion 1: product refusal of the tampered file (exit 1, names it); chain: `update --check`, the client atomic swap, the handoffs, the deferred rule, metrics, the alert listing |\n\
         | the throwaway keypair (`target/test-scratch/T-0042-*/keys/`) | generated per run by `minisign -G -W`, never committed, never reused | every signature in both runs; the trust set `TrustSet::parse` verifies against |\n\
         | the `file://` channel (`target/test-scratch/T-0042-*/channel/`) | `arreo-index.json` + `.minisig`, the two artifacts + signatures, `SHA256SUMS` + its signature | chain stages 1–3: index format (no digests), verification via T-0037's shared code, the byte-identical install |\n\
         | the swap proof, byte-for-byte | `installed == verified`, `.prev == replaced` | T-0070's machinery, client and server |\n\
         | the eight counter panes | `/bin/sh` echo-tick loops on the real daemon | the handoff marker-continuity proof (`handoff-markers.txt`) |\n\
         | `update --check` / `update verify` / `update --server` / `metrics history` / `panes` / `audit` | the real product verbs | every chain stage ran these, not a lookalike |\n\n\
         ## Where each acceptance claim is proven\n\n\
         - **criterion 1 (sign→verify→refuse):** `release-transcript.txt` + `tampered-refusal.txt` — the `verify_with` BadSignature sentence and the product binary's exit-1 refusal, both naming the artifact.\n\
         - **criterion 2 (one-command chain):** `chain-transcript.txt` — one pass/fail line per stage; `handoff-markers.txt` carries the per-pane before/after counts across the 8-pane cut.\n\
         - **criterion 3 (transport-agnostic):** chain stage 2 — the channel drove T-0037's `check`/`fetch` (the same functions an https URL runs; the fetcher is the only divergence, proven unit-side by `file_and_https_channels_differ_only_in_their_fetcher`).\n\
         - **criterion 6 (evidence):** this index + the transcripts above. \"One transcript per OS\" is the CI-matrix half — deferred with T-0063; this run is the Linux transcript.\n\
         - **criterion 7 (hermetic):** the whole slice ran with no network, no published release, no real signing secret; scratch under `target/test-scratch/`, removed on exit and asserted removed.\n\
         - **deferred path:** the Windows branch is `#[cfg(not(unix))]` (T-0039) — compiled out on Unix; the chain's deferred stage shows the Unix-observable loud outcome and names the Windows leg as the real branch's owner.\n\
         - **honest gap (T-0036):** no Apple notarization or Authenticode proof anywhere in either run — no paid identity exists yet; a green chain implies nothing about installers.\n\n\
         ## CI-matrix wiring (reported as text, not applied)\n\n\
         `.github/workflows/**` is the user's during T-0063, so the matrix rows and PR/nightly split \
         are delivered as `ci_text` in the worker report, to be applied once T-0063 settles.\n"
    )
}
