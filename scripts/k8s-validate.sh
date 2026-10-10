#!/usr/bin/env sh
# Spec 032 B-7: render the kustomizations and hold them to the chassis's
# port and volume contract. Exit 0 when every assertion holds, 1 on the
# first that does not, 3 when the render itself cannot run. Needs kubectl
# (or kustomize); uses kubeconform when it is installed.
#
#   scripts/k8s-validate.sh            the shipped manifests (deploy/k8s, deploy/n3,
#                                      deploy/n3-split)
#   scripts/k8s-validate.sh <dir>...   those kustomization directories instead
#                                      (a render whose rahi config names
#                                      RAHI_RAUTHY_MODE=remote is checked as the
#                                      split layout, spec 044)
#
# The run ends with FR-001's fixture: a kustomization that adds a
# ReadWriteMany claim to the base must be refused; then spec 044 FR-001's
# fixtures, one per rule of the split layout, each refused by name.
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

  # Spec 039 B-3 and FR-002: no render names `latest`, which is never
  # published (039 D-3), and the cell's own image comes from the repository
  # a tag of this project publishes, with a version. Images of other tools
  # (the CronJob's kubectl) are pinned but not ours to name.
  grep -E '^ *image: ' "$out" | sed 's/^ *image: //' > "$tmp/images"
  while read -r image; do
    [ -n "$image" ] || continue
    case "$image" in
      *:latest | *:latest@*)
        fail "the render names $image; a deployment pins a version, never latest" ;;
    esac
    case "$image" in
      */rahi | */rahi:* | */rahi@* | */rahi-runtime | */rahi-runtime:* | */rahi-runtime@*)
        case "$image" in
          ghcr.io/statecrafting/rahi:*.*.* | ghcr.io/statecrafting/rahi-runtime:*.*.*) ;;
          ghcr.io/statecrafting/rahi:*.*.*@sha256:* | ghcr.io/statecrafting/rahi-runtime:*.*.*@sha256:*) ;;
          *) fail "the render names $image; the cell's image is ghcr.io/statecrafting/rahi at a version" ;;
        esac ;;
      *:*) ;;
      *) fail "the render names $image with no version" ;;
    esac
  done < "$tmp/images"

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

  # Spec 040 B-12: /binding is off the Ingress like /metrics. A route that
  # names it is refused, and so is a `/` prefix the edge does not close it on.
  if grep -q "path: /binding" "$tmp/ingress.yaml"; then
    fail "the Ingress routes /binding; it is an in-cluster document only"
  fi
  if grep -q "path: /$" "$tmp/ingress.yaml" \
    && ! grep -q "location = /binding { return 404; }" "$tmp/ingress.yaml"; then
    fail "the Ingress routes / and does not close /binding at the edge"
  fi

  # Spec 040 B-12: a declared RAHI_ARTIFACT_IMAGE names the image the pods
  # run. Where a container image is pinned by digest, the declaration must
  # name the same repository and digest (a tag is informative).
  artifact=$(awk '
    /^  RAHI_ARTIFACT_IMAGE: / { print $2; exit }
    /- name: RAHI_ARTIFACT_IMAGE$/ { getline; if ($1 == "value:") { print $2; exit } }
  ' "$out" | tr -d '"')
  if [ -n "$artifact" ]; then
    while read -r image; do
      case "$image" in
        *@sha256:*)
          pinned="$(echo "${image%@*}" | sed 's/:[^/:]*$//')@${image#*@}"
          [ "$pinned" = "$artifact" ] \
            || fail "RAHI_ARTIFACT_IMAGE is $artifact but a container runs $image" ;;
      esac
    done < "$tmp/images"
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

