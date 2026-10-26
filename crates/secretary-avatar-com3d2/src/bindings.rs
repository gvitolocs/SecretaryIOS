//! Explicit axis corrections for the native body's spring joints. The authoring
//! tool may change bone frames while retaining the same rest deformation.
use crate::{animation::globals, compose::scene_frames, glb_io, invalid, model::Model, Result};
use glam::{Mat4, Quat, Vec3};
use serde_json::{json, Value};
use std::path::Path;

pub fn export(model: &Model, bind_asset: &Path) -> Result<Value> {
    let (doc, _) = glb_io::read(bind_asset)?;
    let (_, _, target) = scene_frames(&doc)?;
    let parents = model.bones.iter().map(|b| b.parent).collect::<Vec<_>>();
    let local = model
        .bones
        .iter()
        .map(|b| {
            Mat4::from_scale_rotation_translation(
                Vec3::from_array(b.scale),
                Quat::from_array(b.rotation).normalize(),
                Vec3::from_array(b.translation),
            )
        })
        .collect::<Vec<_>>();
    let world = globals(&parents, &local)?;
    let reflection = Mat4::from_scale(Vec3::new(1., 1., -1.));
    let nodes = doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("binding nodes"))?;
    let correction = |name: &str| -> Result<Mat4> {
        let source = model
            .bones
            .iter()
            .position(|b| b.name == name)
            .ok_or_else(|| invalid(format!("missing native bone {name}")))?;
        let dest = nodes
            .iter()
            .position(|b| b["name"] == name)
            .ok_or_else(|| invalid(format!("missing target bone {name}")))?;
        let frame = target[dest].inverse() * reflection * world[source] * reflection;
        if !frame.is_finite() || frame.determinant().abs() < 1e-8 {
            return Err(invalid("binding frame"));
        }
        Ok(frame)
    };
    let mut joints = Vec::new();
    for name in ["Mune_L", "Mune_R"] {
        let source = model
            .bones
            .iter()
            .position(|b| b.name == name)
            .ok_or_else(|| invalid("spring bone"))?;
        let parent = parents[source];
        if parent < 0 {
            return Err(invalid("spring bone has no parent"));
        }
        let parent = &model.bones[parent as usize].name;
        let child_name=format!("{name}_sub");let child=model.bones.iter().find(|b|b.name==child_name).ok_or_else(||invalid("body deformation child missing"))?;
        joints.push(json!({"name":name,"parent":parent,"correction":correction(name)?.to_cols_array(),"parentCorrection":correction(parent)?.to_cols_array(),"subName":child_name,"subCorrection":correction(&child.name)?.to_cols_array(),"subTranslation":[child.translation[0],child.translation[1],-child.translation[2]],"subScale":child.scale}));
    }
    Ok(json!({"version":1,"space":"GLTF right handed; source Z reflected","joints":joints}))
}
