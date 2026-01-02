# Redis Clone with Raft Consensus

A distributed Redis implementation in Rust featuring Raft consensus for replication, designed for learning async programming, distributed systems, and database internals.

## Overview

This project implements a subset of Redis functionality including:

**Core Features:**
- Core commands: GET, SET, DEL, EXPIRE, PING
- RESP (Redis Serialization Protocol) parser
- Append-only file (AOF) persistence
- Key expiration with background cleanup
- LRU eviction for memory management
- Sharded storage for concurrent access

**Distributed Features (NEW):**
- **Raft consensus protocol** for distributed replication
- **Leader election** with randomized timeouts (1000-1500ms)
- **Log replication** across cluster nodes
- **Persistent state management** (term, votes, log entries)
- **gRPC-based inter-node communication** (Tonic/Protocol Buffers)
- **Docker Compose** support for multi-node clusters

## Quick Start

### Single Node Mode
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

### Distributed Cluster Mode (3 nodes)
```bash
# Start 3-node Raft cluster with Docker Compose
docker-compose up --build

# Connect to different nodes
redis-cli -h 127.0.0.1 -p 6379  # Node 1
redis-cli -h 127.0.0.1 -p 6380  # Node 2
redis-cli -h 127.0.0.1 -p 6381  # Node 3

# Write to leader (automatically replicated)
> SET key1 "value1"
OK

# Read from any node (eventually consistent)
> GET key1
"value1"
```

## Architecture

### Single Node Architecture

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

### Distributed Cluster Architecture (NEW)

```
┌─────────────────────────────────────────────────────────────────┐
│                      3-Node Raft Cluster                         │
│                                                                   │
│  ┌───────────────┐  ┌───────────────┐  ┌───────────────┐      │
│  │   Node 1      │  │   Node 2      │  │   Node 3      │      │
│  │   (Leader)    │  │  (Follower)   │  │  (Follower)   │      │
│  │               │  │               │  │               │      │
│  │ Redis:6379    │  │ Redis:6380    │  │ Redis:6381    │      │
│  │ gRPC:50051 ◄──┼──┼─►gRPC:50052 ◄─┼──┼─►gRPC:50053   │      │
│  │               │  │               │  │               │      │
│  │  ┌─────────┐  │  │  ┌─────────┐  │  │  ┌─────────┐  │      │
│  │  │Raft Log │  │  │  │Raft Log │  │  │  │Raft Log │  │      │
│  │  │Term:5   │──┼──┼─►│Term:5   │──┼──┼─►│Term:5   │  │      │
│  │  │Entry[1] │  │  │  │Entry[1] │  │  │  │Entry[1] │  │      │
│  │  │Entry[2] │  │  │  │Entry[2] │  │  │  │Entry[2] │  │      │
│  │  └─────────┘  │  │  └─────────┘  │  │  └─────────┘  │      │
│  │       │       │  │       │       │  │       │       │      │
│  │  ┌────▼────┐  │  │  ┌────▼────┐  │  │  ┌────▼────┐  │      │
│  │  │Redis DB │  │  │  │Redis DB │  │  │  │Redis DB │  │      │
│  │  │(Sharded)│  │  │  │(Sharded)│  │  │  │(Sharded)│  │      │
│  │  └─────────┘  │  │  └─────────┘  │  │  └─────────┘  │      │
│  │       │       │  │       │       │  │       │       │      │
│  │  ┌────▼────┐  │  │  ┌────▼────┐  │  │  ┌────▼────┐  │      │
│  │  │ AOF     │  │  │  │ AOF     │  │  │  │ AOF     │  │      │
│  │  │Raft Meta│  │  │  │Raft Meta│  │  │  │Raft Meta│  │      │
│  │  └─────────┘  │  │  └─────────┘  │  │  └─────────┘  │      │
│  └───────────────┘  └───────────────┘  └───────────────┘      │
│                                                                   │
│  Heartbeats (100ms) ──►                                          │
│  Leader Election (1000-1500ms timeout)                           │
│  Log Replication (AppendEntries RPC)                             │
└─────────────────────────────────────────────────────────────────┘
```

