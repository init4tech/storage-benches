//! Hot DB benchmarks for reth-db (H1-H6).
//!
//! Uses DatabaseEnv directly (not TempDatabase) with matched config:
//! 1GB max, 64MB growth, SafeNoSync — same as signet side and B-groups.

use alloy_consensus::Header;
use alloy_primitives::{Address, BlockNumber, B256, U256};
use bench_shared::{
    make_key, shuffled_indices, DB_MAX_SIZE, LARGE_N, LOOKUP_COUNT, MEDIUM_N, SEED,
};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use rand::{rngs::SmallRng, Rng, SeedableRng};
use reth_db::{
    mdbx::{DatabaseArguments, DatabaseEnv, DatabaseEnvKind, SyncMode},
    ClientVersion,
};
use reth_db_api::{
    cursor::{DbCursorRO, DbDupCursorRO},
    database::Database,
    tables,
    transaction::{DbTx, DbTxMut},
};
use reth_primitives_traits::{Account, StorageEntry};
use std::hint::black_box;
use tempfile::{tempdir, TempDir};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open a reth DatabaseEnv directly with matched config.
/// No TempDatabase wrapper, no metrics, SafeNoSync.
fn open_db() -> (TempDir, DatabaseEnv) {
    let dir = tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let args = DatabaseArguments::new(ClientVersion::default())
        .with_geometry_max_size(Some(DB_MAX_SIZE))
        .with_growth_step(Some(64 * 1024 * 1024))
        .with_sync_mode(Some(SyncMode::SafeNoSync));

    let mut db = DatabaseEnv::open(&path, DatabaseEnvKind::RW, args).unwrap();
    db.create_tables().unwrap();

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

fn make_header(number: BlockNumber) -> Header {
    Header {
        number,
        gas_limit: 30_000_000,
        gas_used: 15_000_000,
        timestamp: 1_700_000_000 + number,
        parent_hash: B256::from_slice(&make_key(number as u32)),
        state_root: B256::from_slice(&make_key(number as u32 + 1)),
        ..Default::default()
    }
}

fn make_storage_entry(i: u32) -> StorageEntry {
    StorageEntry {
        key: B256::from_slice(&make_key(i)),
        value: U256::from(i as u64 * 42),
    }
}

fn make_slot_subkey(i: u32) -> B256 {
    B256::from_slice(&make_key(i))
}

fn populate_accounts(db: &DatabaseEnv, n: u32) {
    let tx = db.tx_mut().unwrap();
    for i in 0..n {
        tx.put::<tables::PlainAccountState>(make_address(i), make_account(i))
            .unwrap();
    }
    tx.commit().unwrap();
}

fn populate_storage(db: &DatabaseEnv, n: u32, slots_per: u32) {
    let tx = db.tx_mut().unwrap();
    for i in 0..n {
        let addr = make_address(i);
        for s in 0..slots_per {
            let idx = i * slots_per + s;
            tx.put::<tables::PlainStorageState>(addr, make_storage_entry(idx))
                .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn populate_headers(db: &DatabaseEnv, n: u32) {
    let tx = db.tx_mut().unwrap();
    for i in 0..n {
        tx.put::<tables::Headers>(i as BlockNumber, make_header(i as BlockNumber))
            .unwrap();
    }
    tx.commit().unwrap();
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

    group.bench_function("reth/get_account", |b| {
        b.iter(|| {
            let tx = db.tx().unwrap();
            let mut found = 0u64;
            for addr in &lookup_addrs {
                if tx
                    .get::<tables::PlainAccountState>(*addr)
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

    group.bench_function("reth/get_account_50pct_miss", |b| {
        b.iter(|| {
            let tx = db.tx().unwrap();
            let mut found = 0u64;
            for addr in &miss_addrs {
                if tx
                    .get::<tables::PlainAccountState>(*addr)
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
    let lookups: Vec<(Address, B256)> = indices
        .iter()
        .take(LOOKUP_COUNT)
        .map(|&i| {
            let slot_idx = i * slots_per + (i % slots_per);
            (make_address(i), make_slot_subkey(slot_idx))
        })
        .collect();

    group.throughput(Throughput::Elements(LOOKUP_COUNT as u64));

    // Cursor-per-call: matches production LatestStateProviderRef::storage()
    group.bench_function("reth/get_single_slot", |b| {
        b.iter(|| {
            let tx = db.tx().unwrap();
            let mut total = 0u64;
            for (addr, subkey) in &lookups {
                let mut cursor = tx.cursor_dup_read::<tables::PlainStorageState>().unwrap();
                if let Some(entry) = cursor.seek_by_key_subkey(*addr, *subkey).unwrap() {
                    if entry.key == *subkey {
                        total += entry.value.as_limbs()[0];
                    }
                }
            }
            black_box(total);
        })
    });

    // Cursor reuse: what an optimised path could look like
    group.bench_function("reth/get_single_slot_cursor_reuse", |b| {
        b.iter(|| {
            let tx = db.tx().unwrap();
            let mut total = 0u64;
            let mut cursor = tx.cursor_dup_read::<tables::PlainStorageState>().unwrap();
            for (addr, subkey) in &lookups {
                if let Some(entry) = cursor.seek_by_key_subkey(*addr, *subkey).unwrap() {
                    if entry.key == *subkey {
                        total += entry.value.as_limbs()[0];
                    }
                }
            }
            black_box(total);
        })
    });

    let target_addr = make_address(n_addrs / 2);
    group.bench_function("reth/get_all_slots_one_addr", |b| {
        b.iter(|| {
            let tx = db.tx().unwrap();
            let mut cursor = tx.cursor_dup_read::<tables::PlainStorageState>().unwrap();
            let mut count = 0u64;
            let walker = cursor.walk_dup(Some(target_addr), None).unwrap();
            for entry in walker {
                entry.unwrap();
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

    group.bench_function("reth/get_header_random", |b| {
        b.iter(|| {
            let tx = db.tx().unwrap();
            let mut count = 0u64;
            for num in &lookup_nums {
                if tx.get::<tables::Headers>(*num).unwrap().is_some() {
                    count += 1;
                }
            }
            black_box(count);
        })
    });

    group.throughput(Throughput::Elements(n as u64));
    group.bench_function("reth/scan_headers_sequential", |b| {
        b.iter(|| {
            let tx = db.tx().unwrap();
            let mut cursor = tx.cursor_read::<tables::Headers>().unwrap();
            let mut count = 0u64;
            let walker = cursor.walk(None).unwrap();
            for entry in walker {
                entry.unwrap();
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

        group.bench_function(BenchmarkId::new("reth/ingest_blocks", n_blocks), |b| {
            b.iter_batched(
                open_db,
                |(_dir, db)| {
                    for block in 0..n_blocks {
                        let tx = db.tx_mut().unwrap();
                        let block_offset = block * 10_000;

                        for i in 0..accounts_per_block {
                            let idx = block_offset + i;
                            tx.put::<tables::PlainAccountState>(
                                make_address(idx),
                                make_account(idx),
                            )
                            .unwrap();
                        }

                        for i in 0..slots_per_block {
                            let idx = block_offset + i;
                            let addr = make_address(idx % accounts_per_block + block_offset);
                            tx.put::<tables::PlainStorageState>(addr, make_storage_entry(idx))
                                .unwrap();
                        }

                        tx.put::<tables::Headers>(
                            block as BlockNumber,
                            make_header(block as BlockNumber),
                        )
                        .unwrap();

                        tx.commit().unwrap();
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
    group.bench_function("reth/scan_accounts", |b| {
        b.iter(|| {
            let tx = db.tx().unwrap();
            let mut cursor = tx.cursor_read::<tables::PlainAccountState>().unwrap();
            let mut count = 0u64;
            let walker = cursor.walk(None).unwrap();
            for entry in walker {
                entry.unwrap();
                count += 1;
            }
            black_box(count);
        })
    });

    let n_headers = MEDIUM_N;
    let (_dir2, db2) = open_db();
    populate_headers(&db2, n_headers);

    group.throughput(Throughput::Elements(n_headers as u64));
    group.bench_function("reth/scan_headers", |b| {
        b.iter(|| {
            let tx = db2.tx().unwrap();
            let mut cursor = tx.cursor_read::<tables::Headers>().unwrap();
            let mut count = 0u64;
            let walker = cursor.walk(None).unwrap();
            for entry in walker {
                entry.unwrap();
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

    // Production path: cursor per storage lookup
    group.bench_function("reth/block_execution_cycle", |b| {
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
                        let tx = db.tx().unwrap();
                        let mut found = 0u64;
                        for _ in 0..200 {
                            let i: u32 = rng.random_range(0..15_000);
                            if tx
                                .get::<tables::PlainAccountState>(make_address(i))
                                .unwrap()
                                .is_some()
                            {
                                found += 1;
                            }
                        }
                        for _ in 0..1000 {
                            let addr_i: u32 = rng.random_range(0..10_000);
                            let slot_i: u32 = rng.random_range(0..50_000);
                            let mut cursor =
                                tx.cursor_dup_read::<tables::PlainStorageState>().unwrap();
                            cursor
                                .seek_by_key_subkey(make_address(addr_i), make_slot_subkey(slot_i))
                                .unwrap();
                        }
                        black_box(found);
                    }

                    {
                        let tx = db.tx_mut().unwrap();
                        for i in 0..100u32 {
                            let idx = block as u32 * 1000 + i;
                            tx.put::<tables::PlainAccountState>(
                                make_address(idx % 10_000),
                                make_account(idx),
                            )
                            .unwrap();
                        }
                        for i in 0..500u32 {
                            let idx = block as u32 * 1000 + i;
                            tx.put::<tables::PlainStorageState>(
                                make_address(idx % 10_000),
                                make_storage_entry(idx),
                            )
                            .unwrap();
                        }
                        tx.put::<tables::Headers>(block, make_header(block))
                            .unwrap();
                        tx.commit().unwrap();
                    }
                }
            },
            BatchSize::PerIteration,
        )
    });

    // Cursor reuse: one cursor for all storage reads per block
    group.bench_function("reth/block_execution_cycle_cursor_reuse", |b| {
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
                        let tx = db.tx().unwrap();
                        let mut found = 0u64;
                        for _ in 0..200 {
                            let i: u32 = rng.random_range(0..15_000);
                            if tx
                                .get::<tables::PlainAccountState>(make_address(i))
                                .unwrap()
                                .is_some()
                            {
                                found += 1;
                            }
                        }
                        let mut cursor = tx.cursor_dup_read::<tables::PlainStorageState>().unwrap();
                        for _ in 0..1000 {
                            let addr_i: u32 = rng.random_range(0..10_000);
                            let slot_i: u32 = rng.random_range(0..50_000);
                            cursor
                                .seek_by_key_subkey(make_address(addr_i), make_slot_subkey(slot_i))
                                .unwrap();
                        }
                        black_box(found);
                    }

                    {
                        let tx = db.tx_mut().unwrap();
                        for i in 0..100u32 {
                            let idx = block as u32 * 1000 + i;
                            tx.put::<tables::PlainAccountState>(
                                make_address(idx % 10_000),
                                make_account(idx),
                            )
                            .unwrap();
                        }
                        for i in 0..500u32 {
                            let idx = block as u32 * 1000 + i;
                            tx.put::<tables::PlainStorageState>(
                                make_address(idx % 10_000),
                                make_storage_entry(idx),
                            )
                            .unwrap();
                        }
                        tx.put::<tables::Headers>(block, make_header(block))
                            .unwrap();
                        tx.commit().unwrap();
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
