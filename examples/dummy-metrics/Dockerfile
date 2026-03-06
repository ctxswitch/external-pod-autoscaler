FROM rust:1.85 AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12
COPY --from=builder /build/target/release/dummy-metrics /
ENTRYPOINT ["/dummy-metrics"]
