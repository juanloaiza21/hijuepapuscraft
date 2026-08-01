# Build stage pins the rust minor that matches rust-version in Cargo.toml.
FROM docker.io/library/rust:1.96-slim AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Dependency layer: only invalidated when Cargo.toml/lock change.
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/hijuepapus-bot /usr/local/bin/bot
USER nonroot
ENTRYPOINT ["/usr/local/bin/bot"]
