---
id: "010-workspace-and-core-types"
title: "Cargo workspace and the core types: Error, Principal, Revision, FenceToken, Config"
status: approved
kind: "kernel"
domain: "types"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 1
depends_on:
  - "002-chassis-thesis"
establishes:
  - "Cargo.toml"
  - "Cargo.lock"
  - "rust-toolchain.toml"
  - "deny.toml"
  - "apps/.gitkeep"
  - "crates/rahi-types/Cargo.toml"
  - "crates/rahi-types/src/lib.rs"
  - "crates/rahi-types/src/error.rs"
  - "crates/rahi-types/src/principal.rs"
  - "crates/rahi-types/src/revision.rs"
  - "crates/rahi-types/src/config.rs"
  - "crates/rahi-types/src/version.rs"
  - "crates/rahi-types/tests/"
summary: >
  The first build: a virtual Cargo workspace with the toolchain pinned,
  unsafe denied, one shared dependency table, and a supply-chain policy; plus
  rahi-types, the plain-data substrate every other crate depends on and that
  depends on nothing in the workspace. It fixes the Error enum with the four
  exit codes, the Principal as the IdP's subject plus claims, the Revision
  and FenceToken newtypes the store's invariants are written against, the
  Config tree derived from one public URL, and the schema version constants.
---

# 010: Cargo workspace and the core types

## 1. Purpose

Every crate shares a handful of value types, and two of them (`Revision`,
`FenceToken`) are the vocabulary of the store's invariants (constitution
IX). If they are defined twice they drift. This spec creates the workspace
and pins those types in one dependency-free crate, together with the
`Principal` whose only identifier is rauthy's `sub` (constitution VII) and
the `Config` that derives everything from one public URL (thesis §3).

## 2. Territory

The workspace root (`Cargo.toml`, `rust-toolchain.toml`, `deny.toml`) and
the whole of `crates/rahi-types`. Later specs that add a third-party
dependency declare an `extends` edge on this spec's `Cargo.toml` section
`workspace.dependencies`.

## 3. Behavior

- **B-1 (workspace).** The root `Cargo.toml` is a virtual workspace with
  `members = ["crates/*", "apps/*"]`, `resolver = "3"`, a
  `[workspace.package]` table (edition 2024, `rust-version`, license
  `Apache-2.0`, repository URL), `[workspace.lints.rust]` with
  `unsafe_code = "deny"`, `[workspace.lints.clippy]` denying `unwrap_used`,
  `expect_used`, and `indexing_slicing` in library code, and a single
  `[workspace.dependencies]` table where every third-party version lives.
  Crate manifests reference `workspace = true` for everything they inherit.
- **B-2 (toolchain).** `rust-toolchain.toml` pins a stable channel by exact
  version with `components = ["rustfmt", "clippy"]`. `Cargo.lock` is
  committed and every gate passes `--locked`.
- **B-3 (supply chain).** `deny.toml` allows `MIT`, `Apache-2.0`,
  `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-3.0`, `Zlib`, `MPL-2.0`,
  and `MIT-0`; denies unknown registries and git sources; warns on
  duplicate versions; denies advisories with a known fix.
- **B-4 (`Error`).** One `Error` enum for the workspace's library crates
  with variants `Validation(String)`, `NotFound(String)`, `Conflict(String)`,
  `Integrity(String)`, `Denied(String)`, `Unauthorized(String)`,
  `Stale(String)`, `Io(String)`, `Config(String)`, and `Upstream(String)`,
  each carrying an owned message, plus `fn exit_code(&self) -> i32`
  mapping to `1` (validation, not found, conflict, integrity, denied,
  unauthorized), `2` (stale), and `3` (io, config, upstream). Binaries map
  exit codes in exactly one place through this function.
- **B-5 (`Principal`).** `Principal { sub: Sub, email: Option<Email>,
  email_verified: bool, roles: BTreeSet<Role>, issued_at: UnixSeconds }`.
  `Sub` is an opaque newtype over `String` with no constructor from any
  other identifier. `email_verified` defaults to `false` when absent; there
  is no accessor that returns an email for an unverified principal without
  the caller naming it (`email_unverified()`).
- **B-6 (`Revision` and `FenceToken`).** `Revision(u64)` and
  `FenceToken(u64)` are `#[repr(transparent)]` newtypes with `Ord`, no
  arithmetic, and a single `next()` on `Revision`. They are never
  constructed from a clock.
- **B-7 (`Config`).** `Config::from_env(reader: &dyn EnvReader) ->
  Result<Config, Error>` builds the whole tree from `RAHI_PUBLIC_URL` plus
  optional overrides: `public_url`, `data_dir` (default `/data`), the
  hiqlite bind addresses (defaults `127.0.0.1:8300` and `:8400`, leaving
  `8100`/`8200` to rauthy), the rauthy loopback base (`127.0.0.1:8080`),
  the cookie scheme (`Secure` iff the public URL is `https`),
  `trusted_proxy_hops` (default `0`), and the OTLP endpoint (`None` by
  default). `EnvReader` is a trait so tests inject a map.
