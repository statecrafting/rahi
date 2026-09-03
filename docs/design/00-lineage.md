# Lineage: from enrahitu to rahi

Date: 2026-09-03. rahi is a rebuild, not an amendment, of the enrahitu
chassis. This note records what was carried, what was cut, and why, so the
specs can cite it (`enrahitu://<spec>` provenance URIs) without repeating it.

## Why rebuild

enrahitu's corpus grew to 38 specs around one ambition: a template that
outside organizations stamp, extend, and upgrade under. That ambition
produced a membership product inside the chassis (specs 036 through 038), a
stamp contract and scaffold verb (009, 014), a chassis-boundary mechanism
(035), three packaging pivots (008, 010, 018), and a second frontend (013,
015, 023). None of it has a customer. The products that will consume the
chassis (hqgit, aicortex) are in-org, Rust, and want a library, not a
template. Amending around the ambition would have preserved the confusion.

The second reason is the runtime. Encore.ts was kept for its static app
model and infra seam, and everything else about it (the Node runtime, the
vendored toolchain, the napi addon for hiqlite) was overhead. rahi keeps the
model as a hand-authored manifest verified at build, and drops the runtime.

## Carried (with the enrahitu spec that decided it)

| rahi spec | Carried decision | Source |
|---|---|---|
| 011, 012 | The ten hiqlite decisions: txn atomicity, key-only notify, two read calls, ten-second leases with fencing, revision watermark, migrations as a deploy step, CAS by unique index, backup surface, archive separate from backup, local notify only | enrahitu://032 |
| 013 | CAS append on the unique parent index, three retries, fatal at init | enrahitu://024 |
| 014 | Head plus hot window resident, sealed segments archived, verifiable without history | enrahitu://032 §3.7, §3.9 |
| 015 | Declared ceiling, verified emission, deny by default, denials ledgered, gate config hash anchored | enrahitu://020, enrahitu://021 |
| 021 | rauthy same-origin behind a raw `/auth/*` proxy, loopback only, client bootstrap, discovery-driven | enrahitu://005 |
| 022 | The IdP's `sub` is the principal, no local account row, signed envelope cookie, renewal as a round-trip, `email_verified` absent means false | enrahitu://004 |
| 023 | `/metrics` always on, OTel with an in-process ring buffer, decision id on spans | enrahitu://022 |
| 024 | Trusted proxy hops, operator-gated internal surfaces, no rate limiter on operator surfaces | enrahitu://025 |
| 030 | preflight, migrate, backup, restore; one artifact with keys; restore is a cluster reset | enrahitu://027 |
| 031 | One volume layout, one public URL input, die-together supervision, first-boot provisioning, single-shot restore marker | enrahitu://007 |
| 032 | N=1 primary, N=3 the scale path, two Raft clusters per replica, one key set custodied once, rate limiting per node stated | enrahitu://030 |
| 033 | Boot the real binary, wait on `/readyz` never `/healthz`, cookie jar, one instance per test file | enrahitu://033 |

## Cut

The membership domain (036, 037, 038), application mail, the stamp contract
and scaffold verb (009, 014), the chassis-boundary upgrade mechanics (035),
the Pages deploy slot (013), the second frontend flavor (015), the
frontend-admin dashboard as a chassis concern (023), CoreLedger and its
Postgres driver (003, 011), the Encore toolchain (008, 010, 018), and
app-model extraction from a parser (020's TS producer). The manifest
survives as a declared, verified TOML document; extraction from source is a
named later spec if a product needs it.

## Changed

- One store. hiqlite's SQLite group is the application store; there is no
  second relational database. Vector search, when aicortex needs it, is a
  spec in aicortex over the same store.
- One language. The supervisor, the verbs, the proxy, and the harness are
  Rust; no shell beyond the container entrypoint.
- Library, not template. Consumers depend on published crates and pin a
  version. There is no `template.toml` and nothing stamps.
