#![no_main]

//! Client-side defense: fuzz `Response` the same way as `Request` (same hand-tagged serde enum).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<ds_ipc::Response>(data);
});
