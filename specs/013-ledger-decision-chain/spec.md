---
id: "013-ledger-decision-chain"
title: "The decision chain: hash-linked records, CAS append, verification at boot"
status: approved
kind: "kernel"
domain: "ledger"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: critical
wave: 1
depends_on:
  - "011-store-hiqlite"
establishes:
  - "crates/rahi-ledger/Cargo.toml"
  - "crates/rahi-ledger/src/lib.rs"
  - "crates/rahi-ledger/src/record.rs"
  - "crates/rahi-ledger/src/chain.rs"
  - "crates/rahi-ledger/src/append.rs"
  - "crates/rahi-ledger/src/verify.rs"
  - "crates/rahi-ledger/src/signer.rs"
  - "crates/rahi-ledger/tests/append.rs"
  - "crates/rahi-ledger/tests/verify.rs"
  - "crates/rahi-ledger/tests/common/"
  - "crates/rahi-ledger/testdata/chains/"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/rahi-ledger/src/append.rs", note: "append is the unique-parent CAS; constitution XI" }
summary: >
  The governance ledger: a linear chain of signed, hash-linked Decision
  records stored in the app's hiqlite, appended by a compare-and-swap that a
  unique index on the parent hash enforces inside one txn, retried three
  times on a miss and then a typed integrity error, and verified in full at
  every boot with an integrity failure being process-fatal. Records use the
  attest-ledger envelope and canonical-keysort-json bytes so an independent
  verifier can check the chain without this code. Carries enrahitu://024 and
  enrahitu://032 §3.7.
---

# 013: The decision chain

## 1. Purpose

