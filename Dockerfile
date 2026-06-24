# =============================================================================
# Melis AI Gateway - Multi-stage Docker Build
# Produces a minimal static binary with musl target for scratch/distroless runtime
# Final image size target: < 50MB
# =============================================================================

# ---------------------------------------------------------------------------
# Stage 1: Builder - Compile static binary with musl
# ---------------------------------------------------------------------------
FROM rust:1.83-bookworm AS builder

# Install musl tools for static linking
RUN apt-get update && \
    apt-get install -y --no-install-recommends musl-tools && \
    rm -rf /var/lib/apt/lists/*

# Add musl target
RUN rustup target add x86_64-unknown-linux-musl

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

# Build the final binary
RUN cargo build --release --target x86_64-unknown-linux-musl && \
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

# Expose the gateway port
EXPOSE 8080

# Run as non-root user (distroless:nonroot UID 65532)
USER nonroot:nonroot

# Set the entrypoint to the gateway binary
ENTRYPOINT ["/app/melis-gateway"]
