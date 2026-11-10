# jellydl in a container.
#
# The image holds only the static jellydl binary: no shell, no other programs.
# jellydl can read and write only the folders you mount:
#   /config     config.toml           (XDG_CONFIG_HOME)
#   /data       queue, log, session   (XDG_DATA_HOME)
#   /downloads  downloaded files
#
# Two final stages:
#   default     builds jellydl from source:   docker build -t jellydl .
#   release     uses binaries built before (CI): dist/<arch>/jellydl
#
# How to run it: see "Docker" in README.md.

# Empty folders owned by the default user. Runs on the build machine,
# so multi-platform release builds need no emulation.
FROM --platform=$BUILDPLATFORM rust:1.99-alpine3.22 AS dirs
RUN mkdir -p /out/config /out/data /out/downloads /out/home \
 && chown -R 1000:1000 /out

FROM rust:1.99-alpine3.22 AS builder
# A C compiler for the `ring` crate (TLS).
RUN apk add --no-cache musl-dev
WORKDIR /src
# Build the dependencies first, so code changes do not rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
 && cargo build --release --locked \
 && rm -rf src
COPY src ./src
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --locked \
 && strip target/release/jellydl

# Settings shared by both final stages.
FROM scratch AS base
COPY --from=dirs /out/config /config
COPY --from=dirs /out/data /data
COPY --from=dirs /out/downloads /downloads
COPY --from=dirs /out/home /home/jellydl
ENV HOME=/home/jellydl \
    XDG_CONFIG_HOME=/config \
    XDG_DATA_HOME=/data
USER 1000:1000
VOLUME ["/config", "/data", "/downloads"]
LABEL org.opencontainers.image.title="jellydl" \
      org.opencontainers.image.description="Terminal app to browse a Jellyfin library and download videos"

FROM base AS release
ARG TARGETARCH
COPY dist/${TARGETARCH}/jellydl /jellydl
ENTRYPOINT ["/jellydl", "--download-dir", "/downloads"]

FROM base AS default
COPY --from=builder /src/target/release/jellydl /jellydl
ENTRYPOINT ["/jellydl", "--download-dir", "/downloads"]
