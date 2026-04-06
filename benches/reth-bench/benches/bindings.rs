//! Benchmarks for reth-libmdbx bindings (B1-B7).

use bench_shared::{
    make_key, make_value, shuffled_indices, sorted_keys, DB_MAX_SIZE, DUPS_PER_KEY, LARGE_N,
    LOOKUP_COUNT, MEDIUM_N, RANGE_COUNT,
};
use criterion::{
    criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use reth_libmdbx::*;
use std::hint::black_box;
use tempfile::{TempDir, tempdir};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Matched env config: 1GB, SafeNoSync, WRITEMAP, NO_RDAHEAD, 4 DBs.
fn open_env(dir: &TempDir) -> Environment {
    let mut builder = Environment::builder();
    builder
        .set_max_dbs(4)
        .set_geometry(Geometry {
            size: Some(0..DB_MAX_SIZE),
            growth_step: Some(64 * 1024 * 1024),
            ..Default::default()
        })
        .set_kind(EnvironmentKind::WriteMap)
        .set_flags(EnvironmentFlags {
            no_rdahead: true,
            coalesce: true,
            mode: Mode::ReadWrite {
                sync_mode: SyncMode::SafeNoSync,
            },
            ..Default::default()
        });
    builder.open(dir.path()).unwrap()
}

/// Populate the default (unnamed) DB with `n` sorted 32-byte key entries.
fn populate(env: &Environment, n: u32, val_size: usize) {
    let txn = env.begin_rw_txn().unwrap();
    let db = txn.open_db(None).unwrap();
    for i in 0..n {
        let key = make_key(i);
        let val = make_value(i, val_size);
        txn.put(db.dbi(), key, &val, WriteFlags::empty()).unwrap();
    }
    txn.commit().unwrap();
}

/// Populate a DUPSORT table: `n` keys, each with `dups` duplicate 32-byte values.
fn populate_dupsort(env: &Environment, n: u32, dups: u32) {
    let txn = env.begin_rw_txn().unwrap();
    let db = txn
        .create_db(Some("dupsort"), DatabaseFlags::DUP_SORT)
        .unwrap();
    for i in 0..n {
        let key = make_key(i);
        for d in 0..dups {
            let val = make_value(i * dups + d, 32);
            txn.put(db.dbi(), key, &val, WriteFlags::empty()).unwrap();
        }
    }
    txn.commit().unwrap();
}

// ---------------------------------------------------------------------------
// B1: Transaction creation
// ---------------------------------------------------------------------------

fn b1_txn_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("B1_txn_creation");
    let dir = tempdir().unwrap();
    let env = open_env(&dir);
    populate(&env, 100, 32); // need some data so env is initialized

    group.bench_function("reth/ro_txn", |b| {
        b.iter(|| {
            let txn = env.begin_ro_txn().unwrap();
            black_box(&txn);
            drop(txn);
        })
    });

    group.bench_function("reth/rw_txn", |b| {
        b.iter(|| {
            let txn = env.begin_rw_txn().unwrap();
            black_box(&txn);
            drop(txn);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// B2: Point reads (random)
// ---------------------------------------------------------------------------

fn b2_point_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("B2_point_reads");

    for &val_size in &[32usize, 256, 4096] {
        let dir = tempdir().unwrap();
        let env = open_env(&dir);
        let n = LARGE_N;
        populate(&env, n, val_size);

        let indices = shuffled_indices(n);
        let lookup_keys: Vec<[u8; 32]> =
            indices.iter().take(LOOKUP_COUNT).map(|&i| make_key(i)).collect();

        group.throughput(Throughput::Elements(LOOKUP_COUNT as u64));
        group.bench_with_input(
            BenchmarkId::new("reth/get", format!("{val_size}B_val")),
            &lookup_keys,
            |b, keys| {
                let txn = env.begin_ro_txn().unwrap();
                let db = txn.open_db(None).unwrap();
                b.iter(|| {
                    let mut total = 0usize;
                    for key in keys {
                        if let Some(val) = txn
                            .get::<Vec<u8>>(db.dbi(), key.as_slice())
                            .unwrap()
                        {
                            total += val.len();
                        }
                    }
                    black_box(total);
                })
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// B3: Batch writes
// ---------------------------------------------------------------------------

fn b3_batch_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("B3_batch_writes");

    for &batch in &[100u32, 1_000, 10_000] {
        group.throughput(Throughput::Elements(batch as u64));

        // UPSERT (random order)
        group.bench_function(
            BenchmarkId::new("reth/upsert", batch),
            |b| {
                let indices = shuffled_indices(batch);
                let pairs: Vec<([u8; 32], Vec<u8>)> = indices
                    .iter()
                    .map(|&i| (make_key(i), make_value(i, 256)))
                    .collect();

                b.iter_batched(
                    || {
                        let dir = tempdir().unwrap();
                        let env = open_env(&dir);
                        {
                            let txn = env.begin_rw_txn().unwrap();
                            let _ = txn.open_db(None).unwrap();
                            txn.commit().unwrap();
                        }
                        (dir, env)
                    },
                    |(dir, env)| {
                        let txn = env.begin_rw_txn().unwrap();
                        let db = txn.open_db(None).unwrap();
                        for (k, v) in &pairs {
                            txn.put(db.dbi(), k.as_slice(), v.as_slice(), WriteFlags::UPSERT)
                                .unwrap();
                        }
                        txn.commit().unwrap();
                        drop(dir);
                    },
                    BatchSize::PerIteration,
                )
            },
        );

        // APPEND (sorted order — fastest path)
        group.bench_function(
            BenchmarkId::new("reth/append", batch),
            |b| {
                let keys = sorted_keys(batch);
                b.iter_batched(
                    || {
                        let dir = tempdir().unwrap();
                        let env = open_env(&dir);
                        (dir, env)
                    },
                    |(dir, env)| {
                        let txn = env.begin_rw_txn().unwrap();
                        let db = txn.open_db(None).unwrap();
                        for (i, k) in keys.iter().enumerate() {
                            let v = make_value(i as u32, 256);
                            txn.put(db.dbi(), k.as_slice(), v.as_slice(), WriteFlags::APPEND)
                                .unwrap();
                        }
                        txn.commit().unwrap();
                        drop(dir);
                    },
                    BatchSize::PerIteration,
                )
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// B4: Sequential cursor iteration
// ---------------------------------------------------------------------------

fn b4_sequential_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("B4_cursor_iteration");
    let dir = tempdir().unwrap();
    let env = open_env(&dir);
    let n = LARGE_N;
    populate(&env, n, 32);

    group.throughput(Throughput::Elements(n as u64));

    group.bench_function("reth/forward_scan", |b| {
        b.iter(|| {
            let txn = env.begin_ro_txn().unwrap();
            let dbi = txn.open_db(None).unwrap().dbi();
            let mut cursor = txn.cursor(dbi).unwrap();
            let mut count = 0u64;
            if cursor.first::<(), ()>().unwrap().is_some() {
                count += 1;
                while cursor.next::<(), ()>().unwrap().is_some() {
                    count += 1;
                }
            }
            black_box(count);
        })
    });

    group.bench_function("reth/reverse_scan", |b| {
        b.iter(|| {
            let txn = env.begin_ro_txn().unwrap();
            let dbi = txn.open_db(None).unwrap().dbi();
            let mut cursor = txn.cursor(dbi).unwrap();
            let mut count = 0u64;
            if cursor.last::<(), ()>().unwrap().is_some() {
                count += 1;
                while cursor.prev::<(), ()>().unwrap().is_some() {
                    count += 1;
                }
            }
            black_box(count);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// B5: Range queries
// ---------------------------------------------------------------------------

fn b5_range_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("B5_range_queries");
    let dir = tempdir().unwrap();
    let env = open_env(&dir);
    let n = LARGE_N;
    populate(&env, n, 256);

    let mid_key = make_key(n / 2);

    group.throughput(Throughput::Elements(RANGE_COUNT as u64));

    group.bench_function("reth/seek_then_1000", |b| {
        b.iter(|| {
            let txn = env.begin_ro_txn().unwrap();
            let dbi = txn.open_db(None).unwrap().dbi();
            let mut cursor = txn.cursor(dbi).unwrap();
            let mut count = 0u64;
            if cursor.set_range::<(), ()>(mid_key.as_slice()).unwrap().is_some() {
                count += 1;
                for _ in 1..RANGE_COUNT {
                    if cursor.next::<(), ()>().unwrap().is_none() {
                        break;
                    }
                    count += 1;
                }
            }
            black_box(count);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// B6: DUPSORT operations
// ---------------------------------------------------------------------------

fn b6_dupsort(c: &mut Criterion) {
    let mut group = c.benchmark_group("B6_dupsort");
    let dir = tempdir().unwrap();
    let env = open_env(&dir);
    populate_dupsort(&env, MEDIUM_N, DUPS_PER_KEY);

    // Read all dups for a single key
    let target_key = make_key(MEDIUM_N / 2);
    group.throughput(Throughput::Elements(DUPS_PER_KEY as u64));
    group.bench_function("reth/read_dups_one_key", |b| {
        b.iter(|| {
            let txn = env.begin_ro_txn().unwrap();
            let dbi = txn.open_db(Some("dupsort")).unwrap().dbi();
            let mut cursor = txn.cursor(dbi).unwrap();
            let mut count = 0u64;
            let iter = cursor.iter_dup_of::<(), ()>(target_key.as_slice());
            for item in iter {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    // Full DUPSORT scan
    group.throughput(Throughput::Elements((MEDIUM_N * DUPS_PER_KEY) as u64));
    group.bench_function("reth/full_dupsort_scan", |b| {
        b.iter(|| {
            let txn = env.begin_ro_txn().unwrap();
            let dbi = txn.open_db(Some("dupsort")).unwrap().dbi();
            let mut cursor = txn.cursor(dbi).unwrap();
            let mut count = 0u64;
            if cursor.first::<(), ()>().unwrap().is_some() {
                count += 1;
                while cursor.next::<(), ()>().unwrap().is_some() {
                    count += 1;
                }
            }
            black_box(count);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// B7: Commit latency
// ---------------------------------------------------------------------------

fn b7_commit_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("B7_commit_latency");

    for &batch in &[1u32, 100, 1_000, 10_000] {
        group.throughput(Throughput::Elements(batch as u64));
        group.bench_function(BenchmarkId::new("reth/commit", batch), |b| {
            let dir = tempdir().unwrap();
            let env = open_env(&dir);
            // Ensure DB exists
            {
                let txn = env.begin_rw_txn().unwrap();
                let _ = txn.open_db(None).unwrap();
                txn.commit().unwrap();
            }

            let pairs: Vec<([u8; 32], Vec<u8>)> = (0..batch)
                .map(|i| (make_key(i), make_value(i, 256)))
                .collect();
            b.iter(|| {
                let txn = env.begin_rw_txn().unwrap();
                let db = txn.open_db(None).unwrap();
                for (k, v) in &pairs {
                    txn.put(db.dbi(), k.as_slice(), v.as_slice(), WriteFlags::UPSERT)
                        .unwrap();
                }
                let latency = txn.commit().unwrap();
                black_box(latency);
            })
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    b1_txn_creation,
    b2_point_reads,
    b3_batch_writes,
    b4_sequential_iteration,
    b5_range_queries,
    b6_dupsort,
    b7_commit_latency,
);
criterion_main!(benches);
