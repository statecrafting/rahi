---
id: "035-denials-survive-shutdown"
title: "Denials survive a graceful shutdown, and every lost denial is counted by its cause"
status: approved
kind: kernel
domain: kernel
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: complete
risk: high
wave: 3
depends_on:
  - "015-kernel-manifest-and-adjudication"
  - "023-observability"
  - "030-operational-verbs"
establishes:
  - "crates/rahi-cli/tests/shutdown.rs"
  - "crates/rahi-kernel/tests/replicas.rs"
extends:
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/lib.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/observe.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/metrics.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/mod.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/tests/adjudicate.rs", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/tests/common/mod.rs", nature: additive }
  - { spec: "022-session-and-principal", unit: "crates/rahi-idp/tests/oidc/", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/append.rs", nature: amending }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/Cargo.toml", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/tests/append.rs", nature: additive }
  - { spec: "001-agentic-harness", unit: ".claude/rules/", nature: amending }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/02-operational-prerequisites.md" }, role: context }
summary: >
  Constitution X says the kernel ledgers every denial. Spec 015 answers a
  denial before its record is durable and drains the records through a
  bounded queue, and serve never drains that queue on shutdown, so a
  graceful stop silently loses the denials still queued (reproduced: 50
  denials answered 403 with decision ids, 33 and 40 of them in the chain
  after SIGTERM, nothing counted). A second loss is structural: a decision
  id is the chain head at boot plus a counter, so replicas of spec 032's
  StatefulSet that boot on the same head mint the same ids, and the second
  append of an id is refused as a conflict (reproduced in process: two
  kernels on one chain both answered with the same id and one record
  landed). This spec drains the queue within a bound when serve stops,
  counts what the bound abandons, separates a dropped record from a failed
  append on /metrics, and names the replica in every decision id, so the
  only uncounted loss left is a process that is killed.
---

# 035: Denials survive a graceful shutdown

## 1. Purpose

Spec 015 B-6 keeps the denial path off the request path: the record goes
to a bounded channel and one task appends it. D-7 accepts that a full queue
drops a record, provided the drop is counted, because "this is the one
failure that must never be silent". Two losses escape that rule today.

- **Shutdown.** `Kernel::flush` exists and nothing in the serve path calls
  it (`crates/rahi-cli/src/serve.rs`, `serve_until`). A graceful stop drops
  the listener, shuts the store, and ends the runtime with records still
  queued. Nothing counts them and nothing logs them. On 2026-09-11 an
  out-of-tree cell at `444bcf8` answered 50 concurrent denials with `403`
  and a decision id each, received SIGTERM, and kept 40 of them; a second
  run kept 33.
- **Indistinguishable causes.** `rahi-edge` registers one failure observer
  that increments `kernel_ledger_failures_total` for a dropped record and
  for a failed append alike. An operator reading `/metrics` cannot tell a
  saturated queue from a failing store.
- **Replicas share an id space.** Spec 015 D-8 mints a decision id as
  `kernel:<last 16 hex of the chain head at boot>:<12-digit counter>` and
  argues that two boots collide only when the earlier one appended
  nothing. That holds for boots in sequence. Spec 032 runs three replicas
  with `podManagementPolicy: Parallel`, each booting on the head it reads
  through the leader, so replicas that boot with no append between them
  share the nonce, and their counters both start at zero. The second
  replica to append a given id is refused `Error::Conflict` by
  `Ledger::append` (013 D-5), the record is lost and counted as a ledger
  failure, and two callers hold the same id for two different denials, of
  which the chain names one. Added 2026-09-11 by the runtime-binding
  session: two kernels booted over one store and one chain, as two
  replicas reading the same head are, each denied one request; both
  answered `kernel:0910ee4a5d5baaf1:000000000000`, the chain holds one
  record under that id, and the observer reported one `conflict`. Not
  reproduced on a live three-node cluster, where the head read goes
  through the leader and the result is the same by construction.

Measured again on 2026-09-12 at `c13cc70`, whose crate sources equal
`444bcf8` (method, environment, and output in
`docs/design/02-operational-prerequisites.md` sections 2 and 3):

- **Shutdown, 200 concurrent denials, SIGTERM at once.** Every request was
  answered `403` with a distinct id and `serve` exited `0`. The chain kept
  4, 13, 10, 10, and 14 of the 200 on a debug build and 50 and 58 on a
  release build; no line was logged and no counter moved. The control run,
  fifteen seconds between the last answer and SIGTERM, kept 200 of 200 twice.
  Draining a backlog of 200 took 0.26 to 0.27 seconds on a release build and
  0.59 to 0.60 on a debug build, single node, loopback.
- **Three independent replicas.** Three `probe-cell` processes formed one
  three-node hiqlite cluster on loopback (not Kubernetes), started as
  `deploy/README.md` describes N=3: shared keys, `migrate` on every replica
  (the leader exited `0`, the followers `2`), then `serve`. Each replica
  answered ten concurrent denials. All 30 were `403`; the three replicas
  minted the same ten ids, `kernel:e60bab25ba6375f5:000000000000` to
  `...009`; the chain holds ten denial records; replicas 2 and 3 each report
  `kernel_ledger_failures_total 10`. Twenty callers hold an id whose record
  is another replica's denial. The collision is not confined to boot: every
  counter value repeats on every replica for the life of the processes.

This spec closes all three. It serves constitution X and spec 015 D-7 and
changes neither text: it makes the kernel's existing promise true under a
graceful stop, and it states the remaining limit.

## 2. Territory

No new crate. Additive changes to four units others own, and one new test
file in `rahi-cli`.

- `crates/rahi-kernel/src/lib.rs` (015): `Kernel::flush` reports what it
  abandoned; a `Kernel::drain` for the shutdown path; the replica's node
  id in every decision id (B-6).
- `crates/rahi-kernel/tests/replicas.rs` (new): two kernels on one chain.
- `crates/rahi-kernel/src/observe.rs` (015): the failure observer learns
  the cause.
- `crates/rahi-edge/src/obs/metrics.rs` and `obs/mod.rs` (023): two new
  counter families.
- `crates/rahi-cli/src/serve.rs` (030): the drain in the stop sequence.
- `crates/rahi-cli/tests/shutdown.rs` (new): the end-to-end proof.

## 3. Behavior

- **B-1 (drain on stop).** When `serve` receives its stop signal, it stops
  accepting, lets in-flight requests finish as it does today, and then
  drains the kernel's denial queue before it shuts the store: every record
  queued at that moment is appended or abandoned within
  `RAHI_DENIAL_DRAIN_TIMEOUT_SECS` (default 5). The store is not shut while
  the drain runs.
- **B-2 (abandonment is counted).** A record still queued when the bound
  expires is abandoned. Each abandoned record raises
  `kernel_decisions_abandoned_total` and writes one error line at target
  `rahi.decision` naming its decision id. The bound expiring is itself one
  warning line naming how many were abandoned.
- **B-3 (causes are distinct).** The failure observer receives the cause:
  `dropped` (the queue was full), `failed` (the append returned an error),
  or `abandoned` (B-2). `/metrics` exposes `kernel_decisions_dropped_total`
  and `kernel_decisions_abandoned_total` beside the existing
  `kernel_ledger_failures_total`, which from this spec counts only failed
  appends.
- **B-4 (the promise, stated).** A denial answered with a decision id is
  in the chain unless the queue was full, the append failed, the drain
  bound expired, or the process was killed without a stop signal. The
  first three are counted by name; the fourth cannot be, and the
  deployment documentation says so. The id names that denial and no
  other (B-6), with one stated residual (D-3): an id held for a lost
  denial can be re-minted by the same replica after a restart on an
  unmoved head, and B-7's boot line is what makes the re-mint visible.
  The drain is bounded: it is not a guarantee against process death or
  storage failure.
- **B-5 (no request waits).** The request path still never awaits an
  append (015 B-6). This spec adds no synchronous mode.
- **B-6 (an id names one decision across replicas).** The kernel MUST mint
  ids no other replica of the same chain can mint. `KernelOptions` carries
  the replica's hiqlite node id (`StoreConfig::node_id`, the pod ordinal
  plus one under spec 032), and the id becomes
  `kernel:<nonce>:<node>:<counter>`, where the nonce stays 015 D-8's last
  16 hex of the head at boot. The id is still reproducible from the chain
  and the node id, reads no clock, and uses no randomness. Ids already in
  a chain are never rewritten; a verifier that parsed ids (none in this
  repository does) reads both shapes.
