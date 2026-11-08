//! Retarget a native animation sample while preserving target bone axes.
use crate::{animation::Animation, glb_io, invalid, model::Model, retarget::PoseMap, Result};
use serde_json::{json, Value};
use std::path::Path;
pub fn standing(
    model: &Model,
    clip: &Animation,
    input: &Path,
    output: &Path,
    time: f32,
) -> Result<Value> {
    let (mut doc, bin) = glb_io::read(input)?;
    let map = PoseMap::new(model, &doc)?;
    let pose = map.sample(model, clip, time)?;
    let mut count = 0;
    for (i, node) in doc["nodes"].as_array_mut().unwrap().iter_mut().enumerate() {
        if map.mapped[i].is_none() {
            continue;
        }
        let (scale, rotation, translation) = pose[i].to_scale_rotation_translation();
        if !scale.is_finite() || !rotation.is_finite() || !translation.is_finite() {
            return Err(invalid("pose decomposition"));
        }
        node.as_object_mut().unwrap().remove("matrix");
        node["translation"] = json!(translation.to_array());
        node["rotation"] = json!(rotation.normalize().to_array());
        node["scale"] = json!(scale.to_array());
        count += 1;
    }
    doc["extras"]["nativeStandingPose"] =
        json!({"time":time,"duration":clip.duration(),"mappedBones":count});
    glb_io::write(output, &doc, &bin)?;
    Ok(json!({"mappedBones":count,"time":time,"duration":clip.duration(),"output":output}))
}
