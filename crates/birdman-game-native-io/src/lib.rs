//! Native operating-system and network I/O shared by the game host and server administration app.
//!
//! Application decisions remain in the shared runtime; this crate executes their native I/O.

pub mod web_distribution;

/// Shared native controller OS transports.
pub mod controller_io;
/// Shared native file and configuration I/O.
pub mod storage_io;

/// Native OS request execution for the shared authority.
pub mod native_io;

mod storage_worker;

/// Native raw HTTP and WebSocket I/O for the shared endpoint.
pub mod network_io;

mod asset_preparation_worker;
mod http;
/// Shared server command-line parsing.
pub mod server_cli;
/// Native HTTP/WebSocket service and management handle.
pub mod service;
