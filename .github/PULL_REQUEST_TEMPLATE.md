<!-- Fill the top, tick what is true, delete what does not apply. Short beats complete. -->

## What changed, and why

<!-- One paragraph. If it needs three, the PR is probably two PRs (CONTRIBUTING: one concern, ~400 lines). -->

## Proof

**e2e slice(s) that cover this** — paste the PASS lines, and name the slice explicitly (the full list is in `AGENTS.md`):

```console
cargo xtask e2e --slice <chaos|api|compat|state|lifecycle|enforcement|persistence|tui|theme|relay>
```

> `cargo xtask e2e` with no `--slice` prints a stub and exits 0. It is not evidence.

- Slice(s) run:
- OS(es) you ran them on: <!-- Linux / macOS / Windows; the CI matrix is the authority for the other two. -->
- `cargo xtask bench` delta, if this touched anything hot (attach + RSS budgets are executable, not advisory):

## Checklist

- [ ] `cargo test --workspace` is green
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` reports zero warnings
- [ ] `cargo fmt --all -- --check` is clean
- [ ] New behavior has a test that **fails without this change** (strict TDD; "it compiles" is not a test)
- [ ] No secrets in the diff — no keys, tokens or credentials, not even "test" ones
- [ ] Decision recorded in `specs/adr/` if this changes a design — or the PR says why no ADR is needed
- [ ] Every commit is DCO-signed off (`git commit -s`)
- [ ] If this touches `crates/arreo-relay*` (AGPL): second approval requested per CONTRIBUTING
- [ ] New dependency? Rationale recorded per AGENTS.md "Dependencies" — or: no new dependency
