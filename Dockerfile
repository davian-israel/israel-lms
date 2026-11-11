# syntax=docker/dockerfile:1
# Multi-stage build: cargo-chef caches dependencies, final image is Debian slim
# with one binary (`lms`). Run as `lms web`, `lms worker` or `lms all`.

FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin lms

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home lms
COPY --from=builder /app/target/release/lms /usr/local/bin/lms
USER lms
ENV PORT=8080 RUST_LOG=info,sqlx=warn
EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lms"]
# Role: pass web | worker | all as the command, or set LMS_ROLE.
CMD []
