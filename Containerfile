# syntax=docker/dockerfile:1.7

# ---- UI build (Bun + Vite) ----
FROM oven/bun:1 AS ui
WORKDIR /ui
COPY ui/package.json ui/bun.lock ./
RUN bun install --frozen-lockfile
COPY ui/ ./
RUN bun run build

# ---- Rust build ----
FROM rust:1.90-bookworm AS build
WORKDIR /src
ENV MY_CI_SKIP_UI_BUILD=1
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY build.rs ./
COPY src ./src
COPY my-ci ./my-ci
COPY ui/package.json ui/bun.lock ./ui/
COPY --from=ui /ui/dist ./ui/dist
RUN cargo build --release --bin my-ci

# ---- Runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /work
COPY --from=build /src/target/release/my-ci /usr/local/bin/my-ci
EXPOSE 7878
CMD ["my-ci", "--runtime", "docker", "gui", "--host", "0.0.0.0", "--port", "7878"]
