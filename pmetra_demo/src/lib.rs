#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::let_and_return)]
#![allow(clippy::field_reassign_with_default)]
pub mod components;
pub mod plugin;
pub mod plugins;
pub mod resources;
pub mod systems;
pub mod truck_loader;
pub mod utils;
#[cfg(target_arch = "wasm32")]
pub mod wasm_bridge;

pub use plugin::PmetraDemoPlugin;
