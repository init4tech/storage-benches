# Benchmark Results: reth vs signet MDBX Storage

**Date:** 2026-04-05  
**Mode:** Quick (1s warmup, 3s measurement, parallel execution — expect ~5% noise)  
**Platform:** Linux 6.12, x86_64  
**Both sides:** libmdbx 0.13.x, SafeNoSync, WRITEMAP, NO_RDAHEAD, 1GB max geometry

---

## Part 1: Raw MDBX Bindings (B1–B7)

Compares `reth-libmdbx` (sync only) vs `signet-libmdbx` (sync + unsync).

### B1: Transaction Creation

| Benchmark | reth | signet sync | signet unsync |
|-----------|-----:|------------:|--------------:|
| RO txn create | 219 ns | **153 ns** | **122 ns** |
| RW txn create | 26.1 µs | 27.4 µs | **101 ns** |

- RO sync: signet 30% faster (no crossbeam pool overhead)
- RW unsync: **258x faster** than reth — avoids mutex entirely

### B2: Point Reads (1000 random lookups on 100K entries)

| Value size | reth | signet sync | signet unsync |
|-----------|-----:|------------:|--------------:|
| 32B | 266 µs | 267 µs | **247 µs** |
| 256B | 270 µs | 275 µs | **256 µs** |
| 4KB | 399 µs | 401 µs | **271 µs** |

- Sync vs sync: **identical** (same C library underneath)
- Unsync: 7% faster at small values, **32% faster at 4KB** (Arc overhead matters more when values are larger)

### B3: Batch Writes (single txn, N items, commit)

| Operation | reth | signet sync | signet unsync |
|-----------|-----:|------------:|--------------:|
| upsert 100 | 50.2 µs | **46.4 µs** | **13.5 µs** |
| upsert 1K | 260 µs | 266 µs | **208 µs** |
| upsert 10K | 3.34 ms | 3.68 ms | **3.19 ms** |
| append 100 | 567 µs | — | **387 µs** |
| append 1K | 1.62 ms | — | **1.39 ms** |
| append 10K | 9.02 ms | — | **8.82 ms** |

- Small batches: unsync dominates (txn creation overhead is proportionally large)
- At 10K: gap narrows to ~5% — C library work dominates

### B4: Full Cursor Scan (100K entries, 32B values)

| Benchmark | reth | signet sync | signet unsync |
|-----------|-----:|------------:|--------------:|
| Forward scan | 2.01 ms | 2.05 ms | **1.22 ms** |
| Forward iter (signet API) | — | — | **1.20 ms** |
| Reverse scan | 1.87 ms | **1.85 ms** | **1.18 ms** |

- Sync: identical between reth and signet
- Unsync: **1.7x faster** — consistent with B1-B7 binding results
- signet's iterator API (`iter_start`) matches manual `first()/next()` loop

### B5: Range Query (seek + 1000 entries from 100K, 256B values)

| Benchmark | reth | signet sync | signet unsync |
|-----------|-----:|------------:|--------------:|
| seek + 1000 | 21.0 µs | 21.1 µs | **13.5 µs** |

- Sync: identical
- Unsync: **36% faster**

### B6: DUPSORT (10K keys × 10 dups, 32B fixed values)

| Benchmark | reth | signet sync | signet unsync | signet dupfixed |
|-----------|-----:|------------:|--------------:|----------------:|
| Read 10 dups/key | 793 ns | **786 ns** | 660 ns | **550 ns** |
| Full scan (100K) | 2.31 ms | **2.27 ms** | 1.54 ms | **674 µs** |

- Sync vs sync: identical
- **DUPFIXED page-batched scan: 3.4x faster than reth** (674 µs vs 2.31 ms)
- This is signet's biggest single win — reads multiple values per MDBX page

### B7: Commit Latency

| Batch size | reth | signet sync | signet unsync |
|-----------|-----:|------------:|--------------:|
| 1 | 28.7 µs | 29.2 µs | **397 ns** |
| 100 | 56.4 µs | 56.9 µs | **22.0 µs** |
| 1K | 283 µs | 289 µs | **238 µs** |
| 10K | 3.26 ms | 2.92 ms | **2.77 ms** |

- Sync: identical
- Unsync single-item commit: **72x faster** (no mutex acquire/release)
- At 10K: only 15% gap — commit I/O dominates

---

## Part 2: Hot DB Layer (H1–H6)

Compares `reth-db` (sync txns, `Encode`/`Decode` codecs) vs `signet-hot-mdbx` (unsync txns, `KeySer`/`ValSer` codecs).

Both use `DatabaseEnv` directly with matched config (1GB, SafeNoSync). No TempDatabase wrapper, no metrics.

### H1: Account Reads (1000 lookups on 100K PlainAccountState)

| Benchmark | reth | signet | delta |
|-----------|-----:|-------:|------:|
| get_account (100% hit) | 319 µs | **304 µs** | signet 5% faster |
| get_account (50% miss) | 236 µs | 236 µs | **identical** |

