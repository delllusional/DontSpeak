//! `ds-ipc` — newline-delimited-JSON RPC between the in-process engine (server)
//! and its clients (the SwiftUI app via `ds-core`, the `dontspeak` MCP server, and
//! the `dontspeak notify`/`provide` Claude Code hooks). See docs/DAEMON-REFACTOR.md.
//!
//! The byte transport lives behind [`transport`]: a filesystem Unix-domain socket
//! on macOS/Linux (the shipping backend), with a documented `cfg(windows)` seam
//! for the eventual port. The engine owns all model/engine state; clients never
//! load a model. A missing socket means "engine down", and every client call is
//! fallible so callers fall back to their legacy path (hooks spawn
//! `ds-helper`; the UI shows stopped).

pub mod client;
pub mod protocol;
pub mod server;
pub mod transport;

pub use client::{Client, connect, request};
pub use protocol::{Request, Response};
pub use server::{Handler, serve};
