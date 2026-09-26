# syntax=docker/dockerfile:1

FROM rust:1.88-bookworm AS builder
WORKDIR /src

COPY Cargo.toml Cargo.lock build.rs ./
COPY proto ./proto
COPY src ./src

RUN cargo build --locked --release --bin v2_hub

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 65532 cumg \
    && useradd --system --uid 65532 --gid 65532 --home-dir /nonexistent --shell /usr/sbin/nologin cumg \
    && mkdir -p /var/lib/cumg /run/cumg/secrets /run/cumg/config \
    && chown -R 65532:65532 /var/lib/cumg /run/cumg

COPY --from=builder /src/target/release/v2_hub /usr/local/bin/v2_hub

USER 65532:65532

ENV CUMG_V2_HOSTED_PROFILE=true \
    CUMG_V2_HUB_STATE_DIR=/var/lib/cumg/state \
    CUMG_V2_MAX_AGENT_SESSION_LIFETIME_SECS=3300 \
    CUMG_V2_AGENT_SESSION_REAUTH_DRAIN_SECS=30 \
    CUMG_V2_DRAIN_TIMEOUT_SECS=8

ENTRYPOINT ["/usr/local/bin/v2_hub"]
