# ── Builder ──────────────────────────────────────────────────────────────────
# Pinned to the native build arch; cross-compiles to the target arch with zig.
FROM --platform=$BUILDPLATFORM rust:1-slim AS builder
ARG TARGETARCH

# zig provides the cross-linker for every target; cargo-zigbuild wires it up.
RUN apt-get update && apt-get install -y --no-install-recommends curl xz-utils \
 && rm -rf /var/lib/apt/lists/* \
 && curl -fsSL https://ziglang.org/download/0.13.0/zig-linux-x86_64-0.13.0.tar.xz \
      | tar -xJ -C /opt \
 && ln -s /opt/zig-linux-x86_64-0.13.0/zig /usr/local/bin/zig \
 && cargo install cargo-zigbuild \
 && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
 && case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-musl  > /rust-target ;; \
      arm64) echo aarch64-unknown-linux-musl > /rust-target ;; \
      *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac

WORKDIR /build

# Cache dependency compilation separately from source changes
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
 && cargo zigbuild --release --target "$(cat /rust-target)" \
 && rm -rf src

# Build the real binary (cp to a fixed path so the final COPY needs no shell)
COPY src ./src
RUN touch src/main.rs \
 && cargo zigbuild --release --target "$(cat /rust-target)" \
 && cp "target/$(cat /rust-target)/release/ppk2tui" /ppk2tui

# ── Runtime ───────────────────────────────────────────────────────────────────
# scratch = zero-byte base image; the binary is the entire image
FROM scratch
COPY --from=builder /ppk2tui /ppk2tui
ENTRYPOINT ["/ppk2tui"]
