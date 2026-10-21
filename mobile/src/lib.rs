//! Android controller; platform views call the shared remote client.
mod app;
mod components;
mod platform;
pub use app::App;

use nsh_core::ui::prelude::*;

#[native_module("orbit/mobile")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}
