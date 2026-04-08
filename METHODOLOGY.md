# Benchmark Methodology

## Objective

Measure the performance difference between two Ethereum execution client storage
stacks that wrap the same underlying C database (libmdbx 0.13.x):

| Layer | reth | signet |
|-------|------|--------|
| Raw MDBX bindings | `reth-libmdbx` | `signet-libmdbx` |
| Typed hot DB | `reth-db` | `signet-hot-mdbx` |

The goal is to isolate *where* performance diverges — the Rust wrapper, the
transaction model, the codec, or the schema — rather than simply declaring a
winner.

---

## Test Harness

### Framework

All benchmarks use **Criterion.rs 0.5** with identical configuration on both
sides. Criterion was chosen because both projects already depend on it, it
provides statistical rigor (confidence intervals, outlier detection, regression
detection), and it produces comparable output formats.

### Workspace Layout

```
crates/
├── shared/                # bench-shared: constants + data generators
│   └── src/lib.rs
├── reth-bench/            # B1-B7 bindings + H1-H6 hot DB for reth
│   └── benches/
│       ├── bindings.rs
│       └── hotdb.rs
└── signet-bench/          # B1-B7 bindings + H1-H6 hot DB for signet
    └── benches/
        ├── bindings.rs
        └── hotdb.rs
```

The `bench-shared` crate provides all data generation and constants. Both
`reth-bench` and `signet-bench` import it, ensuring identical test data on
both sides.

### Compilation

The workspace `[profile.bench]` is set to maximize codegen quality and minimize
measurement noise:

```toml
[profile.bench]
opt-level = 3
lto = "thin"
debug = "line-tables-only"
strip = false
codegen-units = 1
```

`codegen-units = 1` eliminates non-determinism from parallel codegen.
`lto = "thin"` enables cross-crate inlining without the full-LTO compile
time penalty. `debug = "line-tables-only"` retains enough info for profiling
without affecting codegen.

### Dependency Unification

Both reth and signet depend on `signet-libmdbx` transitively (signet directly,
reth does not). To prevent duplicate type errors from cargo resolving two
versions of the same crate, the workspace patches crates.io with local paths:

Signet crates are pulled from crates.io; reth crates are pulled from GitHub
(pinned to a release tag). No local `[patch.crates-io]` overrides are needed.

---

## Data Generation

All test data is produced by the `bench-shared` crate using deterministic,
seeded pseudo-random number generation. The same seed (`0xBEEF_CAFE_DEAD_F00D`)
is used everywhere, so both sides operate on byte-identical input data.

### Key Generation

```rust
pub fn make_key(index: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[0..4].copy_from_slice(&index.to_be_bytes());
    let mut rng = SmallRng::seed_from_u64(index as u64);
    rng.fill(&mut key[4..]);
    key
}
```

Keys are 32 bytes, matching the size of Ethereum keccak hashes. The first 4
bytes are a big-endian index prefix, giving the keys a natural lexicographic
sort order (important for `APPEND` benchmarks). The remaining 28 bytes are
pseudo-random, preventing MDBX from hitting degenerate B-tree cases that would
not occur with real data.

### Value Generation

```rust
pub fn make_value(index: u32, size: usize) -> Vec<u8> {
    let mut val = vec![0u8; size];
    let mut rng = SmallRng::seed_from_u64(index as u64 ^ 0xFFFF);
    rng.fill(val.as_mut_slice());
    val
}
```

Values are generated with a different XOR mask than keys to avoid any
correlation between key bytes and value bytes. Three value sizes are tested
(32B, 256B, 4096B) to capture the effect of overflow pages and memcpy cost.

### Access Patterns

Random-access benchmarks use a pre-shuffled index array generated once per
benchmark group:

```rust
pub fn shuffled_indices(n: u32) -> Vec<u32> {
    let mut indices: Vec<u32> = (0..n).collect();
    let mut rng = SmallRng::seed_from_u64(SEED);
    indices.shuffle(&mut rng);
    indices
}
```

The same shuffle order is used for both sides. Within each Criterion iteration,
the benchmark loops over `LOOKUP_COUNT` (1,000) entries from this shuffled
array. This amortizes per-iteration overhead and produces stable measurements.

### Constants

| Constant | Value | Purpose |
|----------|-------|---------|
| `LARGE_N` | 100,000 | Account tables, full cursor scans |
| `MEDIUM_N` | 10,000 | DUPSORT key count, header tables |
| `SMALL_N` | 100 | Small batch writes |
| `LOOKUP_COUNT` | 1,000 | Random reads per iteration |
| `RANGE_COUNT` | 1,000 | Entries per range query |
| `DUPS_PER_KEY` | 10 | Duplicate values per DUPSORT key |
| `DB_MAX_SIZE` | 1 GB | MDBX geometry max |

