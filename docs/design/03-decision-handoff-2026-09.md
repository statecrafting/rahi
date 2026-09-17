# Decision handoff, 2026-09-17

Version 0, 2026-09-17, for review. This note was prepared during the
spec-spine 0.20.0 governance upgrade, alongside the status reconciliation in
`01-consumer-contract.md` section 12. It is planning input only.

**Nothing here is approved.** Every item below is a **Recommendation**: a
proposal this note makes, with the evidence it rests on and the alternatives
it rejects. None has been accepted by the rahi maintainer, none amends a
spec, and none approves a draft. A recommendation becomes a decision when
the owner records it as a dated `D-n` in the spec it governs, which is where
`02-operational-prerequisites.md` section 8.1 put the decisions of
2026-09-12. Until then the specs stand exactly as written.

The six items are independent: accepting one implies nothing about the rest.

## 1. Shutdown needs a validated total budget

**Evidence.** Spec 035 D-7 measured the arithmetic and recorded that it does
not close. `serve`'s denial drain (026 B-7, default ten seconds) plus 035's
own bound (five) is fifteen seconds, and the supervisor waits `SERVE_GRACE`,
fifteen seconds (031 D-4). The check fails by equality: fifteen does not
exceed fifteen. The worst case is worse, because `serve` also gives open
connections `DRAIN_BUDGET` (ten seconds, after the stream drain), so a stop
can take twenty-five seconds before the store shuts. Part of the gap
predates 035: 031 D-4's "the fifteen seconds the supervisor waits always
suffice" was written before 026 added a stream drain in front of the
connection budget.

**Recommendation.** Treat the stop budget as one validated total rather than
four independently chosen constants, covering the configurable drains
(`DRAIN_BUDGET`, the stream drain, the denial drain), the store's own
shutdown, rauthy's termination (031 B-n gives it five seconds after
SIGTERM), and the orchestrator's grace period. Two properties are worth
asking for: the total is computed from the parts rather than restated, and
a configuration whose parts exceed the enclosing grace is refused at
preflight rather than discovered at a stop.

**What it costs.** Raising `SERVE_GRACE` to cover twenty-five seconds also
means raising the StatefulSet's thirty-second
`terminationGracePeriodSeconds`, since it must cover `SERVE_GRACE` plus
rauthy's ten. 031's text fixes fifteen seconds, so this is an amendment to a
`complete` spec and is the owner's alone; 035 deliberately amended nothing
in it, which is why the gap is recorded rather than closed.

**Rejected alternative.** Leaving it as counted loss. 035 D-5's guard counts
every owed decision as abandoned when the runtime ends, so the loss is
counted and not silent, and `deploy/README.md` says so. That is honest, and
it is not the same as correct.

## 2. Move the `.env` exclusion into tracked policy

**Evidence.** `.env` is excluded in this clone only, through
`.git/info/exclude`, which is per-clone and not committed. A second clone,
a fresh checkout, or a worktree created before the exclusion was added does
not have it.

**Recommendation.** Move the exclusion into `.gitignore`, which is tracked,
so the protection travels with the repository. Keep the file itself out of
the tree and out of the note: what is tracked is the rule, never the
secret. `docker/` and `deploy/` already carry the shape of a documented
example, so an `.env.example` carrying keys with empty or obviously
placeholder values is the companion worth considering; it is a separate
question from the exclusion and does not block it.

**Rejected alternative.** Leaving it per-clone. The cost of the current
state is not theoretical: the exclusion is invisible to everyone who did not
add it, and the failure mode is committing a secret.

## 3. Prepare 036 for approval

**Evidence.** 036's section 7 reads "None yet. Before approval a human
decides", and lists five questions. The spec is `draft` and
`implementation: pending`; `registry plan` calls it dependency-ready, and
041 is blocked on it.

**Recommendation.** Answer the five in the spec as dated `D-n` entries
before the flip, in this shape:

- **adoption is a flag of `migrate`**, not a verb of its own, which keeps
  spec 030 AC-2's exact verb list intact and amends nothing;
- **the transition record carries the whole canonical model when it fits,
  and its hash otherwise**, with the bound written down, so a manifest
  record cannot grow without limit;
- **B-8's additive marking is kept**: a store ahead of the binary is
  accepted when every difference is additive and refused otherwise, rather
  than always-refuse or today's always-accept;
- **a restart during an N=3 rollout refuses**, rather than booting on the
  previous manifest for a bounded window. Strict is the smaller claim, and
  a bounded window is a property nothing in the corpus currently verifies;