- **B-7 (the boot identity, reported).** Added 2026-09-12 (D-3). After the
  kernel boots and before `serve` listens, `serve` writes one info line at
  target `rahi.decision` naming the nonce and the node id the kernel mints
  under, so an auditor can pair any id with the boot that minted it and
  can see two boots of one replica on one nonce.

## 4. Functional requirements

- **FR-001.** `tests/shutdown.rs` boots a fixture cell whose route is
  denied, sends 200 concurrent requests, asserts 200 answers of `403` each
  carrying a decision id, sends SIGTERM, waits for exit `0`, and asserts
  `ledger verify` reports every one of the 200 ids in the chain. No loss
  is unexplained (D-4): any id the chain lacks fails the test unless the
  counters of B-3 account for it by cause, and under the default bound the
  expected count of such ids is zero. The test runs on a release build
  (P-5) and asserts B-7's boot line.
- **FR-002.** A kernel test holds the appender behind a gate longer than a
  one-second bound, stops, and asserts the abandoned count equals the
  records still queued and that each abandoned id reached the observer
  with the cause `abandoned`.
- **FR-003.** A kernel test fills a queue of capacity 1 and asserts the
  overflow reaches the observer as `dropped`, not `failed`; an edge test
  asserts the three counter families render on `/metrics`.
