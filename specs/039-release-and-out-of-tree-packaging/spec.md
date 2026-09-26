---
id: "039-release-and-out-of-tree-packaging"
title: "Releases and out-of-tree packaging: a tagged version a consumer pins, images that exist under the names the manifests use, and a runtime image a cell in another repository builds on"
status: approved
kind: tooling
domain: ops
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: complete
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
  - ".github/publish-crate.sh"
  - "crates/rahi-ops/rauthy.env.template"
  - "CHANGELOG.md"
  - ".github/consumer-cell/"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.package" }, nature: additive }
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
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
  - { spec: "043-patched-dependency-adoption", unit: "crates/rahi-cli/tests/stop_outcome.rs", nature: additive }
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

- **D-11 (2026-09-16, owner decision; supersedes the mechanism of B-2 and
  D-8).** B-2's last sentence and D-8 made `cargo publish` a command a
  human runs locally at the release checkpoint, and the first `v0.1.0` tag
  was pushed under that reading. The owner reviewed the seven sibling
  statecrafting Rust repositories and found the opposite already
  established: `spec-spine`, `attest-ledger`, `action-gate`,
  `canonical-keysort-json`, `tenant-emit`, `tenant-tail`, and
  `trust-window` each publish from a tag-gated `publish-crates` job in
  their `release.yml`, over `secrets.CARGO_REGISTRY_TOKEN`, idempotent on
  an already-published version. This repository already carries that
  secret. The owner chose the house mechanism, so rahi publishes the way
  its siblings do rather than by a local command whose evidence lives only
  on one machine. What does not change is whose act a release is: the
  human still tags, and the tag still drives publication, exactly as B-3
  already had it drive the images. B-2's and D-8's text is left as written
  so the record shows what was asked as well as what was done; the
  mechanism they name is superseded here and nowhere else. Two
  consequences. `publish-crates` needs `package` and `consumer`, so a tag
  that cannot package or compose publishes nothing, and publication is no
  longer reversible by a human's choice not to run a command: a `v*` tag is
  the irreversible act. And AC-4 becomes provable rather than recorded:
  D-9 deferred it because `release.yml` proved only the git stanza, and
  `consumer-registry` now builds the same cell from `= X.Y.Z` on crates.io
  after `publish-crates` succeeds. The proof body both jobs run moved to
  `.github/consumer-cell/prove.sh` so the two stanzas carry one evidence
  path, and `.github/publish-crate.sh` holds the idempotent publish,
  matching `attest-ledger`'s factoring.

- **D-12 (2026-09-16, build session; what the first release found).** The
  `v0.1.0` run published all nine crates and `consumer-registry` proved the
  registry stanza, so **AC-4 passes**: a cell built outside this workspace
  from `= "0.1.0"` booted, was denied with a decision id, wrote, and
  verified a two-record chain. D-9 had recorded AC-4 as unrunnable; it is
  run now. Two things the release taught. First, crates.io rate-limits the
  creation of new crate *names* much harder than new versions, so a first
  release of nine at once meets a `429` that no later release will: six
  names went through, then `rahi-ops`, `rahi-cli`, and `rahi-harness` each
  needed their own window, costing three red runs that the idempotent skip
  made safe to retry but could not avoid. `.github/publish-crate.sh` now
  reads the moment the body names and waits for it, bounded to four
  attempts and half an hour, so the job finishes on its own. Second,
  **AC-2 does not pass**: `image.yml` built and pushed both images on the
  tag, but the GHCR packages are created private, and an anonymous
  manifest fetch answers `403` where the same fetch against
  `ghcr.io/sebadob/rauthy:0.36.2` answers `200`. B-3's "public" is a
  property of the package, not of the push, and nothing in a workflow can
  set it: making a package public is an owner action in GitHub's package
  settings. AC-2 and AC-3 (which pulls the hello-cell image) stay open
  until the owner takes it, and no document here calls the images public
  meanwhile.