# Spec 044 FR-001: the split N=3 layout, two StatefulSets and the
# boundary between them.
check_split() {
  dir=$1
  out=$tmp/$(echo "$dir" | tr '/' '_').yaml
  echo "k8s-validate: $dir (split, spec 044)"
  if ! render "$dir" > "$out" 2> "$tmp/err"; then
    echo "  render failed:" >&2
    sed 's/^/    /' "$tmp/err" >&2
    exit 3
  fi

  # Two StatefulSets, rahi and rauthy.
  [ "$(grep -c '^kind: StatefulSet$' "$out")" -eq 2 ] \
    || fail "the split layout has $(grep -c '^kind: StatefulSet$' "$out") StatefulSet(s), not two"
  docs_of "$out" StatefulSet rahi > "$tmp/rahi.yaml"
  docs_of "$out" StatefulSet rauthy > "$tmp/rauthy.yaml"
  [ -s "$tmp/rahi.yaml" ] || fail "no StatefulSet named rahi in the split layout"
  [ -s "$tmp/rauthy.yaml" ] || fail "no StatefulSet named rauthy in the split layout"

  # One container per pod in each (no supervisor, no sidecar).
  for n in $(containers_per_pod "$out"); do
    [ "$n" -eq 1 ] || fail "a pod spec declares $n containers; each pod runs one"
  done

  # No ReadWriteMany anywhere (B-8).
  if grep -q "ReadWriteMany" "$out"; then
    fail "a ReadWriteMany volume is in the render; Raft members never share a volume"
  fi

  # Required anti-affinity in both StatefulSets (B-8).
  for sts in rahi rauthy; do
    grep -q "requiredDuringSchedulingIgnoredDuringExecution" "$tmp/$sts.yaml" \
      || fail "the $sts StatefulSet has no required pod anti-affinity"
  done

  # A PodDisruptionBudget of maxUnavailable 1 per StatefulSet (B-8).
  for pdb in rahi rauthy; do
    docs_of "$out" PodDisruptionBudget "$pdb" > "$tmp/pdb.yaml"
    grep -q "^  maxUnavailable: 1$" "$tmp/pdb.yaml" \
      || fail "no PodDisruptionBudget with maxUnavailable 1 for $pdb"
  done

  # Headless Services publish not-ready addresses (B-7).
  for hl in rahi-hl rauthy-hl; do
    docs_of "$out" Service "$hl" > "$tmp/hl.yaml"
    grep -q "^  publishNotReadyAddresses: true$" "$tmp/hl.yaml" \
      || fail "the headless Service $hl does not publish not-ready addresses"
  done

  # rauthy-internal is a ClusterIP Service with no ingress path, and no
  # Service of the cell is a LoadBalancer or a NodePort (B-1).
  docs_of "$out" Service rauthy-internal > "$tmp/internal.yaml"
  [ -s "$tmp/internal.yaml" ] || fail "no Service named rauthy-internal"
  if grep -q "^  type: " "$tmp/internal.yaml" && ! grep -q "^  type: ClusterIP$" "$tmp/internal.yaml"; then
    fail "rauthy-internal is not a ClusterIP Service"
  fi
  if grep -q "type: LoadBalancer\|type: NodePort" "$out"; then
    fail "a Service is a LoadBalancer or a NodePort; Rauthy is reached only inside the cluster"
  fi
  docs_of "$out" Ingress > "$tmp/ingress.yaml"
  if grep -q "name: rauthy" "$tmp/ingress.yaml"; then
    fail "an Ingress routes to rauthy; users reach it only through rahi's origin"
  fi

  # The back channel is https to rauthy-internal (B-1, B-2).
  url=$(grep "^  RAHI_RAUTHY_URL: " "$out" | awk '{print $2}')
  case "$url" in
    https://rauthy-internal.*) ;;
    *) fail "RAHI_RAUTHY_URL is ${url:-unset}; rahi reaches Rauthy only at https://rauthy-internal" ;;
  esac
  grep -q "^  RAHI_RAUTHY_CA: " "$out" || fail "RAHI_RAUTHY_CA is unset; the certificate is verified against a mounted CA"

  # Rahi's liveness and startup probes do not consult Rauthy (B-4, B-5):
  # neither is on /readyz.
  probe_path() {
    awk -v probe="$1" '
      $0 ~ "^ *" probe ":" { on = 1; next }
      on && /path: / { print $2; exit }
    ' "$tmp/rahi.yaml"
  }
  for probe in livenessProbe startupProbe; do
    path=$(probe_path "$probe")
    [ -n "$path" ] || fail "the rahi StatefulSet has no $probe"
    [ "$path" != "/readyz" ] || fail "rahi's $probe is on /readyz, which consults Rauthy"
  done

  # NetworkPolicy (B-3): Rauthy's HTTP port admits rahi's pods only, each
  # hiqlite cluster admits only its own pods, and no rule admits every port.
  awk '
    function flush(   i, j, n) {
      if (kind == "NetworkPolicy") {
        for (i = 1; i <= nrules; i++) {
          n = split(ports[i], ps, " ")
          if (n == 0) print target, (from[i] == "" ? "*" : from[i]), "*"
          for (j = 1; j <= n; j++) print target, (from[i] == "" ? "*" : from[i]), ps[j]
        }
      }
      kind = ""; target = ""; nrules = 0; delete from; delete ports; section = ""
    }
    /^---/ { flush(); next }
    /^kind: / { kind = $2 }
    /^  ingress:/ { section = "ingress"; next }
    /^  podSelector:/ { section = "target"; next }
    /^  policyTypes:/ { section = ""; next }
    section == "ingress" && /^  - / { nrules++; from[nrules] = ""; ports[nrules] = "" }
    section == "ingress" && /app.kubernetes.io\/name:/ { from[nrules] = $2 }
    section == "ingress" && /- port:/ { ports[nrules] = ports[nrules] " " $3 }
    section == "target" && /app.kubernetes.io\/name:/ { target = $2 }
    END { flush() }
  ' "$out" > "$tmp/rules"
  for want in "rauthy rahi 8443" "rauthy rauthy 8100" "rauthy rauthy 8200" \
              "rahi rahi 8300" "rahi rahi 8400"; do
    grep -qx "$want" "$tmp/rules" || fail "no NetworkPolicy admits ${want#* } to ${want%% *}"
  done
  while read -r target from port; do
    case "$port" in
      '*') fail "a NetworkPolicy rule for $target admits every port" ;;
      8443) [ "$target" != rauthy ] || [ "$from" = rahi ] \
              || fail "Rauthy's HTTP port admits $from; only rahi's pods" ;;
      8100 | 8200) { [ "$target" = rauthy ] && [ "$from" = rauthy ]; } \
              || fail "Rauthy's hiqlite port $port admits $from on $target" ;;
      8300 | 8400) { [ "$target" = rahi ] && [ "$from" = rahi ]; } \
              || fail "rahi's hiqlite port $port admits $from on $target" ;;
    esac
  done < "$tmp/rules"

  # B-9: the migration Job adopts the manifest.
  docs_of "$out" Job rahi-migrate > "$tmp/job.yaml"
  grep -q -- "--adopt-manifest" "$tmp/job.yaml" \
    || fail "the migration Job does not run migrate --adopt-manifest"

  if command -v kubeconform >/dev/null 2>&1; then
    kubeconform -strict -summary -ignore-missing-schemas "$out" \
      || fail "kubeconform rejected the render"
  fi
}

