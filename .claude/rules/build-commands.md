---
paths:
  - "crates/**"
  - "apps/**"
  - "docker/**"
  - "deploy/**"
---

# Build commands and ownership

The `Makefile` is the source of truth for what CI validates; `make ci`
green locally means a green CI run. These are the commands behind it.

```sh
make gate                                                    # the spec-spine gate chain, read-only
make ci                                                      # spine + coverage + every cargo gate
cargo build --workspace --locked
cargo test --workspace --locked
cargo test -p rahi-store --locked --test txn                 # one crate, one test file
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check                                      # fix with: cargo fmt --all
cargo deny check                                             # supply chain (deny.toml, spec 010)
spec-spine verify <spec-id>                                  # the spec's declared acceptance
```

Rules:

- Always `--locked`; `Cargo.lock` is committed and part of the determinism
  contract. Third-party versions live only in the root `Cargo.toml`
  `[workspace.dependencies]`; crate manifests inherit with
  `workspace = true`. Adding a dependency needs an `extends` edge on spec
  010's `Cargo.toml` section `workspace.dependencies`.
- `unsafe` is forbidden workspace-wide; clippy runs with `-D warnings`;
  library code denies `unwrap_used`, `expect_used`, `indexing_slicing`, and
  `float_arithmetic`.
- Every crate manifest carries `[package.metadata.spec-spine] spec =
  "<founding spec id>"`.
- Claim every new file in the spec you are implementing (its
  `establishes` list) in the same change: `require_ownership` is on and
  `C-002` refuses an unclaimed source file at PR time. Touching a file
  another spec owns needs an `extends` edge on that spec's unit.
- Crates depend downward only: `rahi-types`, then `rahi-store`, then
  `rahi-ledger` and `rahi-kernel`, then `rahi-idp` and `rahi-edge`, then
  `rahi-ops` and `rahi-cli`. An app under `apps/` depends on the chassis
  crates and the chassis never depends on an app.
- No `tsconfig.json` and no `package.json` at the repository root (spec
  001 D-1): the orchestrator gates a Rust target through `make ci`.
