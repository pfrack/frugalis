# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=lukemathwalker/cargo-chef@sha256:5d6ae9b6f0e0fbba91c7862c1f49ca123183ea4ebb0aea35b63bb98947235f83

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
RUN     cargo build --release --locked --bin frugalis

FROM debian@sha256:1def178129dfb5f24db43afbf2fcac04530012e3264ba4ff81c71184e17a9ee4 AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/frugalis /usr/local/bin/frugalis
USER nobody
ENTRYPOINT ["/usr/local/bin/frugalis"]
