# syntax=docker/dockerfile:1

# The toolchain is pinned to the same version CI uses. A floating tag would
# let the image and CI drift apart silently.
FROM rust:1.95-bookworm AS builder

# dioxus-cli compiles from source and is by far the slowest layer, so it is
# installed before any source is copied and survives every source change.
RUN cargo install dioxus-cli --locked --version 0.7.10 \
    && rustup target add wasm32-unknown-unknown

WORKDIR /src
COPY . .

# --features ui embeds crates/oxidarr-ui/dist at compile time, so the bundle
# must exist before cargo build runs or the build fails at macro expansion.
RUN ./scripts/build-ui.sh \
    && cargo build --release -p oxidarr-prowl --features ui \
    && cargo build --release -p oxidarr-migrate

FROM debian:stable-slim AS runtime

# ca-certificates: TLS verification when fetching definitions and reaching
# trackers. curl: what HEALTHCHECK below uses. Distroless would mean either
# dropping the healthcheck or adding a health subcommand to the binary
# purely to work around the base image.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --create-home --uid 10001 oxidarr

COPY --from=builder /src/target/release/oxidarr-prowl /usr/local/bin/
COPY --from=builder /src/target/release/oxidarr-migrate /usr/local/bin/

# No definitions are baked in. They are third-party and unlicensed; the
# running container fetches them itself on first start. Do not add them to
# a layer to speed up cold start.
ENV OXIDARR_DATA_DIR=/data
RUN mkdir -p /data && chown oxidarr:oxidarr /data
VOLUME ["/data"]

USER oxidarr
EXPOSE 9696

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9696/ping || exit 1

ENTRYPOINT ["oxidarr-prowl"]
