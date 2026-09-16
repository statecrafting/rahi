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

# rauthy 0.36.2, pinned by the digest of its multi-architecture index. Bump
# both the tag and the digest together, here and in docker/Dockerfile.
ARG RAUTHY_IMAGE=ghcr.io/sebadob/rauthy:0.36.2@sha256:f7d3c501402165e023edbd958b032b41c9cfdac5ea7f8ca7d62217327145577e

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
