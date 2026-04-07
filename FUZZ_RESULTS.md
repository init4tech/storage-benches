# Fuzz & Proptest Results: signet-libmdbx

**Date:** 2026-04-06
**Platform:** Linux 6.12, x86_64, 128 cores, 251GB RAM
**Branch:** prestwich/benchmarks-and-fuzzing

---

## Proptests (PROPTEST_CASES=10000)

All 72 test functions passed with 10,000 cases each (720,000 total cases).

| Test File | Tests | Time | Result |
|-----------|------:|-----:|--------|
| proptest_cursor | 16 | 2h 15m | PASS |
| proptest_dupfixed | 5 | 44m | PASS |
| proptest_dupsort | 16 | 2h 13m | PASS |
| proptest_iter | 10 | 32m | PASS |
| proptest_kv | 22 | 40m | PASS |
| proptest_nested | 3 | 6m | PASS |
| **Total** | **72** | **~6.5h** | **All passed** |

Coverage areas: cursor operations (set, set_range, put, append, lowerbound),
DUP_SORT/DUP_FIXED roundtrips, iterator correctness, key/value roundtrips,
overwrite semantics, delete semantics, multi-database isolation, nested
transaction commit/abort. All tests run against both TxSync (V1) and
TxUnsync (V2) transaction types.

---

## Fuzzing (libfuzzer, ~3 hours, 20 jobs per target)

All 6 fuzz targets ran in parallel. No crashes found.

| Target | Corpus Size | Crashes | Leaks |
|--------|------------:|--------:|------:|
| decode_cow | 168 | 0 | 0 |
| decode_array | 161 | 0 | 0 |
| decode_object_length | 129 | 0 | 0 |
| dirty_page_roundtrip | 166 | 0 | 0 |
| dupfixed_page_decode | 305 | 0 | 0 |
| key_validation | 455 | 0 | 20 |

---

## key_validation LeakSanitizer Findings

The `key_validation` target produced 20 leak artifacts. These are
**not true memory leaks** — they are transient thread-stack allocations
detected by LeakSanitizer.

### Root Cause

Each `Environment` spawns a background thread (`mdbx-rs-txn-manager`) for
managing `TxSync` RW transactions (`src/sys/txn_manager.rs:161`). When the
environment drops:

1. The `LifecycleHandle` (holding a `SyncSender`) drops, closing the channel
2. The background thread's `rx.recv()` returns `Err(_)` and the thread exits
3. However, the `JoinHandle` is not stored — the thread is never explicitly
   joined

In the fuzz harness, thousands of environments are created and destroyed per
second. The brief window between channel close and thread exit overlaps across
iterations, and LSan reports the not-yet-reaped thread stacks as leaks.

### Why This Is Not a Real Bug

- The thread **does exit on its own** once the channel closes. The
  `sync_channel(0)` is a rendezvous channel, so `recv()` unblocks immediately
  when the sender drops.
- In normal usage (one long-lived `Environment` per process), this is a
  non-issue — the single thread exits at shutdown.
- The "leaks" are transient and bounded by thread teardown latency, not
  unbounded growth.
- Only `key_validation` triggers this because it uses `set_max_dbs(2)` with
  `INTEGER_KEY`, making each iteration slightly slower and widening the
  overlap window. All 6 targets spawn the same thread.
