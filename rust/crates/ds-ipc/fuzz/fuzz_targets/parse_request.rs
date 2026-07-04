#![no_main]

//! The untrusted-input side of the wire boundary: any local process that can reach
//! `dontspeak.sock` sends bytes that `server.rs::handle_conn` parses as a `Request`.
//! Fuzz arbitrary bytes straight through the same deserializer to catch panics
//! (not just parse errors, which are already handled) hiding in serde's derive or
//! in a future hand-written `Deserialize` impl.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<ds_ipc::Request>(data);
});
