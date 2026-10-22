//! Meshy API credit prices (docs.meshy.ai/api/pricing, read 2026-10-22).
//! Plans are estimates from this table; the job records what Meshy reports
//! as `consumed_credits` once each task finishes.

/// When the table was last checked against Meshy's pricing page.
pub const PRICES_AS_OF: &str = "2026-10-22";

/// Generation models (`ai_model`). `latest` is meshy-7.1.
pub const MODELS: &[&str] = &[
    "latest",
    "meshy-7.1",
    "meshy-6",
    "meshy-6-lite",
    "meshy-t2",
    "meshy-t2.5",
];

/// Smart Topology models (clean triangle meshes at a chosen face count).
pub fn is_smart_topology(model: &str) -> bool {
    matches!(model, "meshy-t2" | "meshy-t2.5")
}

fn cheap(model: &str) -> bool {
    matches!(model, "meshy-6-lite" | "meshy-t2" | "meshy-t2.5")
}

/// Texture tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextureRes {
    #[serde(rename = "2k")]
    K2,
    #[serde(rename = "4k")]
    K4,
    #[serde(rename = "8k")]
    K8,
}

impl TextureRes {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "2k" => Some(Self::K2),
            "4k" => Some(Self::K4),
            "8k" => Some(Self::K8),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::K2 => "2k",
            Self::K4 => "4k",
            Self::K8 => "8k",
        }
    }
}

/// Text to 3D preview (mesh only). `ultra`: geometry_resolution 2k/4k (+5).
pub fn text_preview(model: &str, ultra: bool) -> u32 {
    (if cheap(model) { 5 } else { 20 }) + if ultra { 5 } else { 0 }
}

/// Text to 3D refine (texturing a preview).
pub fn refine(res: TextureRes) -> u32 {
    if res == TextureRes::K8 {
        15
    } else {
        10
    }
}

/// Image (or multi-image) to 3D; `texture: None` = mesh only. `None` when
/// the model does not support that tier.
pub fn image_to_3d(model: &str, texture: Option<TextureRes>, ultra: bool) -> Option<u32> {
    let base = match (model, texture) {
        ("meshy-6-lite", None) => 5,
        ("meshy-6-lite", Some(TextureRes::K2)) => 15,
        ("meshy-6-lite", Some(_)) => return None,
        ("meshy-t2" | "meshy-t2.5", None) => 5,
        ("meshy-t2" | "meshy-t2.5", Some(TextureRes::K8)) => 20,
        ("meshy-t2" | "meshy-t2.5", Some(_)) => 15,
        (_, None) => 20,
        (_, Some(TextureRes::K8)) => 35,
        (_, Some(_)) => 30,
    };
    Some(base + if ultra { 5 } else { 0 })
}

pub const REMESH: u32 = 5;
pub const RIG: u32 = 5;
pub const ANIMATION_PER_ACTION: u32 = 3;
pub const MAX_ACTIONS_PER_REQUEST: usize = 10;
pub const CONVERT: u32 = 1;
pub const RESIZE: u32 = 1;

pub fn retexture(res: TextureRes) -> u32 {
    refine(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        assert_eq!(text_preview("latest", false), 20);
        assert_eq!(text_preview("meshy-t2.5", false), 5);
        assert_eq!(text_preview("latest", true), 25);
        assert_eq!(refine(TextureRes::K4), 10);
        assert_eq!(refine(TextureRes::K8), 15);
        assert_eq!(image_to_3d("latest", Some(TextureRes::K2), false), Some(30));
        assert_eq!(image_to_3d("latest", None, false), Some(20));
        assert_eq!(
            image_to_3d("meshy-6-lite", Some(TextureRes::K4), false),
            None
        );
        assert_eq!(
            image_to_3d("meshy-t2", Some(TextureRes::K8), false),
            Some(20)
        );
        assert_eq!(retexture(TextureRes::K2), 10);
    }
}
