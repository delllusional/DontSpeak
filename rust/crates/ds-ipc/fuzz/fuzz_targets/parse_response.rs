#![no_main]

//! Client-side defense-in-depth: the app/hooks parse `Response` lines coming back
//! from the engine. Lower-stakes than `parse_request` (the engine is our own code,
//! not an arbitrary local process), but the same hand-tagged serde enum, so fuzz it
//! too rather than assume it's fine by symmetry.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<ds_ipc::Response>(data);
});
