FROM --platform=$BUILDPLATFORM rust:slim-bookworm AS builder
ARG TARGETARCH

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl-dev \
        make \
        pkg-config \
        gcc-aarch64-linux-gnu \
        gcc-arm-linux-gnueabihf \
        gcc-x86-64-linux-gnu \
        libc6-dev-arm64-cross \
        libc6-dev-armhf-cross \
        libc6-dev-amd64-cross \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add aarch64-unknown-linux-gnu armv7-unknown-linux-gnueabihf x86_64-unknown-linux-gnu

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
ENV CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
ENV CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
ENV CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
ENV CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc

WORKDIR /src

COPY Cargo.toml Cargo.lock /src/
COPY podping-gossipwatcher /src/podping-gossipwatcher

RUN if [ "$TARGETARCH" = "arm64" ]; then \
        RUST_TARGET=aarch64-unknown-linux-gnu; \
    elif [ "$TARGETARCH" = "arm" ]; then \
        RUST_TARGET=armv7-unknown-linux-gnueabihf; \
    else \
        RUST_TARGET=x86_64-unknown-linux-gnu; \
    fi \
    && cargo build --release --locked --target "$RUST_TARGET" -p podping-gossipwatcher \
    && cp "target/$RUST_TARGET/release/podping-gossipwatcher" target/release/podping-gossipwatcher

FROM debian:trixie-slim AS runner
ARG TARGETARCH

USER root

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates openssl \
    && rm -rf /var/lib/apt/lists/*

# Per-arch jemalloc tuning via /etc/malloc.conf symlink.
#   arm   -> Pi 2  (1GB, A7):    1 arena, aggressive decay
#   arm64 -> Pi 4  (2-8GB, A72): 2 arenas, quick decay
#   amd64 -> midrange x64:       background purge + modest arena cap
RUN case "$TARGETARCH" in \
        arm)   CONF='background_thread:true,narenas:1,dirty_decay_ms:500,muzzy_decay_ms:0' ;; \
        arm64) CONF='background_thread:true,narenas:2,dirty_decay_ms:1000,muzzy_decay_ms:0' ;; \
        amd64) CONF='background_thread:true,narenas:4' ;; \
        *)     CONF='' ;; \
    esac; \
    if [ -n "$CONF" ]; then ln -s "$CONF" /etc/malloc.conf; fi

RUN mkdir -p /data/gossip /opt/podping-gossipwatcher \
    && chown -R 1000:1000 /data /opt/podping-gossipwatcher

WORKDIR /opt/podping-gossipwatcher
COPY --from=builder /src/target/release/podping-gossipwatcher /opt/podping-gossipwatcher/podping-gossipwatcher
COPY --from=builder /src/podping-gossipwatcher/src/web_ui.html /opt/podping-gossipwatcher/web_ui.html

RUN mkdir -p /data && chown 1000:1000 /data
WORKDIR /data

ENV SSE_ENABLED=1
# Mitigate iroh#4390 (pending_open_paths unbounded growth in containers):
# - IPv4-only bind + addr_filter avoids unroutable v6 candidate churn
# - Hourly endpoint recycle + 3s RSS watchdog enforce in-app (see main.rs)
# Also pass at runtime: docker run --sysctl net.ipv6.conf.all.disable_ipv6=1 ...
ENV DISABLE_IPV6=1
ENV ENDPOINT_RESET_INTERVAL_SECS=3600
ENV RSS_CEILING_BYTES=314572800

USER 1000
EXPOSE 8089

ENTRYPOINT ["/opt/podping-gossipwatcher/podping-gossipwatcher"]