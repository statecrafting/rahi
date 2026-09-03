---
id: "002-chassis-thesis"
title: "Chassis thesis: seven responsibilities, one deployment unit, a library not a template"
status: approved
kind: "thesis"
domain: "governance"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: n-a
risk: critical
wave: 1
depends_on:
  - "000-rahi-bootstrap"
constrains:
  - kind: sequencing-plan
    target_specs:
      - "010-workspace-and-core-types"
      - "011-store-hiqlite"
      - "012-store-coordination"
      - "013-ledger-decision-chain"
      - "014-ledger-sealing-and-archive"
      - "015-kernel-manifest-and-adjudication"
    note: >
      Wave 1: the state core. Types, the hiqlite store and its coordination
      primitives, the decision chain and its archive, and the kernel. Ends
      when a manifest can be verified, a decision can be appended and
      verified across a restart, and a lease-guarded write is fenced, all
      without a network listener.
  - kind: sequencing-plan
    target_specs:
      - "020-edge-server"
      - "021-idp-proxy-and-discovery"
      - "022-session-and-principal"
      - "023-observability"
      - "024-hardening"
    note: >
      Wave 2: the edge and identity. The axum server with its middleware
      chain and probes, the rauthy proxy and discovery, sessions and the
      principal, metrics and tracing, and the hardening pass. Ends when a
      browser can log in through the app's origin against a rauthy on
      loopback and the session survives a renewal.
  - kind: sequencing-plan
    target_specs:
      - "030-operational-verbs"
      - "031-single-container-packaging"
      - "032-cluster-topology"
      - "033-dev-substrate-and-harness"
      - "034-hello-cell"
    note: >
      Wave 3: operations and the reference app. The verbs and the binary
      composer, the single image with first boot and die-together
      supervision, the N=3 topology on Kubernetes with S3 backups, the dev
      substrate and test harness, and hello-cell, the app that proves every
      responsibility end to end and is the template consumers copy from.
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
summary: >
  The architectural record every ordinary spec operates inside. rahi is a
  set of Rust library crates and one binary composer that give an
  application exactly seven things: identity, state, ledger, kernel, edge,
  operations, and a dev substrate. It ships as one container with one volume
  and one origin, with rauthy co-deployed and never shared. It is a library
  that apps compose, not a template that apps are stamped from. This spec
  fixes the responsibilities, the crate topology, the dependency direction,
  and the three-wave build order. It owns no code.
---

# 002: Chassis thesis

## 1. Purpose

enrahitu proved the shape (rauthy same-origin, hiqlite in-process, a
decision chain, a deny-by-default kernel, one container) and then grew a
product and a template business around it that nothing consumes. hqgit and
aicortex need the shape as Rust libraries with a pinned version. This thesis
states the shape once, bounds it to seven responsibilities, and fixes the
order in which they are built. It is `implementation: n-a`: it constrains
every ordinary spec and owns no code.

The Encore.ts runtime is not carried. What is carried from it is the idea
of a static application model separated from environment configuration,
which becomes the manifest (spec 015) and the config types (spec 010).

## 2. The seven responsibilities

| Responsibility | `domain` | What it owns | Specs |
|---|---|---|---|
| Types | `types` | Error and exit codes, Principal, Revision, FenceToken, Config, versions | 010 |
| State | `store` | hiqlite in-process: txn, query and query_consistent, migrate, backup; lock with fencing, local notify, outbox and watermark | 011, 012 |
| Ledger | `ledger` | The decision chain: CAS append, boot verification, sealing to an object store | 013, 014 |
| Kernel | `kernel` | The manifest ceiling, build-time verification, runtime adjudication, denial ledgering | 015 |
| Edge | `edge` | axum router and middleware, probes, static slot, metrics, tracing, hardening | 020, 023, 024 |
| Identity | `identity` | rauthy proxy, discovery and JWKS, client bootstrap, session envelope, renewal, the principal | 021, 022 |
| Operations | `ops` | preflight, migrate, backup, restore; first boot, keys, supervision; the image; the N=3 topology; the dev substrate and harness | 030, 031, 032, 033 |

The reference app `hello-cell` (034, `domain: edge`) is the eighth spec of
wave 3 and the proof: it composes every crate, declares a manifest, stores
one resource kind, serves one page behind login, and passes the harness.

Anything not in this table is an application concern. A membership domain,
mail, a dashboard, a stamp contract, a second frontend, a second relational
store: each of these was in enrahitu and none is in rahi.

## 3. The deployment unit

