# ── Builder ──────────────────────────────────────────────────────────────────
# Pinned to the native build arch; cross-compiles to the target arch with zig.
#
# BUILDPLATFORM is predefined by BuildKit but left empty by the legacy builder,
# where a bare $BUILDPLATFORM fails with "failed to parse platform". The
# ${..:-default} form supplies a fallback for that case while still deferring to
# BuildKit's real value when it is set — so arm64 hosts keep a native builder
# stage. Do NOT replace this with `ARG BUILDPLATFORM=linux/amd64`: an explicit
# ARG default overrides BuildKit's value and forces emulated amd64 on arm64.
FROM --platform=${BUILDPLATFORM:-linux/amd64} rust:1-slim AS builder
ARG TARGETARCH

# zig provides the cross-linker for every target; cargo-zigbuild wires it up.
# The zig build must match the arch the *builder* runs on (x86_64 or aarch64),
# which is not necessarily the arch we are compiling for.
RUN apt-get update && apt-get install -y --no-install-recommends curl xz-utils \
 && rm -rf /var/lib/apt/lists/* \
 && ZIG_ARCH="$(uname -m)" \
 && curl -fsSL "https://ziglang.org/download/0.13.0/zig-linux-${ZIG_ARCH}-0.13.0.tar.xz" \
      | tar -xJ -C /opt \
 && ln -s "/opt/zig-linux-${ZIG_ARCH}-0.13.0/zig" /usr/local/bin/zig \
 && cargo install cargo-zigbuild \
 && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
 && TARGET_ARCH="${TARGETARCH:-$(dpkg --print-architecture)}" \
 && case "$TARGET_ARCH" in \
      amd64) echo x86_64-unknown-linux-musl  > /rust-target ;; \
      arm64) echo aarch64-unknown-linux-musl > /rust-target ;; \
      *) echo "unsupported arch: $TARGET_ARCH" >&2; exit 1 ;; \
    esac

WORKDIR /build

# Cache dependency compilation separately from source changes.
# build.rs lives at the package root, so `COPY src` below does not pick it up.
COPY Cargo.toml Cargo.lock* build.rs ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
 && cargo zigbuild --release --target "$(cat /rust-target)" \
 && rm -rf src

# .dockerignore excludes .git, so build.rs cannot read the commit itself — pass
# it in with --build-arg. Declared after the dependency layer so changing it
# does not invalidate that cache.
ARG PPK2TUI_GIT_SHA=unknown
ENV PPK2TUI_GIT_SHA=$PPK2TUI_GIT_SHA

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
