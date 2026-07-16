#![no_main]

//! Untrusted input: any process that can reach `dontspeak.sock` is parsed as `Request`.
//! Fuzz for panics in serde/hand-written `Deserialize` (parse errors are already handled).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<ds_ipc::Request>(data);
});