- **D-13 (2026-09-17, release preparation under B-1 and B-2).** The
  coordinator selected 0.2.0 under Bart's standing build, push, PR, merge,
  and release authority for completed 037 at
  `ea6d0da125aaafe0927570410a8a1b0ee9d1286e`. Its key-set, backup
  authentication, recovery, and export API changes cross the consumer
  compatibility boundary, so B-1 requires a minor release; 0.1.1 is
  rejected. This is maintenance of the completed release mechanism, not a
  new lifecycle completion. The inherited version, nine internal version
  requirements, lockfile, changelog, README, and current consumer guidance
  move together. The added `workspace.dependencies` edge makes D-8's
  second version-edit surface explicit; no third-party pin changes.
  Existing key sets are not relabeled compatible: backup needs an
  origin-bound passkey, and neither automatic old-key-set migration nor
  cross-origin recovery is supplied. Historical records remain intact.

  This preparation commits no invented release SHA. The consumer contract
  retains B-2's in-repository release identity through a prospective
  [`v0.2.0` source link](https://github.com/statecrafting/rahi/tree/v0.2.0).
  After publication, the release revision is the tested merged `main`
  commit to which the annotated `v0.2.0` tag resolves. The link asserts no
  publication; the coordinator's exact SHA and date in forge metadata
  supplement this identity. The preparation names the real 037 merge as
  its implementation basis and labels 0.2.0 a
  candidate. AC-2 through AC-4 and FR-005 must be checked for the new tag;
  0.1.0's evidence cannot pass them for 0.2.0. Local Cargo packaging can
  stage sibling versions; an out-of-tree proof using extracted packages
  and explicit Cargo path patches for only those nine candidate crates is
  packaging evidence, never registry publication. hiqlite and all other
  dependencies remain published registry packages.

  Release and image triggers were read before editing: publication is
  gated on `v*`; main builds and smokes images without pushing them. No
  production deployment trigger is present in these workflows. Deployment
  references stay at their existing 0.1.0 target. A new artifact is not a
  consumer rollout. The changelog and consumer guidance disclose the
  independent hiqlite 0.14.0 stale-release defect after TTL takeover,
  unverified full-node second-lease restart, and resident-only ledger ID
  classification after sealing. Neither 037 nor this release fixes them;
  lifetime idempotence needs separately governed design and migration.
  Drafts 036, 038, 040, and 041 and the lease/ledger implementation remain
  untouched. This decision changes no requirement and grants no waiver.

- **D-14 (2026-09-20, correction session; reads AC-3 against B-3 and §6).**
  AC-3 runs spec 034's AC-2 procedure "against the published hello-cell
  image", and no such image had ever been published: `image.yml` builds
  `hello-cell:smoke`, smokes its page for FR-004, and pushes only
  `rahi:X.Y.Z` and `rahi-runtime:X.Y.Z`. AC-3 was therefore unsatisfiable
  by construction, for `v0.1.0` as much as for this release, and D-12's
  reading that it was waiting only on the owner's GHCR visibility action
  was incomplete: the artifact did not exist either.

  §6 excludes "Publishing `hello-cell` (`publish = false` stays)". Its
  parenthetical names the Cargo manifest key, which is at
  `apps/hello-cell/Cargo.toml:10` and governs crates.io alone, so that
  exclusion is about the **crate**. Read that way, §6 and AC-3 are both
  true as written: the crate is never published to the registry, and the
  image AC-3 names is published to GHCR. The alternative reading, that §6
  excludes the image too, would make an approved acceptance criterion
  impossible to satisfy by any means, which is not a reading an approved
  criterion can bear. No requirement text is amended here; B-3's sentence
  stays true, since publishing a third artifact does not falsify a
  statement about two.

  A tag therefore also publishes
  `ghcr.io/statecrafting/rahi-hello-cell:X.Y.Z` on the existing release
  path: the same `build` matrix, the same `docker/Dockerfile` with the
  three build args the smoke already uses, pushed by digest per
  architecture and joined by the same `manifest` job, gated on `refs/tags/v`,
  and never `latest`. The published digest is then pulled back and smoked
  with `SMOKE_PAGE=1`, so the artifact AC-3 is run against is the artifact
  that was tested rather than a second build of the same recipe; the cell
  and runtime images do not do this today and are left as they are, since
  changing them is not this correction's business. `deploy/k8s` is
  untouched and stays pinned at its existing 0.1.0 target (D-13). AC-3 also
  needs a real login through rauthy, which is an operator step 034 D-1
  already recorded as such, and both AC-2 and AC-3 still need the packages
  made public, which is an owner action in GitHub's package settings that
  no workflow can take (D-12).

- **D-15 (2026-09-20, correction session; reads AC-2 and B-3 against what
  the workflow actually tested).** The same build-versus-published gap
  D-14 closed for hello-cell was open for the cell and runtime images, and
  had been since `v0.1.0`. `image.yml` smoked `rahi:smoke`, a locally
  loaded build, and then *built again* for the push. Two builds of one
  recipe from one cache are very probably identical bytes, and "very
  probably identical" is not what AC-2 claims. Every published artifact is
  now pulled back by the digest that was pushed and exercised as itself.

  The instrument differs by artifact, because the artifacts differ. The
  cell image is a cell, so it is smoked: temp volume, `/readyz`, discovery
  through the proxy, the issuer asserted, clean exit on SIGTERM. The
  runtime image **has no cell and cannot boot alone** (B-4), so expecting
  a boot from it would be the wrong instrument and a failure there would
  mean nothing. What B-4 claims of it is its contents and that a consumer
  can build on it, so both are checked against the pushed digest: that it
  carries no `/usr/local/bin/rahi`, that the pinned rauthy, the entrypoint
  and an empty static slot are present, that it runs as uid 10001, and
  that `RAHI_DATA_DIR`, `RAHI_RAUTHY_BIN` and `RAHI_STATIC_DIR` are the
  values a consumer inherits; and then a consumer is composed on it from
  the two published digests, adding a binary at `/usr/local/bin/rahi` and
  a page at the static directory exactly as B-4 describes, and that is
  smoked. The runtime is therefore proven by use, not only by inspection.

  **Architecture.** These steps run inside the `build` matrix, whose two
  jobs are native runners (`ubuntu-latest` for amd64,
  `ubuntu-24.04-arm` for arm64), and each asserts against
  `steps.*.outputs.digest` for its own platform. The evidence is native
  execution bound to one platform digest and establishes nothing about the
  other; nothing here runs under emulation, and no claim about one
  architecture is derived from a run on the other. B-3's "both
  architectures" is a property of the manifest list rather than of either
  job, so the `manifest` job asserts that each of the three published tags
  resolves to both `linux/amd64` and `linux/arm64`. A tag that silently
  published one architecture now fails there instead of being found by a
  consumer on the other.

  What this does not reach: AC-2 also requires the images to **pull
  anonymously**, which is a package visibility setting and an owner action
  (D-12), and these checks run authenticated inside the workflow. A green
  `image` run is evidence that the published bytes are correct, never that
  they are public.

- **D-16 (2026-09-20, release coordination under B-1 and B-2).** The 0.2.0
  CHANGELOG section carried an instruction rather than a fact: the
  coordinator "must record the final tested main revision and release date
  when creating the annotated `v0.2.0` tag". Both are now known, so the
  instruction is replaced by what it asked for. The revision recorded is
  `8d686bdf7b10d4ee24357a31b4de25ae87cc5b2a`, the merge of this spec's
  reconciliation, chosen because it is the last revision on `main` whose
  `ci` and `image` runs both passed and whose tree the live suite had
  already run against as pull request #71. The release-revision commit adds
  only that record on top of it and is what the tag names, so the tagged
  tree and the tested tree differ by this paragraph alone.

  Two alternatives were rejected. Tagging `8d686bd` directly would leave
  the released tree saying "release candidate (not published)", which the
  tag falsifies. Recording the revision after the tag would put the fact in
  a commit no release artifact contains.

  The paragraph denying registry availability and consumer deployment is
  kept rather than deleted with the heading, because at the moment that
  commit is written neither is true: the nine crates reach crates.io only
  when the tag's `release` run publishes them and its registry consumer
  stanza passes (AC-4), and the three images pull anonymously only after an
  owner changes package visibility (AC-2, D-12), which no workflow can do.
  A release heading is not evidence of either; the release notes record
  those outcomes when they occur.

- **D-17 (2026-09-20, release checkpoint; AC-3 run against the published
  artifact).** AC-3 was executed against
  `ghcr.io/statecrafting/rahi-hello-cell:0.2.0`, pulled by digest
  `sha256:494a566d1ea97aa348a0ccbe0adda4a87522f0b67a87518a46f980d68b66f06b`
  with docker credentials forced off, so the artifact under test is the
  published one and the pull was anonymous. The cell answered `/readyz`,
  `/` and `/.well-known/openid-configuration` with 200; `/api/notes` was
  401 before login; a full authorization-code login with PKCE through the
  cell's own `/auth` proxy succeeded, rauthy logging `JWT Token issued
  hello-cell (authorization_code)`; `/api/notes` was then 200, and an
  authenticated `POST` returned 201 with the note stamped `revision: 1`.
  The login was driven by `rahi-harness` **0.2.0 resolved from crates.io**
  (`source = "registry+https://github.com/rust-lang/crates.io-index"`), so
  the same run is also an out-of-tree registry-consumer proof.

  Running it found a defect in the procedure D-14 added: it published
  `-p 8080:8080`, but the image exposes **8443** (`docker/Dockerfile`
  `EXPOSE 8443`, and `docker/smoke.sh` maps `${port}:8443`), so the
  documented command could not serve the page at all. Corrected to
  `-p 8080:8443`, with the reason stated in the README rather than left as
  a bare number, and the bootstrap-administrator note added because the
  login step needs an account and the container prints one only on first
  boot. This is a documentation correction; no behavior, functional
  requirement or acceptance criterion changes.

  What this does not reach: AC-2 is still unsatisfied. `rahi-hello-cell`
  became public on creation because a new package inherits the public
  repository's visibility, but `rahi` and `rahi-runtime` predate it and
  remain private: with credentials forced off both answer `unauthorized`,
  and AC-2 names those two images. That remains the owner action D-12
  records.

- **D-18 (2026-09-25, release coordination under B-1 and B-2; the 0.3.0
  release, granted by the owner's work order of 2026-09-25).** 0.3.0
  carries 038, 045 and 046. All nine versions and internal requirements
  move to 0.3.0, and `deploy/k8s/kustomization.yaml` moves its `newTag` to
  `0.3.0`: it still named `0.1.0`, which B-3's "names a version" permits
  but which pointed a fresh deployment at an image two releases old. The
  0.3.0 changelog section also records 038, which merged after `v0.2.0`
  and had been left out of the drafted section.

  Unlike 0.2.0 (D-16), the release lands as one pull request, and the tag
  names its squash merge directly. The repository allows squash merges
  only, so the tested pull-request tree becomes one `main` commit with the
  same tree, and the `ci` run on that commit passes before the tag is
  created: B-2's "a `main` commit whose `make ci` passed" is met by the
  tagged commit itself, with no record-only commit between the tested tree
  and the tag. The record therefore names the tested revision as "the
  commit the tag names" rather than by hash, because a commit cannot
  contain its own hash; the hash is stated in the annotated tag's message
  and the release notes. Rejected: D-16's second commit, which makes the
  tagged tree differ from the tested one by a paragraph and costs a second
  full CI cycle for no additional evidence; and recording the hash after
  the tag, which D-16 already rejected.

- **D-19 (2026-09-26, owner release grant; 0.4.0).** The owner explicitly
  granted preparation, merge, signed tag, publication and anonymous
  verification of 0.4.0 after PR 91 is complete and green. The release
  carries spec 043 at the `in-progress` boundary D-22 authorizes, uses
  exactly `hiqlite-patched`, `hiqlite-wal-patched` and
  `hiqlite-derive-patched` `0.15.0-patched.3`, publishes all nine chassis
  crates and all three images, and proves a registry-only consumer with no
  path, Git or patch override. Patched.4 is excluded. The release record,
  README and consumer contract state D-12's two limits, D-20 (c)'s unclean
  shutdown limit, D-21 (c)'s three unexecuted live legs and the lease-wake
  behavior. The annotated signed tag is the publication checkpoint. A
  GitHub Release object remains owner-only and is not created by this grant.

## Verification

```verify:cli
cargo package --workspace --locked
scripts/k8s-validate.sh
cargo test -p rahi-cli --locked
cargo test -p rahi-kernel --locked --test manifest
```
