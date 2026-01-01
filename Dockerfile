FROM opensuse/tumbleweed:latest AS ref_repo

RUN sed -i -E 's/https?:\/\/download.opensuse.org/https:\/\/mirrorcache.firstyear.id.au/g' /etc/zypp/repos.d/*.repo && \
    zypper --gpg-auto-import-keys ref --force

# // setup the builder pkgs
FROM ref_repo AS build_base
RUN zypper install -y cargo rust gcc libopenssl-3-devel sccache perl make gawk

# // setup the runner pkgs
FROM ref_repo AS run_base
RUN zypper install -y sqlite3 openssl-3 timezone iputils iproute2 openldap2-client
COPY SUSE_CA_Root.pem /etc/pki/trust/anchors/
RUN /usr/sbin/update-ca-certificates

# // build artifacts
FROM build_base AS builder

ARG TARGETARCH
ARG SCCACHE_REDIS

COPY . /home/proxy/
RUN mkdir -p /home/proxy/.cargo
COPY cargo_config /home/proxy/.cargo/config
WORKDIR /home/proxy/opensuse-proxy-cache

# Set RUSTFLAGS based on architecture
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

# == end builder setup, we now have static artifacts.
FROM run_base
LABEL org.opencontainers.image.source=https://github.com/${{ github.repository }}
LABEL org.opencontainers.image.description="LDAP Fallback Proxy"
LABEL org.opencontainers.image.licenses=MPL-2.0

EXPOSE 636
WORKDIR /

COPY --from=builder /home/proxy/target/release/ldap-proxy /bin/

STOPSIGNAL SIGINT

ENV RUST_BACKTRACE=1
CMD ["/bin/ldap-proxy", "-c", "/data/config.toml"]