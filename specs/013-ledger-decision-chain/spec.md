---
id: "013-ledger-decision-chain"
title: "The decision chain: hash-linked records, CAS append, verification at boot"
status: approved
kind: "kernel"
domain: "ledger"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
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

None yet.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked --test append
cargo test -p rahi-ledger --locked --test verify
```
