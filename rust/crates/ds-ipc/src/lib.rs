//! Newline-delimited JSON RPC: in-process engine (server) ↔ host UI / MCP / hooks.
//!
//! Transport: filesystem UDS ([`transport`]; `uds_windows` on Windows). Engine owns
//! model state. Missing socket ⇒ engine down; every call is fallible.

pub mod client;
pub mod protocol;
pub mod server;
pub mod transport;

pub use client::{Client, connect, request};
pub use protocol::{DictationEvent, Request, Response};
pub use server::{Conn, HandleOutcome, Handler, ShutdownHandle, serve};
