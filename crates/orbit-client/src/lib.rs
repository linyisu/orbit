//! Shared client operations, with host deployment excluded from remote-only builds.

#[cfg(feature = "local")]
include!("local.rs");

#[cfg(feature = "remote")]
pub mod remote;

pub mod projects;