- **B-8 (versions).** `version.rs` holds `pub const` schema versions as
  `&str` in `MAJOR.MINOR.PATCH`: `STORE_SCHEMA_VERSION`,
  `LEDGER_SCHEMA_VERSION`, `MANIFEST_SCHEMA_VERSION`, all `"1.0.0"`.
  Bumping a MAJOR is a spec amendment.
- **B-9 (plain data).** Every public type is owned, `Clone`, `Debug`,
  `PartialEq`, `Eq`, and serde-derived; no lifetimes, generics, or trait
  objects appear in a public field. Nothing in this crate reads
  `std::time` or `std::env` directly; `BTreeMap` and `BTreeSet` are the
  only collections.

## 4. Functional requirements

- **FR-001.** `cargo build --workspace --locked` and `cargo clippy
  --workspace --all-targets --locked -- -D warnings` pass with
  `rahi-types` as the sole member; `cargo tree -p rahi-types` shows no
  workspace crate.
- **FR-002.** Tests cover every `Error` variant's exit code, `Principal`
  serde round-trip and the `email_verified` default, `Revision::next`,
  `FenceToken` ordering, and `Config::from_env` for `http` and `https`
  public URLs and for each override.
- **FR-003.** A test reads the crate's own sources and asserts none
  contains `std::time`, `std::env`, `HashMap`, or `HashSet` outside a
  comment.
- **FR-004.** `cargo deny check` passes with the `deny.toml` of B-3.
- **FR-005.** Every crate manifest carries
  `[package.metadata.spec-spine] spec = "<founding spec id>"`.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-types --locked` passes.
- **AC-2.** `make ci` exits 0 on the branch (the cargo gates are now live).
- **AC-3.** `spec-spine index` discovers exactly one package, `rahi-types`,
  bound to this spec, and `spec-spine index coverage --fail-on-untraced`
  exits 0.

## 6. Out of scope

The store (011); the manifest types (015, in `rahi-kernel`); session and
token types (022); any I/O.

## 7. Resolved decisions

- **D-1 (2026-09-05, build session).** The toolchain is pinned at
  `1.96.0` and `rust-version` is `1.96`, the current stable at build time.
  Alternative rejected: a channel name (`stable`), which B-2 forbids
  because it is not reproducible.
- **D-2 (2026-09-05, build session).** `serde` (with `derive`) is the
  crate's only runtime dependency; `serde_json` is a dev-dependency for the
  round-trip tests. `Error` implements `Display` and `std::error::Error` by
  hand. Alternative rejected: `thiserror`, which adds a proc-macro
  dependency for ten variants whose messages are one shape.
- **D-3 (2026-09-05, build session).** `apps/.gitkeep` is committed so the
  `apps/*` member glob of B-1 resolves: cargo refuses a glob whose
  directory does not exist, and `apps/` is empty until spec 034.
  Alternative rejected: omitting `apps/*` until 034, which contradicts B-1.
- **D-4 (2026-09-05, build session).** The override variables are
  `RAHI_DATA_DIR`, `RAHI_HIQLITE_API_ADDR`, `RAHI_HIQLITE_RAFT_ADDR`,
  `RAHI_RAUTHY_ADDR`, `RAHI_TRUSTED_PROXY_HOPS`, and `RAHI_OTLP_ENDPOINT`.
  Addresses are `std::net::SocketAddr`; `trusted_proxy_hops` is a `u8`;
  an empty value is absent and takes the default; the cookie scheme is
  derived and has no override. `PublicUrl` is a validated newtype over
  `String` (scheme `http` or `https`, non-empty host, no userinfo, query,
  or fragment, trailing slash stripped). Alternative rejected: the `url`
  crate, which brings `idna` and friends into every crate for one field.
- **D-5 (2026-09-05, build session).** `Revision::next` saturates at
  `u64::MAX` rather than wrapping, so a revision never compares below its
  predecessor. `Revision::ZERO` names the never-written state.
- **D-6 (2026-09-05, build session).** `Error` also exposes `kind()` (a
  stable snake_case label for logs and metrics), `message()`, and the
  three public `EXIT_*` constants that `exit_code()` returns. The variant
  list is exhaustive (no `#[non_exhaustive]`): adding a variant is a spec
  amendment and every downstream match should break. The workspace lint
  table also denies `float_arithmetic` and warns on `missing_docs`.

## Verification

```verify:cli
cargo test -p rahi-types --locked
cargo clippy -p rahi-types --all-targets --locked -- -D warnings
```
