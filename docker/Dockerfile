# syntax=docker/dockerfile:1.7
# One container is a complete authenticated application (spec 031 B-4).
#
# Three stages: the pinned rauthy release, taken from its published image by
# digest; the workspace built with --locked --release; and a slim runtime
# holding the two binaries, a non-root user, one volume, one port, and no
# entry point but entrypoint.sh. linux/amd64 and linux/arm64 build from this
# same file, each on its own native runner (image.yml).

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
# The toolchain rust-toolchain.toml pins (spec 010 B-2).
ARG RUST_VERSION=1.96.0
# The package and binary that become the cell: the empty cell until an app
# (spec 034) names its own.
ARG RAHI_PACKAGE=rahi-cli
ARG RAHI_BIN=rahi
# The cell's static directory in this workspace, if it has one (spec 039
# B-5). It is copied to RAHI_STATIC_DIR, so the page exists where the image
# serves it; hello-cell passes apps/hello-cell/web.
ARG RAHI_STATIC_SRC=

FROM ${RAUTHY_IMAGE} AS rauthy

FROM rust:${RUST_VERSION}-bookworm AS build
ARG RAHI_PACKAGE
ARG RAHI_BIN
ARG RAHI_STATIC_SRC
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p "${RAHI_PACKAGE}" \
    && mkdir -p /out/static \
    && cp "target/release/${RAHI_BIN}" /out/rahi \
    && if [ -n "${RAHI_STATIC_SRC}" ]; then cp -a "${RAHI_STATIC_SRC}/." /out/static/; fi

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
COPY --from=build /out/rahi /usr/local/bin/rahi
# Spec 039 B-5: the cell's page, where RAHI_STATIC_DIR says it is. Empty for
# a cell that has none, which is what the empty cell is.
COPY --from=build /out/static /usr/local/share/rahi/static
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/rahi /usr/local/bin/rauthy
USER rahi
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8443
ENV RAHI_DATA_DIR=/data \
    RAHI_RAUTHY_BIN=/usr/local/bin/rauthy \
    RAHI_STATIC_DIR=/usr/local/share/rahi/static
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
