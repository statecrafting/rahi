# rahi: the one source of truth for what CI validates (spec 001 B-2).
#
# Every target is guarded so the composite is green on the specify-only tree:
# before spec 010 lands there is no Cargo.toml.
# `make ci` locally means a green CI run.

SHELL := /bin/bash
.DEFAULT_GOAL := ci

SPEC_SPINE ?= spec-spine
BASE ?= origin/main

# The one place the governance pin is stated. CI reads this literal out of this
# file (spec 001 D-9), so the pin moves in exactly one place.
SPEC_SPINE_VERSION ?= 0.15.0

.PHONY: setup spine spec-dag ci build test lint fmt deny coverage attest verify help

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

## spine: the governed gate chain (compile, index, lint, index check, couple, spec-dag)
spine:
	$(SPEC_SPINE) compile
	$(SPEC_SPINE) index
	$(SPEC_SPINE) lint --fail-on-warn
	$(SPEC_SPINE) index check
	$(SPEC_SPINE) couple --base $(BASE) --head HEAD
	scripts/spec-dag.sh

## spec-dag: depends_on is acyclic and only names lower-numbered specs
spec-dag:
	scripts/spec-dag.sh

## ci: everything CI runs, in order
ci: spine
	$(SPEC_SPINE) index coverage --fail-on-untraced
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

## verify: run one spec's verify:cli blocks, e.g. make verify SPEC=017-ledger-entry-dag
verify:
	@test -n "$(SPEC)" || { echo "usage: make verify SPEC=<spec-id>"; exit 2; }
	scripts/verify-spec.sh $(SPEC)

## help: list targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## //'
