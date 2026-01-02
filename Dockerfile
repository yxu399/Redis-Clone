# 1. THE BUILDER STAGE
FROM rust:1.83 as builder

# Install protobuf compiler
RUN apt-get update && apt-get install -y protobuf-compiler

WORKDIR /usr/src/redis-clone
COPY Cargo.toml Cargo.lock ./

# --- FIX START ---
# Create dummy files to satisfy Cargo.toml requirements
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN mkdir benches && echo "fn main() {}" > benches/benchmark.rs
# --- FIX END ---

# Build dependencies (this will now pass)
RUN cargo build --release

# Now copy the REAL source code and config
COPY src ./src
COPY benches ./benches
COPY build.rs ./
COPY proto ./proto

# Touch main.rs to force a re-build of the app code
RUN touch src/main.rs
RUN cargo build --release

# 2. THE RUNNER STAGE
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app

COPY --from=builder /usr/src/redis-clone/target/release/redis-clone ./server

EXPOSE 6379 50051

ENTRYPOINT ["./server"]