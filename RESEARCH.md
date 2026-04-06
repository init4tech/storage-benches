# Benchmark Research: reth vs init4tech Storage

## Overview

Two benchmarking axes:

1. **Low-level MDBX bindings**: `reth-libmdbx` vs `signet-libmdbx`
2. **Hot DB layer**: reth's `DatabaseEnv` (reth-db) vs signet's `DatabaseEnv` (signet-hot-mdbx)

Both ultimately wrap the same C library (libmdbx 0.13.x / erthink/libmdbx).

---

## Upstream Dependencies

| Repo | Source | Purpose |
|------|--------|---------|
| paradigmxyz/reth | git (tag v1.11.3) | reth-libmdbx bindings + reth-db hot DB |
| init4tech/mdbx | crates.io (signet-libmdbx 0.8.1) | signet-libmdbx bindings |
| init4tech/storage | crates.io (signet-hot-mdbx 0.6.9) | signet-hot-mdbx hot DB |

---

## Axis 1: Low-Level MDBX Bindings

### reth-libmdbx

- **Crate**: `reth-libmdbx` (paradigmxyz/reth, `crates/storage/libmdbx-rs/`)
- **FFI crate**: `reth-mdbx-sys` (paradigmxyz/reth, `crates/storage/libmdbx-rs/mdbx-sys/`)
- **Binding generation**: Runtime `bindgen` (generates at build time)
- **Key deps**: `crossbeam-queue`, `parking_lot`, `dashmap` (optional)
- **Edition**: 2021

#### Core API Types
- `Environment` + `EnvironmentBuilder`
- `Transaction<K>` where K = `RO` | `RW`
- `Cursor<K>`
- `Database` (DBI handle)
- `CommitLatency`

#### Transaction Creation
- `env.begin_ro_txn() -> Transaction<RO>` — uses internal pool (`crossbeam_queue::ArrayQueue`) for RO txn handle reuse
- `env.begin_rw_txn() -> Transaction<RW>` — blocks until sole writer

#### Key Operations
```
// Read
txn.get(dbi, key) -> Option<Value>
cursor.first/last/next/prev/set/set_range -> Option<(K, V)>

// Write
txn.put(dbi, key, value, flags)
txn.del(dbi, key, optional_value)
txn.commit() -> CommitLatency

// Iteration
cursor.into_iter() -> IntoIter
```

#### Features
- `return-borrowed`: zero-copy reads returning borrowed data
- `read-tx-timeouts`: dashmap-based timeout tracking for long RO txns
- RO transaction pooling via crossbeam ArrayQueue

#### Existing Benchmarks
- None in-crate (only integration tests in `tests/`)

---

### signet-libmdbx

- **Crate**: `signet-libmdbx` (crates.io)
- **FFI crate**: `signet-mdbx-sys` (crates.io)
- **Binding generation**: Pre-generated per-platform (linux/macos/windows)
- **Key deps**: `parking_lot`, `smallvec`
- **Edition**: 2024, MSRV 1.92

#### Core API Types
- `Environment` + `EnvironmentBuilder`
- `TxSync<K>` = `Tx<K, Arc<PtrSync>>` — thread-safe (Send+Sync)
- `TxUnsync<K>` = `Tx<K, PtrUnsync>` — single-thread (!Send, !Sync), ~30% faster
- `Cursor<'tx, K>` — lifetime-tied to transaction
- `Database` (DBI handle with caching)
- `CommitLatency`

#### Transaction Creation (4 variants)
```
env.begin_ro_sync()   -> RoTxSync     // multi-thread safe RO
env.begin_rw_sync()   -> RwTxSync     // multi-thread safe RW
env.begin_ro_unsync() -> RoTxUnsync   // single-thread RO (~30% faster)
env.begin_rw_unsync() -> RwTxUnsync   // single-thread RW (~30% faster)
```

#### Key Operations
```
// Read
txn.get(dbi, key) -> Option<Value>
cursor.first/last/next/prev/set/set_range -> Option<(K, V)>

// Write
txn.put(db, key, value, flags)
txn.append(db, key, value)           // optimized ordered insert
txn.del(db, key, optional_value)
txn.commit() / commit_with_latency()

// Iteration (richer than reth)
cursor.iter() / iter_start() / iter_from()
cursor.iter_dup() / iter_dup_of()
cursor.iter_dupfixed() / iter_dupfixed_of()   // page-batched DUPFIXED

// Write cursor
cursor.put() / cursor.append() / cursor.del()
cursor.put_multiple() / put_multiple_overwrite()
```