---

## Environment Configuration

Both sides open MDBX with matched parameters:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Max DB size | 1 GB | Large enough for 100K entries at 4KB; small enough to avoid TLB pressure |
| Growth step | 64 MB | Default reth value; signet defaults to 4 GB but this has negligible effect at benchmark data sizes |
| Sync mode | `SafeNoSync` | Removes fsync noise — we are measuring MDBX data structure performance, not disk I/O |
| Flags | `WRITEMAP`, `NO_RDAHEAD` | `WRITEMAP` enables mmap-based writes (standard for both projects). `NO_RDAHEAD` disables OS readahead to prevent the OS from prefetching pages outside the benchmark's access pattern |
| Max DBs | 4 (bindings), dynamic (hot DB) | Binding benchmarks use named sub-databases; hot DB opens all project-defined tables |
| Page size | 4096 (default) | Standard 4KB pages matching OS page size |

### One Known Asymmetry (Geometry)

The hot-DB benchmarks have an unmatched geometry configuration: reth explicitly
sets 1 GB max / 64 MB growth, while signet uses its default of 8 TB max / 4 GB
growth (not overridden in the benchmark). For benchmarks using < 100 MB of data,
this affects only virtual address space reservation, not actual page allocation
or I/O. Impact: negligible, documented for completeness.

---

## Measurement Configuration

Two run modes were used:

| Mode | Warmup | Measurement | Parallel |
|------|--------|-------------|----------|
| Quick | 1 s | 3 s | Yes (default Criterion) |
| Sequential | 2 s | 5 s | No (`--bench` run one at a time) |

Both modes collect **100 samples** per benchmark (Criterion default). Criterion
automatically determines the number of iterations per sample to fill the
measurement window.

The **quick** mode was used for initial exploration and spot-checking. The
**sequential** mode was used for the final reported numbers. Sequential
execution avoids interference between benchmarks that share CPU cache, memory
bandwidth, and MDBX's global write lock.

### Statistical Analysis

Criterion reports for each benchmark:
- **Point estimate**: Mean of the sampling distribution
- **95% confidence interval**: `[lower, upper]` bounds on the true mean
- **Outlier classification**: Low severe, low mild, high mild, high severe
- **Change detection**: vs. previous run (p-value at 0.05 threshold)

The reported numbers in RESULTS.md and RESULTS_SEQUENTIAL.md are the Criterion
point estimates (mean). Where two results differ by less than the confidence
interval overlap, they are reported as "identical" or "~same".

---

## Benchmark Design: Binding Layer (B1–B7)

The binding-layer benchmarks compare the Rust wrappers around the same C
library. Each benchmark is written twice — once in `reth-bench/benches/bindings.rs`
and once in `signet-bench/benches/bindings.rs` — calling the equivalent API on
each side.

### B1: Transaction Creation

Measures the time to create and drop a transaction with no operations inside.
This isolates the wrapper overhead: mutex acquisition, channel coordination
(reth RW), Arc construction (reth), vs. raw pointer storage (signet unsync).

| Variant | reth | signet |
|---------|------|--------|
| RO | `env.begin_ro_txn()` | `begin_ro_sync()` / `begin_ro_unsync()` |
| RW | `env.begin_rw_txn()` | `begin_rw_sync()` / `begin_rw_unsync()` |

Signet tests both sync and unsync variants. Reth has only sync transactions.

### B2: Point Reads

Pre-populates a database with `LARGE_N` (100K) entries, then performs
`LOOKUP_COUNT` (1,000) random `get()` calls per Criterion iteration.
Three value sizes (32B, 256B, 4KB) are tested to measure the effect of MDBX
overflow pages on per-access cost.

The random indices are generated once (via `shuffled_indices`) and reused across
iterations to ensure the access pattern is identical between runs and between
sides.

### B3: Batch Writes

