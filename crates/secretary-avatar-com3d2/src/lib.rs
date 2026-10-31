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
pub mod cloth;
pub mod compose;
#[cfg(not(target_arch = "wasm32"))]
pub mod compressed;
pub mod customization;
pub mod garment_fit;
pub mod glb_io;
pub mod gltf;
pub mod legacy;
pub mod legacy_cloth;
pub mod masking;
pub mod mate;
pub mod menu;
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod native_cloth;
#[cfg(not(target_arch = "wasm32"))]
pub mod native_pack;
pub mod native_write;
pub mod physics;
pub mod pose_asset;
#[cfg(not(target_arch = "wasm32"))]
pub mod preview;
pub mod resources;
pub mod retarget;
pub mod secondary_bindings;
pub mod secondary_physics;
pub mod secondary_rig;
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
