# ds-ipc fuzz harness

Fuzzes `Request`/`Response` parsers (untrusted socket input). Separate Cargo workspace
(empty `[workspace]`) — invisible to `rust/` workspace builds; never ships.

Needs **nightly** + Unix (libFuzzer); not native Windows.

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
# from rust/crates/ds-ipc/fuzz/
cargo +nightly fuzz run parse_request -- -max_total_time=120
cargo +nightly fuzz run parse_response -- -max_total_time=120
```

Weekly `.github/workflows/fuzz.yml` is signal-only (never blocks push/PR).
