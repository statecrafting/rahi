#!/usr/bin/env bash
# Rebuild the committed `v0.1.0-sealed` fixture (spec 042 FR-014).
#
# The fixture is a chain written by the *published* 0.1.0 chassis crates,
# pulled from crates.io into a throwaway workspace, so the migration proof of
# AC-3 is taken against the binary a consumer actually ran rather than against
# this checkout pretending to be it. Nothing here writes into the repository
# except the fixture directory beside this script.
#
# The committed fixture is the artefact; this script is only how it was made.
# The tests never run it, and crates.io being unreachable is no reason to skip
# them: an absent fixture directory is a broken checkout, which the tests
# report as a failure naming this script.
#
#   ./write.sh            # rebuild in place
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat > "$work/Cargo.toml" <<'EOF'
[package]
name = "v010-fixture"
version = "0.0.0"
edition = "2024"

[dependencies]
rahi-ledger = "=0.1.0"
rahi-store = "=0.1.0"
rahi-types = "=0.1.0"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "time"] }
tempfile = "3"

[workspace]
EOF

mkdir -p "$work/src"
cp "$here/write.rs" "$work/src/main.rs"

cd "$work"
cargo run --release -- "$here"
