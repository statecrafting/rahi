# Patched adoption: delivery plan

Version 3, 2026-09-23. Owned by `specs/043-patched-dependency-adoption/spec.md`
(draft, revision 4). Maintained by the rahi owner. It follows 043 revision 4;
version 2 followed revision 3, version 1 revision 2.

**Nothing here starts before the owner approves the exact 043 revision**
named in `09-owner-decision-packet-rev4-2026-09-23.md` (which supersedes
`06-...` and `07-...`). Publication is a
separate, later approval (section 4).

## 1. Dependency graph and what stays outside

| work | relation to 043 | what this plan does about it |
|---|---|---|
| 044, N=3 split composition (draft PR #76, branch `corpus/044-n3-split-composition`, head `ecd28cc`) | Independent of 043's N=1 delivery (043 D-2). Blocked on its own by spec 000's `one-deployment-unit` anchor (owner packet decision 4) and on an N=3-qualified hiqlite release (Track S7). When 044 is revised it should add `depends_on: 043`, because an N=3 rahi pod inherits 043's relocated app store, fence, durable revocation and stop outcomes. | Nothing in this session edits 044. The edge is a recorded request to the 044 branch. |
| 040, runtime identity and binding surface (draft, on the primary checkout's `corpus/040-binding-schema`) | No dependency either way. Coupling: 040's binding document reports runtime identity; once 043 lands it should report the pinned `hiqlite-patched` and Rauthy image identities and the store layout (`app-store` plus fence). | Recorded here; 040's text is not edited. |
| 041, deployment epochs (draft, depends on 036 and 040) | 041 `extends` `crates/rahi-ops/src/migrate.rs`. 043 gates `migrate` (B-4a) at the CLI entry, not in `migrate.rs`, so the two do not edit the same unit; whichever lands second rebases. | Recorded; no 041 edit. |
| hiqlite Track S7 and hiqlite 033/034 | N3-only (see `04-patched-adoption-producer-requests.md` section 2). | None. |

## 2. Implementation sequence (after approval)

One spec, one branch: `feat/043-patched-dependency-adoption` from `main`
after the draft PR #75 lands its approval commit (or is merged). One PR.
Each step gates before its commit: `make gate` with the pinned spec-spine
0.20.0 (`target/release/spec-spine`, since the shared global may be a
different version), and `cargo test`, clippy and fmt for the touched crates.
Commits are scoped `feat(043): ...`, with regenerated shards in the same
commit.

| # | step | files (owning spec, edge) | depends on |
|---|---|---|---|
| 1 | Flip 043 `approved` to `in-progress`. Record 011 D-13 (the dependency exception, verbatim from 043 D-1) and the 037 section 6 cross-reference. These two are the only edits to other specs' text, and the approval authorizes them. | specs 043, 011, 037 | approval |
| 2 | Graph: the aliased dependency, drop `[patch]` and the hiqlite allow-git, FR-001's test; 016's extension test asserts the refusal as an error. | `Cargo.toml`, `Cargo.lock`, `deny.toml` (010); `rahi-store/tests/blob.rs` (016); `rahi-store/tests/dependency_identity.rs` (new) | 1 |
| 3 | Store: `NodeFailed` as a terminal error; the revocation, floor, prune-horizon and transition tables, chassis-created like `lease_fence`. | `rahi-store/src/error.rs`, `store.rs`, `lib.rs` (011) | 2 |
| 4 | Layout and gate: `Config::hiqlite_dir()` answers `app-store`, a `legacy_hiqlite_dir()` names the fence path; the rendered Rauthy environment at `<data>/rauthy-env/rauthy.env` (P-10); `cell_lock.rs` with `cell.lock` and `transition.lock`, exclusive and shared, close-on-exec; the no-replace rename helper (FR-011) and the whole-file `link` publisher; the one B-4a gate function, refusing `HQL_CACHE_LEGACY_MOVE_ASIDE`, `HQL_DANGER_RAFT_STATE_RESET` and `HQL_BACKUP_RESTORE` in its own environment, then locks before reads; every entry point, `first-boot` included, calls it; both fences on a fresh volume; debris accepted and reported; FR-008's enumeration test; update the CLI tests that assert on `<data>/hiqlite` to assert on the app store. | `rahi-types/src/config.rs` (010); `rahi-ops/src/lib.rs`, `cell_lock.rs` (new); `rahi-cli/src/lib.rs`, `serve.rs`, `tests/cli.rs` (030); `rahi-cli/tests/shutdown.rs` (035); `rahi-ops/src/supervise.rs`, `first_boot.rs`, `rauthy_env.rs` (031); `rahi-ops/tests/cell_lock.rs` (new) | 3 |
| 5 | Revocation: SQL `jti` and subject rows in the revocation's `txn`; the floor; the admission refusals (no `iat`, claims outside `u64`, `exp < iat` by checked subtraction, the hard `exp - iat > 86,400` that reads no manifest, `exp - iat > L`, future `iat`, every overflow); P-9 as the owner chose it: (i) no pruning code path and `rahi_revocation_rows{kind}`, or (ii) pruning at `revoked_at + V(L_max)` with the persisted watermark in the same `txn`, the horizon raised at boot, and fail-closed admission below the watermark; P-11 only if accepted; FR-014's clock test (including revision 3's rule failing on the counterexample) and tests for AC-6 (a) to (d) and (f). | `rahi-idp/src/revoke.rs` (038), `bearer.rs` (025), `lib.rs` (021); `rahi-idp/tests/revocation_durable.rs` (new) | 3 |
| 6 | The verb: B-4's preconditions printed before T1; T0 to T3 with T1's guard, quiescence and identity checks and the supervisor fence, T2's identity plan into the app store and the aside directory, recovery, FR-013's evidence helper with per-occurrence names, `--abort` with its named side effects; fault points after every intent, action, single rename, fence step and evidence move; FR-012's pre-043 interleaving helper; the verb in 030's `VERBS`. | `rahi-ops/src/upgrade.rs` (new), `lib.rs` (030); `rahi-cli/src/lib.rs`, `verbs.rs` (030); `rahi-ops/tests/upgrade.rs` (new) | 4, 5 |
| 7 | Supervisor consent (T4) and `serve`'s `done` (T5); the refusal of an operator's `HQL_CACHE_LEGACY_MOVE_ASIDE`. | `supervise.rs` (031), `serve.rs` (030) | 6 |
| 8 | Stop: `stop.json`, phase timings, the outcome and its reasons, the next-boot classification and `rahi_previous_stop{outcome,cause}`; exit status carried through every path (FR-009); graces to 40 s and 50 s; the composition test. | `rahi-ops/src/stop.rs` (new), `supervise.rs` (031), `serve.rs` (030), `deploy/k8s/statefulset.yaml` (032); `rahi-cli/tests/stop_budget.rs`, `stop_outcome.rs` (new) | 4 |
| 9 | Terminal: `serve` on `NodeFailed`. | `serve.rs` (030); `rahi-cli/tests/terminal.rs` (new) | 3, 8 |
| 10 | Restore: the fence before the reset; refuse a volume with a legacy store; the pending floor in the marker for a pre-043 archive always, for every archive if P-7 is accepted; the first open raises it before `/readyz`. | `rahi-ops/src/restore.rs` (030) | 4, 5 |
| 11 | Image: the patched Rauthy pin in both Dockerfiles, version and hash assertions on both architectures. | `docker/Dockerfile` (031), `docker/runtime.Dockerfile` (039), `.github/workflows/image.yml` (031) | 2 |
| 12 | Live: an upgrade leg that prepares a volume and an archive with the v0.2.0 image by digest, then runs AC-3, AC-3a on a fresh volume, AC-4 (d) at every persistent state including direct `supervise`, AC-4 (g) recorded, AC-5's v0.2.0 legs including the stop and start races, and AC-9, strict, zero skipped required legs, on both architectures the workflow builds. | `.github/workflows/live.yml` (037) | 6, 7, 10, 11 |
| 13 | Docs: README upgrade procedure and B-4's two preconditions word for word (every old process stopped and removed with its restart sources disabled, direct supervisors included; neither destructive hiqlite variable in any old process's environment), the fences and the supervisor fence's exact residual guarantee, `--abort` and rollback, the floor's consequence and clock assumption, P-9's chosen retention or watermark and its cost, Rauthy's cache-only losses and the ban export and re-import step, the restore boundary including revived refresh credentials and an in-place restore's surviving cache, the graces, `deploy/n3` unqualified, the cache-group correction; consumer contract. | `deploy/README.md` (032), `docs/design/01-consumer-contract.md` | all |
| 14 | AC-7's workload series and AC-7a, recorded with per-run phases. | tests of step 8, run in CI | 8, 11 |
| 15 | Independent review over base..head; remediate; PR; CI green on the final head. The PR can merge with 043 at `in-progress` (implemented, AC-4 (g) recorded as not met). | | all |
| 16 | **Release gate (B-4b).** When a hiqlite-patched release with 035 B-5 and a Rauthy image rebuilt on it are published: record their identities as D-P1 was recorded, prepare the governed repin amendment of B-1 and B-2 with the exact artifacts and evidence, and put it to the owner. Only after that decision: repin, rerun AC-1, AC-2, AC-4 (g) and AC-9, flip 043 `complete`, merge, `make verify SPEC=043` on the merged revision. | specs 043 (and 011 D-13's artifact list), `Cargo.toml`, `Cargo.lock`, both Dockerfiles | 15, producer artifacts, owner decision |

Parallelism: steps 2 then 3 are serial; 4 and 5 can run in parallel once 3
is committed; 6 needs both; 8 can start after 4; 11 after 2. Agents own
disjoint files as listed; only the lead edits spec files, `Cargo.lock` after
step 2, and documents.

## 3. Release 0.3.0: qualification (prepared alongside)

- **Version.** `0.3.0`: published are crates and images `0.2.0`, tag
  `v0.2.0`; the dependency rename, the relocated store and the transition are
  not patch-level. Recheck immediately before tagging that `0.3.0` is unused
  on crates.io for all nine chassis crates, on GHCR for `rahi`,
  `rahi-runtime` and `rahi-hello-cell`, and as a git tag.
- **Gate.** No candidate before step 16 completes.
- **Candidate evidence.** `make ci` on the release commit; the live workflow
  strict on both architectures with zero skipped required legs; the image
  workflow on both architectures; AC-3 to AC-9 records with run ids.
- **Inventory.** The nine chassis crates at `0.3.0`;
  `ghcr.io/statecrafting/rahi:0.3.0`, `rahi-runtime:0.3.0` and
  `rahi-hello-cell:0.3.0` on both architectures, each with the patched Rauthy
  pin; release notes carrying the upgrade procedure, the floor's
  consequence, the precondition, the unqualified `deploy/n3`, and C-1.
- **Candidate qualification** is reported as such: it proves the packaged
  candidate, not a published consumer.

## 4. Publication (a separate approval)

Requested only once section 3 is concrete, naming the exact commit, tag
and artifact list. After that approval, through CI (the house mechanism, 039
D-11), never from a laptop:

1. Publish crates and images.
2. Prove anonymous accessibility: an unauthenticated registry read of each
   image and tag, and an unauthenticated crates.io download of each crate.
   GHCR packages start private (a 404 there is not a 403); make them public
   and re-read.
3. Prove a fresh registry-only consumer: a new repository, `rahi-*` at
   `0.3.0` from crates.io with no path, git or `[patch]` override, its own
   lockfile resolving `hiqlite-patched`, running on `rahi-runtime:0.3.0`, and
   exercising login, a write, a denial, backup, restore and `ledger verify`.
4. Report candidate qualification (section 3) and published-consumer proof
   (this section) separately.
