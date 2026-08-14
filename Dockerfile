FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/cors-proxy /usr/local/bin/cors-proxy
ENV PROXY_PATH=/m3u8-proxy
ENV ALLOW_PRIVATE_IPS=false
EXPOSE 3000
USER 65532:65532
ENTRYPOINT ["/bin/sh", "-c", "export LISTEN_ADDR=0.0.0.0:${PORT:-3000}; exec /usr/local/bin/cors-proxy"]
