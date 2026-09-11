//! T-0043 acceptance tests: the machine directory's schema, name rules,
//! concurrency, rename atomicity, tombstones and consistency.
//!
//! Every test drives the real `Directory` over a real SQLite store (a file for
//! the concurrency and restart cases, memory elsewhere), because the properties
//! under test are properties of the *transactions* — an in-process mock would
//! assert nothing about the thing that can actually go wrong.

use arreo_core::identity::{DeviceKey, RootKey, VerifyingKey};
use arreo_core::mesh::directory::{
    DirectoryError, MachineId, Name, Presence, STALE_AFTER_SECS, TOMBSTONE_SECS,
};
use arreo_relay::directory::{Directory, DirectoryFailure, JoinTicket};
use arreo_relay::store::RelayStore;
use std::sync::Arc;

const ACCOUNT: &str = "acct-1";

/// The dial key a test machine claims. Any 64 hex characters: the *directory*
/// stores it verbatim — the relay is what verifies a real one, from the
/// certificate, in `router.rs`.
const DIAL_KEY: &str = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";

fn directory() -> Directory {
    let store = RelayStore::open_memory().expect("in-memory store");
    let directory = Directory::new(store);
    let root = RootKey::generate().expect("entropy");
    directory
        .create_account(ACCOUNT, &root.public(), 1_000)
        .expect("account");
    directory
}

fn machine() -> MachineId {
    let key = DeviceKey::generate().expect("entropy");
    machine_of(&key.public())
}

fn machine_of(key: &VerifyingKey) -> MachineId {
    MachineId::from_key(key)
}

fn name(text: &str) -> Name {
    Name::parse(text).expect("valid name")
}

fn join(
    directory: &Directory,
    machine: &MachineId,
    requested: &str,
    now: i64,
) -> arreo_core::mesh::MachineRow {
    let ticket = JoinTicket::issue(ACCOUNT, now);
    directory
        .join(ticket, machine, &name(requested), 1, DIAL_KEY, now)
        .expect("join")
}

/// The schema is metadata only, and the test fails if a column ever suggests
/// otherwise — that is the criterion, expressed as an assertion rather than a
/// review habit.
#[test]
fn the_schema_holds_directory_metadata_and_nothing_else() {
    let directory = directory();
    let machine_columns = directory.store().columns("machine").expect("columns");
    assert_eq!(
        machine_columns,
        vec![
            "machine_id",
            "account_id",
            "name",
            "name_key",
            "presence",
            "last_seen_ms",
            "proto_version",
            "tombstone_until_ms",
            "name_conflict",
            // v4 (T-0045) added the key a peer dials to reach this machine's
            // daemon. Metadata by the same argument the account's root key gets: a
            // **public** key, not a secret, and one a client in the account must
            // know to open a session — it is the Noise identity the handshake
            // proves, and the target's own trust ledger (T-0046) is what decides
            // whether the caller gets anywhere. Nor is it self-reported: the relay
            // writes it from the certificate that authenticated the session.
            "daemon_key",
        ],
        "the machine table's columns are a contract: adding one is a decision, not a detail"
    );
    // v2 (T-0029) added the account's root public key: the anchor a device
    // certificate is verified against. Still metadata — a public key, not a
    // secret — and the denylist below still applies to it.
    let account_columns = directory.store().columns("account").expect("columns");
    assert_eq!(
        account_columns,
        vec!["account_id", "created_at_ms", "root_key"]
    );

    // Nothing that would put a secret, an agent's state, or a device grant in
    // the relay. The column set above is the strong check; this is the readable
    // statement of *why* it is that set.
    for column in machine_columns.iter().chain(account_columns.iter()) {
        for forbidden in [
            "secret",
            "private",
            "token",
            "password",
            "grant",
            "agent",
            "prompt",
            "scrollback",
            "key_pem",
        ] {
            assert!(
                !column.contains(forbidden),
                "column {column:?} suggests the relay is storing {forbidden:?}"
            );
        }
    }
}

