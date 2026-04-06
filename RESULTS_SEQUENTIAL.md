# Sequential Benchmark Results: reth vs signet MDBX Storage

**Date:** 2026-04-05
**Mode:** Sequential (2s warmup, 5s measurement, no parallel execution)
**Platform:** Linux 6.12, x86_64
**Both sides:** libmdbx 0.13.x, SafeNoSync, WRITEMAP, NO_RDAHEAD, 1GB max geometry

## Binding Layer (B1–B7)

| Benchmark | reth | signet sync | signet unsync | unsync vs reth |
|---|--:|--:|--:|---|
| **B1: RO txn create** | 212 ns | **151 ns** | **118 ns** | 1.8x |
| **B1: RW txn create** | 27.8 µs | 35.2 µs | **97 ns** | **286x** |
| **B2: get 32B** | 258 µs | 253 µs | **238 µs** | 8% |
| **B2: get 256B** | 258 µs | 261 µs | **244 µs** | 6% |
| **B2: get 4KB** | 393 µs | 384 µs | **253 µs** | **36%** |
| **B3: upsert 100** | 54.6 µs | 51.9 µs | **12.4 µs** | **4.4x** |
| **B3: upsert 1K** | 220 µs | 221 µs | **180 µs** | 18% |
| **B3: upsert 10K** | 2.64 ms | 2.79 ms | **2.55 ms** | 3% |
| **B3: append 100** | 536 µs | — | **411 µs** | 23% |
| **B3: append 1K** | 1.56 ms | — | **1.42 ms** | 9% |
| **B3: append 10K** | 7.36 ms | — | **6.30 ms** | 14% |
| **B4: forward scan 100K** | 1.88 ms | 1.92 ms | **1.16 ms** | **1.6x** |
| **B4: reverse scan 100K** | 2.28 ms | **1.76 ms** | **1.06 ms** | **2.2x** |
| **B5: seek + 1000** | 19.8 µs | 20.2 µs | **12.9 µs** | **35%** |
| **B6: 10 dups/key** | 742 ns | **753 ns** | 651 ns | 12% |
| **B6: dupfixed 10 dups** | — | — | **523 ns** | — |
| **B6: full dupsort 100K** | 2.19 ms | **2.15 ms** | 1.46 ms | 33% |
| **B6: full dupfixed 100K** | — | — | **640 µs** | **3.4x** |
| **B7: commit 1** | 33.4 µs | 32.8 µs | **382 ns** | **87x** |
| **B7: commit 100** | 51.4 µs | 51.3 µs | **20.8 µs** | **2.5x** |
| **B7: commit 1K** | 246 µs | 259 µs | **213 µs** | 14% |
| **B7: commit 10K** | 2.55 ms | 2.79 ms | **2.52 ms** | 1% |

## Hot DB Layer (H1–H6)

| Benchmark | reth | signet | delta |
|---|--:|--:|---|
| **H1: get account (100% hit)** | 297 µs | **283 µs** | signet 5% faster |
| **H1: get account (50% miss)** | 222 µs | **213 µs** | signet 4% faster |
| **H2: single slot lookup ×1000** | **281 µs** | 532 µs | **reth 1.9x faster** |
| **H2: all slots for 1 addr** | **965 ns** | 1.19 µs | reth 19% faster |
| **H3: header random ×1000** | **348 µs** | 354 µs | ~same |
| **H3: header sequential scan 10K** | 1.71 ms | **1.69 ms** | ~same |
| **H4: ingest 1 block** | 11.2 ms | **10.2 ms** | signet 9% faster |
| **H4: ingest 10 blocks** | **95.6 ms** | 102 ms | reth 6% faster |
| **H4: ingest 100 blocks** | 995 ms | **994 ms** | identical |
| **H5: scan accounts 100K** | 5.76 ms | **2.36 ms** | **signet 2.4x faster** |
| **H5: scan headers 10K** | 1.75 ms | **1.70 ms** | signet 3% faster |
| **H6: mixed workload (10 blocks)** | **126 ms** | 134 ms | reth 6% faster |

## Notes

- Binding sync-vs-sync is identical — confirms same C library, negligible wrapper cost.
- signet unsync is the primary binding-layer differentiator (up to 286x for RW txn creation).
- Hot DB layer: reth wins random DUPSORT access (1.9x); signet wins sequential scans (2.4x).
- Hot DB uses each side's idiomatic API: reth=sync txns, signet=unsync txns.
- Architectural schema differences: reth StorageEntry=64B DUPSORT value, signet=32B; signet Headers store SealedHeader (+32B hash).
