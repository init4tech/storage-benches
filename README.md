# storage-benches

Performance comparison of two Ethereum execution client storage stacks wrapping
libmdbx 0.13.x:

| Layer | reth | signet |
|-------|------|--------|
| Raw MDBX bindings | `reth-libmdbx` | `signet-libmdbx` |
| Typed hot DB | `reth-db` | `signet-hot-mdbx` |

## Quick Start

```bash
# Full run (bindings + hot DB)
./run_benches.sh

# Quick mode (~10 min, ~5% noise)
./run_benches.sh --quick

# Subset
./run_benches.sh --bindings   # B1-B7 only
./run_benches.sh --hotdb      # H1-H6 only
./run_benches.sh B2           # filter to a specific group
```

**Prerequisites:** Rust toolchain (edition 2024, MSRV 1.92), Linux x86_64.

## Methodology

All benchmarks use **Criterion.rs 0.5** with deterministic, seeded
pseudo-random data (shared `bench-shared` crate, seed `0xBEEF_CAFE_DEAD_F00D`).
Both sides use matched MDBX configuration: 1 GB max, SafeNoSync, WRITEMAP,
NO_RDAHEAD, 4 KB pages.

Compilation uses `opt-level = 3`, `lto = "fat"`, `codegen-units = 1` to
minimize measurement noise.

Two run modes:

| Mode | Warmup | Measurement | Notes |
|------|--------|-------------|-------|
| Quick | 1 s | 3 s | Parallel, ~5% variance |
| Sequential | 2 s | 5 s | No parallel, authoritative |

See [METHODOLOGY.md](METHODOLOGY.md) for full details on data generation,
environment configuration, fairness controls, and benchmark design.

## Benchmarks

### Part 1: Raw Bindings (B1-B7)

Compares the Rust wrappers around the same C library. Signet provides both
sync and unsync (`!Send`, `!Sync`) transaction types; reth has sync only.

| Benchmark | What it measures |
|-----------|-----------------|
| **B1** Transaction creation | Wrapper overhead: mutex, channel, Arc vs raw pointer |
| **B2** Point reads | 1,000 random gets on 100K entries (32B, 256B, 4KB values) |
| **B3** Batch writes | N upserts/appends in a single RW txn (N = 100, 1K, 10K) |
| **B4** Cursor iteration | Full forward/reverse scan over 100K entries |
| **B5** Range queries | Seek to midpoint + 1,000 entries forward |
| **B6** DUPSORT | Per-key dup reads, full scan, DUPFIXED page-batched scan |
| **B7** Commit latency | Commit cost at batch sizes 1, 100, 1K, 10K |

**Key finding:** Sync-vs-sync performance is identical (same C library).
Signet's unsync transactions are the differentiator: 286x faster RW txn
creation, 1.6-2.2x faster cursor scans, 87x faster single-item commits.
DUPFIXED page-batched scan is 3.4x faster (signet-only feature).

### Part 2: Hot DB Layer (H1-H6)

Compares the typed abstraction layers with serialization codecs, table schemas,
and cursor management. Each side uses its idiomatic API (reth = sync txns,
signet = unsync txns).

| Benchmark | What it measures |
|-----------|-----------------|
| **H1** Account reads | 1,000 random lookups on 100K PlainAccountState (100% hit, 50% miss) |
| **H2** Storage slot reads | 1,000 random DUPSORT lookups (10K addrs x 10 slots) |
| **H3** Header reads | Random + sequential access on 10K headers |
| **H4** Block ingestion | Write 100 accounts + 500 slots + 1 header per block (1, 10, 100 blocks) |
| **H5** Full table scans | Complete iteration over accounts (100K) and headers (10K) |
| **H6** Mixed workload | Simulated block execution: reads + writes + commit, 10 blocks |

**Key findings:**
- **Signet wins scans**: 2.4x faster full account scan (fixed-size codec +
  unsync cursors)
- **Reth wins random DUPSORT**: ~20% faster storage slot lookup (signet's
  `exact_dual` does a redundant B-tree traversal — tracked as ENG-2126)
- **Mixed workload**: reth 10% faster in production path; signet 4% faster
  with cursor reuse

### Summary

| Category | Winner | Magnitude |
|----------|--------|-----------|
| RW txn creation | signet | 286x |
| Small commits | signet | 87x |
| Cursor iteration | signet | 1.6-2.2x |
| Full account scan | signet | 2.4x |
| DUPFIXED scan | signet | 3.4x |
| Point reads | signet | 6-36% |
| Random slot lookup | reth | 20% |
| Mixed workload (production) | reth | 10% |
| Mixed workload (cursor reuse) | signet | 4% |
| Large batches / headers | ~tied | <3% |

Neither side is categorically faster. Signet wins sequential and
transaction-heavy workloads via unsync transactions. Reth wins random DUPSORT
access due to a fixable redundant traversal in signet's `exact_dual`.

## Results

- [RESULTS.md](RESULTS.md) — quick mode results
- [RESULTS_SEQUENTIAL.md](RESULTS_SEQUENTIAL.md) — sequential mode results
  (authoritative)
- [ANALYSIS.md](ANALYSIS.md) — detailed analysis with root cause explanations
- [FUZZ_RESULTS.md](FUZZ_RESULTS.md) — fuzz and proptest results for
  signet-libmdbx
- [results/](results/) — raw Criterion output

## Project Structure

```
├── crates/
│   ├── shared/          # bench-shared: constants + data generators
│   ├── reth-bench/      # B1-B7 bindings + H1-H6 hot DB for reth
│   └── signet-bench/    # B1-B7 bindings + H1-H6 hot DB for signet
├── results/             # raw benchmark output
├── run_benches.sh       # benchmark runner script
├── METHODOLOGY.md       # detailed methodology
├── BENCH_SPEC.md        # original benchmark specification
├── RESULTS.md           # quick mode results
├── RESULTS_SEQUENTIAL.md # sequential mode results
└── ANALYSIS.md          # detailed analysis
```