/// Joining is explicit: a machine enters only through a spent, unexpired ticket.
#[test]
fn a_machine_enters_only_through_a_live_join_ticket() {
    let directory = directory();
    let machine = machine();

    // Expired: refused.
    let expired = JoinTicket::issue(ACCOUNT, 0);
    let refused = directory.join(expired, &machine, &name("workbox"), 1, DIAL_KEY, 600_000);
    assert!(
        matches!(
            refused,
            Err(DirectoryFailure::Rule(DirectoryError::Ticket {
                state: "expired"
            }))
        ),
        "an expired ticket must not admit a machine: {refused:?}"
    );
    assert!(directory.list(ACCOUNT, 600_000).expect("list").is_empty());

    // A ticket for an account that does not exist: refused, and no row appears.
    let ghost = JoinTicket::issue("nobody", 0);
    assert!(matches!(
        directory.join(ghost, &machine, &name("workbox"), 1, DIAL_KEY, 0),
        Err(DirectoryFailure::NoSuchAccount(_))
    ));
    assert!(directory.list(ACCOUNT, 0).expect("list").is_empty());

    // A live ticket: admitted.
    join(&directory, &machine, "workbox", 1_000);
    assert_eq!(directory.list(ACCOUNT, 1_000).expect("list").len(), 1);
}

/// A second machine claiming a live name is admitted under a deterministic
/// suffix and flagged — never silently renamed, never rejected.
#[test]
fn a_name_conflict_suffixes_and_says_so() {
    let directory = directory();
    let first = machine();
    let second = machine();
    let third = machine();

    let a = join(&directory, &first, "workbox", 1_000);
    assert_eq!(a.name.as_str(), "workbox");
    assert!(!a.name_conflict, "the first claim is not a conflict");

    let b = join(&directory, &second, "workbox", 1_000);
    assert_eq!(b.name.as_str(), "workbox-2");
    assert!(b.name_conflict, "the loser of a claim must be told");

    let c = join(&directory, &third, "WORKBOX", 1_000);
    assert_eq!(
        c.name.as_str(),
        "workbox-3",
        "case is normalized before uniqueness, so WORKBOX is the same name"
    );
    assert!(c.name_conflict);

    // Every machine is present; nobody was rejected.
    let rows = directory.list(ACCOUNT, 1_000).expect("list");
    assert_eq!(rows.len(), 3);
}

