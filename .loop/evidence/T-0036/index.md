# T-0036 evidence index — signed releases (2026-09-13, linux x86_64)

Each acceptance criterion, with the artifact that fails without it.

| # | Claim | Artifact |
| --- | --- | --- |
| 1 | Typed errors per failure mode; tamper refusal asserted as `BadSignature` naming the file, against a locally generated throwaway keypair | `tests-update-verify.log` (`one_flipped_byte_is_refused_as_a_bad_signature_naming_the_file`, `a_signature_from_another_key_is_refused_by_key_id`, `a_missing_signature_is_refused_and_names_what_it_looked_for`, `an_unreadable_artifact_is_an_io_refusal`, `an_artifact_the_manifest_does_not_describe_is_refused`); the keypair is generated inside the test (`getrandom` seed + `blake2` prehash), written to a per-test temp dir that removes itself, and never committed or reused. Removing the flip assertion turns the test red. |
| 2 | A valid signature verifies (the negative tests mean nothing without it) | `tests-update-verify.log` (`a_signed_fixture_verifies_and_reports_its_key_id_and_digest`, `real_minisign_agrees_with_this_files_fixtures`) |
| 3 | Key id checked, not just signature validity: unknown id → `UnknownKeyId`; a *different* throwaway key's valid signature refused; the same bytes verify against the stranger's own key | `tests-update-verify.log` (`a_signature_from_another_key_is_refused_by_key_id`, `a_two_key_trust_set_accepts_either_key_and_refuses_a_stranger`, `a_valid_signature_over_a_different_file_is_refused`) |
| 4 | `arreo update verify` — exit 0 naming artifact/key id/digest; exit 1 on refusal; exit 2 usage; no bypass flag | `cli-door.log` (refusals + `--force` refused as an unknown flag, through the real binary); `release-simulation.log` (exit 0 path, through a real build whose pinned key is a throwaway — the product binary only ever trusts its compiled-in key, so the success path cannot be exercised against the operator's key). |
| 5 | Supply chain green with the new dependency | `tests-and-supply-chain.log`: `cargo vet --locked` (Vetting Succeeded, 337 exempted — `minisign-verify` 0.2.5 exempted with justification in `docs/release.md` "Supply chain"), `cargo audit` (0 vulnerabilities, 344 deps), `cargo deny check` (advisories ok, bans ok, licenses ok, sources ok); `package-dry-run.log` (the `[PASS]` rows for the pinned trust set and the release-job structure, green on every PR) |
| 6 | `docs/release.md` states the decision (minisign over keyless Sigstore + reason), the honest gap (no notarization/Authenticode, tag job never run with a real key), and rotation as a two-key trust set (current + next) | `docs/release.md` ("The trust decision", "What a signature proves and what it does not", "Rotation: a two-key trust set", the status blockquote) — the old "artifacts are UNSIGNED" step is replaced, and the still-unproven half (the release job running with a real key) is written rather than implied |

## How the throwaway-key refusal was made real

The workspace ships no signing code (that is the point), so the test writes
minisign signatures itself — ed25519 over the BLAKE2b-512 prehash plus the global
signature over `signature || trusted comment` — using only crates already in the
graph (`ed25519-dalek`, `blake2`, `base64`). The fixtures are proved to be the
real format by `real_minisign_agrees_with_this_files_fixtures`, which signs and
verifies in both directions with the `minisign` binary on PATH (and self-skips
with a loud note where it is absent, per the T-0019 no-delegation precedent).

## What only CI can prove

- The tag job running with the **real** `MINISIGN_SECRET_KEY` (no tag has been
  pushed; the workflow is machine-checked structurally on every PR, but its
  runtime behavior against the real key is unproven — `docs/release.md` says so).
- Runs on macOS/Windows runners (the simulate-here leg was Linux).