- Small advantage for signet on hits (unsync txn + simpler codec)
- Misses are identical — both hit MDBX "not found" at the same speed

### H2: Storage Slot Reads (DUPSORT — 10K addrs × 10 slots)

| Benchmark | reth | signet | delta |
|-----------|-----:|-------:|------:|
| get_single_slot (1000 random) | **295 µs** | 551 µs | **reth 1.9x faster** |
| get_all_slots_one_addr (10 slots) | **1.02 µs** | 1.24 µs | reth 18% faster |

- **Reth wins significantly on single-slot lookup.** reth's `seek_by_key_subkey` with a cursor is faster than signet's `get_dual` which goes through the HotKvRead abstraction (open cursor, seek, decode dual-key format per call).
- All-slots iteration: reth's `walk_dup` is also faster — simpler iteration without FSI cache lookups per step.

### H3: Header Reads (10K headers)

| Benchmark | reth | signet | delta |
|-----------|-----:|-------:|------:|
| get_header_random (1000 lookups) | **362 µs** | 390 µs | reth 7% faster |
| scan_headers_sequential (10K) | **1.82 ms** | 1.88 ms | reth 3% faster |

- reth is faster despite sync txns. signet's overhead: `SealedHeader` is larger (extra 32B hash), `KeySer`/`ValSer` decode is heavier.

### H4: Batch Block Ingestion (100 accounts + 500 slots + 1 header per block)

| Blocks | reth | signet | delta |
|--------|-----:|-------:|------:|
| 1 | **14.4 ms** | 16.9 ms | reth 15% faster |
| 10 | **153 ms** | 158 ms | reth 3% faster |
| 100 | 1.41 s | **1.40 s** | identical |

- reth's direct `put()` is faster than signet's `queue_put_dual()` for DUPSORT storage writes (which delete-then-insert internally).
- At scale (100 blocks) the difference vanishes — MDBX I/O dominates.

### H5: Full Table Scans

| Benchmark | reth | signet | delta |
|-----------|-----:|-------:|------:|
| scan_accounts (100K) | 6.04 ms | **2.52 ms** | **signet 2.4x faster** |
| scan_headers (10K) | 1.86 ms | **1.76 ms** | signet 5% faster |

- **Account scan is signet's biggest hot-DB win.** signet's unsync cursors + `ValSer` iterator avoid the per-entry overhead that reth's `Walker` + `Decode` + compression buffer path incurs.
- Header scan: smaller advantage due to larger value sizes (decode cost proportionally smaller vs I/O).

### H6: Mixed Workload (10 blocks: read 200 accts + 1000 slots, write 100 accts + 500 slots + 1 header)

| Benchmark | reth | signet | delta |
|-----------|-----:|-------:|------:|
| block_execution_cycle | **167 ms** | 179 ms | reth 7% faster |

- reth wins the mixed workload. The storage slot reads (which reth is 1.9x faster at) dominate the cycle. signet's scan advantage doesn't help here since the workload is random-access-heavy.

---

## Summary

### Binding Layer (sync vs sync)

**Identical performance.** Same C library, same overhead. The Rust wrapper cost is noise.

### Binding Layer (signet unsync)

| Category | Improvement |
|----------|------------|
| Txn creation (RW) | 258x faster |
| Single-item commits | 72x faster |
| Small batch writes (100) | 3.7x faster |
| Full cursor scans | 1.7x faster |
| Range queries | 36% faster |
| Point reads | 7-32% faster |
| DUPFIXED page-batched scan | 3.4x faster than reth sync |

Unsync is signet's primary differentiator at the binding layer.

### Hot DB Layer

| Category | Winner | Margin |
|----------|--------|--------|
| Account point reads | signet | 5% |
| Storage slot point reads | **reth** | **1.9x** |
| Header point reads | reth | 7% |
| Block ingestion (small) | reth | 15% |
| Block ingestion (large) | tie | — |
| Full account scan | **signet** | **2.4x** |
| Full header scan | signet | 5% |
| Mixed workload | reth | 7% |

The hot DB results are more nuanced than the binding results:
- **reth wins random access** — thinner abstraction for DUPSORT seeks, no FSI cache overhead, pre-cached DBI handles
- **signet wins sequential scans** — unsync cursors + lighter iteration overhead
- **Mixed workload favors reth** because random storage lookups dominate

### Known Architectural Differences (Not Bugs)

1. **reth tx() = sync, signet reader() = unsync** — this is by design in each codebase
2. **reth PlainStorageState** stores `StorageEntry` (64B key+value) as DUPSORT value; **signet** stores `U256` (32B) with slot as DUPSORT subkey — signet's schema is more space-efficient but its `get_dual` path is currently slower
3. **reth Headers** stores `Header`; **signet** stores `SealedHeader` (header + 32B keccak hash) — larger values, heavier encode/decode
