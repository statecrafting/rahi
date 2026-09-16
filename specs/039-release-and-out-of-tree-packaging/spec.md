---
id: "039-release-and-out-of-tree-packaging"
title: "Releases and out-of-tree packaging: a tagged version a consumer pins, images that exist under the names the manifests use, and a runtime image a cell in another repository builds on"
status: approved
kind: tooling
domain: ops
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: in-progress
risk: medium
wave: 3
depends_on:
  - "010-workspace-and-core-types"
  - "015-kernel-manifest-and-adjudication"
  - "030-operational-verbs"
  - "031-single-container-packaging"
  - "032-cluster-topology"
  - "034-hello-cell"
establishes:
  - "docker/runtime.Dockerfile"
  - ".github/workflows/release.yml"
  - "crates/rahi-ops/rauthy.env.template"
  - "CHANGELOG.md"
  - ".github/consumer-cell/"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.package" }, nature: additive }
  - { spec: "031-single-container-packaging", unit: ".github/workflows/image.yml", nature: additive }
  - { spec: "031-single-container-packaging", unit: "docker/Dockerfile", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/rauthy_env.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/kustomization.yaml", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/statefulset.yaml", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/migrate-job.yaml", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/src/cell.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/README.md", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/manifest.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/tests/manifest.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/testdata/manifests/", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/manifest.toml", nature: additive }
  # The manifest of every crate inherits the workspace version (B-1).
  - { spec: "010-workspace-and-core-types", unit: "crates/rahi-types/Cargo.toml", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/Cargo.toml", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/Cargo.toml", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/Cargo.toml", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/Cargo.toml", nature: additive }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/Cargo.toml", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/Cargo.toml", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/Cargo.toml", nature: additive }
  - { spec: "033-dev-substrate-and-harness", unit: "crates/rahi-harness/Cargo.toml", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/Cargo.toml", nature: additive }
  # Every manifest names the schema it was written for (B-8).
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/cell.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/tests/common/mod.rs", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/testdata/manifest.toml", nature: additive }
  - { spec: "022-session-and-principal", unit: "crates/rahi-idp/testdata/oidc/", nature: additive }
  - { spec: "035-denials-survive-shutdown", unit: "crates/rahi-cli/tests/shutdown.rs", nature: additive }
  # The static directory, the page in the image, and the render's images.
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/tests/cli.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "docker/smoke.sh", nature: additive }
  - { spec: "032-cluster-topology", unit: "scripts/k8s-validate.sh", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/secret.example.yaml", nature: additive }
  # The template's new home is hashed where it lives (B-6).
  - { spec: "001-agentic-harness", unit: "spec-spine.toml", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
summary: >
  The thesis says consumers depend on published crates and pin a version
  (002 D5), and nothing is published: no tag, no release, no crate, no
  image. The one route that works is a git dependency pinned to a sha.
  cargo package fails on rahi-ops because it includes a template from
  outside the crate, which also blocks rahi-cli, the crate that carries
  Cell. The deploy manifests name ghcr.io/bartekus/rahi:latest, a
  namespace and a tag no workflow produces. The image recipe builds only
  a package of this workspace, and a cell's static directory named under
  CARGO_MANIFEST_DIR does not exist in the image (hello-cell's page answers
  404 there). This spec defines the version and the release, publishes
  the images a tag builds under the names the manifests use, adds a
  runtime image an out-of-tree cell builds on, makes static assets exist
  in the image, makes the crates packageable, and proves the consumer
  stanza on every release.
---

# 039: Releases and out-of-tree packaging

## 1. Purpose

Verified on 2026-09-11 at `444bcf8`:

- `github.com/statecrafting/rahi` is public; no tag, release, crate, or
  image exists. Every crate declares `0.1.0` on its own; the workspace
  declares no version.
- A consumer outside the repository builds, boots, writes through the
  kernel, and is denied, from `rahi-* = { git = ..., rev = <sha> }` with
  rustc 1.96. Its lockfile re-resolves 20 packages differently from
  rahi's.
- `cargo package --workspace` fails on `rahi-ops`:
  `crates/rahi-ops/src/rauthy_env.rs:19` includes
  `docker/rauthy.env.template`, which is outside the package.
- `image.yml` would push `ghcr.io/statecrafting/rahi:<tag>` on a `v*` tag
  and never `latest`; `deploy/k8s` and `deploy/README.md` name
  `ghcr.io/bartekus/rahi:latest`.
- `docker/Dockerfile` builds `-p $RAHI_PACKAGE` from this workspace. The
  hello-cell image serves `404` for its page, because
  `static_dir()` names `/src/apps/hello-cell/web`, which the runtime stage
  never copies; spec 034's AC-2 procedure fails at "see the page".
- aicortex requires rahi from a registry by exact version (its 010 D-1);
  hqgit and Statecraft will pin something. They need a name that exists.

## 2. Territory

A release workflow and a runtime image recipe, the rauthy environment
template moved inside `rahi-ops`, and additive changes to the workspace
manifest (010), the image workflow and recipe (031), the deploy manifests
(032), the serve composition (030), and hello-cell's static directory
(034).

## 3. Behavior

- **B-1 (one version).** `[workspace.package] version` is the version of
  every chassis crate, inherited. The chassis crates are the nine a
  consumer can depend on: `rahi-types`, `rahi-store`, `rahi-ledger`,
  `rahi-kernel`, `rahi-idp`, `rahi-edge`, `rahi-ops`, `rahi-cli` (which
  carries `Cell` and `run`), and `rahi-harness`. Pre-1.0, a minor bump may
  change the consumer contract and a patch bump may not. The consumer
  contract is the
  `Cell` trait, the manifest schema, the environment surface, the exit
  codes, the archive format, the chain record format, and the chassis's
  HTTP surfaces (`/auth`, `/session`, `/.well-known`, `/operator`,
  probes).
- **B-2 (a release).** A release is an annotated tag `vX.Y.Z` on a `main`
  commit whose `make ci` passed. `CHANGELOG.md` names every change to the
  consumer contract under its version, and the release updates
  `docs/design/01-consumer-contract.md` to the commit it tags. A release
  publishes all nine chassis crates to the crates.io registry at the
  tag's version, so a crate version, a tag, and the images of B-3 name one
  release (D-1). Publication is a step a human takes at the release
  checkpoint; nothing in this spec's build publishes.
- **B-3 (the images a tag builds).** On a tag, `image.yml` publishes
  `ghcr.io/statecrafting/rahi:X.Y.Z` (the empty cell) and
  `ghcr.io/statecrafting/rahi-runtime:X.Y.Z` (B-4), public, both
  architectures. `deploy/k8s` names `ghcr.io/statecrafting/rahi` and a
  version, never `latest`, and a consumer pins an image by that version
  and its digest.
- **B-4 (the runtime image).** `docker/runtime.Dockerfile` is the runtime
  stage of `docker/Dockerfile` on its own: the pinned rauthy by digest, the
  non-root user, `/data`, the entrypoint, and no cell. A cell in another
  repository builds its binary and adds it at `/usr/local/bin/rahi`, and
  its static directory at `/usr/local/share/rahi/static`. `docker/Dockerfile`
  builds this workspace's cells on the same base, so there is one runtime.
- **B-5 (static assets exist where they are served).** The composer
  serves `RAHI_STATIC_DIR` when it is set and names a directory, and the
  cell's `static_dir()` otherwise. The runtime image sets
  `RAHI_STATIC_DIR=/usr/local/share/rahi/static`, and `docker/Dockerfile`
  copies a workspace cell's static directory there. hello-cell's page
  answers `200` in its image.
- **B-6 (packageable crates).** The rauthy environment template lives in
  `crates/rahi-ops/`, and `cargo package --workspace --locked` passes in
  `release.yml` on every pull request, for all nine chassis crates, and
  locally before a release.
- **B-7 (the consumer stanza, proven).** `release.yml`, on a tag, builds a
  scratch cell outside the workspace from `rahi-* = { git = ..., tag =
  "vX.Y.Z" }`, runs `first-boot`, `migrate`, and `serve` with
  `RAHI_RAUTHY_MODE=none`, and asserts `/readyz`, one governed write, one
  ledgered denial, and `ledger verify`. The git-tag stanza is the proof
  available before publication; once a release is published, the same job
  proves the registry stanza (`rahi-* = "X.Y.Z"`), and a consumer's
  acceptance that needs a published crate is not passed before then (D-1).
- **B-8 (the manifest schema version).** `rahi_types::MANIFEST_SCHEMA_VERSION`
  is either read by `Manifest::parse` from the manifest it parses, so a
  manifest names the schema it was written for, or deleted; today it is
  declared and read by nothing. D-2 selects the first: a manifest names
  its schema version, `Manifest::parse` refuses a manifest that names none
  or names a major other than `MANIFEST_SCHEMA_VERSION`'s, and unknown
  sections stay refused at every level (015 B-1).

## 4. Functional requirements

- **FR-001.** `cargo package --workspace --locked` exits 0.
- **FR-002.** `scripts/k8s-validate.sh` refuses a render whose image is
  `:latest` or outside `ghcr.io/statecrafting/`.
- **FR-003.** A serve test asserts `RAHI_STATIC_DIR` wins over
  `static_dir()` and that a missing directory is a startup error naming
  it, not a silent `404`.
- **FR-004.** The image smoke test (031) asserts hello-cell's `/` answers
  `200` in its image.
- **FR-005.** `release.yml`'s consumer job passes on the tag that lands
  this spec.
- **FR-006 (the manifest names its schema).** Added 2026-09-12 (D-2).
  Kernel manifest tests refuse a manifest that names no schema version, one
  that names another major, and one with an unknown section, each with a
  named `Error::Validation`, and accept one that names the same major with
  another minor. hello-cell's manifest names the version.

## 5. Acceptance criteria

- **AC-1.** `make ci` exits 0, `cargo package --workspace --locked` exits
  0, and `scripts/k8s-validate.sh` passes.
- **AC-2.** A `v*` tag publishes both images and they pull anonymously.
  An operator check, since it needs the registry.
- **AC-3.** Spec 034's AC-2 procedure passes end to end against the
  published hello-cell image: the page, a login through rauthy.
- **AC-4.** Added 2026-09-12 (D-1). After a human publishes a release, all
  nine chassis crates resolve from crates.io at the tag's version and
  `release.yml` proves the registry stanza. An operator check at the
  release checkpoint; until it passes, no document in this repository
  calls a crate published, and no consumer's published-only acceptance is
  marked passed.

## 6. Out of scope

- Helm packaging (032).
- A stamp, template, or upgrade verb (thesis §6).
- Signing images or crates (a later supply-chain spec).
- Publishing `hello-cell` (`publish = false` stays).
- The act of publishing: a human publishes at the release checkpoint
  (D-1); the build implements and tests packaging locally.

## 7. Resolved decisions

The owner decided the publication question and B-8 on 2026-09-12 (decisions
RH-05 and RH-06 of the revision-3 register), and approved the spec on
2026-09-16 (D-3), closing the three questions this section had left open.

- **D-1 (2026-09-12, owner decision RH-05; registry publication).** The
  question was whether chassis crates go to crates.io at each tag, which
  aicortex's 010 D-1 needs, or consumers pin git tags only. The owner chose
  registry publication for all nine chassis crates, `rahi-cli` included
  (aicortex 010 B-2 and hqgit 003 B-2 both omit it, and a cell cannot be
  built without it), with release tags whose version matches the crates'
  and pinned images. It fits the published-dependency expectation of
  aicortex 010 and hqgit 003. This spec's build implements and tests
  packaging locally (B-6, FR-001); publication itself follows the release
  checkpoint and is a human's step (B-2, AC-4). Consumers may prepare their
  code against the planned version, but none marks an acceptance that needs
  a published crate as passed before a release exists. Publishing
  `rahi-cli` needs `rahi-ops` to package, so B-6's template move is now on
  this spec's critical path; the claim-list edit it needs still waits on
  the approval listed below.
- **D-2 (2026-09-12, owner decision RH-06; B-8).** The manifest schema
  version is wired and validated, not deleted: a manifest names the schema
  it was written for and `Manifest::parse` refuses a missing value or
  another major (FR-006). Unknown sections stay refused. The capability
  vocabulary's evolution rule is recorded where the vocabulary lives, spec
  015 D-9. Noticed for the build session and not decided here: the parsed
  model is what `Manifest::hash()` covers (015 D-1), so a new manifest
  member moves the hash of every manifest that gains it, and a volume whose
  chain was rooted before the change refuses to boot until spec 036 lands
  or the volume is recreated. The build session records which, and names
  the member and its TOML key.

- **D-3 (2026-09-16, owner decision; approval).** The owner approved this
  spec and answered the three questions this section left open, so nothing
  in it waits on a further flip.
  - **The runtime image stands (B-4).** A cell in another repository builds
    on `ghcr.io/statecrafting/rahi-runtime:X.Y.Z` rather than copying a
    Dockerfile into its own tree, so the pinned rauthy, the non-root user,
    `/data`, and the entrypoint are patched in one place. Rejected: a
    Dockerfile each consumer vendors, which drifts from the rauthy the
    chassis pins.
  - **The template moves (B-6).** `docker/rauthy.env.template` becomes
    `crates/rahi-ops/rauthy.env.template`, which removes it from spec 031's
    `establishes` and from the `docker/*.template` hashed input, the kind of
    claim-list edit spec 034 D-10 made. It is on the critical path: without
    it `cargo package` fails on `rahi-ops`, so neither it nor `rahi-cli`
    can be published, and RH-05 cannot be met.
  - **`latest` is never published.** A tag pushes `X.Y.Z` only, `deploy/k8s`
    names a version, and FR-002 makes the validation script refuse a render
    that names `:latest` or an image outside `ghcr.io/statecrafting/`.
  - The first release is `v0.1.0`: the version every crate already declares,
    and the one aicortex pins (`rahi-* = "=0.1.0"`, its 010 D-1).

- **D-4 (2026-09-16, build session; the member D-2 asked this build to
  name).** The manifest schema version is `Manifest::schema_version`, the TOML
  key `schema_version` at the document root, above the first table. It is
  `Option<String>` in the type so that a manifest naming none is refused by a
  message about the schema rather than by serde's about a missing field;
  `validate` refuses a missing value, a value that is not
  `MAJOR.MINOR.PATCH`, and a major other than
  `rahi_types::MANIFEST_SCHEMA_VERSION`'s, and reads a later minor of the
  same major. The consequence D-2 noticed holds: the member is inside
  `Manifest::hash` (015 D-1), so every manifest's hash moved, and a volume
  whose chain was rooted before this change refuses to boot with
  `Error::Integrity` until spec 036 lands or the volume is recreated. Nothing
  is deployed from this repository and no image is published, so the volumes
  that exist are development ones: `docker/.data` under the compose file and
  whatever a contributor kept. Delete them. Rejected: leaving the member out
  of the hash, which would let a cell change the schema it claims without the
  chain noticing, and the chain's whole purpose is to notice.
- **D-5 (2026-09-16, build session; reads B-4's "one runtime").** Docker has
  no include, so `docker/runtime.Dockerfile` repeats `docker/Dockerfile`'s
  runtime stage rather than either file building on the other, and
  `image.yml` refuses a build whose two `ARG RAUTHY_IMAGE` lines differ. That
  is what keeps them one runtime. Rejected: `docker/Dockerfile` building
  `FROM ghcr.io/statecrafting/rahi-runtime`, which cannot build before that
  image is published and would make a local `docker build` reach the
  registry, against spec 031's acceptance.
- **D-6 (2026-09-16, build session; reads B-5).** `RAHI_STATIC_DIR` wins over
  the cell's `static_dir()`, and a directory either of them names that does
  not exist is `Error::Config` at boot: `serve` checks it before the store is
  opened, so the failure costs nothing and names the path. An image always
  carries the directory (the recipe creates it), so a cell built without
  `RAHI_STATIC_SRC` serves `404` for its page rather than refusing to start;
  that is the empty cell's ordinary case and hello-cell's warning.
- **D-7 (2026-09-16, build session; reads B-7).** The consumer cell is three
  files in `.github/consumer-cell/`, not a heredoc inside the workflow: a
  heredoc in a YAML block scalar cannot close at column zero, and a fixture
  in the tree is reviewable, hashed, and diffable. `release.yml` copies it
  and substitutes `__REPO__`, `__KEY__` (`rev` or `tag`), and `__REV__`. The
  job runs on a tag and on `workflow_dispatch`, so the stanza can be proven
  against a commit before anyone tags it; FR-005 is the run on the tag
  itself.
- **D-8 (2026-09-16, build session; what a release still needs from a
  human).** This build publishes nothing and cannot: `cargo publish` and the
  tag push are the human's steps at the release checkpoint (D-1), in
  dependency order `rahi-types`, `rahi-store`, `rahi-ledger`, `rahi-kernel`,
  `rahi-idp`, `rahi-edge`, `rahi-ops`, `rahi-cli`, `rahi-harness`. A version
  bump edits two places, `[workspace.package] version` and the `version` of
  each `rahi-*` entry in `[workspace.dependencies]`, so `release.yml`
  refuses a tree where they disagree, and refuses a tag that does not name
  the version the crates carry.

- **D-9 (2026-09-16, build session; what this build could not run).** AC-1
  passed here: `make ci`, `cargo package --workspace --locked`, and
  `scripts/k8s-validate.sh` all exit 0, and FR-001 through FR-004 and FR-006
  have tests that run in CI. Three criteria need a release that does not
  exist yet and are recorded rather than run, the way spec 032 D-1 records
  its rollout check: AC-2 (a `v*` tag publishes both images and they pull
  anonymously), AC-3 (spec 034's AC-2 procedure against the published
  hello-cell image), and AC-4 (the nine crates resolve from crates.io and
  `release.yml` proves the registry stanza). FR-005 is the same: the consumer
  job runs on the tag. What this build could prove locally, it did: the
  runtime image builds and carries rauthy, the entrypoint, the non-root user,
  an empty `/usr/local/share/rahi/static`, and no cell; hello-cell's image
  builds with `RAHI_STATIC_SRC` and `docker/smoke.sh` with `SMOKE_PAGE=1`
  answers `200` for its page. Until a human tags and publishes, no document
  in this repository calls a crate published (D-1).

- **D-10 (2026-09-16, build session; what the consumer job had to learn).**
  Run against a local build of the fixture before it was written into the
  workflow, the cell answered `403` to its granted write. The refusal was
  spec 020 B-4's double-submit check, not the kernel: an unsafe method needs
  the CSRF cookie echoed in `X-CSRF-Token`, and `/readyz` mints no cookie
  because the probes are mounted outside that layer (020 B-2). The job now
  does what a page does: a safe request on a guarded route first, which is
  also the denial assertion, then the write with the cookie and the header.
  A consumer reading `docs/design/01-consumer-contract.md` meets the same
  rule. Proven locally against path dependencies (the code of the fixture)
  before the tag proves the git stanza: `/readyz` ready, `403` with
  `kernel:<nonce>:1:000000000000`, `200` and `wrote 1`, a clean stop, and
  `ledger verify` with two resident records.

## Verification

```verify:cli
cargo package --workspace --locked
scripts/k8s-validate.sh
cargo test -p rahi-cli --locked
cargo test -p rahi-kernel --locked --test manifest
```
