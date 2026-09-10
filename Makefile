# rahi: the one source of truth for what CI validates (spec 001 B-2).
#
# Every target is guarded so the composite is green on the specify-only tree:
# before spec 010 lands there is no Cargo.toml.
# `make ci` locally means a green CI run.

SHELL := /bin/bash
.DEFAULT_GOAL := ci

SPEC_SPINE ?= spec-spine

# The one place the governance pin is stated. CI reads this literal out of this
# file (spec 001 D-9), so the pin moves in exactly one place.
SPEC_SPINE_VERSION ?= 0.18.0

# The coupling base follows the branch this repository actually has, rather
# than being assumed to be `origin/main` (spec-spine spec 072). The same three
# steps the push gate resolves with, in the same order: the environment (make
# imports it, so `?=` leaves an exported value alone), then the remote's own
# HEAD, then `main`. An explicit `BASE=` on the command line still wins.
SPEC_SPINE_DEFAULT_BRANCH ?= $(shell git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||')
BASE ?= origin/$(or $(SPEC_SPINE_DEFAULT_BRANCH),main)

.PHONY: setup gate refresh spec-dag k8s ci build test lint fmt deny coverage attest verify help

## setup: install the pinned spec-spine and prove the governed loop once
setup:
	@if [ -x "$$(command -v $(SPEC_SPINE))" ] && $(SPEC_SPINE) --version 2>/dev/null | grep -q "$(SPEC_SPINE_VERSION)"; then \
	  echo "[setup] spec-spine $(SPEC_SPINE_VERSION) already present"; \
	else \
	  echo "[setup] installing spec-spine $(SPEC_SPINE_VERSION)"; \
	  cargo install spec-spine-cli --version "$(SPEC_SPINE_VERSION)" --locked \
	    || SPEC_SPINE_VERSION="v$(SPEC_SPINE_VERSION)" sh -c 'curl -fsSL https://raw.githubusercontent.com/statecrafting/spec-spine/main/install.sh | sh'; \
	fi
	$(SPEC_SPINE) --version

## gate: the governed loop, READ-ONLY throughout (check, lint, coverage, couple, dag)
# A gate that writes repairs what it is meant to judge, so this uses `check`
# and never `compile` or `index`. `check` (spec-spine spec 075) is both
# freshness reads in one verb; `--fail-on-warn` forwards to its compile half.
#
# `--fail-on-unresolved` is deliberately NOT passed. It refuses any unresolved
# claim, which on a specified-before-built corpus is every unit of every
# pending spec: 57 of them today, all legitimate. spec-spine's own CI opts in
# because that repository builds what it claims inside one PR; rahi does not,
# and will not until the last wave lands.
gate:
	$(SPEC_SPINE) check --fail-on-warn
	$(SPEC_SPINE) lint --fail-on-warn
	$(SPEC_SPINE) index coverage --fail-on-untraced
	$(SPEC_SPINE) couple --base $(BASE) --head HEAD
	scripts/spec-dag.sh

## refresh: the writing half, for a live session that can commit the shards
refresh:
	$(SPEC_SPINE) compile
	$(SPEC_SPINE) index

## spec-dag: depends_on is acyclic and only names lower-numbered specs
spec-dag:
	scripts/spec-dag.sh

## k8s: the manifests against the chassis's port and volume contract (spec 032 B-7; guarded on the script)
k8s:
	@if [ -x scripts/k8s-validate.sh ]; then scripts/k8s-validate.sh; else echo "k8s: no scripts/k8s-validate.sh yet (lands with spec 032)"; fi

## ci: everything CI runs, in order
ci: gate
	$(MAKE) k8s
	$(MAKE) build
	$(MAKE) test
	$(MAKE) lint
	$(MAKE) fmt
	$(MAKE) deny

## build: cargo build (guarded on Cargo.toml)
build:
	@if [ -f Cargo.toml ]; then cargo build --workspace --locked; else echo "build: no Cargo.toml yet (lands with spec 010)"; fi

## test: cargo test (guarded on Cargo.toml)
test:
	@if [ -f Cargo.toml ]; then cargo test --workspace --locked; else echo "test: no Cargo.toml yet (lands with spec 010)"; fi

## lint: clippy with warnings denied (guarded on Cargo.toml)
lint:
	@if [ -f Cargo.toml ]; then cargo clippy --workspace --all-targets --locked -- -D warnings; else echo "lint: no Cargo.toml yet (lands with spec 010)"; fi

## fmt: rustfmt check (guarded on Cargo.toml)
fmt:
	@if [ -f Cargo.toml ]; then cargo fmt --all --check; else echo "fmt: no Cargo.toml yet (lands with spec 010)"; fi

## deny: cargo-deny supply-chain check (guarded on deny.toml and the tool)
deny:
	@if [ -f deny.toml ]; then \
	  if command -v cargo-deny >/dev/null 2>&1; then cargo deny check; \
	  else echo "deny: cargo-deny not installed (cargo install cargo-deny --locked); skipped locally, CI runs it"; fi; \
	else echo "deny: no deny.toml yet (lands with spec 010)"; fi

## coverage: which source files no spec specifically claims
coverage:
	$(SPEC_SPINE) index coverage

## attest: the corpus attestation (spec-spine's ledger seal), never committed
attest:
	@mkdir -p .derived/attestation
	$(SPEC_SPINE) attest --with-coupling > .derived/attestation/corpus.json
	@echo "attestation written to .derived/attestation/corpus.json"

## verify: run one spec's declared acceptance, e.g. make verify SPEC=017-ledger-entry-dag
verify:
	@test -n "$(SPEC)" || { echo "usage: make verify SPEC=<spec-id>"; exit 3; }
	$(SPEC_SPINE) verify $(SPEC)

## help: list targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## //'
