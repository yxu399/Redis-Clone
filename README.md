# Redis Clone

A minimal Redis implementation in Rust for learning async programming, network protocols, and database internals.

## Overview

This project implements a subset of Redis functionality including:
- Core commands: GET, SET, DEL, EXPIRE, PING
- RESP (Redis Serialization Protocol) parser
- Append-only file (AOF) persistence
- Key expiration with background cleanup
- LRU eviction for memory management
- Sharded storage for concurrent access

## Quick Start

```bash
# Build and run
cargo build --release
cargo run

# In another terminal, connect with redis-cli
redis-cli -h 127.0.0.1 -p 6379

# Basic usage
> SET mykey "hello"
OK
> GET mykey
"hello"
> EXPIRE mykey 5
(integer) 1
> DEL mykey
(integer) 1
```

## Architecture

```
┌─────────────────────────────────────────┐
│         TCP Clients (redis-cli)         │
└────────────────┬────────────────────────┘
                 │ RESP Protocol
┌────────────────▼────────────────────────┐
│      Connection Handler (Tokio)         │
│         Async Task per Client           │
└────────────────┬────────────────────────┘
                 │
┌────────────────▼────────────────────────┐
│         Command Parser (cmd.rs)         │
│      GET | SET | DEL | EXPIRE | PING    │
└────────────────┬────────────────────────┘
                 │
┌────────────────▼────────────────────────┐
│         Sharded Database (db.rs)        │
│  ┌──────┐ ┌──────┐       ┌──────┐      │
│  │Shard0│ │Shard1│  ...  │Shard │      │
│  │ LRU  │ │ LRU  │       │  15  │      │
│  │10k   │ │10k   │       │ 10k  │      │
│  └──┬───┘ └──┬───┘       └──┬───┘      │
│     │  Expiration HashMap    │          │
└─────┼────────┼───────────────┼──────────┘
      │        │               │
      └────────┴───────────────┘
                 │
┌────────────────▼────────────────────────┐
│    AOF Writer (aof.rs)                  │
│    Async Channel → redis.aof            │
└─────────────────────────────────────────┘

Background Tasks:
- Expiration cleanup (every 1s)
- Orphaned entry cleanup
- AOF async writes
```

**Storage**: 16 shards with LRU caches (10k keys per shard, 160k total capacity)

**Persistence**: Write-ahead logging to `redis.aof` with automatic recovery on startup

**Expiration**: Passive expiration on access + active background cleanup every 1 second

**Concurrency**: Per-shard locking with async I/O using Tokio

## Commands

| Command | Syntax | Description |
|---------|--------|-------------|
| GET | `GET key` | Retrieve value |
| SET | `SET key value` | Store key-value pair |
| DEL | `DEL key [key ...]` | Delete keys |
| EXPIRE | `EXPIRE key seconds` | Set TTL |
| PING | `PING [message]` | Connection test |

## Project Structure

```
src/
├── main.rs     # TCP server
├── codec.rs    # RESP protocol encoder/decoder
├── cmd.rs      # Command parsing and execution
├── db.rs       # Sharded storage with LRU and expiration
├── aof.rs      # Persistence layer
└── frame.rs    # RESP frame types

tests/
├── integration.rs         # End-to-end tests
├── chaos.rs              # Corruption handling tests
└── fixes_verification.rs # Regression tests

benches/
└── benchmark.rs  # Performance benchmarks
```

## Testing

```bash
# Run all tests
cargo test

# Run benchmarks
cargo bench

# Test with redis-cli
redis-cli -h 127.0.0.1 -p 6379
```

## Performance

Benchmarked on Apple M3:
- Writes: ~3M ops/sec
- Reads: ~13M ops/sec  
- Mixed workload: ~5M ops/sec

Note: Benchmarks are in-process without network overhead.

## Technical Highlights

**Async Runtime**: Built on Tokio for concurrent connection handling

**Zero-Copy Parsing**: Uses `Bytes` crate for efficient buffer management

**Crash Recovery**: AOF recovery tolerates truncation but rejects corruption

**Race Condition Fixes**: Two-phase AOF initialization prevents recovery/write conflicts

## Limitations

This is a learning project with intentional scope limitations:
- Single server only (no replication or clustering)
- Five commands (vs 200+ in Redis)
- AOF only (no RDB snapshots)
- No transactions, pub/sub, or Lua scripting

## Dependencies

```toml
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }
bytes = "1"
lru = "0.12"
futures = "0.3"
```

## Learning Goals

This project explores:
- Async/await patterns in Rust
- Network protocol implementation (RESP)
- Database storage strategies (sharding, LRU, expiration)
- Concurrent data structures with Mutex and Arc
- File I/O and crash recovery
- Testing strategies (unit, integration, chaos)
- Performance benchmarking with Criterion

