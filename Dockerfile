# syntax=docker/dockerfile:1.7
#
# Operator-built, GPU-free dedicated server image.
#
# The owned disc and the disc-derived `acvd-data` generated tables are supplied as BuildKit
# secrets/bind mounts for the compile step only; they are never copied into an image layer and
# are never part of this repository. Build from the repo root:
#
#   docker build -f Dockerfile -t acvd-server \
#     --secret id=generated,src=crates/acvd-data/src/generated \
#     .
#
# If your generated tables live elsewhere, point the secret at that directory. At runtime mount
# the owned disc and pass its path:
#
#   docker run --rm -p 5000:5000/udp \
#     -v "$PWD/private:/disc:ro" acvd-server \
#     --disc /disc/x360/vd/ACV2.pe --bind 0.0.0.0:5000
#
# The server fails fast with a clear message when the disc is missing.

FROM rust:1-bookworm AS builder
WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY tools ./tools

# `generated` is gitignored and excluded from the build context by .dockerignore; mount it only
# for this compile so the resulting layer contains compiled artifacts, not the tables.
RUN --mount=type=secret,id=generated,target=/src/crates/acvd-data/src/generated \
    cargo build --release -p acvd-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/acvd-server /usr/local/bin/acvd-server

EXPOSE 5000/udp
ENTRYPOINT ["/usr/local/bin/acvd-server"]
