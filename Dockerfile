# One linux/amd64 artifact for API, worker, schema migration, DB bootstrap and onboarding.
# Pin the cargo-chef release, Rust toolchain, Debian generation, and image bytes.
FROM lukemathwalker/cargo-chef:0.1.78-rust-1.97.1-slim-bookworm@sha256:6c017fdefb29c981fe97b5394630807728b1ddcaabe963806b60e12df719c898 AS chef
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
ENV SQLX_OFFLINE=true

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked -p er-server --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked -p er-server --bin er-server --bin er-worker --bin er-migrate --bin er-db-bootstrap --bin er-onboard
RUN ldd target/release/er-server && ldd target/release/er-worker && ldd target/release/er-migrate && ldd target/release/er-db-bootstrap && ldd target/release/er-onboard

FROM debian:bookworm-slim@sha256:5ae3c39ebd15e229dcedd5cee596b2497182493d41ff162e824ba13fc1b2b867 AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/er-server /usr/local/bin/er-server
COPY --from=builder /app/target/release/er-worker /usr/local/bin/er-worker
COPY --from=builder /app/target/release/er-migrate /usr/local/bin/er-migrate
COPY --from=builder /app/target/release/er-db-bootstrap /usr/local/bin/er-db-bootstrap
COPY --from=builder /app/target/release/er-onboard /usr/local/bin/er-onboard
COPY certs/rds-global-bundle.pem /etc/ssl/certs/rds-global-bundle.pem
RUN echo 'e5bb2084ccf45087bda1c9bffdea0eb15ee67f0b91646106e466714f9de3c7e3  /etc/ssl/certs/rds-global-bundle.pem' | sha256sum -c -
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/er-server"]
# ECS worker, migration, bootstrap and onboarding tasks override entryPoint with their exact binary path.
