#!/usr/bin/env sh
# Spec 032 B-7: render the kustomizations and hold them to the chassis's
# port and volume contract. Exit 0 when every assertion holds, 1 on the
# first that does not, 3 when the render itself cannot run. Needs kubectl
# (or kustomize); uses kubeconform when it is installed.
#
#   scripts/k8s-validate.sh            the shipped manifests (deploy/k8s, deploy/n3)
#   scripts/k8s-validate.sh <dir>...   those kustomization directories instead
#
# The run ends with FR-001's fixture: a kustomization that adds a
# ReadWriteMany claim to the base must be refused.
set -u

here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here" || exit 3

if command -v kustomize >/dev/null 2>&1; then
  render() { kustomize build "$1"; }
elif command -v kubectl >/dev/null 2>&1; then
  render() { kubectl kustomize "$1"; }
else
  echo "k8s-validate: neither kustomize nor kubectl is installed" >&2
  exit 3
fi

tmp=$(mktemp -d) || exit 3
# kustomize only follows relative roots inside the tree, so the FR-001
# fixture lives beside the base for the length of the run.
fixture=$here/deploy/.k8s-validate-fixture
trap 'rm -rf "$tmp" "$fixture"' EXIT
failures=0

fail() {
  echo "  FAIL: $1"
  failures=$((failures + 1))
}

# The document of `kind: <kind>` named <name> (or every one of that kind).
docs_of() {
  awk -v kind="$2" -v want="${3:-}" '
    /^---/ { if (keep) print buf; buf = ""; keep = 0; k = ""; n = ""; next }
    { buf = buf $0 "\n" }
    /^kind: / { k = $2 }
    /^  name: / && n == "" && k != "" { n = $2 }
    { if (k == kind && (want == "" || n == want)) keep = 1 }
    END { if (keep) print buf }
  ' "$1"
}

# Count the list items directly under each `containers:` list and print one
# count per pod spec. The renderer puts an item's dash at the list's own
# indentation, so an item is a `- ` at exactly that column.
containers_per_pod() {
  awk '
    /^[ ]*containers:[ ]*$/ { match($0, /^[ ]*/); ind = RLENGTH; inlist = 1; count = 0; next }
    inlist {
      if ($0 ~ /^[ ]*$/) next
      match($0, /^[ ]*/)
      if (RLENGTH < ind) { print count; inlist = 0 }
      else if (RLENGTH == ind && $0 ~ /^[ ]*- /) count++
      else if (RLENGTH == ind) { print count; inlist = 0 }
    }
    END { if (inlist) print count }
  ' "$1"
}

