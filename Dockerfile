# =============================================================================
# Melis AI Gateway - Multi-stage Docker Build
# Produces a minimal static binary with musl target for scratch/distroless runtime
# Final image size target: < 50MB
# =============================================================================

# ---------------------------------------------------------------------------
# Stage 1: Builder - Compile static binary with musl
# ---------------------------------------------------------------------------
FROM rust:1.86-bookworm AS builder

# Install musl tools and C++ compiler for static linking
RUN apt-get update && \
    apt-get install -y --no-install-recommends musl-tools musl-dev g++ && \
    rm -rf /var/lib/apt/lists/*

# Add musl target
RUN rustup target add x86_64-unknown-linux-musl

# Set musl C++ compiler so crates like esaxx-rs can compile
ENV CXX=g++
ENV CC=musl-gcc

WORKDIR /app

# Cache dependency builds: copy manifests first
COPY Cargo.toml Cargo.lock ./

# Create a dummy main.rs to build dependencies (layer caching optimization)
RUN mkdir src && \
    echo 'fn main() { println!("dummy"); }' > src/main.rs && \
    cargo build --release --target x86_64-unknown-linux-musl || true && \
    rm -rf src

# Copy actual source code
COPY src/ src/

# Touch main.rs to invalidate the cached dummy binary and force recompilation
RUN touch src/main.rs && \
    cargo build --release --target x86_64-unknown-linux-musl && \
    strip /app/target/x86_64-unknown-linux-musl/release/melis-gateway

# ---------------------------------------------------------------------------
# Stage 2: Runtime - Minimal distroless image
# ---------------------------------------------------------------------------
FROM gcr.io/distroless/static-debian12:nonroot

# Copy the statically linked binary
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/melis-gateway /app/melis-gateway

# Copy default configuration files (from examples)
COPY config.yaml.example /app/config.yaml
COPY routes.yaml.example /app/routes.yaml

WORKDIR /app

# Expose the gateway port (feature/catia-vertex-docker-readiness: was
# stale at 8080 -- the real canonical port is 9090, per
# config.yaml.example's server.port, baked in below as the image's
# default /app/config.yaml, and confirmed live via the running
# container's own "Listening on 0.0.0.0:9090" startup log. Metadata
# only -- EXPOSE never controls the actual bind, does not affect
# runtime behavior.)
EXPOSE 9090

# Run as non-root user (distroless:nonroot UID 65532)
USER nonroot:nonroot

# Set the entrypoint to the gateway binary
ENTRYPOINT ["/app/melis-gateway"]