- **the genesis-anchor change in B-4 is approved explicitly or dropped.**
  It moves the anchor from the booted manifest to the stored genesis record
  plus the ledger key's signatures, which is a change to spec 013's
  verification. 036 itself says a human approves that one explicitly.

Two additions are worth making at the same time. B-7's checksum per
migration has no baseline for rows recorded before the spec: say in the
spec what the first run does with a historical row that has no checksum,
rather than leaving it to the implementation. And the acceptance should
cover interruption and concurrency directly: a transition interrupted
between the ledger append and the store write, and two replicas attempting
the same transition, are the two cases where a ledgered migration can go
wrong in a way a single-replica happy-path test will not show.

## 4. Reconcile 037's manual fallback with its automated acceptance

**Evidence.** 037 AC-2 requires `live.yml` to pass on the pull request that
lands the spec. AC-4 (added 2026-09-12, D-1) says that if FR-005 fails,
`deploy/README.md` carries the operator-assisted backup and no document
claims unattended recovery. AC-3 lets `deploy/README.md` keep whichever gap
paragraph the spec did not close, with the reason. Those can all be
satisfied at once by a live job that skips its rauthy steps and names them,
which AC-1 explicitly permits for the non-live tests.

**Recommendation.** Decide which of the two the spec is asking for, and say
so in its acceptance: a live job whose identity and recovery steps actually
ran, or a documented operator procedure. Both are legitimate; a green check
that skipped the steps it exists to prove is not. Make AC-2 require that
the live job's identity and recovery steps executed, so a skip is a red
check rather than a green one, and keep AC-4's fallback as the documented
outcome when the owner chooses the manual path.

**Context that is already known.** Rauthy's backup routes answer `401` to an
API key (030 D-3), `ADMIN_FORCE_MFA` is instance-wide, and the native rauthy
boot measured about 48 seconds against a 60-second budget on the
maintainer's machine, so a loaded CI runner is where this flakes. The choice
between an upstream rauthy change and a dedicated backup admin session is
already open in `01-consumer-contract.md` section 10.

## 5. Resolve 038's five open points

**Evidence.** 038 carries three owner decisions from 2026-09-12 (D-1
lifetimes and device refresh, D-2 bearer writes and CSRF, D-3 runners) and
is still `draft`. FR-006 and B-5 leave five mechanisms underdetermined.

**Recommendation.** Settle these before the flip:

- **the manifest dependency**: a public client for a CLI is declared in the
  manifest, which makes 038 depend on 036's manifest work if the declaration
  rides the transition record. Decide whether 038 declares it in the
  manifest as it stands today, which keeps the two specs independent;
- **the registration default**: whether a native client is registered by
  default or only when declared;
- **revocation leeway**: FR-006 admits a token past `exp` plus a leeway. Fix
  the leeway's value and say whether B-5's deny-list instant uses the same
  one, since a revocation that is leniently interpreted is not a revocation;
- **refresh-chain termination**: whether revoking by subject terminates an
  outstanding refresh chain, or only the access tokens issued from it;
- **cache-loss semantics**: B-5's two deny-lists live in the cache group,
  and nothing durable lives there by the chassis invariant. Say what a lost
  cache means: a revoked token becoming valid again is the failure this
  question exists to prevent.

## 6. Keep 040 and 041 deferred

**Evidence.** 041 D-4 (2026-09-12, owner decision RH-07) already records the
sequencing. 041 is blocked on 036 and 040 in `registry plan`. Both specs
describe formats a consumer and a control plane read, and both were written
before the v0.1.0 release and before spec-spine shipped `attest --snapshot`.

**Recommendation.** Leave both deferred, and use the deferral: when they are
picked up, align their record formats with what now exists rather than what
was proposed in September. Two anchors are available that were not before.
The consumer contract's sections 11.1 to 11.5 now describe what a replica
can say about itself against shipped code. And spec-spine 0.20.0's
authority snapshot (its spec 087) emits a document naming which inputs were
read, what they hashed to, whether the committed ledger matches the
recompute, and every spec's territory digest; 041's deployment epoch links
to external build and deployment records, and the snapshot is the shape
those records already have upstream. Aligning to it costs nothing now and
avoids a second format later.

**Rejected alternative.** Building 040 first because it is dependency-ready.
Ready is not approved, and a binding surface published before the formats it
reports are settled is a compatibility obligation taken on early.
