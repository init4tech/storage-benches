//! Benchmarks for signet-libmdbx bindings (B1-B7).

use bench_shared::{
    make_key, make_value, shuffled_indices, sorted_keys, DB_MAX_SIZE, DUPS_PER_KEY, LARGE_N,
    LOOKUP_COUNT, MEDIUM_N, RANGE_COUNT,
};
use criterion::{
    criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use signet_libmdbx::*;
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
        .set_kind(signet_libmdbx::sys::EnvironmentKind::WriteMap)
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
    let txn = env.begin_rw_unsync().unwrap();
    let db = txn.open_db(None).unwrap();
    for i in 0..n {
        let key = make_key(i);
        let val = make_value(i, val_size);
        txn.put(db, key, &val, WriteFlags::empty()).unwrap();
    }
    txn.commit().unwrap();
}

/// Populate a DUPSORT table: `n` keys, each with `dups` duplicate 32-byte values.
fn populate_dupsort(env: &Environment, n: u32, dups: u32) {
    let txn = env.begin_rw_unsync().unwrap();
    let db = txn
        .create_db(Some("dupsort"), DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED)
        .unwrap();
    for i in 0..n {
        let key = make_key(i);
        for d in 0..dups {
            let val = make_value(i * dups + d, 32);
            txn.put(db, key, &val, WriteFlags::empty()).unwrap();
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
    populate(&env, 100, 32);

    group.bench_function("signet/ro_sync", |b| {
        b.iter(|| {
            let txn = env.begin_ro_sync().unwrap();
            black_box(&txn);
            drop(txn);
        })
    });

    group.bench_function("signet/rw_sync", |b| {
        b.iter(|| {
            let txn = env.begin_rw_sync().unwrap();
            black_box(&txn);
            drop(txn);
        })
    });

    group.bench_function("signet/ro_unsync", |b| {
        b.iter(|| {
            let txn = env.begin_ro_unsync().unwrap();
            black_box(&txn);
            drop(txn);
        })
    });

    group.bench_function("signet/rw_unsync", |b| {
        b.iter(|| {
            let txn = env.begin_rw_unsync().unwrap();
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

        // Sync variant (comparable to reth)
        group.bench_with_input(
            BenchmarkId::new("signet/get_sync", format!("{val_size}B_val")),
            &lookup_keys,
            |b, keys| {
                let txn = env.begin_ro_sync().unwrap();
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

        // Unsync variant (signet-only advantage)
        let dir2 = tempdir().unwrap();
        let env2 = open_env(&dir2);
        populate(&env2, n, val_size);

        group.bench_with_input(
            BenchmarkId::new("signet/get_unsync", format!("{val_size}B_val")),
            &lookup_keys,
            |b, keys| {
                let txn = env2.begin_ro_unsync().unwrap();
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

        // UPSERT — sync (comparable to reth)
        {
            let indices = shuffled_indices(batch);
            let pairs: Vec<([u8; 32], Vec<u8>)> = indices
                .iter()
                .map(|&i| (make_key(i), make_value(i, 256)))
                .collect();

            group.bench_function(
                BenchmarkId::new("signet/upsert_sync", batch),
                |b| {
                    b.iter_batched(
                        || {
                            let dir = tempdir().unwrap();
                            let env = open_env(&dir);
                            {
                                let txn = env.begin_rw_sync().unwrap();
                                let _ = txn.open_db(None).unwrap();
                                txn.commit().unwrap();
                            }
                            (dir, env)
                        },
                        |(dir, env)| {
                            let txn = env.begin_rw_sync().unwrap();
                            let db = txn.open_db(None).unwrap();
                            for (k, v) in &pairs {
                                txn.put(db, k.as_slice(), v.as_slice(), WriteFlags::UPSERT)
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

        // UPSERT — unsync (signet advantage)
        {
            let indices = shuffled_indices(batch);
            let pairs: Vec<([u8; 32], Vec<u8>)> = indices
                .iter()
                .map(|&i| (make_key(i), make_value(i, 256)))
                .collect();

            group.bench_function(
                BenchmarkId::new("signet/upsert_unsync", batch),
                |b| {
                    b.iter_batched(
                        || {
                            let dir = tempdir().unwrap();
                            let env = open_env(&dir);
                            {
                                let txn = env.begin_rw_unsync().unwrap();
                                let _ = txn.open_db(None).unwrap();
                                txn.commit().unwrap();
                            }
                            (dir, env)
                        },
                        |(dir, env)| {
                            let txn = env.begin_rw_unsync().unwrap();
                            let db = txn.open_db(None).unwrap();
                            for (k, v) in &pairs {
                                txn.put(db, k.as_slice(), v.as_slice(), WriteFlags::UPSERT)
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

        // APPEND — unsync (sorted, fastest path)
        group.bench_function(
            BenchmarkId::new("signet/append_unsync", batch),
            |b| {
                let keys = sorted_keys(batch);
                b.iter_batched(
                    || {
                        let dir = tempdir().unwrap();
                        let env = open_env(&dir);
                        (dir, env)
                    },
                    |(dir, env)| {
                        let txn = env.begin_rw_unsync().unwrap();
                        let db = txn.open_db(None).unwrap();
                        for (i, k) in keys.iter().enumerate() {
                            let v = make_value(i as u32, 256);
                            txn.put(db, k.as_slice(), v.as_slice(), WriteFlags::APPEND)
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

    // Sync — comparable to reth
    group.bench_function("signet/forward_scan_sync", |b| {
        b.iter(|| {
            let txn = env.begin_ro_sync().unwrap();
            let db = txn.open_db(None).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
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

    // Unsync — signet advantage
    let dir2 = tempdir().unwrap();
    let env2 = open_env(&dir2);
    populate(&env2, n, 32);

    group.bench_function("signet/forward_scan_unsync", |b| {
        b.iter(|| {
            let txn = env2.begin_ro_unsync().unwrap();
            let db = txn.open_db(None).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
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

    // Iterator API (idiomatic signet)
    let dir3 = tempdir().unwrap();
    let env3 = open_env(&dir3);
    populate(&env3, n, 32);

    group.bench_function("signet/forward_iter_unsync", |b| {
        b.iter(|| {
            let txn = env3.begin_ro_unsync().unwrap();
            let db = txn.open_db(None).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
            let mut count = 0u64;
            for item in cursor.iter_start::<(), ()>().unwrap() {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    // Reverse — sync (comparable to reth)
    group.bench_function("signet/reverse_scan_sync", |b| {
        b.iter(|| {
            let txn = env.begin_ro_sync().unwrap();
            let db = txn.open_db(None).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
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

    // Reverse scan — unsync
    let dir4 = tempdir().unwrap();
    let env4 = open_env(&dir4);
    populate(&env4, n, 32);

    group.bench_function("signet/reverse_scan_unsync", |b| {
        b.iter(|| {
            let txn = env4.begin_ro_unsync().unwrap();
            let db = txn.open_db(None).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
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

    group.bench_function("signet/seek_then_1000_sync", |b| {
        b.iter(|| {
            let txn = env.begin_ro_sync().unwrap();
            let db = txn.open_db(None).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
            let mut count = 0u64;
            if cursor
                .set_range::<(), ()>(mid_key.as_slice())
                .unwrap()
                .is_some()
            {
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

    let dir2 = tempdir().unwrap();
    let env2 = open_env(&dir2);
    populate(&env2, n, 256);

    group.bench_function("signet/seek_then_1000_unsync", |b| {
        b.iter(|| {
            let txn = env2.begin_ro_unsync().unwrap();
            let db = txn.open_db(None).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
            let mut count = 0u64;
            if cursor
                .set_range::<(), ()>(mid_key.as_slice())
                .unwrap()
                .is_some()
            {
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

    let target_key = make_key(MEDIUM_N / 2);

    // Read all dups — sync (comparable to reth)
    group.throughput(Throughput::Elements(DUPS_PER_KEY as u64));
    group.bench_function("signet/read_dups_one_key_sync", |b| {
        b.iter(|| {
            let txn = env.begin_ro_sync().unwrap();
            let db = txn.open_db(Some("dupsort")).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
            let mut count = 0u64;
            for item in cursor.iter_dup_of::<()>(target_key.as_slice()).unwrap() {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    // Read all dups — unsync
    group.bench_function("signet/read_dups_one_key_unsync", |b| {
        b.iter(|| {
            let txn = env.begin_ro_unsync().unwrap();
            let db = txn.open_db(Some("dupsort")).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
            let mut count = 0u64;
            for item in cursor.iter_dup_of::<()>(target_key.as_slice()).unwrap() {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    // Read all dups via page-batched DUPFIXED iteration (signet-unique)
    let dir2 = tempdir().unwrap();
    let env2 = open_env(&dir2);
    populate_dupsort(&env2, MEDIUM_N, DUPS_PER_KEY);

    group.bench_function("signet/read_dups_one_key_dupfixed", |b| {
        b.iter(|| {
            let txn = env2.begin_ro_unsync().unwrap();
            let db = txn.open_db(Some("dupsort")).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
            let mut count = 0u64;
            for item in cursor
                .iter_dupfixed_of::<()>(target_key.as_slice())
                .unwrap()
            {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    // Full DUPSORT scan
    let dir3 = tempdir().unwrap();
    let env3 = open_env(&dir3);
    populate_dupsort(&env3, MEDIUM_N, DUPS_PER_KEY);

    group.throughput(Throughput::Elements((MEDIUM_N * DUPS_PER_KEY) as u64));

    // Full scan — sync (comparable to reth)
    group.bench_function("signet/full_dupsort_scan_sync", |b| {
        b.iter(|| {
            let txn = env.begin_ro_sync().unwrap();
            let db = txn.open_db(Some("dupsort")).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
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

    // Full scan — unsync
    group.bench_function("signet/full_dupsort_scan_unsync", |b| {
        b.iter(|| {
            let txn = env3.begin_ro_unsync().unwrap();
            let db = txn.open_db(Some("dupsort")).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
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

    // Full scan via page-batched DUPFIXED
    let dir4 = tempdir().unwrap();
    let env4 = open_env(&dir4);
    populate_dupsort(&env4, MEDIUM_N, DUPS_PER_KEY);

    group.bench_function("signet/full_dupfixed_scan", |b| {
        b.iter(|| {
            let txn = env4.begin_ro_unsync().unwrap();
            let db = txn.open_db(Some("dupsort")).unwrap();
            let mut cursor = txn.cursor(db).unwrap();
            let mut count = 0u64;
            for item in cursor.iter_dupfixed_start::<(), ()>().unwrap() {
                item.unwrap();
                count += 1;
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

        // Sync
        group.bench_function(BenchmarkId::new("signet/commit_sync", batch), |b| {
            let dir = tempdir().unwrap();
            let env = open_env(&dir);
            {
                let txn = env.begin_rw_sync().unwrap();
                let _ = txn.open_db(None).unwrap();
                txn.commit().unwrap();
            }

            let pairs: Vec<([u8; 32], Vec<u8>)> = (0..batch)
                .map(|i| (make_key(i), make_value(i, 256)))
                .collect();
            b.iter(|| {
                let txn = env.begin_rw_sync().unwrap();
                let db = txn.open_db(None).unwrap();
                for (k, v) in &pairs {
                    txn.put(db, k.as_slice(), v.as_slice(), WriteFlags::UPSERT)
                        .unwrap();
                }
                let latency = txn.commit_with_latency().unwrap();
                black_box(latency);
            })
        });

        // Unsync
        group.bench_function(BenchmarkId::new("signet/commit_unsync", batch), |b| {
            let dir = tempdir().unwrap();
            let env = open_env(&dir);
            {
                let txn = env.begin_rw_unsync().unwrap();
                let _ = txn.open_db(None).unwrap();
                txn.commit().unwrap();
            }

            let pairs: Vec<([u8; 32], Vec<u8>)> = (0..batch)
                .map(|i| (make_key(i), make_value(i, 256)))
                .collect();
            b.iter(|| {
                let txn = env.begin_rw_unsync().unwrap();
                let db = txn.open_db(None).unwrap();
                for (k, v) in &pairs {
                    txn.put(db, k.as_slice(), v.as_slice(), WriteFlags::UPSERT)
                        .unwrap();
                }
                let latency = txn.commit_with_latency().unwrap();
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
