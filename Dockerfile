# syntax=docker/dockerfile:1

# ---------- builder: fully static musl binary (rustls + ring, no glibc) ----------
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev gcc binutils
WORKDIR /app

# Pre-build dependencies in their own layer for caching.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
 && echo 'fn main() {}' > src/main.rs \
 && cargo build --release \
 && rm -rf src

# Build the real binary.
COPY src ./src
RUN touch src/main.rs \
 && cargo build --release \
 && strip target/release/gitea-mirror-sync

# ---------- runtime: empty image, just the static binary ----------
FROM scratch
# Root CAs are compiled in (reqwest "rustls-tls-webpki-roots"), so no /etc/ssl
# and no shared libraries are needed.
COPY --from=builder /app/target/release/gitea-mirror-sync /gitea-mirror-sync
# Default state location; mount a volume here to persist it.
ENV GMS_STATE_FILE=/data/gms-state.json
ENTRYPOINT ["/gitea-mirror-sync"]
