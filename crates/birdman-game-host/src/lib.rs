//! Native server administration app and packaging provenance for the shared host.
//!
//! Native operating-system and network I/O lives in `birdman-game-native-io`;
//! this crate provides the administration UI that drives it.

/// Exact build provenance declared by Native packaging.
pub const DECLARED_SOURCE_SHA: Option<&str> = option_env!("BPG_NATIVE_SOURCE_SHA");

/// Native server administration UI.
#[cfg(feature = "server-ui")]
pub mod admin;