**Consensus**: Raft protocol ensures all nodes agree on committed log entries

**Replication**: Leader replicates all SET/DEL/EXPIRE commands to followers

**Fault Tolerance**: Cluster remains available with majority (2/3 nodes)

**Persistence**: Each node persists Raft state (term, votes, log) to disk

**Communication**: gRPC with Protocol Buffers for inter-node messaging

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
├── main.rs          # TCP server + Raft initialization
├── codec.rs         # RESP protocol encoder/decoder
├── cmd.rs           # Command parsing and execution
├── db.rs            # Sharded storage with LRU and expiration
├── aof.rs           # Persistence layer
├── frame.rs         # RESP frame types
├── raft_node.rs     # Raft consensus implementation (NEW)
└── raft_storage.rs  # Raft state persistence (NEW)

proto/
└── raft.proto       # gRPC service definitions (NEW)

tests/
├── integration.rs         # End-to-end tests
├── chaos.rs              # Corruption handling tests
├── fixes_verification.rs # Regression tests
└── stress.rs             # Stress tests

benches/
└── benchmark.rs     # Performance benchmarks

docker-compose.yml   # 3-node cluster setup (NEW)
Dockerfile           # Container build (NEW)
build.rs             # Protobuf code generation (NEW)
```

## Testing

```bash
# Run all tests
cargo test

# Run benchmarks
cargo bench

# Test with redis-cli
redis-cli -h 127.0.0.1 -p 6379

# Test distributed cluster
docker-compose up
# Watch leader election in logs
# Kill leader container and observe failover
```

## Performance

Benchmarked on Apple M3:
- Writes: ~3M ops/sec
- Reads: ~13M ops/sec
- Mixed workload: ~5M ops/sec

Note: Benchmarks are in-process without network overhead. Distributed mode adds network latency and consensus overhead.

## Technical Highlights

**Async Runtime**: Built on Tokio for concurrent connection handling

**Zero-Copy Parsing**: Uses `Bytes` crate for efficient buffer management

**Crash Recovery**: AOF recovery tolerates truncation but rejects corruption

**Race Condition Fixes**: Two-phase AOF initialization prevents recovery/write conflicts

**Raft Implementation**: Core consensus algorithm with leader election, log replication, and persistence

**gRPC Communication**: Tonic framework with Protocol Buffers for efficient inter-node messaging

**Persistent Raft State**: Term, votes, and log entries survive crashes and restarts

## Limitations

This is a learning project with intentional scope limitations:

**Distributed Mode:**
- Basic Raft implementation (no log compaction/snapshotting)
- No dynamic membership changes (fixed 3-node cluster)
- Eventual consistency for reads (no linearizable reads)
- No client request routing (clients must find leader)

**General:**
- Five commands (vs 200+ in Redis)
- AOF only (no RDB snapshots)
- No transactions, pub/sub, or Lua scripting
- No Redis Cluster hash slot partitioning

## Dependencies

```toml
# Core dependencies
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }
bytes = "1"
lru = "0.12"
futures = "0.3"

# Distributed/Raft dependencies (NEW)
tonic = "0.10"           # gRPC framework
prost = "0.12"           # Protocol Buffers
clap = "4.5"             # CLI argument parsing
rand = "0.8"             # Randomized election timeouts
serde = "1.0"            # Serialization
serde_json = "1.0"       # JSON persistence

# Error handling
anyhow = "1"
thiserror = "1"

# Observability
tracing = "0.1"
tracing-subscriber = "0.3"
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
- **Distributed consensus algorithms (Raft)**
- **Leader election and fault tolerance**
- **gRPC and Protocol Buffers**
- **Multi-node cluster orchestration with Docker**
- **Persistent state management in distributed systems**

## References

- [Raft Consensus Algorithm](https://raft.github.io/)
- [Redis Protocol Specification](https://redis.io/docs/reference/protocol-spec/)
- [Tonic gRPC Framework](https://github.com/hyperium/tonic)
