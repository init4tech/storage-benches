# Storage Benches

## Commands

- `cargo +nightly fmt` - format
- `cargo clippy --workspace --all-targets` - lint
- `cargo bench` - run benchmarks

Pre-push: clippy + fmt. Never use `cargo check/build`.

### Pre-push Checks (enforced by Claude hook)

A Claude hook in `.claude/settings.json` runs `.claude/hooks/pre-push.sh`
before every `git push`. The push is blocked if any check fails. The checks:

- `cargo +nightly fmt -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`

Clippy and doc warnings are hard failures.
