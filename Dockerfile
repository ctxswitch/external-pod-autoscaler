FROM rust:1.94 AS builder
WORKDIR /usr/src/app
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    echo "" > src/lib.rs && \
    cargo build --release && \
    rm -rf src target/release/.fingerprint/external-pod-autoscaler*

COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/app/target/release/external-pod-autoscaler /usr/local/bin/
ENTRYPOINT ["external-pod-autoscaler"]
