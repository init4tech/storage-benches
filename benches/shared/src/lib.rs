//! Shared data generation and constants for benchmarks.

use rand::{Rng, SeedableRng, rngs::SmallRng};

/// Fixed seed for deterministic data generation.
pub const SEED: u64 = 0xBEEF_CAFE_DEAD_F00D;

/// Number of entries for "small" benchmarks.
pub const SMALL_N: u32 = 100;
/// Number of entries for "medium" benchmarks.
pub const MEDIUM_N: u32 = 10_000;
/// Number of entries for "large" benchmarks.
pub const LARGE_N: u32 = 100_000;
/// Number of random lookups per iteration.
pub const LOOKUP_COUNT: usize = 1000;
/// Number of entries to read in range queries.
pub const RANGE_COUNT: usize = 1000;
/// Number of duplicate values per key in DUPSORT benchmarks.
pub const DUPS_PER_KEY: u32 = 10;

/// 1 GB max DB size for benchmarks.
pub const DB_MAX_SIZE: usize = 1024 * 1024 * 1024;

/// Generate a deterministic 32-byte key from an index.
/// Keys are sorted lexicographically (big-endian index prefix + padding).
pub fn make_key(index: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[0..4].copy_from_slice(&index.to_be_bytes());
    // Fill rest with deterministic bytes for realistic key distribution
    let mut rng = SmallRng::seed_from_u64(index as u64);
    rng.fill(&mut key[4..]);
    key
}

/// Generate a deterministic value of the given size from an index.
pub fn make_value(index: u32, size: usize) -> Vec<u8> {
    let mut val = vec![0u8; size];
    let mut rng = SmallRng::seed_from_u64(index as u64 ^ 0xFFFF);
    rng.fill(val.as_mut_slice());
    val
}

/// Generate a shuffled list of indices for random access patterns.
pub fn shuffled_indices(n: u32) -> Vec<u32> {
    use rand::prelude::SliceRandom;
    let mut indices: Vec<u32> = (0..n).collect();
    let mut rng = SmallRng::seed_from_u64(SEED);
    indices.shuffle(&mut rng);
    indices
}

/// Generate a sorted list of keys for pre-populating a DB.
pub fn sorted_keys(n: u32) -> Vec<[u8; 32]> {
    (0..n).map(make_key).collect()
}