- **FR-004.** `tests/replicas.rs` boots two kernels over one store and one
  chain with node ids 1 and 2, has each deny one request before either
  appends, flushes both, and asserts two distinct ids, both resident in
  the chain, and no failure reported. The same test with equal node ids
  is the regression the old shape fails.
- **FR-005 (independent replicas).** Proposed 2026-09-12 and required by
  D-4, because two kernels in one process share a runtime and a store
  handle and three replicas do not. A test in `tests/shutdown.rs` (or a
  sibling the build session claims) starts three fixture-cell processes as
  one three-node cluster on loopback, has each deny ten requests
  concurrently, and asserts 30 distinct ids, 30 denial records in the
  chain, and every
  `kernel_decisions_dropped_total`, `kernel_decisions_abandoned_total`, and
  `kernel_ledger_failures_total` at zero. The procedure of
  `docs/design/02-operational-prerequisites.md` section 3 is the template;
  at `c13cc70` it yields 10 distinct ids and 10 records.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-kernel --locked`, `cargo test -p rahi-edge
  --locked`, and `cargo test -p rahi-cli --locked --test shutdown` pass;
  `tests/replicas.rs` is part of the first.
- **AC-2.** Spec 015 AC-2 still holds: `cargo tree -p rahi-kernel` shows
  `rahi-ledger`, `rahi-store`, and `rahi-types` as its only workspace
  dependencies.
- **AC-3.** `deploy/README.md` states B-4's promise, its one uncounted
  case, the re-mint residual of D-3, and that the drain is bounded, not a
  guarantee against process death or storage failure.

## 6. Out of scope

- A synchronous denial, where the `403` waits for the append. It would
  make the denial path as slow as a Raft write and is the next step only
  if a consumer needs a denial to be durable before it is answered.
- Ledgering allows (015 D-5 stands).
- Durability across SIGKILL, an out-of-memory kill, or power loss.
- The supervisor's own stop order beyond giving `serve` its bound (031).

## 7. Resolved decisions

The owner decided P-1, P-3, and P-4 on 2026-09-12 (decision RH-01 of the
revision-3 register); D-1 to D-4 record them. P-2 was not part of that
decision: synchronous denials stay out of scope (§6) exactly as drafted. The
owner then approved the spec on 2026-09-12, flipping it from `draft` to
`approved` with `implementation: pending`, so it is a work order.

- **D-1 (2026-09-12, owner decision RH-01; adopts P-1).** The drain bound
  is five seconds, `RAHI_DENIAL_DRAIN_TIMEOUT_SECS`. The owner named it
  bounded draining, not a guarantee against process death or storage
  failure, and B-4 now says so. The build session checks that the
  supervisor's stop budget (031) exceeds the stream drain (026) plus this
  bound and records the sum as a decision of its own.
- **D-2 (2026-09-12, owner decision RH-01; adopts P-3).** The decision id
  is `kernel:<nonce>:<node>:<counter>` (B-6). Of the three shapes this
  section weighed, it is the one whose uniqueness an auditor checks from
  the id and the deployment's node list without recomputing a hash, and it
  keeps 015 D-8's reproducibility: no clock, no randomness. The change is
  recorded here and not in spec 015's text; 015 D-8 stays as the history of
  the three-part shape. Rejected: folding the node into the nonce
  (`tail16(sha256(head, node))`), which hides the replica; offsetting each
  node's counter, which changes what the counter means.
- **D-3 (2026-09-12, owner decision RH-01; adopts P-4).** The re-mint
  residual is accepted: a replica whose denials were all lost and which
  restarts before any replica appends boots on the same head with the same
  node id and mints the ids its lost denials' callers hold. The owner
  accepted it on two conditions, both now requirements: the boot identity
  is explicit (B-7, the nonce and node logged at boot) and every loss that
  can be counted is reported by cause (B-2, B-3). B-4 states the residual.
  Rejected: a per-boot sequence per node kept in the store and read at
  boot, which closes the case at the cost of a store write on every boot
  for a case that needs a lost denial and an unmoved cluster-wide head.
- **D-4 (2026-09-12, owner decision RH-01; the evidence).** The build
  proves three things. No unexplained loss on a graceful shutdown (FR-001:
  every answered id is in the chain or counted by cause, and with the
  default bound none is missing). Unique ids across three independent
  processes (FR-005, which this decision makes a requirement rather than a
  proposal; FR-004 in process is not a substitute). Exhaustion is
  observable. This spec reads "exhaustion" as the two bounded resources it
  owns: the drain bound (FR-002: the abandoned count, one error line per
  abandoned id, one warning line) and the queue's capacity (FR-003: the
  dropped count, distinct from a failed append on `/metrics`). The counter
  segment is a zero-padded `u64` that widens past twelve digits rather than
  wrapping in any reachable run, so it is not a third bound; a reader who
  meant something else by "exhaustion" corrects this entry before approval.
- **D-5 (2026-09-13, build session; reads the Territory's "`Kernel::flush`
  reports what it abandoned").** `flush` stays the wait that abandons
  nothing: its timeout error names how many decisions the appender still
  owes, which is its report, and every owed decision stays with the
  appender. `Kernel::drain(bound) -> Drained` is the stop: it waits the
  bound, then takes every owed decision off the queue, counts each on
  `kernel_decisions_abandoned`, reports each to the failure observers with
  the cause, and returns their ids. The denial queue is shared state rather
  than a channel, so a drain whose bound expired can name what the appender
  never took; a decision that was mid-append when abandoned is counted even
  if it lands afterwards, because a count that can overstate a loss is
  better than one that can miss one. When a runtime ends the appender with
  decisions still owed (a supervisor that stops waiting, a test runtime
  torn down), a guard in the appender counts them as abandoned on its way
  out. Rejected: a `flush` that abandons at its deadline, which would make
  every caller that only meant to wait discard records.
- **D-6 (2026-09-13, build session; reads B-2 and B-7's "line at target
  `rahi.decision`").** The chassis installs no formatting subscriber, and
  spec 023's ring merges `rahi.decision` events onto a request span only.
  The appender and a stop run outside every request, which is why a lost
  denial left no line at `c13cc70`. A line is therefore one line of process
  output, `LEVEL rahi.decision: message`. The edge's failure observer writes
  `ERROR rahi.decision: decision <id> was not written to the chain (<cause>):
  <error>` to stderr beside its tracing event, for every cause and not only
  `abandoned`, since each is a loss B-4 names. `serve` writes the `WARN` line
  of B-2 to stderr and the `INFO` boot line of B-7 to stdout. Rejected:
  `tracing-subscriber`'s formatting layer, which needs a workspace feature
  change under spec 010 and would print one line per decision, allowed or
  denied.
- **D-7 (2026-09-13, build session; the sum D-1 asks for).** The stream
  drain (026 B-7, default ten seconds) plus this bound (five) is fifteen
  seconds, and the supervisor waits `SERVE_GRACE`, fifteen seconds (031
  D-4). The check fails by equality: fifteen does not exceed fifteen. It is
  worse in the worst case, since `serve` also gives open connections
  `DRAIN_BUDGET` (ten seconds, after the stream drain), so a stop can take
  twenty-five seconds before the store shuts. Part of the gap predates this
  spec: 031 D-4's "the fifteen seconds the supervisor waits always suffice"
  was written before 026 added a stream drain in front of the connection
  budget. 031's text fixes fifteen seconds and this spec amends nothing in
  it, so `SERVE_GRACE` is unchanged. A stop with no open streams and no slow
  connections gives the denial drain its whole bound inside the fifteen;
  when the supervisor stops waiting first, D-5's guard counts every owed
  decision as abandoned as the runtime ends, so the loss is counted, not
  silent, and `deploy/README.md` says so. Open for the owner: raising
  `SERVE_GRACE` to cover twenty-five seconds, which also means raising the
  StatefulSet's thirty-second `terminationGracePeriodSeconds`, since it must
  cover `SERVE_GRACE` plus rauthy's ten.
- **D-8 (2026-09-13, build session; reads B-6's `<node>`).** The node
  segment is the hiqlite node id in plain decimal, unpadded:
  `kernel:<nonce>:2:<counter>`. Node ids are small integers and ids are
  compared as text, never sorted. `KernelOptions::node_id` zero means node
  1 (`DEFAULT_NODE_ID`), the id a single node runs as, the way a zero
  `queue_capacity` means the default; `serve` passes
  `StoreConfig::node_id`, and `Kernel::nonce` and `Kernel::node_id` expose
  what B-7's line names. A new field breaks struct literals, so three test
  fixtures other specs own (015's `tests/adjudicate.rs`, 020's
  `tests/common/mod.rs`, 022's `tests/oidc/`) gain `..KernelOptions::default()`,
  and 015's test observer takes the new cause argument: `extends` edges on
  those units, and nothing they assert changes.
- **D-9 (2026-09-13, build session; the fixture cell).** The cell FR-001
  and FR-005 boot is the test binary itself. `tests/shutdown.rs` holds a
  `DenyCell` and a `fixture_cell` test that, started as a child with
  `RAHI_TEST_FIXTURE_VERB` set, runs `rahi_cli::run_with::<DenyCell>` and
  exits with the verb's code; in an ordinary run it returns at once. No
  second binary ships in `rahi-cli` and no crate manifest changes. The
  fixture declares no migrations, so `serve` starts on a fresh volume
  without `migrate`, and FR-005 starts three `serve` processes at once
  rather than design note 02 section 3's migrate-then-serve on each replica,
  which would put the follower's exit 2 and a start-order race into a test
  about ids. HTTP goes over std's `TcpStream`, so the test adds no
  dependency. Rejected: a fixture `[[bin]]` in `rahi-cli`, which ships a
  test cell with the product, and `rahi-harness` as a dev-dependency, which
  needs a binary path and a manifest change.
- **D-10 (2026-09-13, build session and owner decision; the second loss
  FR-005 found).** FR-005 first failed. With ids distinct, three replicas
  appending at once made the ledger lose three compare-and-swaps in a row,
  and 4 and then 3 of 30 denials were lost, each counted as `failed` and
  written as an `ERROR rahi.decision` line (B-3 doing what it says). The old
  id shape had hidden this: a colliding id failed earlier, as a conflict.
  Spec 013 B-3's three immediate attempts could not be satisfied beside
  FR-005's zero, and the owner decided on 2026-09-13 that 013 B-3 be amended
  in this build rather than FR-005 relaxed or held (013 D-9): a wait between
  attempts, twelve attempts. With it the in-process three-appender test in
  `rahi-ledger` passes where it failed three runs of three before, FR-005
  passed three runs of three, and a one-off run at fifty denials per replica
  passed. `extends` edges on 013's `append.rs`, `Cargo.toml`, and
  `tests/append.rs`, and on 001's `.claude/rules/`, whose chassis-invariants
  checklist quoted the old count.
- **D-11 (2026-09-13, build session; FR-001 "on a release build").** The
  test picks no profile: it runs under whichever profile cargo builds, and
  the verification block builds the dev profile. P-5 asked for release-build
  evidence, so this build also ran `cargo test --release -p rahi-cli --locked
  --test shutdown`: all four tests passed, FR-001 and FR-005 among them, in
  23.5 seconds.

### Proposals for the owner (2026-09-12)

Recorded by the operational-prerequisites session from the measurements in
section 1. Kept as written; P-1, P-3, and P-4 were adopted on 2026-09-12
(D-1 to D-3), P-5 is D-4's starting point, and P-2 was not taken up.

- **P-1 (bound).** Keep 5 seconds. A 200-record backlog drained in 0.27
  seconds on a release build and 0.60 on a debug build at N=1; the default
  queue holds 1024. The appender at N=3 writes through the leader over the
  network and is not measured here, so B-2's abandoned counter, not the
  bound, is what makes an exceeded bound visible. The supervisor's own stop
  budget (031) must exceed the stream drain (026) plus this bound; the
  build session checks the sum and records it.
- **P-2 (synchronous denials).** Leave out, as drafted. Nothing measured
  here needs a durable-before-answer denial once B-1 to B-3 hold, and it
  would put a Raft write on the refusal path.
- **P-3 (id shape).** Adopt the visible node segment,
  `kernel:<nonce>:<node>:<counter>`. It is the only one of the three whose
  uniqueness an auditor can check from the id and the deployment's node
  list without recomputing a hash, and it keeps 015 D-8's reproducibility
  (no clock, no randomness). Record it as a dated decision in this spec,
  not an edit to 015's text; 015 D-8 stays as the history of the old shape.
- **P-4 (the denial that is not persisted, accounted for).** B-4 lists four
  ways a denial answered with an id can be absent from the chain. A fifth
  consequence follows from the id shape and is not closed by B-6: a replica
  whose denials were all lost (dropped, failed, abandoned, or killed) and
  which restarts before any replica appends anything boots on the same head
  with the same node id, so its counter restarts at zero and it mints the
  ids the lost denials' callers already hold. After B-1 to B-3 this needs a
  lost denial and an unmoved cluster-wide head. Proposed: accept it, state it
  in B-4 as "an id held for a lost denial can be re-minted by the same
  replica after a restart on an unmoved head", and have `serve` log the
  booted nonce and node at startup so an auditor can see a re-mint. The
  alternative that closes it is a per-boot sequence per node kept in the
  store and read at boot (a store write at every boot, no randomness); this
  proposal does not take it because the case is narrow and counted.
- **P-5 (evidence the build must produce).** FR-004 in process and FR-005
  across three processes, both failing at `c13cc70` and passing after, plus
  FR-001 on a release build. The out-of-tree probes in the design note are
  the starting point; they are not repository code and nothing here claims
  them.
- **D-12 (2026-09-24, correction session, owner-authorized work order; reads
  no B-n: a test-support defect).** `free_addr` in
  `crates/rahi-kernel/tests/replicas.rs` and `free_port` in
  `crates/rahi-cli/tests/shutdown.rs` bound `127.0.0.1:0`, read the port,
  and dropped the listener before hiqlite (or the spawned cell) bound it.
  That is a time-of-check race: under parallel test binaries another process
  could take the port in the gap, and hiqlite then panicked in its
  `start.rs` with `valid RPC socket address: AddrInUse`. hiqlite binds the
  address itself and accepts no listener, so holding the socket until the
  bind is not available. The allocator now hands out ports from
  20000..32000, below the Linux (32768..60999) and macOS (49152..65535)
  ephemeral ranges so no outbound connection is given one, starting at an
  offset derived from the process id and advanced by an in-process atomic
  counter. Each candidate is claimed by an exclusive `File::try_lock` on
  `<temp>/rahi-test-ports/<port>.lock`, held until the process exits, and
  then probed with a bind; a held lock or a failed bind moves on to the next
  candidate. Every copy of the allocator in the workspace (the store,
  ledger, kernel, edge, idp, ops and cli tests and the harness) uses the
  same range and lock directory, so two concurrently running test processes,
  from this checkout or another, never hand out the same port. This applies
  to the replica and shutdown tests. The helpers keep their signatures, so
  no caller changed. Rejected: a shared helper crate or a dev-dependency on
  `rahi-harness`, which would add a dependency edge from a lower-numbered
  spec to a higher one; and retrying on `AddrInUse`, which hiqlite reports
  as a panic rather than an error.

## Verification

```verify:cli
cargo test -p rahi-kernel --locked
cargo test -p rahi-edge --locked
cargo test -p rahi-cli --locked --test shutdown
```