The chain is what a buyer of a governed cell is paying for: every admitted
mutation and every denial is a record whose link to its predecessor can be
checked offline. enrahitu proved the CAS-by-unique-index design and the
fail-closed init (enrahitu://024); this spec carries both into Rust on the
`attest-ledger` primitives the family already publishes.

## 2. Territory

The whole of `crates/rahi-ledger` after this spec. Sealing and archive are
spec 014's modules inside the same crate.

## 3. Behavior

- **B-1 (record).** `Decision { id: DecisionId, prev_hash: Hash, kind:
  DecisionKind, actor: Sub, capability: Option<CapabilityId>, outcome:
  Outcome, reason: String, payload: CanonicalJson, at: Revision }` is the
  payload of an `attest_ledger_types::LedgerRecord`. Bytes are canonical
  (`canonical-keysort-json`); the hash is sha256 over them; the record is
  Ed25519-signed by the cell's ledger key. `at` is a store revision, never
  a wall clock; wall time is a payload field supplied by the caller.
- **B-2 (schema).** `kernel_decisions (id TEXT PRIMARY KEY, prev_hash TEXT
  NOT NULL, hash TEXT NOT NULL, record BLOB NOT NULL)` with `CREATE UNIQUE
  INDEX kernel_decisions_parent ON kernel_decisions (prev_hash)`. The
  genesis record's parent is the booted manifest hash (spec 015), so
  distinct chains cannot collide.
- **B-3 (append is CAS).** `Ledger::append(decision) -> Result<Hash>` runs
  head read (`query_consistent`), record build, and insert inside one
  `txn`. A unique violation is a miss: reload the head, re-chain the same
  payload, retry; three attempts, then `Error::Integrity`. Callers that
  await the append get the error; the denial path (spec 015) logs it with
  the decision id and never swallows it.
- **B-4 (verify at boot).** `Ledger::open(store, signer) -> Result<Ledger>`
  creates the table and index, writes genesis if absent, then runs
  `verify_chain` (hash links, signatures, linearity) over the resident
  chain. Any failure, including an index that cannot build over
  pre-existing damage, is `Error::Integrity`, and the caller (the cell's
  boot path) exits the process. A transient store error is not an
  integrity error.
- **B-5 (signer).** `LedgerSigner` wraps an Ed25519 key loaded from
  `/data/keys/ledger.key` (created by spec 031's first boot); the key is
  never `Debug`-printed or serialized.
- **B-6 (independent verification).** `rahi ledger verify` (spec 030)
  and the published `attest-ledger-cli` both verify an exported chain; the
  record format adds nothing the CLI cannot check.

## 4. Functional requirements

- **FR-001.** Two concurrent appends claiming the same parent commit
  exactly once; the loser retries and lands on the new head; the chain has
  no fork.
- **FR-002.** A fixture chain with a broken link under `testdata/chains/`
  makes `Ledger::open` return `Error::Integrity`; a fixture with a forged
  signature does the same.
- **FR-003.** A chain appended in one process and reopened in another
  verifies clean and continues from the same head.
- **FR-004.** Exporting the chain and running `attest-ledger-cli verify`
  over it exits 0 (skipped with a message when the CLI is not installed).

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ledger --locked` passes.
- **AC-2.** `cargo tree -p rahi-ledger` shows only `rahi-store` and
  `rahi-types` from the workspace.

## 6. Out of scope

Sealing and archive (014); what a Decision is about (015 owns the kinds
and emits them); key generation (031).

## 7. Resolved decisions

- **D-1 (2026-09-06, build session; refines B-4's signature).**
  `Ledger::open` takes the genesis parent as a third argument:
  `open(store, signer, genesis_parent) -> Result<Ledger>`. B-4 sketches two
  arguments, but B-2 requires the genesis record's parent to be the booted
  manifest hash, and spec 015 B-8 requires a ledger whose genesis parent
  differs from the booted manifest's hash to be `Error::Integrity`; neither
  is reachable unless `open` is told the hash. `Ledger::genesis_parent()`
  reads it back, so 015's check is a comparison rather than a second
  source. Rejected alternative: a default parent written at first boot and
  compared afterwards, which would let a cell open a chain rooted at a
  manifest it is not running.
- **D-2 (2026-09-06, build session).** The record signature is a sibling
  of the `attest-ledger` envelope, not a field inside it, and it covers
  `record_hash`. A stored row and an exported line hold
  `{id, timestamp, previous_record_hash, record_hash, payload, signature,
  public_key}`: the first five are exactly `LedgerRecord`, so
  `attest_ledger_core::compute_record_hash` recomputes byte-identically and
  the published `attest-ledger` CLI verifies an export without knowing the
  other two (B-6). Signing `record_hash` rather than the payload is what
  binds a decision to its place in the chain: the hash already covers the
  id, the parent link, and the payload, so a record cannot be replayed at a
  different position under a valid signature. Verification pins the
  expected public key (`LedgerVerifier`) rather than trusting the one a row
  carries, because an adversary who can write rows can write a key beside a
  signature they minted. No `ChainAnchor` is written: the anchor's job here
  is done by the manifest hash of D-1, which every record transitively
  commits to, and an anchor would add a second root with a `chain_id` and a
  genesis timestamp this crate has no source for. Rejected alternative:
  the signature inside the `Decision` payload, which cannot cover the
  record hash without circularity and would make the payload something
  other than the nine fields B-1 names.
- **D-3 (2026-09-06, build session).** `Decision::at` is supplied by the
  emitter, as the store revision the decision was made against
  (`Revision::ZERO` when it concerns nothing stored). B-1 fixes what `at`
  *means* (a store revision, never a wall clock) and is silent on who sets
  it; the ledger has no revision of its own to offer, because B-2's schema
  carries no revision column and the chain's own height stops being
  monotonic once spec 014 seals its tail. The envelope's `timestamp` slot,
  which `attest-ledger` takes as a caller argument and never reads a clock
  for, carries `revision:<n>` for the same value, so a reader of an export
  is never tempted to parse it as a date. Wall time stays a payload field
  the caller supplies, as B-1 says.
- **D-4 (2026-09-06, build session).** The `kernel_decisions` table and its
  unique parent index are created idempotently by `Ledger::open`, as B-4
  requires. This is the chassis's own baseline rather than an application
  migration, in the sense `rahi-store` already uses for its
  `schema_version` table: the shape is fixed by B-2, is never versioned by
  an app, and a boot that found no table could not tell an empty chain from
  a deleted one, which is exactly the distinction constitution XI exists to
  make. Application DDL still goes through the `migrate` verb (spec 011
  B-4, constitution IX); nothing here is exposed as a `Migration`.
- **D-5 (2026-09-06, build session).** A lost compare-and-swap is told from
  a duplicate decision id by re-reading the chain, not by matching the
  store's error text. hiqlite reports every failing statement inside a
  transaction as one `Error::Transaction` (spec 011 maps it to
  `Error::Validation`), so the variant carries no signal and matching
  SQLite's wording would make it a contract. After a failed insert
  `append` asks the chain two questions instead: whether its id is already
  resident, which is `Error::Conflict` (or a success, when the row is ours
  and the error came after the fact), and whether the head has moved off
  the parent it chained onto, which is the miss B-3 retries. A head that
  has not moved means the insert failed for some other reason, and that
  error is returned unchanged, so a store failure is never reported as an
  integrity failure.
- **D-6 (2026-09-06, build session).** `/data/keys/ledger.key` holds the
  base64 of a 32-byte Ed25519 seed, the format the rest of the
  `attest-ledger` family reads, with surrounding whitespace ignored. A
  missing file is `Error::Io` and a malformed one is `Error::Config`;
  neither generates a key. Spec 031's first boot is the only writer, and a
  silently minted key would start a second chain that nothing could verify
  against the first, with the key custodied outside the backup
  (constitution XII).
- **D-7 (2026-09-06, build session).** `testdata/chains/` holds the key the
  fixtures are signed with (`signing-key.b64`), the hash they are rooted at
  (`genesis-parent.txt`), and five chains: `clean`, and one edit away from
  it each of `broken-link` (link changed, hash and signature recomputed),
  `tampered-payload` (content changed, stored hash left binding),
  `forged-signature` (a stranger's key over an intact chain), and `forked`
  (two records claiming the genesis parent, which makes the unique index
  fail to build). The key material lives beside the chains rather than in
  the test source, so the directory is auditable on its own, and the
  fixtures are rebuilt from a real ledger by
  `RAHI_LEDGER_FIXTURES=write cargo test -p rahi-ledger --test verify`,
  which is what makes them evidence about this code rather than about
  themselves. FR-004 additionally runs `attest-ledger verify` over an
  export and skips with a message when that separately released binary is
  not installed; the same verification runs in-process either way.

- **D-8 (2026-09-06, build session; reads B-3's "inside one `txn`" as the
  insert).** The `txn` an append submits holds the insert. The head read
  cannot be inside it: B-3 itself names `query_consistent` for that read,
  which is a leader round-trip rather than a statement, and hiqlite's `txn`
  takes write statements and returns rows affected, so there is no
  read-decide-write inside one batch to be had. The atomicity B-3 needs is
  supplied by the unique parent index, exactly as its next sentence says
  ("a unique violation is a miss"), and the sequence is: read the head
  through the leader, build and sign the record, submit the insert as a
  `txn`. It is a `txn` rather than an `execute` because that is the seam
  spec 014 extends, where the statements that seal the tail commit with the
  append that overflowed the window.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked --test append
cargo test -p rahi-ledger --locked --test verify
```
