//! Native asset compatibility, not a replacement Unity engine.
//! Binary format notes: MeidoSerialization (BSD-3-Clause); see THIRD_PARTY_NOTICES.
pub mod accessor;
pub mod animation;
pub mod arc;
pub mod binary;
pub mod bindings;
pub mod body_physics;
pub mod catalog;
pub mod clip_asset;
pub mod compose;
#[cfg(not(target_arch = "wasm32"))]
pub mod compressed;
pub mod glb_io;
pub mod gltf;
pub mod legacy;
pub mod masking;
pub mod mate;
pub mod menu;
pub mod model;
pub mod physics;
pub mod pose_asset;
#[cfg(not(target_arch = "wasm32"))]
pub mod preview;
pub mod resources;
pub mod retarget;
#[cfg(not(target_arch = "wasm32"))]
pub mod skin_assets;
pub mod skin_color;
pub mod skin_preview;
pub mod surface;
pub mod texture;
#[cfg(not(target_arch = "wasm32"))]
pub mod wardrobe;
#[cfg(target_arch = "wasm32")]
mod wasm;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid COM3D2 data: {0}")]
    Invalid(String),
    #[error("unsupported COM3D2 feature: {0}")]
    Unsupported(String),
}
pub(crate) fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}
