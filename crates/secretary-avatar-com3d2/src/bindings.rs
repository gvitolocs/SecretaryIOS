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
            .position(|b| b.name == name.strip_suffix("_SCL_").unwrap_or(name))
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
        let child_name = format!("{name}_sub");
        let child = model
            .bones
            .iter()
            .find(|b| b.name == child_name)
            .ok_or_else(|| invalid("body deformation child missing"))?;
        joints.push(json!({"name":name,"parent":parent,"correction":correction(name)?.to_cols_array(),"parentCorrection":correction(parent)?.to_cols_array(),"subName":child_name,"subCorrection":correction(&child.name)?.to_cols_array(),"subTranslation":[child.translation[0],child.translation[1],-child.translation[2]],"subScale":child.scale}));
    }
    let mut shape_bones = Vec::new();
    for (index, name) in crate::customization::bone_names().iter().enumerate() {
        let source_name = name.strip_suffix("_SCL_").unwrap_or(name);
        let Some(source) = model.bones.iter().position(|b| b.name == source_name) else {
            continue;
        };
        if !nodes.iter().any(|n| n["name"] == *name)
            || (parents[source] < 0 && !name.ends_with("_SCL_"))
        {
            continue;
        }
        let parent = if name.ends_with("_SCL_") {
            &model.bones[source].name
        } else {
            &model.bones[parents[source] as usize].name
        };
        shape_bones.push(json!({"name":name,"index":index,"correction":correction(name)?.to_cols_array(),"parentCorrection":correction(parent)?.to_cols_array()}));
    }
    Ok(
        json!({"version":2,"customization":{"version":1,"controls":crate::customization::controls(),"bones":shape_bones},"space":"GLTF right handed; source Z reflected","joints":joints}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Bone;
    #[test]
    fn generated_scale_helpers_keep_native_shape_axes() {
        let names = [
            "Bip01 Spine",
            "Mune_L",
            "Mune_L_sub",
            "Mune_R",
            "Mune_R_sub",
        ];
        let bones = names
            .iter()
            .enumerate()
            .map(|(i, &name)| Bone {
                name: name.into(),
                parent: match i {
                    0 => -1,
                    2 => 1,
                    4 => 3,
                    _ => 0,
                },
                scale_helper: i == 0,
                translation: [0.; 3],
                rotation: Quat::from_rotation_y(0.3).to_array(),
                scale: [1.; 3],
            })
            .collect::<Vec<_>>();
        let model = Model {
            version: 2001,
            name: "body".into(),
            root: names[0].into(),
            bones,
            skin_bones: vec![],
            inverse_bind: vec![],
            vertices: vec![],
            tangents: vec![],
            skin: vec![],
            submeshes: vec![],
            materials: vec![],
            morphs: vec![],
        };
        let mut nodes = names.iter().map(|&n| json!({"name":n})).collect::<Vec<_>>();
        nodes[0]["children"] = json!([1, 3, 5]);
        nodes[0]["rotation"] = json!(Quat::from_rotation_z(0.4).to_array());
        nodes[1]["children"] = json!([2]);
        nodes[3]["children"] = json!([4]);
        nodes.push(
            json!({"name":"Bip01 Spine_SCL_","rotation":Quat::from_rotation_x(0.2).to_array()}),
        );
        let doc = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":nodes,"buffers":[{"byteLength":0}]});
        let p = std::env::temp_dir().join(format!("shape-binding-{}.glb", std::process::id()));
        glb_io::write(&p, &doc, &[]).unwrap();
        let out = export(&model, &p).unwrap();
        std::fs::remove_file(p).unwrap();
        let binding = out["customization"]["bones"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["name"] == "Bip01 Spine_SCL_")
            .expect(
                "generated helper must not be skipped because it is not an explicit model bone",
            );
        let matrix = |key: &str| {
            Mat4::from_cols_array(&std::array::from_fn(|i| {
                binding[key][i].as_f64().unwrap() as f32
            }))
        };
        let local = Mat4::from_rotation_x(0.2);
        let vals = crate::customization::values(&std::collections::BTreeMap::new()).unwrap();
        let result = crate::customization::evaluate(
            "Bip01 Spine_SCL_",
            local,
            matrix("parentCorrection"),
            matrix("correction"),
            &vals,
        )
        .unwrap();
        assert!(result.abs_diff_eq(local, 1e-5));
    }
}
