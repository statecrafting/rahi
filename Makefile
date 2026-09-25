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
SPEC_SPINE_VERSION ?= 0.25.0

# The coupling base follows the branch this repository actually has, rather
# than being assumed to be `origin/main` (spec-spine spec 072). The same three
# steps the push gate resolves with, in the same order: the environment (make
# imports it, so `?=` leaves an exported value alone), then the remote's own
# HEAD, then `main`. An explicit `BASE=` on the command line still wins.
SPEC_SPINE_DEFAULT_BRANCH ?= $(shell git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||')
BASE ?= origin/$(or $(SPEC_SPINE_DEFAULT_BRANCH),main)
# The head side of the same question (spec 001 D-15). `HEAD` locally; CI's
# pull-request leg passes the event's frozen head SHA, because the checked-out
# merge ref re-resolves against the base on every run (D-9).
HEAD ?= HEAD

# The gate's three controls (spec 001 D-15, carried from the spec-spine kit's
# gate, now spec-spine spec 094 3.3 and 3.4). Each unrecognised value is
# refused at exit 3, never read as the default, and every skip is announced.
#
#   OWNERSHIP  auto (default): run `index coverage --fail-on-untraced` when the
#              effective config's `[coupling] require_ownership` is on, which
#              in this repository it is; 1 always; 0 never.
#   COUPLE     1 (default): run the coupling gate; 0 is for the one caller that
#              must not couple, CI's push and merge-queue leg, which has
#              already merged and carries no PR body.
#   PR_BODY    a FILE holding the PR body, passed to `couple` as `--pr-body`
#              only when set. Never inferred from, or inferring, COUPLE.
OWNERSHIP ?= auto
COUPLE ?= 1
PR_BODY ?=

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

## gate: the governed loop, READ-ONLY throughout (check, lint, coverage, couple, dag); OWNERSHIP=auto|1|0 COUPLE=1|0 PR_BODY=<file>
# A gate that writes repairs what it is meant to judge, so this uses `check`
# and never `compile` or `index`. `check` (spec-spine spec 075) is both
# freshness reads in one verb; `--fail-on-warn` forwards to its compile half.
#
# `--fail-on-unresolved` is deliberately NOT passed. It refuses any unresolved
# claim, which on a specified-before-built corpus is every unit of every
# pending spec: 57 of them today, all legitimate. spec-spine's own CI opts in
# because that repository builds what it claims inside one PR; rahi does not,
# and will not until the last wave lands.
#
# The ownership probe CAPTURES the governed read, checks its status, and only
# then reads the text. Never `config show | grep -q`: a pipeline reports grep's
# status and discards the read's, so a failed read would look like "ownership
# is off" and produce a green gate. A failed read is not a skip.
gate:
	$(SPEC_SPINE) check --fail-on-warn
	$(SPEC_SPINE) lint --fail-on-warn
	@run=no; why="[coupling] require_ownership is off"; \
	cfg="$${TMPDIR:-/tmp}/rahi-gate-config.$$$$"; \
	if test "$(OWNERSHIP)" = "1"; then \
	  run=yes; \
	elif test "$(OWNERSHIP)" = "0"; then \
	  run=no; why="OWNERSHIP=0"; \
	elif test "$(OWNERSHIP)" != "auto"; then \
	  echo "gate: OWNERSHIP=$(OWNERSHIP) is not one of auto, 1, 0" >&2; exit 3; \
	else \
	  $(SPEC_SPINE) config show > "$$cfg"; st=$$?; \
	  if test $$st -ne 0; then rm -f "$$cfg"; exit $$st; fi; \
	  if grep -qF 'require_ownership = true' "$$cfg"; then \
	    run=yes; \
	  elif ! grep -qF 'require_ownership = false' "$$cfg"; then \
	    rm -f "$$cfg"; \
	    echo "gate: the effective config named no require_ownership setting, so the ownership decision could not be read" >&2; \
	    exit 3; \
	  fi; \
	  rm -f "$$cfg"; \
	fi; \
	if test "$$run" = yes; then \
	  echo "$(SPEC_SPINE) index coverage --fail-on-untraced"; \
	  $(SPEC_SPINE) index coverage --fail-on-untraced; \
	else \
	  echo "gate: $$why, so whole-tree ownership was NOT verified (the --fail-on-untraced assertion did not run; set OWNERSHIP=1 to demand it)"; \
	fi
	@if test "$(COUPLE)" = "0"; then \
	  echo "gate: COUPLE=0, so drift against a base was NOT checked (the coupling gate did not run)"; \
	elif test "$(COUPLE)" != "1"; then \
	  echo "gate: COUPLE=$(COUPLE) is not one of 1, 0" >&2; exit 3; \
	else \
	  echo "$(SPEC_SPINE) couple --base $(BASE) --head $(HEAD)$(if $(PR_BODY), --pr-body $(PR_BODY))"; \
	  $(SPEC_SPINE) couple --base $(BASE) --head $(HEAD) $(if $(PR_BODY),--pr-body "$(PR_BODY)"); \
	fi
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
