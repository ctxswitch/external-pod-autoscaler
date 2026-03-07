FROM rust:1.94-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /usr/src/app
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    echo "" > src/lib.rs && \
    cargo build --release && \
    rm -rf src target/release/.fingerprint/external-pod-autoscaler*

COPY src ./src
RUN cargo build --release

FROM alpine:3
RUN apk add --no-cache ca-certificates
COPY --from=builder /usr/src/app/target/release/external-pod-autoscaler /usr/local/bin/
ENTRYPOINT ["external-pod-autoscaler"]