/// The serialization criterion: N concurrent claims of one name yield exactly
/// one plain name and N−1 suffixed names.
#[test]
fn concurrent_claims_serialize_into_one_plain_name_and_n_minus_one_suffixed() {
    const CLAIMANTS: usize = 8;
    let dir = std::env::temp_dir().join(format!("arreo-relay-directory-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let path = dir.join("relay.db");

    // One store per thread, so the serialization under test is SQLite's (the
    // file lock), not a mutex we hold in this process.
    let store = RelayStore::open(&path).expect("store");
    let root = RootKey::generate().expect("entropy");
    Directory::new(store)
        .create_account(ACCOUNT, &root.public(), 0)
        .expect("account");

    let barrier = Arc::new(std::sync::Barrier::new(CLAIMANTS));
    let mut handles = Vec::new();
    for _ in 0..CLAIMANTS {
        let barrier = Arc::clone(&barrier);
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            let store = RelayStore::open(&path).expect("store");
            let directory = Directory::new(store);
            let machine = machine();
            let ticket = JoinTicket::issue(ACCOUNT, 0);
            // All threads reach the claim together, so the transaction is what
            // separates them.
            barrier.wait();
            directory
                .join(ticket, &machine, &name("workbox"), 1, DIAL_KEY, 1_000)
                .expect("every claimant is admitted, none rejected")
                .name
                .as_str()
                .to_string()
        }));
    }
    let mut names: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    names.sort();

    let mut expected = vec!["workbox".to_string()];
    for ordinal in 2..=CLAIMANTS {
        expected.push(format!("workbox-{ordinal}"));
    }
    expected.sort();
    assert_eq!(
        names, expected,
        "exactly one plain name and N-1 suffixed ones, with no duplicates"
    );

    // And the result is stable: re-reading the directory gives the same set.
    let store = RelayStore::open(&path).expect("store");
    let reread = Directory::new(store);
    let mut persisted: Vec<String> = reread
        .list(ACCOUNT, 1_000)
        .expect("list")
        .into_iter()
        .map(|row| row.name.as_str().to_string())
        .collect();
    persisted.sort();
    assert_eq!(
        persisted, expected,
        "the assignment is durable, not incidental"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rename is atomic and identity-preserving; a collision is refused and leaves
/// the old name intact.
#[test]
fn rename_is_atomic_and_refuses_a_collision_without_partial_state() {
    let directory = directory();
    let first = machine();
    let second = machine();
    join(&directory, &first, "alpha", 1_000);
    join(&directory, &second, "beta", 1_000);

    // Renaming onto a live name: refused, old name untouched.
    let refused = directory.rename(&second, &name("alpha"), 2_000);
    assert!(
        matches!(
            refused,
            Err(DirectoryFailure::Rule(DirectoryError::NameTaken(_)))
        ),
        "a colliding rename must be refused: {refused:?}"
    );
    assert_eq!(
        directory
            .resolve(ACCOUNT, &name("beta"))
            .expect("resolve")
            .as_ref(),
        Some(&second),
        "the old name must survive a refused rename"
    );

    // A successful rename preserves identity and clears a stale conflict flag.
    let renamed = directory
        .rename(&second, &name("beta-prime"), 2_000)
        .expect("rename");
    assert_eq!(renamed.machine_id, second, "identity never moves");
    assert_eq!(renamed.name.as_str(), "beta-prime");
    assert!(!renamed.name_conflict);
    assert!(directory
        .resolve(ACCOUNT, &name("beta"))
        .expect("resolve")
        .is_none());
}

/// A removed name is held for the machine that had it: nobody else may take it,
/// and the machine itself gets it back.
#[test]
fn a_tombstone_holds_the_name_for_its_machine() {
    let directory = directory();
    let owner = machine();
    let stranger = machine();
    join(&directory, &owner, "workbox", 1_000);

    let removed = directory.remove(&owner, 2_000).expect("remove");
    assert!(removed.tombstone_active(2_000), "the tombstone is set");

    // Another machine cannot take the name while the tombstone holds.
    let taken = join(&directory, &stranger, "workbox", 2_000);
    assert_eq!(
        taken.name.as_str(),
        "workbox-2",
        "the tombstoned name is held"
    );

    // The machine that had it gets it back, and the tombstone clears.
    let returning = join(&directory, &owner, "workbox", 3_000);
    assert_eq!(
        returning.name.as_str(),
        "workbox",
        "the owner keeps its name"
    );
    assert!(
        returning.tombstone_until_ms.is_none(),
        "a rejoin clears the tombstone"
    );
}

/// ...and after the tombstone expires, the name is free for anyone.
///
/// This is a separate scenario from the one above on purpose: a machine that
/// *reclaims* its name leaves a live row holding it, so the two cannot both be
/// observed in one directory.
#[test]
fn an_expired_tombstone_releases_the_name() {
    let directory = directory();
    let owner = machine();
    join(&directory, &owner, "workbox", 1_000);
    directory.remove(&owner, 2_000).expect("remove");

    // The tombstone holds until its deadline: at `until - 1` the name is taken.
    // (`MachineRow::tombstone_active` is `until > now`, so the deadline itself is
    // the first free instant — the boundary is stated once, in core, and this
    // asserts the store agrees with it.)
    let until = 2_000 + (TOMBSTONE_SECS as i64) * 1000;
    let early = machine();
    let held = join(&directory, &early, "workbox", until - 1);
    assert_eq!(
        held.name.as_str(),
        "workbox-2",
        "held until the instant it expires"
    );

    // At the deadline, it is free for a machine that has no row yet.
    let latecomer = machine();
    let freed = join(&directory, &latecomer, "workbox", until);
    assert_eq!(
        freed.name.as_str(),
        "workbox",
        "an expired tombstone frees the name"
    );
    assert!(!freed.name_conflict);
}

/// Presence comes from the one stated rule, and `remove --stale` prunes exactly
/// that set, idempotently.
#[test]
fn stale_is_pruned_from_the_one_presence_rule_and_is_idempotent() {
    let directory = directory();
    let live = machine();
    let gone = machine();
    join(&directory, &live, "live", 1_000);
    join(&directory, &gone, "gone", 1_000);

    // `live` heartbeats now; `gone` was last seen in 1970, which is past the
    // stale threshold — the rule is the same one every reader sees.
    let now = 1_000 + (STALE_AFTER_SECS as i64 + 1) * 1000;
    directory.heartbeat(&live, now).expect("heartbeat");
    let rows = directory.list(ACCOUNT, now).expect("list");
    assert_eq!(
        rows.iter()
            .find(|r| r.machine_id == live)
            .expect("live row")
            .presence,
        Presence::Online
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.machine_id == gone)
            .expect("gone row")
            .presence,
        Presence::Stale,
        "a machine last seen in 1970 is stale, not merely offline"
    );

    let pruned = directory.remove_stale(ACCOUNT, now).expect("prune");
    assert_eq!(pruned, vec![gone.clone()], "exactly the stale set");
    assert_eq!(
        directory.remove_stale(ACCOUNT, now).expect("prune again"),
        Vec::<MachineId>::new(),
        "a second prune removes nothing"
    );
    assert!(directory
        .resolve(ACCOUNT, &name("live"))
        .expect("resolve")
        .is_some());
}

