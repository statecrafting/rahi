# rahi

**The Rust chassis for governed cells.**

rahi is the substrate the statecrafting products stand on. It gives an
application seven things it should never build twice: identity (rauthy,
co-deployed, reached only through the app's own origin), replicated state
(hiqlite, in-process, its own Raft cluster), a hash-chained decision ledger,
a deny-by-default capability kernel, an axum edge with probes, metrics, and
tracing, single-container packaging with preflight, migrate, backup, and
restore, and a dev substrate that boots the real binary in tests. An app is
a Rust binary that composes the crates and declares a manifest. It extends
the chassis; it never forks it.

The name is the lineage: enrahitu was Encore, Rauthy, Hiqlite, Turso. Drop
Encore and Turso and what remains is what this is.

## Status: implemented, not released, exercised at N=1

This repository is a specification corpus, the harness that builds it, and
the code it specifies. The corpus holds 22 `status: approved` specs in three
waves: twenty are `implementation: complete`, and the two records, 000 and
002, are `n-a`. Spec ordinals are the build order, and each spec is bounded
to one driven session's territory. Seven more, 035 to 041, are `status:
draft`: proposals from the consumer contract below that schedule nothing
until a human approves them. Nine crates and the reference app exist
(`rahi-types`, `rahi-store`, `rahi-ledger`, `rahi-kernel`, `rahi-edge`,
`rahi-idp`, `rahi-ops`, `rahi-cli`, `rahi-harness`, and `apps/hello-cell`),
every source file claimed by the spec that built it. `spec-spine registry
plan` is the live answer to what is buildable now.

Four words mean four different things here, and only the first describes
the whole repository:

| | State |
|---|---|
| Implemented | the twenty `complete` specs. Complete means the code landed and the spec's verification block passed; two operator procedures inside complete specs were recorded and never run: the N=3 rollout check (032 D-1) and the compose walkthrough (034 D-1). |
| Released or installable | nothing: no tag, release, crate, or image. A consumer pins a git commit (draft 039). |
| Exercised against the pinned rauthy | by hand, never in CI, which runs without a rauthy. The end-to-end login, write, denial, and restore pass against rauthy 0.36.2. `rahi backup` against a real rauthy fails (030 D-3), and a restore brings back the rows and the chain but no users (draft 037). |
| Supported topology | N=1. Three replicas have run only as three processes on one host, without rauthy, and there every replica mints the same decision ids (draft 035). N=3 on Kubernetes has never run. |

[`docs/design/01-consumer-contract.md`](docs/design/01-consumer-contract.md)
states what a consumer gets and what is proven, by which test, against which
identity provider;
[`docs/design/02-operational-prerequisites.md`](docs/design/02-operational-prerequisites.md)
records the measurements behind the table above, the maintainer's decisions
of 2026-09-12 on them (section 8.1, recorded in the drafts; no draft is
approved by them), and the decisions still open.

The corpus is built by
[claude-observatory](https://github.com/bartekus/claude-observatory), which
schedules the lowest-numbered ready spec, drives one fresh session through
`AGENTS.md`'s backlog protocol, ships through this repo's own `/ship`
skill and hooks, shepherds the PR through the CI wired here, and runs the
spec's `## Verification` block after merge. Done is never self-authored.

## Reading the corpus

| Start here | What it is |
|---|---|
| `specs/002-chassis-thesis/spec.md` | the seven responsibilities, the crate topology, the three-wave build order |
| `docs/design/00-lineage.md` | what was carried from enrahitu, what was cut, and why |
| `standards/spec/constitution.md` | the fourteen principles, seven of them frozen at tier 1 |
| `specs/000-rahi-bootstrap/spec.md` | what a spec is, and the frozen invariants |
| `AGENTS.md` | the session protocol and the backlog discipline |
| `docs/design/01-consumer-contract.md` | how to consume the chassis today, and the exact guarantees it gives |
| `docs/design/02-operational-prerequisites.md` | the gaps a hosted consumer meets first, measured, with the open decisions |

The crate topology:

```
apps/hello-cell        the reference app: manifest, one resource kind, one page   (034)
rahi-cli               the binary composer: serve, preflight, migrate, backup, restore  (030)
rahi-ops               the verbs, first boot, keys, the die-together supervisor   (030, 031)
rahi-idp   rahi-edge   rauthy proxy, discovery, sessions   axum, middleware, probes, obs  (020-024)
rahi-kernel rahi-ledger manifest and adjudication          decision chain, sealing  (013-015)
rahi-store             hiqlite: txn, query, lock, notify, watermark, migrate, backup  (011, 012)
rahi-types             Error and exit codes, Principal, Revision, Fence, config  (010)
rahi-harness           dev-dependency only: boots the built binary, waits on /readyz  (033)
```

Rust throughout; no Node, no Encore. rauthy is consumed as a released
binary in the same container and never forked.

## Governance

The corpus is governed by [spec-spine](https://github.com/statecrafting/spec-spine)
0.18.0. `make gate` runs the gate, read-only throughout (freshness, lint,
ownership coverage, coupling, DAG check); `make refresh` is the writing half,
for a session that can commit the regenerated shards; `make ci` adds the
cargo gates once a workspace exists. Derived artifacts under `.derived/` are committed
and read only through `spec-spine` subcommands. Every source file inside a
crate must be specifically claimed by a spec; a session that adds a file
claims it in the spec it is implementing.

```sh
cargo install spec-spine-cli --locked   # the pin lives in the Makefile: 0.18.0
make gate
spec-spine registry list
scripts/spec-dag.sh
```

## Building it with claude-observatory

```sh
cd ../claude-observatory
bun src/index.ts orchestrator projects add /path/to/rahi   # registers, qualifies, arms
bun src/index.ts orchestrator dag                          # the readiness view
bun src/index.ts orchestrator next                         # the lowest-numbered ready spec
bun src/index.ts orchestrator daemon start
```

The orchestrator's state root for this project lives under `data/`, which
is gitignored. Any spec can be pulled back to `status: draft` to hold it for
human review; drafts are visible as blockers and never scheduled.

## License

Apache-2.0, see [`LICENSE`](LICENSE). `aicortex` is Apache-2.0 and
`hqgit` is AGPL-3.0; Apache-2.0 into AGPL-3.0 is the sanctioned direction,
so both consume this chassis and both may contribute back under Apache-2.0.
