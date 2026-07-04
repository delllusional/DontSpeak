# ds-ipc fuzz harness

Fuzzes the wire-protocol parsers (`Request`/`Response`, both hand-tagged serde
enums) against arbitrary byte input — the untrusted-input boundary where any local
process talking to the engine's socket can send bytes.

This is its own independent Cargo workspace (see `Cargo.toml`'s empty `[workspace]`
table), so it's invisible to `rust/`'s `cargo build/clippy/test --workspace` and
never ships in any release artifact.

Requires nightly (libFuzzer/cargo-fuzz's hard requirement) and is Unix-only
(libFuzzer doesn't support Windows) — run from a Linux CI runner or a Linux/macOS
dev machine, not native Windows.

## Setup

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Run

From this directory (`rust/crates/ds-ipc/fuzz/`):

```sh
cargo +nightly fuzz run parse_request -- -max_total_time=120
cargo +nightly fuzz run parse_response -- -max_total_time=120
```

A weekly scheduled job (`.github/workflows/fuzz.yml`) runs both targets; it is
signal-only and never blocks a push/PR.
