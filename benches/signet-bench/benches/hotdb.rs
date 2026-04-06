//! Hot DB benchmarks for signet-hot-mdbx (H1-H6).

use alloy::{
    consensus::{Header, Sealable},
    primitives::{Address, B256, BlockNumber, U256},
};
use bench_shared::{make_key, shuffled_indices, LARGE_N, LOOKUP_COUNT, MEDIUM_N, SEED};
use criterion::{
    criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use rand::{Rng, SeedableRng, rngs::SmallRng};
use signet_hot::{
    model::{HotKv, HotKvRead, HotKvWrite},
    tables,
};
use signet_hot_mdbx::{DatabaseArguments, DatabaseEnv, DatabaseEnvKind};
use signet_storage_types::{Account, SealedHeader};
use std::hint::black_box;
use tempfile::{TempDir, tempdir};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_db() -> (TempDir, DatabaseEnv) {
    let dir = tempdir().unwrap();
    let args = DatabaseArguments::new().with_sync_mode(Some(signet_libmdbx::SyncMode::SafeNoSync));
    let db = DatabaseEnv::open(dir.path(), DatabaseEnvKind::RW, args).unwrap();
    (dir, db)
}

fn make_address(i: u32) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0..4].copy_from_slice(&i.to_be_bytes());
    let mut rng = SmallRng::seed_from_u64(i as u64);
    rng.fill(&mut bytes[4..]);
    Address::from(bytes)
}

fn make_account(i: u32) -> Account {
    Account {
        nonce: i as u64,
        balance: U256::from(i as u64 * 1_000_000),
        bytecode_hash: if i.is_multiple_of(3) {
            Some(B256::from_slice(&make_key(i)))
        } else {
            None
        },
    }
}

fn make_header(number: BlockNumber) -> SealedHeader {
    Header {
        number,
        gas_limit: 30_000_000,
        gas_used: 15_000_000,
        timestamp: 1_700_000_000 + number,
        parent_hash: B256::from_slice(&make_key(number as u32)),
        state_root: B256::from_slice(&make_key(number as u32 + 1)),
        ..Default::default()
    }
    .seal_slow()
}

fn make_slot_key(i: u32) -> U256 {
    U256::from_be_bytes(make_key(i))
}

fn make_slot_value(i: u32) -> U256 {
    U256::from(i as u64 * 42)
}

fn populate_accounts(db: &DatabaseEnv, n: u32) {
    let writer = db.writer().unwrap();
    for i in 0..n {
        writer
            .queue_put::<tables::PlainAccountState>(&make_address(i), &make_account(i))
            .unwrap();
    }
    writer.raw_commit().unwrap();
}

fn populate_storage(db: &DatabaseEnv, n: u32, slots_per: u32) {
    let writer = db.writer().unwrap();
    for i in 0..n {
        let addr = make_address(i);
        for s in 0..slots_per {
            let idx = i * slots_per + s;
            writer
                .queue_put_dual::<tables::PlainStorageState>(
                    &addr,
                    &make_slot_key(idx),
                    &make_slot_value(idx),
                )
                .unwrap();
        }
    }
    writer.raw_commit().unwrap();
}

fn populate_headers(db: &DatabaseEnv, n: u32) {
    let writer = db.writer().unwrap();
    for i in 0..n {
        writer
            .queue_put::<tables::Headers>(&(i as BlockNumber), &make_header(i as BlockNumber))
            .unwrap();
    }
    writer.raw_commit().unwrap();
}

// ---------------------------------------------------------------------------
// H1: Account state reads
// ---------------------------------------------------------------------------

