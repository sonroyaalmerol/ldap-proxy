FROM debian:trixie-slim AS build_base

RUN apt-get update && apt-get install -y \
    cargo \
    rustc \
    gcc \
    libssl-dev \
    pkg-config \
    perl \
    make \
    gawk \
    && rm -rf /var/lib/apt/lists/*

FROM build_base AS builder

ARG TARGETARCH
ARG SCCACHE_REDIS

COPY . /home/proxy/
WORKDIR /home/proxy

RUN if [ "$TARGETARCH" = "amd64" ]; then \
        export RUSTFLAGS="-Ctarget-cpu=x86-64-v3 --cfg tokio_unstable"; \
    elif [ "$TARGETARCH" = "arm64" ]; then \
        export RUSTFLAGS="--cfg tokio_unstable"; \
    fi && \
    if [ -n "$SCCACHE_REDIS" ]; then \
        export SCCACHE_REDIS="$SCCACHE_REDIS"; \
        export RUSTC_WRAPPER=sccache; \
    fi && \
    RUST_BACKTRACE=full \
    cargo build --release

FROM debian:trixie-slim

ARG GITHUB_REPOSITORY
LABEL org.opencontainers.image.source=https://github.com/${GITHUB_REPOSITORY}
LABEL org.opencontainers.image.description="LDAP Fallback Proxy"
LABEL org.opencontainers.image.licenses=MPL-2.0

RUN apt-get update && apt-get install -y \
    sqlite3 \
    openssl \
    tzdata \
    iputils-ping \
    iproute2 \
    ldap-utils \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

EXPOSE 636
WORKDIR /

COPY --from=builder /home/proxy/target/release/ldap-proxy /bin/

STOPSIGNAL SIGINT

ENV RUST_BACKTRACE=1
CMD ["/bin/ldap-proxy", "-c", "/data/config.toml"]