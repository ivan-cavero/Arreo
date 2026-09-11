# ADR 0012: the machine directory — metadata at the relay, names that resolve anyway

- Status: accepted (2026-09-11, T-0043)
- Context: ROADMAP §3.7 asks the account to own *machines*, not just panes:
  a name per host, presence, and a directory that `arreo machines` (T-0044),
  cross-server attach (T-0045) and per-machine trust (T-0046) all stand on. Two
  constraints shape every choice. First, names must resolve **with the relay
  unreachable** — the whole point of a local-first runtime is that the relay is
  an accelerant, not a dependency. Second, the relay is the AGPL, self-hostable
  component and must stay **metadata-only**: a self-hosted relay is run by the
  user, and a user-run box must never hold another user's traffic or keys.
- Decision: the directory is a **relay-side SQLite registry** (`account`,
  `machine`) whose rows are metadata only — identity fingerprint, name, presence,
  last-seen, protocol version, tombstone — and every server keeps a **read-only
  mirror** (`arreo_core::mesh::DirectoryCache`) that is replaced wholesale by
  what the relay last said and stamped with `as_of`. Membership is **explicit**:
  the only insert path is `Directory::join` with a spent, unexpired single-use
  `JoinTicket` (5 minutes). Names are ASCII `[a-z0-9][a-z0-9-]{0,31}`,
  case-normalized, with `UNIQUE(account_id, name_key)` as the hard backstop; a
  second claim of a live name is admitted under a deterministic suffix
  (`workbox-2`) and flagged `name_conflict`. Removal is a **30-day tombstone**,
  not a delete.
- Why this one:
  - **Metadata at the relay, names resolvable locally.** A pure relay-side
    directory would make the relay a hard dependency for the most common action
    in the product (attach to my own machine). A pure server-side directory would
    give every machine its own truth, and "which machines exist" would depend on
    who you asked. The split — relay decides, servers remember, every answer
    labeled with when it was learned — is the only shape where both statements
    are true.
  - **Read-only is a type, not a policy.** The risk with a cache is that it
    starts acting like a source of truth: a stale copy renames a machine, or
    resurrects one that was removed. So `DirectoryCache` has exactly one mutator
    (`mirror`, which replaces the whole set) and deliberately no `insert`,
    `rename` or `remove`. The invariant is the absence of an API.
  - **Explicit join over discovery.** LAN mDNS is genuinely useful for finding
    *where* a known machine is, and catastrophic as a registry: a hostile home
    LAN could then create or rename machines. So `resolve_known` takes a
    `machine_id` and returns `None` for anything not already in the directory —
    there is no name argument, so discovery cannot invent an entry even by
    accident.
  - **A tombstone, not a delete.** Deleting a row frees its name instantly,
    which means a machine that reboots after a network blip can come back to find
    its name taken by somebody else. Holding the name for the machine that had it
    for 30 days makes "my machine is `workbox`" survive an outage; expiry then
    makes the name reusable, so the directory cannot accumulate permanent
    claims from machines that no longer exist.
  - **Reject non-ASCII names rather than normalize them.** The rule admits ASCII
    only, so Unicode NFC is the identity over every name it accepts; validating
    the casefolded form is therefore equivalent to normalizing first. Rejecting
    (`Name::parse` refuses `wоrkbox` with a Cyrillic `о`) is *stronger* than
    normalizing, because a confusable can never enter the directory to collide
    with an existing machine — and it costs no dependency, which adding
    `unicode-normalization` for a no-op would have.
  - **`BEGIN IMMEDIATE` for claims.** The suffix scan is a read-then-write, and
    two machines claiming one name at the same instant must not both read
    "free". Taking the write lock up front serializes them; the loser waits and
    then observes the winner's row, so N concurrent claims produce exactly one
    plain name and N−1 suffixed ones — asserted by a test that runs the claims
    from separate threads against a file-backed store.
- Alternatives rejected:
  - **mDNS as the registry** (§3.4's tempting shortcut): no authority, no
    revocation, and a hostile LAN can inject names. Kept as address resolution
    for known machines only.
  - **Directory in the server**: one authority every machine would have to agree
    on, which is the relay's job, and it would not survive the relay being
    reachable from a machine that has never met the others.
  - **Device grants in the directory**: T-0046 keeps trust per-machine; putting a
    grant here would make the relay the arbiter of who may use which machine, and
    a self-hosted relay would hold authority it should not.
  - **A second SQLite style for the relay**: `arreo-core`'s session store already
    fixed the WAL + versioned-migration shape (T-0018). The relay reuses it
    (`RelayStore` is the single connection and migration owner) rather than
    inventing a parallel convention that would drift.
  - **Hard delete on removal**: see "a tombstone, not a delete".
- Known trade-offs, stated rather than hidden:
  - **Presence is relay-observed only.** No NAT probing in v1, so a machine
    behind a router that drops idle UDP looks offline until its next heartbeat.
    The thresholds (30 s heartbeat, online ≤ 90 s, stale > 30 days) are fixed here
    as the contract; T-0031 consumes this rule rather than re-deriving one.
  - **A machine that never returns ages out only via the stale/tombstone
    policy.** There is no admin "force release" in v1.
  - **The stale threshold and the tombstone length are both 30 days**, so a name
    becomes reclaimable at about the same moment its owner is pruned. That is
    deliberate but worth re-examining if real deployments show machines that
    return after a month.
  - **The `name_key` column is nullable**, which is what makes expiry work: a
    live row and an unexpired tombstone hold the casefolded name, an expired
    tombstone holds `NULL` (SQLite treats `NULL`s as distinct, so any number of
    released rows coexist). A reader that assumes `name_key = casefold(name)`
    would be wrong for expired rows; the column's job is stated in the schema.

## Consequences

- `arreo-relay` gains SQLite (`rusqlite`, the same version and bundled build the
  session store uses) and the single `RelayStore` connection/migration owner that
  the inbox (T-0030) and presence (T-0031) register their tables with.
- `arreo-core::mesh` holds the vocabulary — identity, name rules, presence
  thresholds, the read-only cache — so the AGPL relay and the non-AGPL server
  agree on what a name is without the server linking the relay.
- The canonical export (`export_rows`/`parse_export`) is sorted and tab-separated
  so two runs over one directory can be compared byte-for-byte; the consistency
  check depends on that, not on a hash.
