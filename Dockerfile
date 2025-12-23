# 1. THE BUILDER STAGE
# We use the official Rust image to compile the code.
FROM rust:1.83 as builder

# Create a dummy project and build dependencies first
# This caches the dependencies so we don't re-download them every time we change src/
WORKDIR /usr/src/redis-clone
COPY Cargo.toml Cargo.lock ./
# Create a dummy main.rs to satisfy cargo build
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release

# Now copy the REAL source code
COPY src ./src
# Touch the main file to force a re-build of the app code
RUN touch src/main.rs
RUN cargo build --release

# 2. THE RUNNER STAGE
# We use a tiny Linux image (Debian Slim) for the actual server
FROM debian:bookworm-slim

# Install OpenSSL (often needed) and ca-certificates
RUN apt-get update && apt-get install -y libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from the builder stage
COPY --from=builder /usr/src/redis-clone/target/release/redis-clone ./server

# Expose the Redis port
EXPOSE 6379

# Run the server
CMD ["./server"]