#### Key Differentiators from reth-libmdbx
1. **Unsync transactions**: `TxUnsync` avoids Arc overhead, ~30% faster for single-threaded use
2. **Lifetime-correct cursors**: `Cursor<'tx, K>` tied to transaction lifetime
3. **DBI caching**: Cached database handles per transaction
4. **Page-batched DUPFIXED iteration**: `iter_dupfixed()` reads multiple values per page
5. **Pre-generated bindings**: No runtime bindgen dependency
6. **`TableObjectOwned` trait**: Separate from `TableObject<'a>` for owned deserialization
7. **`ReadError` type**: Separates MDBX errors from decode errors

#### Existing Benchmarks (5 files in `benches/`)
1. `cursor.rs` — sequential iteration (sync vs unsync vs raw FFI)
2. `transaction.rs` — random get/put (sync vs unsync vs raw), tx creation overhead
3. `db_open.rs` — DBI opening with/without cache, named vs unnamed
4. `iter.rs` — DUPFIXED batched vs simple iteration (2000 items)
5. `deletion.rs` — bulk vs individual deletion (100/2000/10000 items)

---

## Axis 2: Hot Database Layer

### reth-db (Hot DB)

- **Crate**: `reth-db` (paradigmxyz/reth, `crates/storage/db/`)
- **Table definitions**: `reth-db-api` (paradigmxyz/reth, `crates/storage/db-api/`)

#### Architecture
```
DatabaseEnv
  ├── inner: reth_libmdbx::Environment
  ├── dbis: Arc<HashMap<&str, MDBX_dbi>>    // pre-opened table handles
  ├── metrics: Option<Arc<DatabaseEnvMetrics>>
  └── _lock_file: Option<StorageLock>
```

#### Key Traits
```rust
trait Database {
    type TX: DbTx;
    type TXMut: DbTxMut;
    fn tx(&self) -> Result<Self::TX>;
    fn tx_mut(&self) -> Result<Self::TXMut>;
}

trait DbTx {
    fn get<T: Table>(&self, key: T::Key) -> Result<Option<T::Value>>;
    fn cursor_read<T: Table>(&self) -> Result<impl DbCursorRO<T>>;
}

trait DbTxMut: DbTx {
    fn put<T: Table>(&self, key: T::Key, value: T::Value) -> Result<()>;
    fn delete<T: Table>(&self, key: T::Key, value: Option<T::Value>) -> Result<()>;
    fn cursor_write<T: Table>(&self) -> Result<impl DbCursorRW<T>>;
    fn commit(self) -> Result<()>;
}
```

#### Cursor Traits
```rust
trait DbCursorRO<T: Table> {
    fn first() -> Option<(Key, Value)>;
    fn seek(key) -> Option<(Key, Value)>;
    fn next() -> Option<(Key, Value)>;
    fn prev() -> Option<(Key, Value)>;
    fn last() -> Option<(Key, Value)>;
    fn walk(start) -> Walker;
    fn walk_range(range) -> RangeWalker;
    fn walk_back(start) -> ReverseWalker;
}

trait DbCursorRW<T: Table>: DbCursorRO<T> {
    fn upsert(key, value) -> Result<()>;
    fn insert(key, value) -> Result<()>;
    fn append(key, value) -> Result<()>;
    fn delete_current() -> Result<()>;
}
```

#### Tables (~40+)
Key hot-path tables:
- `PlainAccountState`: Address -> Account
- `PlainStorageState`: Address -> StorageEntry (DUPSORT by B256)
- `Headers`: BlockNumber -> Header
- `Transactions`: TxNumber -> TransactionSigned
- `AccountChangeSets`: BlockNumber -> AccountBeforeTx (DUPSORT)
- `StorageChangeSets`: BlockNumberAddress -> StorageEntry (DUPSORT)
- `HashedAccounts`, `HashedStorages`, `AccountsTrie`, `StoragesTrie`

