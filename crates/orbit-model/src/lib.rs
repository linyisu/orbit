//! Provider request and stream adapters with shared model-facing data types.
//! Credential acquisition and persistence belong to orbit-auth.

pub mod anthropic;
pub mod catalog;
pub mod client;
pub mod error;
pub mod openai;
mod reasoning;
pub mod runtime_context;
pub mod stream;
pub mod tool_purpose;
pub mod types;

pub mod account_catalog;
mod collect;
pub mod discovery;
mod google;