fn h1_account_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("H1_account_reads");
    let n = LARGE_N;
    let (_dir, db) = open_db();
    populate_accounts(&db, n);

    let indices = shuffled_indices(n);
    let lookup_addrs: Vec<Address> = indices
        .iter()
        .take(LOOKUP_COUNT)
        .map(|&i| make_address(i))
        .collect();

    group.throughput(Throughput::Elements(LOOKUP_COUNT as u64));

    group.bench_function("signet/get_account", |b| {
        b.iter(|| {
            let reader = db.reader().unwrap();
            let mut found = 0u64;
            for addr in &lookup_addrs {
                if reader
                    .get::<tables::PlainAccountState>(addr)
                    .unwrap()
                    .is_some()
                {
                    found += 1;
                }
            }
            black_box(found);
        })
    });

    let miss_addrs: Vec<Address> = (0..LOOKUP_COUNT as u32)
        .map(|i| {
            if i % 2 == 0 {
                make_address(indices[i as usize])
            } else {
                make_address(n + i)
            }
        })
        .collect();

    group.bench_function("signet/get_account_50pct_miss", |b| {
        b.iter(|| {
            let reader = db.reader().unwrap();
            let mut found = 0u64;
            for addr in &miss_addrs {
                if reader
                    .get::<tables::PlainAccountState>(addr)
                    .unwrap()
                    .is_some()
                {
                    found += 1;
                }
            }
            black_box(found);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// H2: Storage slot reads (DUPSORT)
// ---------------------------------------------------------------------------

fn h2_storage_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("H2_storage_reads");
    let n_addrs = MEDIUM_N;
    let slots_per = 10u32;
    let (_dir, db) = open_db();
    populate_storage(&db, n_addrs, slots_per);

    let indices = shuffled_indices(n_addrs);
    let lookups: Vec<(Address, U256)> = indices
        .iter()
        .take(LOOKUP_COUNT)
        .map(|&i| {
            let slot_idx = i * slots_per + (i % slots_per);
            (make_address(i), make_slot_key(slot_idx))
        })
        .collect();

    group.throughput(Throughput::Elements(LOOKUP_COUNT as u64));

    // Cursor-per-call: matches production get_storage() -> get_dual()
    group.bench_function("signet/get_single_slot", |b| {
        b.iter(|| {
            let reader = db.reader().unwrap();
            let mut total = 0u64;
            for (addr, slot) in &lookups {
                if let Some(val) = reader
                    .get_dual::<tables::PlainStorageState>(addr, slot)
                    .unwrap()
                {
                    total += val.as_limbs()[0];
                }
            }
            black_box(total);
        })
    });

    // Cursor reuse: what an optimised path could look like
    group.bench_function("signet/get_single_slot_cursor_reuse", |b| {
        b.iter(|| {
            let reader = db.reader().unwrap();
            let mut cursor = reader
                .traverse_dual::<tables::PlainStorageState>()
                .unwrap();
            let mut total = 0u64;
            for (addr, slot) in &lookups {
                if let Some(val) = cursor.exact_dual(addr, slot).unwrap() {
                    total += val.as_limbs()[0];
                }
            }
            black_box(total);
        })
    });

    let target_addr = make_address(n_addrs / 2);
    group.bench_function("signet/get_all_slots_one_addr", |b| {
        b.iter(|| {
            let reader = db.reader().unwrap();
            let mut cursor = reader
                .traverse_dual::<tables::PlainStorageState>()
                .unwrap();
            let mut count = 0u64;
            for item in cursor.iter_k2(&target_addr).unwrap() {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// H3: Header reads
// ---------------------------------------------------------------------------

fn h3_header_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("H3_header_reads");
    let n = MEDIUM_N;
    let (_dir, db) = open_db();
    populate_headers(&db, n);

    let indices = shuffled_indices(n);
    let lookup_nums: Vec<BlockNumber> = indices
        .iter()
        .take(LOOKUP_COUNT)
        .map(|&i| i as BlockNumber)
        .collect();

    group.throughput(Throughput::Elements(LOOKUP_COUNT as u64));

    group.bench_function("signet/get_header_random", |b| {
        b.iter(|| {
            let reader = db.reader().unwrap();
            let mut count = 0u64;
            for num in &lookup_nums {
                if reader.get::<tables::Headers>(num).unwrap().is_some() {
                    count += 1;
                }
            }
            black_box(count);
        })
    });

    group.throughput(Throughput::Elements(n as u64));
    group.bench_function("signet/scan_headers_sequential", |b| {
        b.iter(|| {
            let reader = db.reader().unwrap();
            let mut cursor = reader.traverse::<tables::Headers>().unwrap();
            let mut count = 0u64;
            for item in cursor.iter().unwrap() {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// H4: Batch block ingestion
// ---------------------------------------------------------------------------

fn h4_batch_ingestion(c: &mut Criterion) {
    let mut group = c.benchmark_group("H4_batch_ingestion");
    group.sample_size(10);

    for &n_blocks in &[1u32, 10, 100] {
        let accounts_per_block = 100u32;
        let slots_per_block = 500u32;

        group.throughput(Throughput::Elements(n_blocks as u64));

        group.bench_function(BenchmarkId::new("signet/ingest_blocks", n_blocks), |b| {
            b.iter_batched(
                open_db,
                |(_dir, db)| {
                    for block in 0..n_blocks {
                        let writer = db.writer().unwrap();
                        let block_offset = block * 10_000;

                        for i in 0..accounts_per_block {
                            let idx = block_offset + i;
                            writer
                                .queue_put::<tables::PlainAccountState>(
                                    &make_address(idx),
                                    &make_account(idx),
                                )
                                .unwrap();
                        }

                        for i in 0..slots_per_block {
                            let idx = block_offset + i;
                            let addr = make_address(idx % accounts_per_block + block_offset);
                            writer
                                .queue_put_dual::<tables::PlainStorageState>(
                                    &addr,
                                    &make_slot_key(idx),
                                    &make_slot_value(idx),
                                )
                                .unwrap();
                        }

                        writer
                            .queue_put::<tables::Headers>(
                                &(block as BlockNumber),
                                &make_header(block as BlockNumber),
                            )
                            .unwrap();

                        writer.raw_commit().unwrap();
                    }
                },
                BatchSize::PerIteration,
            )
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// H5: Full table scans
// ---------------------------------------------------------------------------

fn h5_full_scans(c: &mut Criterion) {
    let mut group = c.benchmark_group("H5_full_scans");

    let n = LARGE_N;
    let (_dir, db) = open_db();
    populate_accounts(&db, n);

    group.throughput(Throughput::Elements(n as u64));
    group.bench_function("signet/scan_accounts", |b| {
        b.iter(|| {
            let reader = db.reader().unwrap();
            let mut cursor = reader.traverse::<tables::PlainAccountState>().unwrap();
            let mut count = 0u64;
            for item in cursor.iter().unwrap() {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    let n_headers = MEDIUM_N;
    let (_dir2, db2) = open_db();
    populate_headers(&db2, n_headers);

    group.throughput(Throughput::Elements(n_headers as u64));
    group.bench_function("signet/scan_headers", |b| {
        b.iter(|| {
            let reader = db2.reader().unwrap();
            let mut cursor = reader.traverse::<tables::Headers>().unwrap();
            let mut count = 0u64;
            for item in cursor.iter().unwrap() {
                item.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// H6: Mixed read/write workload
// ---------------------------------------------------------------------------

fn h6_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("H6_mixed_workload");
    group.sample_size(10);

    // Production path: get_dual per storage lookup (cursor per call)
    group.bench_function("signet/block_execution_cycle", |b| {
        b.iter_batched(
            || {
                let (dir, db) = open_db();
                populate_accounts(&db, 10_000);
                populate_storage(&db, 10_000, 5);
                populate_headers(&db, 100);
                (dir, db)
            },
            |(_dir, db)| {
                let mut rng = SmallRng::seed_from_u64(SEED);

                for block in 100u64..110 {
                    {
                        let reader = db.reader().unwrap();
                        let mut found = 0u64;
                        for _ in 0..200 {
                            let i: u32 = rng.random_range(0..15_000);
                            if reader
                                .get::<tables::PlainAccountState>(&make_address(i))
                                .unwrap()
                                .is_some()
                            {
                                found += 1;
                            }
                        }
                        for _ in 0..1000 {
                            let addr_i: u32 = rng.random_range(0..10_000);
                            let slot_i: u32 = rng.random_range(0..50_000);
                            reader
                                .get_dual::<tables::PlainStorageState>(
                                    &make_address(addr_i),
                                    &make_slot_key(slot_i),
                                )
                                .unwrap();
                        }
                        black_box(found);
                    }

                    {
                        let writer = db.writer().unwrap();
                        for i in 0..100u32 {
                            let idx = block as u32 * 1000 + i;
                            writer
                                .queue_put::<tables::PlainAccountState>(
                                    &make_address(idx % 10_000),
                                    &make_account(idx),
                                )
                                .unwrap();
                        }
                        for i in 0..500u32 {
                            let idx = block as u32 * 1000 + i;
                            writer
                                .queue_put_dual::<tables::PlainStorageState>(
                                    &make_address(idx % 10_000),
                                    &make_slot_key(idx),
                                    &make_slot_value(idx),
                                )
                                .unwrap();
                        }
                        writer
                            .queue_put::<tables::Headers>(&block, &make_header(block))
                            .unwrap();
                        writer.raw_commit().unwrap();
                    }
                }
            },
            BatchSize::PerIteration,
        )
    });

    // Cursor reuse: one cursor for all storage reads per block
    group.bench_function("signet/block_execution_cycle_cursor_reuse", |b| {
        b.iter_batched(
            || {
                let (dir, db) = open_db();
                populate_accounts(&db, 10_000);
                populate_storage(&db, 10_000, 5);
                populate_headers(&db, 100);
                (dir, db)
            },
            |(_dir, db)| {
                let mut rng = SmallRng::seed_from_u64(SEED);

                for block in 100u64..110 {
                    {
                        let reader = db.reader().unwrap();
                        let mut found = 0u64;
                        for _ in 0..200 {
                            let i: u32 = rng.random_range(0..15_000);
                            if reader
                                .get::<tables::PlainAccountState>(&make_address(i))
                                .unwrap()
                                .is_some()
                            {
                                found += 1;
                            }
                        }
                        let mut cursor = reader
                            .traverse_dual::<tables::PlainStorageState>()
                            .unwrap();
                        for _ in 0..1000 {
                            let addr_i: u32 = rng.random_range(0..10_000);
                            let slot_i: u32 = rng.random_range(0..50_000);
                            cursor
                                .exact_dual(
                                    &make_address(addr_i),
                                    &make_slot_key(slot_i),
                                )
                                .unwrap();
                        }
                        black_box(found);
                    }

                    {
                        let writer = db.writer().unwrap();
                        for i in 0..100u32 {
                            let idx = block as u32 * 1000 + i;
                            writer
                                .queue_put::<tables::PlainAccountState>(
                                    &make_address(idx % 10_000),
                                    &make_account(idx),
                                )
                                .unwrap();
                        }
                        for i in 0..500u32 {
                            let idx = block as u32 * 1000 + i;
                            writer
                                .queue_put_dual::<tables::PlainStorageState>(
                                    &make_address(idx % 10_000),
                                    &make_slot_key(idx),
                                    &make_slot_value(idx),
                                )
                                .unwrap();
                        }
                        writer
                            .queue_put::<tables::Headers>(&block, &make_header(block))
                            .unwrap();
                        writer.raw_commit().unwrap();
                    }
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

// ---------------------------------------------------------------------------
criterion_group!(
    benches,
    h1_account_reads,
    h2_storage_reads,
    h3_header_reads,
    h4_batch_ingestion,
    h5_full_scans,
    h6_mixed_workload,
);
criterion_main!(benches);
