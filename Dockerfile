# myMoney (Rust). The Python version it replaces is kept in python/ (with its own Dockerfile).
#
# The binary is compiled on the build machine's own architecture, for the target one (no slow emulation for arm64):
# the vendored OpenSSL and SQLCipher are cross-compiled with the matching gcc.
FROM --platform=$BUILDPLATFORM rust:1-slim-trixie AS build
ARG TARGETARCH
RUN apt-get update \
 && apt-get install -y --no-install-recommends perl make gcc libc6-dev \
 && if [ "$TARGETARCH" = "arm64" ] && [ "$(dpkg --print-architecture)" != "arm64" ]; then \
      apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
      && rustup target add aarch64-unknown-linux-gnu; \
    fi \
 && rm -rf /var/lib/apt/lists/*
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY web ./web
RUN set -e; \
    if [ "$TARGETARCH" = "arm64" ] && [ "$(dpkg --print-architecture)" != "arm64" ]; then target=aarch64-unknown-linux-gnu; \
    else target=$(rustc -vV | sed -n 's/^host: //p'); fi; \
    cargo build --release --locked --target "$target"; \
    cp "target/$target/release/mymoney" /mymoney

FROM debian:trixie-slim
ARG VERSION=dev
LABEL org.opencontainers.image.title="myMoney" \
      org.opencontainers.image.description="Gestionnaire de patrimoine auto-hébergé, chiffré au repos" \
      org.opencontainers.image.source="https://github.com/ferrandj/myMoney"
RUN apt-get update \
 && apt-get upgrade -y --no-install-recommends \
 && apt-get install -y --no-install-recommends tzdata \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 app \
 && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin app \
 && mkdir -p /data \
 && chown 10001:10001 /data \
 && chmod 700 /data
COPY --from=build /mymoney /usr/local/bin/mymoney
ENV HOME=/tmp \
    MYMONEY_DATA_DIR=/data \
    MYMONEY_HOST=0.0.0.0 \
    MYMONEY_PORT=8000 \
    MYMONEY_VERSION=${VERSION}
USER 10001:10001
WORKDIR /tmp
VOLUME ["/data"]
EXPOSE 8000
HEALTHCHECK --interval=60s --timeout=5s --start-period=20s \
  CMD ["mymoney", "healthcheck"]
ENTRYPOINT ["mymoney", "serve"]