check_render() {
  dir=$1
  out=$tmp/$(echo "$dir" | tr '/' '_').yaml
  echo "k8s-validate: $dir"
  if ! render "$dir" > "$out" 2> "$tmp/err"; then
    echo "  render failed:" >&2
    sed 's/^/    /' "$tmp/err" >&2
    exit 3
  fi

  # No shared volume between Raft members: a ReadWriteMany claim anywhere
  # in the render is refused (B-1, B-7).
  if grep -q "ReadWriteMany" "$out"; then
    fail "a ReadWriteMany volume is in the render; Raft members never share a volume"
  fi

  # One container per pod, in every pod spec (B-1).
  for n in $(containers_per_pod "$out"); do
    [ "$n" -eq 1 ] || fail "a pod spec declares $n containers; the supervisor is the one container"
  done

  # The volume is mounted at /data in the StatefulSet (B-1).
  docs_of "$out" StatefulSet rahi > "$tmp/sts.yaml"
  [ -s "$tmp/sts.yaml" ] || fail "no StatefulSet named rahi in the render"
  grep -q "^ *- mountPath: /data$" "$tmp/sts.yaml" \
    || fail "the StatefulSet does not mount the claim at /data"
  grep -q "volumeClaimTemplates:" "$tmp/sts.yaml" \
    || fail "the StatefulSet has no volumeClaimTemplates"
  grep -q "podManagementPolicy: Parallel" "$tmp/sts.yaml" \
    || fail "podManagementPolicy is not Parallel"

  # The keys are a read-only Secret mount at /data/keys (B-2).
  awk '/mountPath: \/data\/keys/ { found = 1; getline; getline; if ($0 !~ /readOnly: true/) exit 1 }
       END { if (!found) exit 1 }' "$tmp/sts.yaml" \
    || fail "/data/keys is not mounted read-only"

  # /metrics is off the Ingress (B-3).
  docs_of "$out" Ingress > "$tmp/ingress.yaml"
  [ -s "$tmp/ingress.yaml" ] || fail "no Ingress in the render"
  if grep -q "path: /metrics" "$tmp/ingress.yaml"; then
    fail "the Ingress routes /metrics; it is scraped in-cluster only"
  fi

  # Both probe paths (B-4).
  grep -q "path: /healthz" "$tmp/sts.yaml" || fail "no probe on /healthz"
  grep -q "path: /readyz" "$tmp/sts.yaml" || fail "no probe on /readyz"

  # The peers name the app's Raft port and none of rauthy's (FR-002), and
  # the count matches the replicas.
  replicas=$(grep "^  replicas: " "$tmp/sts.yaml" | awk '{print $2}')
  peers=$(awk '/^  RAHI_HIQ_NODES: \|/ { on = 1; next } on && /^    / { print; next } on { exit }' "$out")
  count=$(printf '%s\n' "$peers" | grep -c .)
  [ "$count" -eq "${replicas:-0}" ] \
    || fail "RAHI_HIQ_NODES names $count peer(s) for $replicas replica(s)"
  printf '%s\n' "$peers" | grep -q . || fail "RAHI_HIQ_NODES is empty"
  if printf '%s\n' "$peers" | grep -qv ":8400 .*:8300$"; then
    fail "a RAHI_HIQ_NODES peer does not carry the app's ports 8400 and 8300"
  fi
  if printf '%s\n' "$peers" | grep -q ":8100\|:8200"; then
    fail "a RAHI_HIQ_NODES peer carries one of rauthy's ports"
  fi

  # Schema validation when the tool is here; the ServiceMonitor CRD has no
  # schema in the default catalog and is skipped by name.
  if command -v kubeconform >/dev/null 2>&1; then
    kubeconform -strict -summary -ignore-missing-schemas -skip ServiceMonitor "$out" \
      || fail "kubeconform rejected the render"
  else
    echo "  kubeconform: not installed, skipped"
  fi
}

if [ "$#" -gt 0 ]; then
  for dir in "$@"; do check_render "$dir"; done
else
  check_render deploy/k8s
  check_render deploy/n3
fi

if [ "$failures" -ne 0 ]; then
  echo "k8s-validate: $failures failure(s)"
  exit 1
fi

# FR-001: the fixture that adds a ReadWriteMany claim must be refused.
if [ "$#" -eq 0 ]; then
  mkdir -p "$fixture"
  cat > "$fixture/kustomization.yaml" <<'YAML'
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - ../k8s
  - shared.yaml
YAML
  cat > "$fixture/shared.yaml" <<'YAML'
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: shared
spec:
  accessModes: ["ReadWriteMany"]
  resources:
    requests:
      storage: 1Gi
YAML
  echo "k8s-validate: FR-001 fixture (a ReadWriteMany claim must be refused)"
  if "$0" deploy/.k8s-validate-fixture > "$tmp/fixture.out" 2>&1; then
    echo "  FAIL: the fixture with a ReadWriteMany claim passed"
    exit 1
  fi
  grep -q "ReadWriteMany" "$tmp/fixture.out" || { echo "  FAIL: the fixture failed for another reason"; cat "$tmp/fixture.out"; exit 1; }
  echo "  refused, as required"
fi

echo "k8s-validate: ok"