check_dir() {
  out=$tmp/probe.yaml
  if render "$1" > "$out" 2> /dev/null && grep -q "^  RAHI_RAUTHY_MODE: remote$" "$out"; then
    check_split "$1"
  else
    check_render "$1"
  fi
}

if [ "$#" -gt 0 ]; then
  for dir in "$@"; do check_dir "$dir"; done
else
  check_render deploy/k8s
  check_render deploy/n3
  check_split deploy/n3-split
fi

# Spec 040 B-12: the other deployment path the repository ships, the
# developer Compose file, publishes the port on the loopback only, so
# neither /binding nor /metrics is reachable from outside the machine.
if [ "$#" -eq 0 ]; then
  echo "k8s-validate: docker/compose.yml"
  awk '/^ *ports:/ { on = 1; next } on && /^ *- / { print; next } on { on = 0 }' docker/compose.yml \
    > "$tmp/compose-ports"
  [ -s "$tmp/compose-ports" ] || fail "docker/compose.yml publishes no port"
  if grep -v '"127\.0\.0\.1:' "$tmp/compose-ports" | grep -q .; then
    fail "docker/compose.yml publishes a port beyond the loopback"
  fi
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

  # Spec 040 FR-009: a render that routes /binding through the Ingress, and
  # one whose RAHI_ARTIFACT_IMAGE disagrees with a digest-pinned image.
  refused() {
    want=$1
    if "$0" deploy/.k8s-validate-fixture > "$tmp/fixture.out" 2>&1; then
      echo "  FAIL: the fixture passed"
      exit 1
    fi
    grep -q "$want" "$tmp/fixture.out" \
      || { echo "  FAIL: the fixture failed for another reason"; cat "$tmp/fixture.out"; exit 1; }
    echo "  refused, as required"
  }
  # Each child run's own exit trap removes the fixture directory.
  mkdir -p "$fixture"
  cat > "$fixture/kustomization.yaml" <<'YAML'
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - ../k8s
patches:
  - target:
      kind: Ingress
      name: rahi
    patch: |-
      - op: add
        path: /spec/rules/0/http/paths/-
        value:
          path: /binding
          pathType: Exact
          backend:
            service:
              name: rahi
              port:
                name: http
