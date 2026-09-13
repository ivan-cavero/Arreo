# T-0037 evidence index — the channel (2026-09-13, linux x86_64)

Each acceptance criterion, with the artifact that fails without it. The channel
half of the client update: a URL + the T-0036 verifier, so `--check` and the
anonymous `arreo update` report or refuse — never guess.

| # | Claim | Artifact |
| --- | --- | --- |
| 1a | `--check` against a signed `file://` index reports the newest version + artifact | `core-tests.log` (`a_signed_index_reports_the_newest_version_and_its_artifact`, 15/15 channel tests green); `slice-transcript.txt` ("a signed index is fetched, verified and reports its version and artifact" PASS, and the CRITICAL end-to-end one: "the verified artifact installs through the same path --from uses" PASS, 27/27); `release-simulation.log` steps 1–3 — a real **product binary rebuilt with a throwaway pinned key** prints `version: 0.1.0`, `artifact: arreo-sim`, the key id `11A0D55DC921BBEC` and exits 0. The product binary only ever trusts the key compiled into it, so this is the one path that cannot run in a test — the same call T-0036 made and documented. |
| 1b | `--check` against a tampered index refuses with the verifier's sentence, naming the file | `release-simulation.log` step 6 (one flipped byte, the valid signature carried over: `signature does not authenticate this file`, `exit=1`); `core-tests.log` (`a_tampered_index_is_refused_as_a_bad_signature_naming_the_file`, `one_flipped_byte` refusal asserted as `BadSignature`); `cli-door.log` (`check_refuses_an_index_whose_signature_is_not_a_signature`); `slice-transcript.txt` ("a signature that is not a signature is refused as a bad one"). |
| 1c | Empty channel → "no releases yet", exit 0 | `release-simulation.log` step 9 (`no releases yet at …`, `exit=0`, `--json` `"available":false`); `cli-door.log` (`check_reports_no_releases_yet_on_an_empty_channel`); `slice-transcript.txt` ("an empty channel reports \"no releases yet\" with exit 0"). |
| 2 | The anonymous update fetches, verifies, stages, and hands a verified artifact to the existing install path; a tampered artifact is refused before anything is installed | `release-simulation.log` steps 4–7: the anonymous update installs through the product binary (`installed …`, `previous kept at … .prev`, byte-identical, runs, `exit=0`), the tampered channel is refused before installing (`exit=1`, binary "not replaced"); `slice-transcript.txt` ("the anonymous update refuses before it installs anything", "the verified artifact installs through the same path --from uses"); `core-tests.log` (`a_signed_artifact_is_fetched_verified_and_made_runnable`, `a_tampered_artifact_is_refused_as_a_bad_signature`, `an_artifact_with_no_signature_is_refused`); `cli-door.log` (`the_anonymous_update_refuses_before_installing_anything`: exit 1, binary byte-identical, nothing staged). |
| 3 | `file://` and `https://` share every line of code except the fetcher, stated and proven | `core-tests.log` (`file_and_https_channels_differ_only_in_their_fetcher`: one channel's bytes served under both schemes through one `check`, same file names requested, same release returned; `the_transport_is_chosen_by_scheme_alone`); the slice runs every channel case over `file://` with no network, which is the same URL machinery an `https://` channel uses (`update::channel::Channel`, `Fetcher` — one dispatch, two transport arms). |
| 4 | Refusals are the verifier's typed errors verbatim | Every refusal above prints the verifier's own sentence — `no signature at … — refusing an unverified artifact`, `signature does not authenticate this file (…)`, `signed by key X, this build trusts Y` — because `channel::Error::Verify` wraps `verify::Error` with `#[from]` and `#[error("{0}")]`. Asserted in `core-tests.log` against the typed variants; the CLI and slice assert the printed sentences, byte for byte, and the CLI adds only the channel URL as a second line. |
| 5 | The update slice is the one story, extended in place; scoped tests/clippy/fmt green | `slice-transcript.txt` (27 passed, 0 skipped, 0 failed, including the new channel cases — no second slice; a second slice for one story was called a defect and avoided); `core-tests.log` + `cli-door.log`; scoped `cargo clippy … --all-targets -- -D warnings` clean on arreo-core/arreo-cli/xtask, `cargo fmt` clean, `cargo check -p arreo-core --no-default-features --all-targets` clean (the T-0010 lite gate). The signed cases in the slice need the real `minisign` binary and would skip loudly without it (T-0019 precedent) — here they ran; CI installs nothing and will report those two rows as skips, with the deterministic `ed25519-dalek` writer in `update/channel.rs`'s tests covering the same accept path. |

## The pinned-key boundary, stated rather than hidden

`update/channel.rs` is generic over `verify::TrustSet`; the CLI passes
`TrustSet::pinned()`, the key compiled into the binary. No test or slice holds the
secret of the pinned key (it lives only in the CI secret store), so the *accept*
path through the shipped binary cannot be exercised on a developer machine or in
CI — exactly the reason `verify_with` exists and the reason T-0036's own evidence
rebuilt a workspace copy with a throwaway key. T-0037 does the same:

- **in-process**: `channel`'s tests sign a fixture with a throwaway keypair and
  check against its public half (deterministic, no external tool);
- **through the real binary**: `simulate.sh` + `release-simulation.log` rebuild a
  copy of this checkout with a throwaway key swapped into `supply-chain/arreo.pub`
  and drive `--check` and the anonymous update for real (steps 1–9);
- **in the slice**: the accept path runs against the fixture's trust set with the
  real channel code, and the verified artifact is then installed through the real
  `--from` path (the "hands to the existing install path without duplicating it"
  criterion, end to end).

The one thing no capture can show is the shipped binary accepting a *real*
release before one exists — which is the empty channel, documented as the honest
default.

## Re-running

- Core + CLI tests: `cargo test -p arreo-core --lib update::` and
  `cargo test -p arreo-cli --test update` (no network; `file://` channels in a
  temp dir; the tests set `ARREO_CHANNEL_URL` to an empty local channel so a
  default-channel slip cannot dial GitHub).
- Slice: `cargo xtask e2e --slice update` (the channel cases are rows 8a–8g of
  the update slice; needs `minisign` only for the two signed rows, loud skip
  otherwise).
- End-to-end accept path through a product binary: `bash simulate.sh` with the
  prerequisites printed at the top of the script.