Writes N entries in a single RW transaction, then commits. Two write modes:
- **UPSERT**: Random key order (exercises B-tree insertion with splits)
- **APPEND**: Pre-sorted key order (exercises MDBX's append-optimized path)

Batch sizes: 100, 1,000, 10,000. The scaling reveals whether advantages come
from fixed per-transaction costs (which amortize away) or per-operation costs
(which scale linearly).

### B4: Sequential Cursor Iteration

Full forward and reverse scans over `LARGE_N` (100K) entries. This benchmark
has the highest call count per iteration (100K `cursor.next()` / `cursor.prev()`
calls), making it highly sensitive to per-call overhead. The ~7 ns per-step
saving from unsync transactions compounds to ~700 µs over a full scan.

Signet additionally benchmarks its `iter_start()` iterator API to confirm it
matches manual `first()` + `next()` loop performance.

### B5: Range Queries

Seeks to the midpoint of a 100K-entry database (`set_range` on key 50,000),
then reads `RANGE_COUNT` (1,000) entries forward. This simulates the common
pattern of prefix-bounded iteration used in state trie traversal.

### B6: DUPSORT Operations

Creates a table with `MEDIUM_N` (10K) keys, each having `DUPS_PER_KEY` (10)
duplicate values of 32 bytes.

**Intentional asymmetry**: Reth creates the table with `DUP_SORT` only. Signet
creates it with `DUP_SORT | DUP_FIXED`. This is not a configuration error — it
reflects a real API difference. Reth's bindings do not expose the `DUP_FIXED`
flag; signet's do. The DUPFIXED benchmark variants are labeled as signet-only.
The non-DUPFIXED comparisons (sync vs sync) are apples-to-apples.

Benchmarks:
- **Per-key read**: Open cursor, seek to a random key, iterate its 10 duplicates
- **Full scan**: Iterate all 100K entries (10K keys × 10 dups)
- **DUPFIXED full scan** (signet only): Page-batched bulk read via
  `iter_dupfixed_start()`, which reads multiple values per MDBX page fetch

### B7: Commit Latency

Writes N entries in a single RW transaction, then measures `commit()` /
`commit_with_latency()`. Batch sizes: 1, 100, 1K, 10K. This isolates the
commit path: transaction manager notification, mutex release, and actual MDBX
page flush.

The scaling pattern (87x at batch=1, 1% at batch=10K) reveals that the unsync
advantage is a fixed-cost savings in the commit wrapper, not a per-page
improvement.

---

## Benchmark Design: Hot DB Layer (H1–H6)

The hot-DB benchmarks compare the typed abstraction layers that each project
builds on top of their bindings. These introduce serialization codecs, table
schemas, cursor management, and write buffering.

Both sides use `DatabaseEnv` directly with SafeNoSync. No `TempDatabase`
wrapper, no metrics layer.

### Data Generation (Hot DB)

The hot-DB benchmarks generate Ethereum-like typed data:

- **Addresses**: 20-byte, deterministic from index (`make_address`)
- **Accounts**: nonce (u64), balance (U256), code_hash (B256) (`make_account`)
- **Headers**: Sequential block numbers with random hashes (`make_header`)
- **Storage entries**: B256 slot key → U256 value (`make_storage_entry` / `make_slot_key` + `make_slot_value`)

Each side's populate function uses its own idiomatic write API (reth: `tx.put()`,
signet: `writer.queue_put()`) to insert data. This means the on-disk format
matches what each project would produce in production.

### H1: Account Reads

1,000 random lookups on 100K `PlainAccountState` entries. Two sub-benchmarks:
- **100% hit**: All lookup keys exist in the database
- **50% miss**: Half the keys are beyond the populated range

The miss variant isolates B-tree traversal cost from deserialization cost (misses
skip deserialization entirely).

### H2: Storage Slot Reads (DUPSORT)

10K addresses × 10 storage slots each, stored in `PlainStorageState` (a DUPSORT
table). 1,000 random (address, slot) lookups per iteration.

**Schema difference**: Reth stores `StorageEntry` (64 bytes: 32B slot key + 32B
value) as the DUPSORT value with the address as the primary key. Signet stores
the 32B slot key as the MDBX subkey and the 32B value as the DUPSORT value
(DUP_FIXED). Signet reads half the bytes per entry but pays for a more complex
lookup path (`get_dual` → `exact_dual` → two MDBX calls).

Two variants are benchmarked per side to isolate the cursor-creation cost from
the lookup cost:
- **Production path**: New cursor per `get()` / `get_dual()` call (matches how
  both projects behave in production — every EVM `SLOAD` creates a fresh cursor)
- **Cursor reuse**: Single cursor reused across all 1,000 lookups (shows the
  theoretical optimum)

Additionally: **All slots for 1 address** — retrieve all 10 slots for a single
address via `walk_dup` (reth) / `exact_dual` iteration (signet).

### H3: Header Reads

10K sequential headers. Two access patterns:
- **Random**: 1,000 random block-number lookups
- **Sequential**: Full forward scan of all 10K headers

Headers is a simple key-value table (BlockNumber → Header/SealedHeader). Signet
stores `SealedHeader` (header + 32B block hash), making its values slightly
larger than reth's `Header`.

### H4: Batch Block Ingestion

Simulates writing realistic block data. Each block consists of:
- 100 account updates
- 500 storage slot updates
- 1 header

