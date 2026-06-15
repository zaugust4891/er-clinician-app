FROM rust:1-slim AS chef
RUN cargo install cargo-chef && apt-get update && apt-get install -y pkg-config libssl-dev
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release -p er-server --recipe-path recipe.json   # cached deps layer
COPY . .
RUN cargo build --release -p er-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/er-server /usr/local/bin/
USER 65532:65532
ENTRYPOINT ["er-server"]