/// Add → rename → remove → re-add of the same machine round-trips byte-identically.
#[test]
fn add_rename_remove_readd_leaves_a_directory_that_round_trips() {
    let directory = directory();
    let machine = machine();
    join(&directory, &machine, "workbox", 1_000);
    directory
        .rename(&machine, &name("workbox-prime"), 2_000)
        .expect("rename");
    directory.remove(&machine, 3_000).expect("remove");
    // The same machine rejoins: it keeps its name, and the tombstone clears.
    let readded = join(&directory, &machine, "workbox-prime", 4_000);
    assert_eq!(readded.name.as_str(), "workbox-prime");
    assert!(readded.tombstone_until_ms.is_none());

    let export = directory.export(ACCOUNT, 4_000).expect("export");
    let parsed = arreo_core::mesh::directory::parse_export(&export).expect("our export parses");
    assert_eq!(
        arreo_core::mesh::directory::export_rows(&parsed),
        export,
        "the sorted export must round-trip byte-identically"
    );
    // No orphaned tombstone is left behind by the re-add.
    assert!(
        parsed.iter().all(|row| row.tombstone_until_ms.is_none()),
        "a re-added machine leaves no tombstone: {parsed:?}"
    );
}

/// Discovery may locate a known machine and may never invent one.
#[test]
fn discovery_resolves_known_machines_and_cannot_create_them() {
    let directory = directory();
    let known = machine();
    let stranger = machine();
    join(&directory, &known, "workbox", 1_000);

    assert_eq!(
        directory.resolve_known(ACCOUNT, &known).expect("known"),
        Some(known.clone())
    );
    assert_eq!(
        directory
            .resolve_known(ACCOUNT, &stranger)
            .expect("unknown"),
        None,
        "a machine that never joined is not in the directory"
    );
    assert_eq!(
        directory.list(ACCOUNT, 1_000).expect("list").len(),
        1,
        "resolution never creates a row"
    );
    // Renaming through the discovery door is not possible: the call takes an id,
    // and the only rename entry point takes a name plus a machine already known.
    assert_eq!(
        directory
            .resolve(ACCOUNT, &name("workbox"))
            .expect("resolve"),
        Some(known)
    );
}
