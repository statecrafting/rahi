# The composed consumer fixture (spec 041 FR-003, section 7)

Each producer's record as a template, in the order the producers run.
`tests/epochs.rs` fills each placeholder with digests of records earlier in
this order, never later (B-4):

1. `authority-snapshot.json`: the authority snapshot spec-spine would
   produce. It names no other record.
2. `build-provenance.json`: an in-toto Statement with an SLSA provenance
   predicate. Its subjects are the image digest and the cell binary's
   sha256; its materials (SLSA v1 `resolvedDependencies`) name the snapshot
   (`{{AUTHORITY_SHA256}}`).
3. `deployment-record.json`: the deployer's record for the rollout. It names
   the Statement's digest and the image.

The deploy step then appends the epoch record naming all three, and a
replica's `/binding` and each denial name the epoch. Those last three carry
a minted instance id and fresh record hashes, so the test composes them on
every run instead of committing their bytes (041 D-13).

The reference type URIs are **provisional** (041 D-1, D-6). They are rahi's
counterproposal to Statecraft, spec-spine and the CLI, and no producer has
answered yet:

| member | type |
|---|---|
| `build` | `https://in-toto.io/Statement/v1` |
| `deployment` | `https://statecraft.ing/deployment-record/v0` |
| `authority` | `https://spec-spine.dev/authority-snapshot/v0` |
