# The `model_status` schema & FFI boundary

## One Rust definition, hand-mirrored on the edges

The engine→app status contract (`model_status`) is defined **once** in Rust:
`rust/crates/ds-status` (plain serde structs). The engine (`dontspeakd/src/status.rs`)
builds a `ModelStatus`; the C ABI (`ds-core`) ships it to each UI as JSON; each
platform's UI parses it into its **own hand-written DTOs** that mirror that shape:

- **Windows**: `apps/windows/winui/Native.cs` (`ModelStatusDto` → `HealthSnapshot` projection).
- **macOS**: the Swift mirror in the SwiftUI app.

The Rust `ds-status` round-trip test pins the wire byte-shape; the per-platform mirrors
are kept in lockstep by reviewing against that crate. The FFI itself is a small hand-rolled
`extern "C"` surface (`ds-core/src/ffi.rs`, ~20 functions) returning strings/primitives,
with the engine lifecycle owned in one place (`host.rs`).

## Why hand-written, not codegen (uniffi)

uniffi (and its C# backend `uniffi-bindgen-cs`) was evaluated and **deliberately not adopted**:
for a ~20-function surface it brings a lot of machinery (per-field serialization runtime,
generated scaffolding, a contract checksum) plus a third-party 0.x C# generator — more than
this boundary needs. The only real drift risk is the status DTOs, and that's covered by the
single Rust schema + the contract test at a fraction of the dependency cost. **Don't
re-introduce a codegen toolchain here** without revisiting that trade-off.

## If you change the schema

Edit `ds-status` (the Rust source of truth), then update the hand mirrors:
`apps/windows/winui/Native.cs` and the macOS Swift DTOs. Run the `ds-status` test.

## macOS — in lockstep

The Swift status DTOs (`apps/macos/Sources/DontSpeak/DontSpeakCore.swift` — `ModelStatusDTO` &
friends) are the hand mirror of `ds-status`, matching it field-for-field (verified against
this crate). Keys the macOS UI doesn't read — `caps_events`, `build_id`, and the `running`
engine booleans — are omitted on purpose (`Decodable` ignores unknown keys). The engine/status/
push reworks already landed on macOS (the AsyncStream status push, the typed decode). When the
schema changes, update these DTOs alongside `Native.cs`; do **not** switch to generated bindings.