Tested at 1, 10, and 100 blocks. Each block is written in its own RW
transaction. The scaling from 1→100 blocks reveals whether write-path
differences are per-transaction (fixed cost) or per-operation (scales with data).

### H5: Full Table Scans

Complete forward iteration over:
- `PlainAccountState`: 100K accounts
- `Headers`: 10K headers

These benchmarks are dominated by deserialization cost and per-step cursor
overhead. They expose codec efficiency differences (reth's variable-length
`Compact` encoding vs signet's fixed-size 72B account encoding).

### H6: Mixed Read/Write Workload

The most realistic benchmark — simulates a block execution cycle. Per block:
1. Read 200 accounts (some miss)
2. Read 1,000 storage slots (random)
3. Write 100 updated accounts
4. Write 500 updated storage slots
5. Write 1 header
6. Commit

Repeated for 10 blocks. Like H2, two variants are tested per side:
- **Production path**: Cursor per call (matches production behavior)
- **Cursor reuse**: Persistent read cursor for storage slot lookups

This benchmark weights storage slot reads heavily (1,000 per block × 10 blocks =
10,000 random DUPSORT lookups), making it sensitive to the `exact_dual`
performance issue documented as ENG-2126.

---

## Fairness Controls

### Controlled Variables

| Variable | How controlled |
|----------|---------------|
| C library | Same libmdbx 0.13.x source compiled from both binding crates |
| MDBX config | Matched geometry, sync mode, flags, page size |
| Test data | Identical: shared `bench-shared` crate, deterministic seed |
| Access pattern | Same shuffled indices, same lookup count, same range count |
| Measurement | Same Criterion version (0.5), same warmup/measurement times |
| Compiler | Same `rustc`, same `[profile.bench]`, same machine, same run |
| Entry counts | Same N values for all corresponding benchmarks |

### Controlled-for Asymmetries

These differences are intentional — they reflect genuine architectural choices,
not benchmark configuration errors:

1. **Transaction type**: reth `tx()` = sync, signet `reader()` = unsync. Each
   side uses its idiomatic production path.
2. **DUPSORT schema**: reth `StorageEntry` = 64B value; signet = 32B DUP_FIXED
   value with slot as subkey.
3. **Account codec**: reth `Compact` (variable-length, bitflag header); signet
   fixed-size 72B.
4. **Header type**: reth `Header`; signet `SealedHeader` (+32B hash).
5. **DUP_FIXED flag**: Reth bindings do not expose it; signet's do. DUPFIXED
   benchmarks are labeled as signet-only features.
6. **Write API**: reth `tx.put()` (direct `mdbx_put`); signet
   `writer.queue_put()` (buffered, then flushed on commit).

### What Is NOT Controlled

- **Geometry in hot-DB benchmarks**: reth explicitly sets 1GB/64MB; signet uses
  its 8TB/4GB default. Negligible at benchmark data sizes (< 100 MB).
- **Metrics layer**: reth's `DatabaseEnvMetrics` is not instantiated in the
  benchmarks. In production, reth optionally records per-operation metrics;
  this overhead is excluded.

---

## Interpreting the Results

### Signal vs Noise

Differences under 5% should be treated as noise unless consistent across both
quick and sequential runs. The sequential run (2s warmup, 5s measurement, no
parallel execution) is the authoritative dataset.

### Amortization Pattern

Many of signet's advantages come from skipping fixed per-transaction costs
(mutex, channel, Arc). These benchmarks are designed to reveal this pattern by
testing multiple batch sizes. When the advantage is 87x at batch=1 and 1% at
batch=10K, the conclusion is "fixed-cost optimization" — not "signet is 87x
faster."

### Production Relevance

The hot-DB benchmarks (H1–H6) are closer to production behavior than the
binding-layer benchmarks (B1–B7). However, they still benchmark in isolation
with warm caches and no concurrent readers. Real-world performance depends on
additional factors: concurrent transaction load, OS page cache pressure, actual
disk I/O (these benchmarks use SafeNoSync), and application-level cursor
management.

---

## Reproduction

### Prerequisites

- Rust toolchain (edition 2024 compatible, MSRV 1.92 for signet)
- Linux x86_64 (tested on 6.12)
- ~12 GB disk (repos + build artifacts)

### Steps

```bash
# 1. Run benchmarks (dependencies are fetched automatically via cargo)
./run_benches.sh              # full run (bindings + hotdb)
./run_benches.sh --quick      # faster, noisier (~5% variance)
./run_benches.sh --bindings   # binding layer only
./run_benches.sh --hotdb      # hot DB layer only
./run_benches.sh B2           # filter to a specific group
```

### Expected Runtime

- Quick mode: ~10 minutes
- Sequential mode: ~30 minutes