YAML
  echo "k8s-validate: FR-009 fixture (/binding routed publicly must be refused)"
  refused "routes /binding"

  mkdir -p "$fixture"
  digest=0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9
  cat > "$fixture/kustomization.yaml" <<YAML
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - ../k8s
images:
  - name: ghcr.io/statecrafting/rahi
    newTag: 0.4.0
    digest: sha256:$digest
configMapGenerator:
  - name: rahi-config
    behavior: merge
    literals:
      - RAHI_ARTIFACT_IMAGE=ghcr.io/statecrafting/rahi@sha256:$(echo "$digest" | tr '0' 'f')
YAML
  echo "k8s-validate: FR-009 fixture (an artifact image that disagrees must be refused)"
  refused "RAHI_ARTIFACT_IMAGE is"

  # Spec 044 FR-001: one fixture per rule of the split layout, each a
  # patch of deploy/n3-split that the run must refuse by name.
  split_fixture() {
    want=$1
    label=$2
    patch=$3
    mkdir -p "$fixture"
    cat > "$fixture/kustomization.yaml" <<YAML
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - ../n3-split
patches:
$patch
YAML
    echo "k8s-validate: spec 044 fixture ($label must be refused)"
    refused "$want"
  }
  split_fixture "not two" "a single StatefulSet" '  - target: {kind: StatefulSet, name: rauthy}
    patch: |-
      $patch: delete
      apiVersion: apps/v1
      kind: StatefulSet
      metadata: {name: rauthy}'
  split_fixture "containers" "a sidecar" '  - target: {kind: StatefulSet, name: rahi}
    patch: |-
      - op: add
        path: /spec/template/spec/containers/-
        value: {name: sidecar, image: "busybox:1.36"}'
  split_fixture "not a ClusterIP" "a NodePort rauthy-internal" '  - target: {kind: Service, name: rauthy-internal}
    patch: |-
      - op: replace
        path: /spec/type
        value: NodePort'
  split_fixture "routes to rauthy" "an Ingress path to Rauthy" '  - target: {kind: Ingress, name: rahi}
    patch: |-
      - op: add
        path: /spec/rules/0/http/paths/-
        value: {path: /idp, pathType: Prefix, backend: {service: {name: rauthy-internal, port: {name: https}}}}'
  split_fixture "admits" "a NetworkPolicy opening Rauthy's port" '  - target: {kind: NetworkPolicy, name: rauthy-https-from-rahi}
    patch: |-
      - op: remove
        path: /spec/ingress/0/from'
  split_fixture "maxUnavailable 1" "a PDB of two" '  - target: {kind: PodDisruptionBudget, name: rauthy}
    patch: |-
      - op: replace
        path: /spec/maxUnavailable
        value: 2'
  split_fixture "anti-affinity" "no anti-affinity" '  - target: {kind: StatefulSet, name: rauthy}
    patch: |-
      - op: remove
        path: /spec/template/spec/affinity'
  split_fixture "ReadWriteMany" "a shared claim" '  - target: {kind: StatefulSet, name: rahi}
    patch: |-
      - op: replace
        path: /spec/volumeClaimTemplates/0/spec/accessModes
        value: [ReadWriteMany]'
  split_fixture "not-ready addresses" "a headless Service that hides unready pods" '  - target: {kind: Service, name: rauthy-hl}
    patch: |-
      - op: replace
        path: /spec/publishNotReadyAddresses
        value: false'
  split_fixture "livenessProbe is on /readyz" "liveness on /readyz" '  - target: {kind: StatefulSet, name: rahi}
    patch: |-
      - op: replace
        path: /spec/template/spec/containers/0/livenessProbe/httpGet/path
        value: /readyz'
  split_fixture "startupProbe is on /readyz" "startup on /readyz" '  - target: {kind: StatefulSet, name: rahi}
    patch: |-
      - op: replace
        path: /spec/template/spec/containers/0/startupProbe/httpGet/path
        value: /readyz'
  split_fixture "RAHI_RAUTHY_URL is" "a plaintext back channel" '  - target: {kind: ConfigMap, name: rahi-config}
    patch: |-
      - op: replace
        path: /data/RAHI_RAUTHY_URL
        value: http://rauthy-internal.rahi.svc.cluster.local:8443'
fi

echo "k8s-validate: ok"
