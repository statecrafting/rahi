#!/bin/sh
# The image smoke test (spec 031 B-6, FR-005): boot the image against a temp
# volume, wait on /readyz, fetch discovery through the proxy, assert the
# issuer is the public URL, and check the container exits 0 on SIGTERM.
#
#   docker/smoke.sh <image> [port]
#
# Exit 0 when every assertion holds; the container's log is printed on any
# failure. Needs docker, curl, and jq.
set -eu

image="${1:?usage: docker/smoke.sh <image> [port]}"
port="${2:-18443}"
public="http://localhost:${port}"
name="rahi-smoke-$$"

cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm "$name" >/dev/null 2>&1 || true
}
fail() {
  echo "smoke: FAIL: $1" >&2
  docker logs "$name" >&2 2>&1 || true
  cleanup
  exit 1
}

docker volume create "$name" >/dev/null
docker run -d --name "$name" \
  -p "${port}:8443" \
  -e RAHI_PUBLIC_URL="$public" \
  -v "$name:/data" \
  "$image" >/dev/null

ready=0
i=0
while [ "$i" -lt 90 ]; do
  if curl -fsS "$public/readyz" >/dev/null 2>&1; then ready=1; break; fi
  if [ "$(docker inspect -f '{{.State.Running}}' "$name")" != "true" ]; then break; fi
  sleep 1
  i=$((i + 1))
done
[ "$ready" = "1" ] || fail "the cell did not answer /readyz within ninety seconds"

docker logs "$name" 2>&1 | grep -q "first-boot: keys generated" || fail "first boot did not generate keys"
docker logs "$name" 2>&1 | grep -q "custodied" || fail "the OIDC client was not custodied"

discovery="$(curl -fsS "$public/auth/v1/.well-known/openid-configuration")" || fail "discovery is not served through the proxy"
issuer="$(printf '%s' "$discovery" | jq -r '.issuer')"
[ "$issuer" = "$public/auth/v1/" ] || fail "issuer is $issuer, expected $public/auth/v1/"
curl -fsS "$public/healthz" >/dev/null || fail "/healthz did not answer"

docker stop -t 30 "$name" >/dev/null
code="$(docker inspect -f '{{.State.ExitCode}}' "$name")"
[ "$code" = "0" ] || fail "the container exited $code on SIGTERM, expected 0"

echo "smoke: ok, issuer $issuer, clean exit on SIGTERM"
cleanup
