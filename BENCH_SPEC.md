# Benchmark Specification: reth vs init4tech MDBX Storage

## Goals

Compare performance of two matched pairs:

1. **`reth-libmdbx` vs `signet-libmdbx`** — raw MDBX Rust bindings
2. **`reth-db` (hot) vs `signet-hot-mdbx`** — typed hot database layer

## Methodology

- **Framework**: Criterion.rs 0.8 (both projects already depend on it)
- **Environment**: Single machine, pinned CPU, tmpfs or NVMe for DB path
- **Warmup**: 3s warmup, 5s measurement (Criterion defaults)
- **Runs**: Minimum 100 iterations per benchmark
- **DB Sync Mode**: `SafeNoSync` for all benchmarks (removes fsync noise)
- **Data**: Pre-populated databases with deterministic pseudo-random data (seeded RNG)

## Project Structure

```
storage-benches/
├── RESEARCH.md              # API research notes
├── BENCH_SPEC.md            # This file
└── benches/                 # Benchmark crate
    ├── Cargo.toml
    └── benches/
        ├── binding_reads.rs
        ├── binding_writes.rs
        ├── binding_iteration.rs
        ├── binding_txn.rs
        ├── hotdb_reads.rs
        ├── hotdb_writes.rs
        └── hotdb_iteration.rs
```

---

## Part 1: Raw Bindings Benchmarks

### Common Setup

Both sides open an MDBX environment with matched configuration:
- Max size: 1 GB
- Page size: 4096
- Sync mode: SafeNoSync
- Max DBs: 4
- WRITEMAP enabled
- NO_RDAHEAD enabled

Data generation:
- Keys: 32 bytes (keccak-like), deterministic from index
- Values: configurable (32B, 256B, 4KB)
- Pre-populated DB with N entries (N = 10K, 100K, 1M)

### B1: Transaction Creation

| Benchmark | reth-libmdbx | signet-libmdbx |
|-----------|-------------|----------------|
| RO txn create | `env.begin_ro_txn()` | `env.begin_ro_sync()` |
| RW txn create | `env.begin_rw_txn()` | `env.begin_rw_sync()` |
| RO txn unsync | N/A | `env.begin_ro_unsync()` |
| RW txn unsync | N/A | `env.begin_rw_unsync()` |

Measure: time to create + drop (no operations inside).

### B2: Point Reads (Random)

Pre-populate DB with 100K entries. Read 1000 random keys per iteration.

| Benchmark | reth-libmdbx | signet-libmdbx |
|-----------|-------------|----------------|
| get 32B values | `txn.get::<Vec<u8>>(dbi, &key)` | `txn.get::<Vec<u8>>(dbi, &key)` |
| get 256B values | same | same |
| get 4KB values | same | same |
| get (unsync) | N/A | `unsync_txn.get(...)` |

### B3: Point Writes (Batch)

Write N random key-value pairs in a single RW transaction, then commit.

| N | reth-libmdbx | signet-libmdbx |
|---|-------------|----------------|
| 100 | `txn.put(dbi, k, v, flags)` × 100 + commit | same |
| 1,000 | same | same |
| 10,000 | same | same |

Also measure:
- `WriteFlags::UPSERT` vs `WriteFlags::APPEND` (pre-sorted keys)
- Sync vs Unsync (signet only)

### B4: Sequential Cursor Iteration

Pre-populate with 100K entries. Full forward scan.

| Benchmark | reth-libmdbx | signet-libmdbx |
|-----------|-------------|----------------|
| Forward (sync) | `cursor.first(); loop { cursor.next() }` | `cursor.iter_start()` |
| Forward (unsync) | N/A | `cursor.iter_start()` on unsync txn |
| Reverse | `cursor.last(); loop { cursor.prev() }` | same pattern |

### B5: Range Queries

Pre-populate with 100K sorted keys. Seek to midpoint, read 1000 entries.

| Benchmark | reth-libmdbx | signet-libmdbx |
|-----------|-------------|----------------|
| set_range + 1000 next | `cursor.set_range(&key)` + loop | same |

### B6: DUPSORT Operations

Table with DUPSORT flag. 10K keys, each with 10 duplicate values (32B fixed).

| Benchmark | reth-libmdbx | signet-libmdbx |
|-----------|-------------|----------------|
| Insert dups | `put` with DUP_SORT flag | same |
| Read all dups for key | `first_dup()` + `next_dup()` | `iter_dup_of(key)` |
| DUPFIXED batch read | N/A (no batched API) | `iter_dupfixed_of(key)` |
| Full DUPSORT scan | `first()` + `next()` | `iter_dup_start()` |

