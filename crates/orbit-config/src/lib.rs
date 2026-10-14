//! Validated declarations, connection editing and refreshable model metadata.
//! Persisted user choices remain separate from provider discovery caches.

pub mod config;
pub mod models;
pub mod subagents;
pub(crate) use models::account_models;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use orbit_model::client::ApiFormat;