#### Configuration
- `DatabaseArguments::new(client_version)` — 8TB max, 4GB growth, Durable sync
- `DatabaseArguments::test()` — 64MB max, 4MB growth
- Opens with: WRITEMAP (RW), NO_RDAHEAD, COALESCE
- `rp_augment_limit = 256 * 1024`
- Max readers: 32,000

#### Encoding
- `Encode`/`Decode` traits on table types
- Compression buffer in cursor (`buf: Vec<u8>`)
- Optional metrics recording per operation

#### Existing Benchmarks
- None found in storage crates

---

### signet-hot-mdbx (Hot DB)

- **Crate**: `signet-hot-mdbx` (crates.io)
- **Trait crate**: `signet-hot` (crates.io)
- **Types crate**: `signet-storage-types` (crates.io)

#### Architecture
```
DatabaseEnv
  ├── inner: signet_libmdbx::Environment
  ├── fsi_cache: FsiCache    // FixedSizeInfo per table (DUPSORT metadata)
  └── _lock_file: Option<StorageLock>
```

#### Key Traits
```rust
trait HotKv {
    type RoTx: HotKvRead;
    type RwTx: HotKvWrite;
    fn reader() -> Result<Self::RoTx>;
    fn writer() -> Result<Self::RwTx>;
}

trait HotKvRead {
    fn raw_get(table, key) -> Option<Cow<[u8]>>;
    fn raw_get_dual(table, k1, k2) -> Option<Cow<[u8]>>;
    fn traverse::<T>() -> TableCursor<T>;       // typed cursor
    fn get::<T>(key) -> Option<T::Value>;       // typed get
    fn get_dual::<T>(k1, k2) -> Option<T::Value>;  // typed dual-key get
}

trait HotKvWrite: HotKvRead {
    fn queue_raw_put(table, key, value);
    fn queue_raw_put_dual(table, k1, k2, value);
    fn queue_raw_delete(table, key);
    fn queue_raw_delete_dual(table, k1, k2);
    fn queue_put::<T>(key, value);
    fn queue_append::<T>(key, value);           // optimized ordered insert
    fn commit(self) -> Result<()>;
}
```

#### Cursor Traits
```rust
trait KvTraverse {
    fn first() -> Option<(key, value)>;
    fn last() -> Option<(key, value)>;
    fn exact(key) -> Option<value>;
    fn lower_bound(key) -> Option<(key, value)>;
    fn read_next() -> Option<(key, value)>;
    fn read_prev() -> Option<(key, value)>;
    fn iter() -> Iterator;
    fn iter_from(key) -> Iterator;
}

trait DualKeyTraverse {
    fn first() -> Option<(k1, k2, value)>;
    fn exact_dual(k1, k2) -> Option<value>;
    fn next_dual_above(k1, k2) -> Option<(k1, k2, value)>;
}

trait KvTraverseMut {
    fn delete_current();
    fn append(key, value);
}
```

#### Tables (9 predefined)
1. `Headers`: BlockNumber -> SealedHeader
2. `HeaderNumbers`: B256 -> BlockNumber
3. `Bytecodes`: B256 -> Bytecode
4. `PlainAccountState`: Address -> Account
5. `PlainStorageState`: Address -> U256 -> U256 (DUP_FIXED 32B)
6. `AccountsHistory`: Address -> u64 -> BlockNumberList (DUPSORT)
7. `AccountChangeSets`: BlockNumber -> Address -> Account (DUP_FIXED, FullReplacements)
8. `StorageHistory`: Address -> ShardedKey<U256> -> BlockNumberList (DUPSORT)
9. `StorageChangeSets`: (u64, Address) -> U256 -> U256 (DUP_FIXED, FullReplacements)

#### Dual-Key Design
- Uses MDBX DUPSORT for tables with two keys
- `FixedSizeInfo` cache tracks which tables are DUPSORT/DUP_FIXED and their key2/value sizes
- Stored as metadata in the default table (DBI 0)

#### Configuration
- `DatabaseArguments::new()` — 8TB max, 4GB growth, Durable sync
- Opens with: WRITEMAP (RW), NO_RDAHEAD, COALESCE
- `rp_augment_limit = 256 * 1024`
- Max readers: 32,000

