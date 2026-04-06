#!/usr/bin/env bash
set -euo pipefail

# Run reth vs signet MDBX benchmarks.
#
# Usage:
#   ./run_benches.sh              # full run (bindings + hotdb)
#   ./run_benches.sh --quick      # ~3s measurement per benchmark
#   ./run_benches.sh --bindings   # binding benchmarks only (B1-B7)
#   ./run_benches.sh --hotdb      # hot DB benchmarks only (H1-H6)
#   ./run_benches.sh B2           # filter to specific group

cd "$(dirname "$0")/benches"

FILTER=""
EXTRA_ARGS=""
RUN_BINDINGS=true
RUN_HOTDB=true

while [[ $# -gt 0 ]]; do
    case "$1" in
        --quick)      EXTRA_ARGS="--warm-up-time 1 --measurement-time 3"; shift ;;
        --bindings)   RUN_HOTDB=false; shift ;;
        --hotdb)      RUN_BINDINGS=false; shift ;;
        *)            FILTER="$1"; shift ;;
    esac
done

echo "=== Building all benchmarks (release) ==="
cargo build --release --benches 2>&1

if $RUN_BINDINGS; then
    echo ""
    echo "=== reth-libmdbx bindings (B1-B7) ==="
    cargo bench -p reth-bench --bench bindings -- $EXTRA_ARGS $FILTER 2>&1

    echo ""
    echo "=== signet-libmdbx bindings (B1-B7) ==="
    cargo bench -p signet-bench --bench bindings -- $EXTRA_ARGS $FILTER 2>&1
fi

if $RUN_HOTDB; then
    echo ""
    echo "=== reth-db hot DB (H1-H6) ==="
    cargo bench -p reth-bench --bench hotdb -- $EXTRA_ARGS $FILTER 2>&1

    echo ""
    echo "=== signet-hot-mdbx hot DB (H1-H6) ==="
    cargo bench -p signet-bench --bench hotdb -- $EXTRA_ARGS $FILTER 2>&1
fi

echo ""
echo "=== Done ==="