### B7: Commit Latency

Measure `commit_with_latency()` / `commit() -> CommitLatency`.

| Batch size | Both |
|-----------|------|
| 1 item | write 1 + commit |
| 100 items | write 100 + commit |
| 1,000 items | write 1000 + commit |
| 10,000 items | write 10000 + commit |

---

## Part 2: Hot DB Layer Benchmarks

### Common Setup

Both sides:
- Create a temporary MDBX-backed hot database
- Pre-populate with realistic Ethereum-like data
- Same DatabaseArguments (8TB max, SafeNoSync for bench)

Data generation:
- Addresses: 20B, deterministic from index
- Account: nonce (u64), balance (U256), code_hash (B256)
- Storage slots: B256 key -> U256 value
- Block headers: sequential block numbers with random hashes

### H1: Account State Read

Pre-populate `PlainAccountState` with 100K accounts.

| Benchmark | reth-db | signet-hot-mdbx |
|-----------|--------|----------------|
| Get account by address | `tx.get::<PlainAccountState>(addr)` | `reader.get::<PlainAccountState>(&addr)` |
| Get 1000 random accounts | loop of above | loop of above |
| Miss rate (nonexistent keys) | 50% hit / 50% miss | same |

### H2: Storage Slot Read (DUPSORT)

Pre-populate `PlainStorageState`: 10K addresses × 10 slots each.

| Benchmark | reth-db | signet-hot-mdbx |
|-----------|--------|----------------|
| Get single slot | `cursor.seek_by_key_subkey(addr, slot)` | `reader.get_dual::<PlainStorageState>(&addr, &slot)` |
| Get all slots for address | `cursor.walk_dup(addr)` | `traverse_dual.exact_dual(...)` + iterate |
| Random slot lookups × 1000 | loop | loop |

### H3: Block Header Read

Pre-populate `Headers` with 10K sequential blocks.

| Benchmark | reth-db | signet-hot-mdbx |
|-----------|--------|----------------|
| Get header by number | `tx.get::<Headers>(num)` | `reader.get::<Headers>(&num)` |
| Sequential header scan | `cursor.walk(0..)` | `traverse().iter()` |
| Random header lookups × 1000 | loop | loop |

### H4: Batch Block Ingestion

Simulate writing N blocks of state changes. Each block:
- 100 account updates
- 500 storage slot updates
- 1 header

| N blocks | reth-db | signet-hot-mdbx |
|----------|--------|----------------|
| 1 | single txn, put all, commit | single txn, queue all, commit |
| 10 | 10 txns | 10 txns |
| 100 | 100 txns | 100 txns |

### H5: Full Table Scan

| Benchmark | reth-db | signet-hot-mdbx |
|-----------|--------|----------------|
| Scan PlainAccountState (100K) | `cursor.walk(None)` | `traverse().iter()` |
| Scan Headers (10K) | `cursor.walk(0..)` | `traverse().iter()` |

### H6: Mixed Read/Write Workload

Simulate block execution cycle:
1. Read 200 accounts (some miss)
2. Read 1000 storage slots
3. Write 100 updated accounts
4. Write 500 updated storage slots
5. Write 1 header
6. Commit

Measure total cycle time. Repeat for 10 blocks.

---

## Metrics Collected

For each benchmark:
- **Throughput**: ops/sec or items/sec
- **Latency**: p50, p95, p99 (from Criterion)
- **Commit latency breakdown** (where available via CommitLatency struct)

## Output

Criterion HTML reports + summary CSV for cross-comparison.

## Fairness Controls

1. **Same C library**: Both use libmdbx 0.13.x from erthink/libmdbx
2. **Same env config**: Matched geometry, sync mode, flags
3. **Same data**: Identical pre-populated databases (same RNG seed)
4. **Same hardware**: All benchmarks on same machine, same run
5. **Compiler**: Same rustc version, same opt-level (release profile)
6. **No metrics overhead**: reth-db metrics disabled for benchmarks
7. **Warm cache**: Each benchmark does warmup iterations first

## Notes

- signet-libmdbx's `Unsync` transactions are a unique feature with no reth equivalent.
  They will be benchmarked separately as a "best-case signet" data point.
- reth-db has ~40 tables vs signet's 9. Benchmarks use only the overlapping tables
  (PlainAccountState, PlainStorageState, Headers) for fair comparison.
- reth's `crossbeam` RO txn pooling may show advantages under repeated short-lived reads.
- signet's queue-based write API (`queue_raw_put`) may differ in overhead from reth's
  direct `put` — both ultimately call the same MDBX C function.
