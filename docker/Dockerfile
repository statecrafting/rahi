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

FROM ${RAUTHY_IMAGE} AS rauthy

FROM rust:${RUST_VERSION}-bookworm AS build
ARG RAHI_PACKAGE
ARG RAHI_BIN
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p "${RAHI_PACKAGE}" \
    && mkdir -p /out \
    && cp "target/release/${RAHI_BIN}" /out/rahi

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --user-group --home-dir /data --shell /usr/sbin/nologin rahi \
    && mkdir -p /data \
    && chown rahi:rahi /data \
    && chmod 0700 /data
COPY --from=rauthy /app/rauthy /usr/local/bin/rauthy
COPY --from=build /out/rahi /usr/local/bin/rahi
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/rahi /usr/local/bin/rauthy
USER rahi
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8443
ENV RAHI_DATA_DIR=/data \
    RAHI_RAUTHY_BIN=/usr/local/bin/rauthy
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
