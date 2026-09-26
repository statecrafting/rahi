# syntax=docker/dockerfile:1.7
# The runtime a cell is built on (spec 039 B-4).
#
# This is `docker/Dockerfile`'s runtime stage on its own: the pinned rauthy,
# a non-root user, `/data`, the entrypoint, and no cell. A cell in another
# repository builds its own binary and lands on top of this image:
#
#   FROM ghcr.io/statecrafting/rahi-runtime:0.1.0
#   COPY --from=build /out/my-cell /usr/local/bin/rahi
#   COPY web/ /usr/local/share/rahi/static/
#
# The binary is always `/usr/local/bin/rahi`, whatever the cell's package
# calls it, because the entrypoint names that path. The static directory is
# already declared by `RAHI_STATIC_DIR` (spec 039 B-5), so a page dropped
# there is served and a cell with no page leaves it empty.
#
# The two files are kept in step by `image.yml`, which refuses a build whose
# pinned rauthy differs between them.

# Rauthy, pinned by the digest of its multi-architecture index (spec 043
# B-2, D-14, D-15): the downstream build `rauthy-patched` 0.36.2-patched.3,
# upstream base v0.36.2 (dd61ac3c), source da8fb522, qualified for N=1 only.
# The build refuses a binary whose version or hash is not the one pinned
# here (FR-002). Bump the image, the version and both hashes together, here
# and in the other Dockerfile; image.yml refuses the two files disagreeing.
ARG RAUTHY_IMAGE=ghcr.io/bartekus/rauthy-patched:0.36.2-patched.3@sha256:d75cac0f708f3e238c458b622fea2f0b7dda9b67e9435eeafa37698d88a2a3c8
ARG RAUTHY_VERSION="rauthy 0.36.2-patched.3"
ARG RAUTHY_SHA256_AMD64=809aa9eb97e7f0b331279719a37de9191fd8e66bfc051f84997221c9a303a188
ARG RAUTHY_SHA256_ARM64=d30f38213465acd4db132d3af2d9ba85a68ff9567eaec4c371b0c9b7e23ad257

FROM ${RAUTHY_IMAGE} AS rauthy

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --user-group --home-dir /data --shell /usr/sbin/nologin rahi \
    && mkdir -p /data /usr/local/share/rahi/static \
    && chown rahi:rahi /data \
    && chmod 0700 /data
COPY --from=rauthy /app/rauthy /usr/local/bin/rauthy
# Spec 043 FR-002: the binary is the pinned build, by version and by hash.
ARG TARGETARCH
ARG RAUTHY_VERSION
ARG RAUTHY_SHA256_AMD64
ARG RAUTHY_SHA256_ARM64
RUN set -eu; \
    case "${TARGETARCH}" in \
      amd64) want="${RAUTHY_SHA256_AMD64}" ;; \
      arm64) want="${RAUTHY_SHA256_ARM64}" ;; \
      *) echo "no pinned rauthy hash for ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    echo "${want}  /usr/local/bin/rauthy" | sha256sum -c -; \
    got="$(/usr/local/bin/rauthy --version)"; \
    if [ "${got}" != "${RAUTHY_VERSION}" ]; then \
      echo "rauthy --version printed '${got}', pinned '${RAUTHY_VERSION}'" >&2; exit 1; \
    fi
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/rauthy
USER rahi
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8443
ENV RAHI_DATA_DIR=/data \
    RAHI_RAUTHY_BIN=/usr/local/bin/rauthy \
    RAHI_STATIC_DIR=/usr/local/share/rahi/static
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
