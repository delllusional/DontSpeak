//! `ds-ipc` — newline-delimited JSON RPC between the in-process engine (server) and clients
//! (`ds-core`/host UI, `dontspeak` MCP server, Claude Code hooks).
//!
//! Byte transport is [`transport`]: filesystem Unix-domain socket (native macOS/Linux;
//! `uds_windows` on Windows). Engine owns all model state; clients never load a model.
//! Missing socket ⇒ "engine down"; every call is fallible so callers use their legacy path.

pub mod client;
pub mod protocol;
pub mod server;
pub mod transport;

pub use client::{Client, connect, request};
pub use protocol::{Request, Response};
pub use server::{Handler, serve};
