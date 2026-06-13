# ── Builder ──────────────────────────────────────────────────────────────────
FROM rust:1-slim AS builder

RUN rustup target add x86_64-unknown-linux-musl \
 && apt-get update && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation separately from source changes
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
 && cargo build --release --target x86_64-unknown-linux-musl \
 && rm -rf src

# Build the real binary
COPY src ./src
RUN touch src/main.rs \
 && cargo build --release --target x86_64-unknown-linux-musl

# ── Runtime ───────────────────────────────────────────────────────────────────
# scratch = zero-byte base image; the binary is the entire image
FROM scratch
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/ppk2tui /ppk2tui
ENTRYPOINT ["/ppk2tui"]
