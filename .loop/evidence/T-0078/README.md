T-0078: who may read the daemon's state and connect to its socket
================================================================

Policy (SECURITY.md, "File permissions"): owner-only by default — 0600 for
files, 0700 for directories — applied at creation and re-applied at every
open. Files: `<socket>`, `<socket>.db` (+ `-wal`/`-shm`), `<socket>.lock`,
`<socket>.handoff`. The default socket lands in /tmp when XDG_RUNTIME_DIR is
unset, so the file mode is the only control there.

Failing-first proof (each test was run against the pre-fix code — the three
fix sites commented out — and went red, then green with the fix):

1. crates/arreo-server/tests/file_modes.rs — real daemon under `umask 0002`,
   pane output written to the store, all five files asserted exactly 0600.

   Pre-fix (command: the fix sites in daemon.rs/store.rs/lock.rs commented):
   ```
   thread 'every_file_the_daemon_creates_is_owner_only' panicked at
   crates/arreo-server/tests/file_modes.rs:188:5:
   assertion `left == right` failed: socket "/tmp/arreo-modes-modes-2334862-ThreadId(2).sock"
   is 775, must be 0600 — it holds the pane's output "ARREO-MODE-TOKEN-2334862",
   so every local user could read it
     left: 509
    right: 384
   ```
   The assertion order is socket, store, lock; the store would have failed
   next at 644, then the lock at 664 — the modes the task measured. The
   failure message reads pane content out of the file, not a bare mode.

   Post-fix:
   ```
   running 1 test
   test every_file_the_daemon_creates_is_owner_only ... ok
   ```

   Manual stat of a live daemon under umask 0002 (stat-evidence.sh):
   ```
   600 /tmp/arreo-ev-vlql.sock
   600 /tmp/arreo-ev-vlql.sock.db
   600 /tmp/arreo-ev-vlql.sock.lock
   ```
   The -wal/-shm sidecars are recreated per store open and deleted with the
   last connection, so the test asserts their re-application: loosen to 0644,
   trigger the daemon's next open, require 0600 again.

2. crates/arreo-core/tests/store.rs `an_existing_loose_store_is_tightened_by_the_next_open`
   — an existing install is fixed: files widened to 0644 are re-chmodded by
   the next open.

   Pre-fix:
   ```
   assertion `left == right` failed: store must be owner-only (0600) after the next open, was 644
     left: 420
    right: 384
   ```

3. crates/arreo-core/src/lock.rs `a_new_lock_file_is_owner_only` /
   `acquire_tightens_an_existing_loose_lock_file`.

Scoped validation: cargo test -p arreo-core -p arreo-server (all targets),
cargo clippy --all-targets -- -D warnings, cargo fmt — all green.