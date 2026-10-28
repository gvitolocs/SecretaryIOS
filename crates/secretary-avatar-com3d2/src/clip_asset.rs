//! Mesh-free native animation export. Hermite curves are sampled at 30 Hz;
//! quaternion signs are made continuous before glTF interpolation.
use crate::{animation::Animation, glb_io, invalid, model::Model, Result};
use glam::Quat;
use serde_json::{json, Value};
use std::path::Path;
fn floats(doc: &mut Value, data: &mut Vec<u8>, values: &[f32], width: usize, kind: &str) -> usize {
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let start = data.len();
    for v in values {
        data.extend(v.to_le_bytes());
    }
    let view = doc["bufferViews"].as_array().unwrap().len();
    doc["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":start,"byteLength":values.len()*4}));
    let a = doc["accessors"].as_array().unwrap().len();
    doc["accessors"].as_array_mut().unwrap().push(
        json!({"bufferView":view,"componentType":5126,"count":values.len()/width,"type":kind}),
    );
    a
}
pub fn export(model: &Model, clip: &Animation, input: &Path, output: &Path) -> Result<Value> {
    let (mut doc, _) = glb_io::read(input)?;
    let map = crate::retarget::PoseMap::new(model, &doc)?;
    let duration = clip.duration();
    if !duration.is_finite() || duration <= 0. || duration > 600. {
        return Err(invalid("animation duration"));
    }
    let n = (duration * 30.).ceil() as usize;
    let times = (0..=n)
        .map(|i| duration * i as f32 / n as f32)
        .collect::<Vec<_>>();
    let poses = times
        .iter()
        .map(|time| map.sample(model, clip, *time))
        .collect::<Result<Vec<_>>>()?;
    let mapped = &map.mapped;
    let local = &map.local;
    for node in doc["nodes"].as_array_mut().unwrap() {
        let o = node.as_object_mut().unwrap();
        o.remove("mesh");
        o.remove("skin");
        o.remove("camera");
    }
    for key in [
        "meshes",
        "skins",
        "materials",
        "images",
        "textures",
        "samplers",
        "extensionsUsed",
        "extensionsRequired",
        "animations",
    ] {
        doc.as_object_mut().unwrap().remove(key);
    }
    doc["bufferViews"] = json!([]);
    doc["accessors"] = json!([]);
    let mut data = Vec::new();
    let time = floats(&mut doc, &mut data, &times, 1, "SCALAR");
    doc["accessors"][time]["min"] = json!([0]);
    doc["accessors"][time]["max"] = json!([duration]);
    let mut samplers = Vec::new();
    let mut channels = Vec::new();
    let mut count = 0;
    for i in 0..mapped.len() {
        if mapped[i].is_none() {
            continue;
        }
        count += 1;
        let (mut positions, mut rotations, mut scales) = (Vec::new(), Vec::new(), Vec::new());
        let mut last = Quat::IDENTITY;
        let (s, q, p) = local[i].to_scale_rotation_translation();
        doc["nodes"][i].as_object_mut().unwrap().remove("matrix");
        doc["nodes"][i]["translation"] = json!(p.to_array());
        doc["nodes"][i]["rotation"] = json!(q.to_array());
        doc["nodes"][i]["scale"] = json!(s.to_array());
        for (frame, pose) in poses.iter().enumerate() {
            let (s, mut q, p) = pose[i].to_scale_rotation_translation();
            if !s.is_finite() || !q.is_finite() || !p.is_finite() {
                return Err(invalid("clip transform"));
            }
            q = q.normalize();
            if frame > 0 && last.dot(q) < 0. {
                q = -q;
            }
            last = q;
            positions.extend(p.to_array());
            rotations.extend(q.to_array());
            scales.extend(s.to_array());
        }
        for (path, values, width, kind) in [
            ("translation", positions, 3, "VEC3"),
            ("rotation", rotations, 4, "VEC4"),
            ("scale", scales, 3, "VEC3"),
        ] {
            let output = floats(&mut doc, &mut data, &values, width, kind);
            let index = samplers.len();
            samplers.push(json!({"input":time,"output":output,"interpolation":"LINEAR"}));
            channels.push(json!({"sampler":index,"target":{"node":i,"path":path}}));
        }
    }
    doc["animations"] = json!([{"name":"native_idle","samplers":samplers,"channels":channels}]);
    doc["buffers"] = json!([{"byteLength":data.len()}]);
    doc["extras"] = json!({"nativeAnimation":{"fps":30,"duration":duration,"mappedBones":count,"sourceInterpolation":"Hermite sampled; no game scripts executed"}});
    glb_io::write(output, &doc, &data)?;
    Ok(json!({"output":output,"frames":times.len(),"duration":duration,"mappedBones":count}))
}