#### Encoding
- `KeySer` trait: fixed-size keys (max 64B), lexicographic ordering
- `ValSer` trait: variable-size values with optional fixed-size optimization
- `FIXED_SIZE: Option<usize>` enables DUP_FIXED optimization

#### Existing Benchmarks
- None found in storage workspace

---

## Key Architectural Differences

### Bindings Layer

| Feature | reth-libmdbx | signet-libmdbx |
|---------|-------------|----------------|
| Edition | 2021 | 2024 |
| Bindgen | Runtime (build.rs) | Pre-generated |
| Sync/Unsync | Only Sync txns | Both Sync + Unsync (~30% faster) |
| RO Pooling | crossbeam ArrayQueue | None visible (txn manager) |
| Cursor lifetime | Not tied to txn | Tied to txn (`'tx`) |
| DBI caching | No (done at DB layer) | Yes, per-transaction |
| DUPFIXED iter | Basic | Page-batched `iter_dupfixed()` |
| Extra deps | crossbeam-queue, dashmap | smallvec |
| Benchmarks | None | 5 Criterion suites |

### Hot DB Layer

| Feature | reth-db | signet-hot-mdbx |
|---------|--------|----------------|
| Table count | ~40+ | 9 |
| DBI caching | Pre-opened HashMap in DatabaseEnv | FixedSizeInfo cache per-txn |
| Dual-key model | DUPSORT via SubKey in table macro | Explicit DualKey trait + FixedSizeInfo |
| Write API | Direct put/delete on txn | Queue-based (`queue_raw_put`) |
| Metrics | Built-in optional metrics layer | None |
| Compression | Cursor-level buf for compression | None visible |
| History | Via separate tables | Integrated HistoryRead/HistoryWrite traits |
| Test helpers | `create_test_rw_db()` | `create_test_rw_db()` |

### C Library
Both use **libmdbx 0.13.x** (erthink/libmdbx). Same C source, same compile flags (MDBX_TXN_CHECKOWNER=0, march=native propagation). Only difference: reth uses runtime bindgen, signet uses pre-generated bindings.

---

## What To Benchmark

### Tier 1: Raw MDBX Bindings (reth-libmdbx vs signet-libmdbx)

These benchmarks isolate the Rust wrapper overhead from the C library:

1. **Transaction creation overhead**
   - reth: `begin_ro_txn()` / `begin_rw_txn()`
   - signet: `begin_ro_sync()` / `begin_rw_sync()` / `begin_ro_unsync()` / `begin_rw_unsync()`

2. **Point reads (random key lookup)**
   - `txn.get(dbi, random_key)` — varying value sizes (32B, 256B, 4KB)

3. **Point writes (random key insert)**
   - `txn.put(dbi, key, value, flags)` — varying batch sizes

4. **Sequential cursor iteration**
   - Forward scan of N entries via `cursor.next()`
   - Reverse scan via `cursor.prev()`

5. **Range queries**
   - `cursor.set_range(prefix)` then iterate N entries

6. **DUPSORT operations**
   - Insert/read multiple values per key
   - `iter_dup()` vs signet's `iter_dupfixed()` (page-batched)

7. **Commit latency**
   - Single-item commit, batch commit (100, 1000, 10000 items)
   - Measure via `CommitLatency`

### Tier 2: Hot DB Layer (reth-db vs signet-hot-mdbx)

These benchmarks test the typed abstraction layer:

1. **Account state read**
   - `PlainAccountState`: get account by address (both have this table)

2. **Storage slot read**
   - `PlainStorageState`: get storage by (address, slot) — DUPSORT table

3. **Block header read**
   - `Headers`: get header by block number

4. **Batch block ingestion**
   - Write N blocks of account/storage state changes
   - reth: `tx_mut().put::<Table>(key, value)` loop + commit
   - signet: `writer().queue_put::<Table>(key, value)` loop + commit

5. **History query**
   - Account history lookup: find blocks where account changed
   - Storage history lookup: find blocks where slot changed

6. **Sequential scan**
   - Full table scan of PlainAccountState
   - Range scan of Headers by block number range

7. **Mixed read/write workload**
   - Simulate block execution: read accounts/storage, write updated state
