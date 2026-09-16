# syntax=docker/dockerfile:1.7
# One container is a complete authenticated application (spec 031 B-4).
#
# Three stages: the pinned rauthy release, taken from its published image by
# digest; the workspace built with --locked --release; and a slim runtime
# holding the two binaries, a non-root user, one volume, one port, and no
# entry point but entrypoint.sh. linux/amd64 and linux/arm64 build from this
# same file, each on its own native runner (image.yml).

# rauthy 0.36.2, pinned by the digest of its multi-architecture index. Bump
# both the tag and the digest together.
ARG RAUTHY_IMAGE=ghcr.io/sebadob/rauthy:0.36.2@sha256:f7d3c501402165e023edbd958b032b41c9cfdac5ea7f8ca7d62217327145577e
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
