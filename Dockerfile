# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=lukemathwalker/cargo-chef:0.1.77-rust-1.85.0-bookworm

FROM ${RUST_IMAGE} AS chef
WORKDIR /app
ENV SQLX_OFFLINE=true

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin cerebrum

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/cerebrum /usr/local/bin/cerebrum
USER nobody
ENTRYPOINT ["/usr/local/bin/cerebrum"]
