//! Consistent preview lighting across the joined skin surfaces. Original texture
//! pixels are retained; eyes/lashes keep their original unlit rendering.
use crate::{glb_io, Result};
use serde_json::{json, Value};
use std::path::Path;
pub fn harmonize(input: &Path, output: &Path) -> Result<Value> {
    let (mut doc, data) = glb_io::read(input)?;
    let mut changed = Vec::new();
    if let Some(materials) = doc["materials"].as_array_mut() {
        for material in materials {
            let name = material["name"].as_str().unwrap_or("").to_owned();
            if name == "FaceSkin" || name == "SkinTinted_COM3D2_Adapter" {
                if let Some(extensions) = material["extensions"].as_object_mut() {
                    extensions.remove("KHR_materials_unlit");
                }
                material["pbrMetallicRoughness"]["metallicFactor"] = json!(0);
                material["pbrMetallicRoughness"]["roughnessFactor"] = json!(1);
                material["extras"]["nativeSkinLighting"] =
                    json!("matte PBR shared across head/neck/body; source textures retained");
                changed.push(name);
            }
        }
    }
    glb_io::write(output, &doc, &data)?;
    Ok(json!({"output":output,"changed":changed}))
}
