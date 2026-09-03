---
paths:
  - "crates/rahi-store/**"
  - "crates/rahi-ledger/**"
  - "crates/rahi-idp/**"
  - "crates/rahi-kernel/**"
---

# Chassis invariants

These hold in every change to the store, ledger, identity, and kernel crates.
They are the constitution's principles VI through XII in checkable form; the
specs that own each rule are named so a violation can be traced.

## Store (`rahi-store`, specs 011 and 012)

- A resource write and its outbox row commit in one `txn`. Notify is issued
  after, outside the transaction, and is a hint; the revision column is
  truth. Never treat notify delivery as a guarantee.
- `query` reads the local replica; `query_consistent` takes the leader
  round-trip. Admission and the chain head use `query_consistent`; lists,
  details, and controller scans use `query`. A call site that could be
  either states which and why.
- A lease-guarded write carries the fencing token and the SQL predicate
  `fence <= :token`. Leases are ten seconds and not configurable; work is
  chunked or re-acquired, never assumed to fit.
- Nothing durable lives in the cache group. KV, counters, and lock state
  are rebuilt on demand after a restart.
- Migrations are DDL through `txn`, guarded by `schema_version` read with
  `query_consistent`, run by the `migrate` verb, never at boot.
- rauthy's hiqlite directory is never opened, listed, or backed up by app
  code. Two stores, two Raft clusters, always.

## Ledger (`rahi-ledger`, specs 013 and 014)

- Append is a compare-and-swap: head read, record build, and insert inside
  one `txn`, with the unique index on the parent hash making the insert the
  CAS. A unique violation is a miss, retried three times, then a typed
  integrity error. Never fork the chain.
- Chain verification runs at boot and fails closed: an integrity error on
  the init path is process-fatal.
- Sealed segments are immutable, hash-linked to their predecessor, and
  verifiable without archived history being resident. They are never
  rewritten and never conflated with backups.

## Identity (`rahi-idp`, specs 021 and 022)

- rauthy's `sub` is the only principal identifier. No local account row is
  ever written at login.
- `email_verified` absent means false. `preferred_username` never stands in
  for an email.
- The session cookie is a signed envelope holding rauthy's refresh token,
  httpOnly, same-origin. Renewal is a round-trip to rauthy; roles and
  `email_verified` are re-read on every renewal, never carried forward.
- The `/auth/*` proxy is raw and unfiltered; rauthy binds loopback only.

## Kernel (`rahi-kernel`, spec 015)

- The manifest is a declared ceiling: observed usage must be a subset of it
  or the build fails. Absence is never permission.
- Every denial is a ledgered Decision. The request path never awaits the
  ledger append; the denial path never swallows an integrity error.
- The gate's config hash is part of the anchored surface.
