# The `model_status` schema & FFI boundary

## One Rust definition, hand-mirrored on the edges

The engine→app status contract (`model_status`) is defined **once** in Rust:
`rust/crates/ds-status` (plain serde structs) is the single source of truth. The engine
builds a `ModelStatus`; `ds-core` ships it to each UI as JSON; each platform's UI parses it
into its **own hand-written DTOs** that mirror that shape — Windows in
`apps/windows/winui/Native.cs`, macOS in its Swift DTOs — kept in lockstep via the
`ds-status` round-trip test that pins the wire byte-shape.

## Why hand-written, not codegen (uniffi)

uniffi (and its C# backend `uniffi-bindgen-cs`) was evaluated and **deliberately not adopted**:
for a ~29-function surface it brings a lot of machinery (per-field serialization runtime,
generated scaffolding, a contract checksum) plus a third-party 0.x C# generator — more than
this boundary needs. The only real drift risk is the status DTOs, and that's covered by the
single Rust schema + the contract test at a fraction of the dependency cost. **Don't
re-introduce a codegen toolchain here** without revisiting that trade-off.

## If you change the schema

Edit `ds-status` (the Rust source of truth), then update the two hand mirrors —
`apps/windows/winui/Native.cs` and the macOS Swift DTOs (`apps/macos/Sources/DontSpeak/DontSpeakCore.swift`,
which omits keys the macOS UI doesn't read) — and run the `ds-status` test. Do **not**
switch to generated bindings.