One container, one volume, one public origin, one command. Inside the
container: the app binary (which is also the supervisor and the verb host)
and the rauthy release binary. rauthy binds loopback; the app reverse-proxies
`/auth/*` to it and is the only exposed listener. The volume holds
`/data/hiqlite` (the app's Raft state), `/data/rauthy` (rauthy's, never
opened by the app), and `/data/keys`. One public URL environment variable
derives every issuer, redirect, and cookie setting.

N=1 is primary and pays nothing for N=3. At N=3 every replica is identical:
a StatefulSet pod running both processes with its own volume, two Raft
clusters per replica, one key set injected into all of them and custodied
once. Backups of both stores are encrypted with those keys and land in S3.

## 4. Crate topology and dependency direction

```
apps/hello-cell ──▶ rahi-cli ──▶ rahi-ops ──▶ rahi-idp, rahi-edge ──▶ rahi-kernel, rahi-ledger ──▶ rahi-store ──▶ rahi-types
                                 rahi-harness (dev-dependency only)
```

- `rahi-types` has no workspace dependency.
- `rahi-store` wraps hiqlite and depends only on types.
- `rahi-ledger` and `rahi-kernel` depend on store and types; the kernel
  depends on the ledger to record decisions.
- `rahi-idp` and `rahi-edge` depend on the kernel (every request is
  adjudicated) and therefore on everything beneath it. `rahi-edge` does not
  depend on `rahi-idp`; the app composes them.
- `rahi-ops` depends on store, ledger, and idp (the verbs touch both
  stores' backup surfaces, rauthy's through its HTTP API).
- `rahi-cli` depends on all of the above and exposes `run(cell)`.
- `rahi-harness` depends on nothing in the workspace at compile time; it
  drives a built binary over HTTP.
- The chassis never depends on an app. An app pins one chassis version.

`unsafe` is denied workspace-wide and allowed only in a named FFI block with
a `// SAFETY:` comment; today no crate needs one, since hiqlite is a Rust
crate and rauthy is a separate process.

## 5. Build order

Wave 1 (010 to 016) is the state core and is buildable and testable with no
network listener: a temp directory, a single-voter hiqlite, and fixtures.
Wave 2 (020 to 026) adds the edge and identity; its tests need a rauthy
binary on loopback (spec 021 names it as an operator prerequisite for its
integration tests and ships unit tests that do not). Wave 3 (030 to 034)
adds the verbs, the image, the topology, the harness, and the reference app.
Within a wave the ordinal is the order; across waves every spec's
`depends_on` points into the same or an earlier wave.

## 6. What the thesis refuses

- A second relational store beside hiqlite's SQLite group. If a product
  needs vectors or full-text search, it is a spec in that product over the
  same store, reading binary values through 016 and ranking in its own
  process. Loadable SQLite extensions are refused (016 B-4): hiqlite
  replicates statements, so an engine that differs between nodes diverges
  silently.
- A template, a stamp verb, or an upgrade mechanism beyond a version bump.
- Any app code that opens rauthy's storage, or any deployment that shares a
  volume between Raft members.
- A boot-time migration.
- A notify consumer that does not also poll a watermark.

## 7. Resolved decisions

- **D-A (2026-09-03, amendment).** The refusal list originally recorded
  that SQLite extensions are loadable, on the strength of hiqlite building
  rusqlite with the feature enabled. Spec 016 examined the replication
  model and closed it: the feature being compiled in says nothing about
  whether every node in a Raft group has the same extension present, and a
  follower that applies a statement against a different engine breaks the
  state machine. The waves in §5 grew to 016, 025, and 026 in the same
  amendment, for binary values, the resource server, and streaming.

- **D1 (rebuild, not amend).** The enrahitu corpus is superseded by this
  corpus; decisions are carried by citation (`enrahitu://NNN`), never by
  inheritance. Alternative rejected: amending enrahitu in place, which
  would have kept the template ambition and the Node runtime.
- **D2 (Rust, not on the Encore runtime).** Encore's Rust core is a runtime
  for another language's app model and an internal crate. Its model is
  kept as the manifest; its crate is not a dependency.
- **D3 (one store).** CoreLedger and the Postgres driver are cut. hiqlite's
  SQLite group is the application store.
- **D4 (rauthy as a process, never a library).** rauthy is a binary; it is
  co-deployed and consumed at its HTTP and OIDC surfaces.
- **D5 (library, not template).** Consumers depend on published crates.
  `hello-cell` is an example to copy, not a template to stamp.
- **D6 (three waves).** State core first because everything above it is
  untestable without it; identity second because the reference app cannot
  exist without login; operations third because they wrap what exists.
- **D7 (specify first).** The entire corpus is authored before any code,
  every spec is a bounded session's territory, and the orchestrator builds
  it in ordinal order. Only 000, 001, and 002 are non-`pending` at authoring
  time.
