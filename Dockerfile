# Stage 1: Build the Rust application
FROM rust:slim-bookworm AS builder

WORKDIR /usr/src/app

# Option: Copy manifests first to cache dependencies (if Cargo.lock exists)
COPY Cargo.toml ./

# Create a dummy main.rs to compile dependencies
RUN mkdir src && \
    echo "fn main() { println!(\"dummy\"); }" > src/main.rs && \
    cargo build --release || true

# Remove dummy source and copy actual source code
RUN rm -rf src
COPY src ./src

# Touch the main file to ensure cargo rebuilds it
RUN touch src/main.rs
RUN cargo build --release

# Stage 2: Minimal Runtime Environment
FROM debian:bookworm-slim

# The runtime needs PostgreSQL client tools for pg_dump, pg_restore, etc.
# We use the slim debian image to keep the size small.
RUN apt-get update && \
    apt-get install -y --no-install-recommends postgresql-client && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the compiled binary from the builder stage
COPY --from=builder /usr/src/app/target/release/mcp-dba-postgres /usr/local/bin/mcp-dba-postgres

# The MCP Server communicates over stdio
ENTRYPOINT ["mcp-dba-postgres"]
