//! Derive physical lever arms from the geometry a secondary joint actually skins.
//! A world-down placeholder has zero gravitational torque and is not a hair axis.
use crate::{accessor, compose, glb_io, invalid, Result};
use glam::{Mat4, Vec3};
use serde_json::json;
use std::{collections::HashMap, path::Path};
pub fn annotate(input: &Path, output: &Path) -> Result<serde_json::Value> {
    let (mut doc, mut data) = glb_io::read(input)?;
    let refined = distal_hair(&mut doc, &mut data)?;
    let (_, _, world) = compose::scene_frames(&doc)?;
    let nodes = doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("secondary nodes"))?
        .clone();
    let mut kinds = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if n["extras"]["secretarySecondary"]["grid"].is_object() {
            continue;
        }
        let name = n["name"].as_str().unwrap_or("").to_lowercase();
        let kind = n["extras"]["secretarySecondary"]["kind"]
            .as_str()
            .or_else(|| {
                if name.contains("sprhir") || name.contains("sprban") {
                    Some("hair")
                } else {
                    None
                }
            });
        if let Some(kind) = kind {
            kinds.insert(i, kind.to_owned());
        }
    }
    let mut sums = HashMap::<usize, (Vec3, f32, usize)>::new();
    for n in &nodes {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let s = &doc["skins"][skin as usize];
        let joints = s["joints"]
            .as_array()
            .ok_or_else(|| invalid("secondary skin joints"))?;
        let inverse = accessor::read(
            &doc,
            &data,
            s["inverseBindMatrices"]
                .as_u64()
                .ok_or_else(|| invalid("secondary bind matrices"))? as usize,
        )?;
        let matrices = joints
            .iter()
            .enumerate()
            .map(|(i, j)| {
                let j = j.as_u64().ok_or_else(|| invalid("secondary joint index"))? as usize;
                let ib = inverse
                    .get(i)
                    .filter(|v| v.len() == 16)
                    .ok_or_else(|| invalid("secondary inverse bind"))?;
                Ok(world[j] * Mat4::from_cols_array(ib.as_slice().try_into().unwrap()))
            })
            .collect::<Result<Vec<_>>>()?;
        for p in doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .ok_or_else(|| invalid("secondary mesh primitives"))?
        {
            let mat = &doc["materials"][p["material"].as_u64().unwrap_or(0) as usize];
            if mat["extras"]["baseHairHidden"] == true {
                continue;
            }
            let a = &p["attributes"];
            if a["JOINTS_0"].is_null() {
                continue;
            }
            let read = |name: &str| {
                accessor::read(
                    &doc,
                    &data,
                    a[name]
                        .as_u64()
                        .ok_or_else(|| invalid("secondary vertex attribute"))?
                        as usize,
                )
            };
            let positions = read("POSITION")?;
            let ids = read("JOINTS_0")?;
            let weights = read("WEIGHTS_0")?;
            let used = accessor::read(
                &doc,
                &data,
                p["indices"]
                    .as_u64()
                    .ok_or_else(|| invalid("secondary triangle indices"))? as usize,
            )?
            .into_iter()
            .map(|v| v[0] as usize)
            .collect::<std::collections::HashSet<_>>();
            for v in used {
                if v >= positions.len() || v >= ids.len() || v >= weights.len() {
                    return Err(invalid("secondary vertex bounds"));
                }
                let mut point = Vec3::ZERO;
                for k in 0..4 {
                    let weight = weights[v][k];
                    if weight > 0. {
                        let index = ids[v][k] as usize;
                        point += matrices
                            .get(index)
                            .ok_or_else(|| invalid("secondary skin weight index"))?
                            .transform_point3(Vec3::from_slice(&positions[v]))
                            * weight;
                    }
                }
                for k in 0..4 {
                    let weight = weights[v][k];
                    if weight < 0.02 {
                        continue;
                    }
                    let joint = joints[ids[v][k] as usize].as_u64().unwrap() as usize;
                    if !kinds.contains_key(&joint) {
                        continue;
                    }
                    let offset = point - world[joint].w_axis.truncate();
                    let lever_weight = weight * offset.length_squared();
                    let sum = sums.entry(joint).or_insert((Vec3::ZERO, 0., 0));
                    sum.0 += offset * lever_weight;
                    sum.1 += lever_weight;
                    sum.2 += 1;
                }
            }
        }
    }
    let mut records = Vec::new();
    for (joint, (sum, weight, count)) in sums {
        if weight < 1e-9 {
            continue;
        }
        let chain = nodes[joint]["extras"]["secretarySecondary"]["chain"] == true;
        let kind = &kinds[&joint];
        let mut source = if chain {
            "authored-chain"
        } else {
            "weighted-skinned-geometry"
        };
        let mut lever = if chain {
            let values = nodes[joint]["extras"]["secretarySecondary"]["tail"]
                .as_array()
                .filter(|v| v.len() == 3)
                .ok_or_else(|| invalid("chain endpoint"))?;
            let mut axis = [0f32; 3];
            for k in 0..3 {
                axis[k] = values[k]
                    .as_f64()
                    .ok_or_else(|| invalid("chain endpoint value"))?
                    as f32;
            }
            world[joint].transform_vector3(Vec3::from_array(axis))
        } else {
            sum / weight
        };
        if !chain && kind == "hair" {
            if let Some(children) = nodes[joint]["children"].as_array() {
                let axis = lever.normalize_or_zero();
                if let Some(child) = children
                    .iter()
                    .filter_map(|n| n.as_u64().map(|i| i as usize))
                    .filter(|i| kinds.get(i).is_some_and(|k| k == "hair"))
                    .map(|i| world[i].w_axis.truncate() - world[joint].w_axis.truncate())
                    .filter(|v| v.length() > 0.005 && v.normalize().dot(axis) > 0.7)
                    .min_by(|a, b| a.length_squared().total_cmp(&b.length_squared()))
                {
                    lever = child;
                    source = "authored-child";
                }
            }
        }
        let length = lever.length();
        if !(0.005..=1.5).contains(&length) {
            continue;
        }
        let lever = lever.normalize() * length.min(0.45);
        let local = world[joint].inverse().transform_vector3(lever);
        let name = nodes[joint]["name"].as_str().unwrap_or("");
        let bang = name.to_lowercase().contains("sprban");
        let parameters = if chain {
            [8., 10., 9.81, 1.7, 0.009]
        } else if kind == "cloth" {
            [35., 12., 9.81, 0.85, 0.009]
        } else if bang
            || (length < 0.09 && nodes[joint]["extras"]["secretarySecondary"]["distal"] != true)
        {
            [160., 16., 9.81, 0.25, 0.006]
        } else {
            [12., 10., 9.81, 1.7, 0.008]
        };
        let meta = &mut doc["nodes"][joint]["extras"]["secretarySecondary"];
        meta["kind"] = json!(kind);
        meta["tail"] = json!(local.to_array());
        meta["parameters"] = json!(parameters);
        meta["tailSource"] = json!(source);
        meta["chain"] = json!(chain);
        meta["weightedVertices"] = json!(count);
        records.push(json!({"name":name,"kind":kind,"restWorldLever":lever.to_array(),"length":length,"vertices":count}));
    }
    doc["extras"]["secondaryGravityBindings"] =
        json!({"version":1,"joints":records.len(),"gravityMetersPerSecondSquared":9.81});
    glb_io::write(output, &doc, &data)?;
    Ok(json!({"joints":records.len(),"refinedHairWeights":refined,"bindings":records}))
}
/// Add the missing distal bend to Lucina's long back lock, without changing its
/// bind shape or the face/crown weights. The source's lateral D joints are not a
/// continuation of this lock, so they cannot make its lower third bend.
fn distal_hair(doc: &mut serde_json::Value, data: &mut Vec<u8>) -> Result<usize> {
    if doc["extras"]["secretaryDistalHair"] == 1 {
        return Ok(0);
    }
    let (_, _, world) = compose::scene_frames(doc)?;
    let nodes = doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("hair nodes"))?
        .clone();
    let Some(root) = nodes.iter().position(|n| n["name"] == "c_sprhirE1_jnt") else {
        return Ok(0);
    };
    let pivot = world[root].w_axis.truncate();
    // The lower back lock extends down and behind the skull, in bind space.
    let axis = Vec3::new(0., -0.85, 0.53).normalize();
    let child_world = Mat4::from_translation(pivot + axis * 0.12);
    let (scale, rotation, translation) =
        (world[root].inverse() * child_world).to_scale_rotation_translation();
    let child = nodes.len();
    doc["nodes"].as_array_mut().unwrap().push(json!({"name":"SecretarySecondary_LucinaBackTip","translation":translation.to_array(),"rotation":rotation.to_array(),"scale":scale.to_array(),"extras":{"secretarySecondary":{"kind":"hair","distal":true}}}));
    if !doc["nodes"][root]["children"].is_array() {
        doc["nodes"][root]["children"] = json!([]);
    }
    doc["nodes"][root]["children"]
        .as_array_mut()
        .unwrap()
        .push(json!(child));
    let skins = doc["skins"]
        .as_array()
        .ok_or_else(|| invalid("hair skins"))?
        .len();
    let mut offsets = Vec::new();
    for skin in 0..skins {
        offsets.push(doc["skins"][skin]["joints"].as_array().unwrap().len());
        let ib = doc["skins"][skin]["inverseBindMatrices"].as_u64().unwrap() as usize;
        let mut inverse = accessor::read(doc, data, ib)?;
        inverse.push(child_world.inverse().to_cols_array().to_vec());
        let ib = accessor::floats(doc, data, &inverse, 16)?;
        doc["skins"][skin]["inverseBindMatrices"] = json!(ib);
        doc["skins"][skin]["joints"]
            .as_array_mut()
            .unwrap()
            .push(json!(child));
    }
    let mut changed = 0;
    for (ni, n) in nodes.iter().enumerate() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let (mesh, skin) = (mesh as usize, skin as usize);
        let Some(root_index) = doc["skins"][skin]["joints"]
            .as_array()
            .unwrap()
            .iter()
            .position(|j| j.as_u64() == Some(root as u64))
        else {
            continue;
        };
        let primitives = doc["meshes"][mesh]["primitives"]
            .as_array()
            .unwrap()
            .clone();
        for (pi, p) in primitives.iter().enumerate() {
            let mat = &doc["materials"][p["material"].as_u64().unwrap_or(0) as usize];
            if mat["extras"]["baseHairHidden"] == true {
                continue;
            }
            let a = &p["attributes"];
            if a["JOINTS_0"].is_null() {
                continue;
            }
            let pos = accessor::read(doc, data, a["POSITION"].as_u64().unwrap() as usize)?;
            let mut joints = accessor::read(doc, data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let mut weights = accessor::read(doc, data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let mut count = 0;
            for v in 0..pos.len() {
                let distance =
                    (world[ni].transform_point3(Vec3::from_slice(&pos[v])) - pivot).dot(axis);
                let t = ((distance - 0.04) / 0.16).clamp(0., 1.);
                let t = t * t * (3. - 2. * t);
                if t <= 0. {
                    continue;
                }
                let mut influences = Vec::new();
                for k in 0..4 {
                    let j = joints[v][k] as usize;
                    let w = weights[v][k];
                    if j == root_index && w > 0. {
                        influences.push((j, w * (1. - t)));
                        influences.push((offsets[skin], w * t));
                        count += 1;
                    } else {
                        influences.push((j, w));
                    }
                }
                influences.sort_by(|a, b| b.1.total_cmp(&a.1));
                influences.truncate(4);
                let sum: f32 = influences.iter().map(|v| v.1).sum();
                for (k, (j, w)) in influences.iter().enumerate() {
                    joints[v][k] = *j as f32;
                    weights[v][k] = *w / sum;
                }
            }
            if count > 0 {
                changed += count;
                let j = accessor::shorts(
                    doc,
                    data,
                    &joints
                        .iter()
                        .map(|v| std::array::from_fn(|i| v[i] as u16))
                        .collect::<Vec<[u16; 4]>>(),
                );
                let w = accessor::floats(doc, data, &weights, 4)?;
                doc["meshes"][mesh]["primitives"][pi]["attributes"]["JOINTS_0"] = json!(j);
                doc["meshes"][mesh]["primitives"][pi]["attributes"]["WEIGHTS_0"] = json!(w);
            }
        }
    }
    doc["extras"]["secretaryDistalHair"] = json!(1);
    doc["buffers"][0]["byteLength"] = json!(data.len());
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distal_bend_preserves_bind_surface_and_pins_upper_lock() {
        let mut d = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,1]}],"nodes":[{"name":"c_sprhirE1_jnt"},{"mesh":0,"skin":0}],"materials":[{}],"meshes":[],"skins":[],"buffers":[{"byteLength":0}],"accessors":[],"bufferViews":[]});
        let mut b = Vec::new();
        let positions = vec![
            vec![0., -0.02, 0.01],
            vec![0., -0.15, 0.08],
            vec![0., -0.30, 0.16],
        ];
        let p = accessor::floats(&mut d, &mut b, &positions, 3).unwrap();
        let j = accessor::shorts(&mut d, &mut b, &[[0; 4]; 3]);
        let w = accessor::floats(&mut d, &mut b, &vec![vec![1., 0., 0., 0.]; 3], 4).unwrap();
        let ib = accessor::floats(
            &mut d,
            &mut b,
            &[Mat4::IDENTITY.to_cols_array().to_vec()],
            16,
        )
        .unwrap();
        d["meshes"] = json!([{"primitives":[{"attributes":{"POSITION":p,"JOINTS_0":j,"WEIGHTS_0":w},"material":0}]}]);
        d["skins"] = json!([{"joints":[0],"inverseBindMatrices":ib}]);
        assert_eq!(distal_hair(&mut d, &mut b).unwrap(), 2);
        let (_, _, world) = compose::scene_frames(&d).unwrap();
        let a = &d["meshes"][0]["primitives"][0]["attributes"];
        let j = accessor::read(&d, &b, a["JOINTS_0"].as_u64().unwrap() as usize).unwrap();
        let w = accessor::read(&d, &b, a["WEIGHTS_0"].as_u64().unwrap() as usize).unwrap();
        let ib = accessor::read(
            &d,
            &b,
            d["skins"][0]["inverseBindMatrices"].as_u64().unwrap() as usize,
        )
        .unwrap();
        for v in 0..3 {
            let mut skinned = Vec3::ZERO;
            for k in 0..4 {
                let joint = j[v][k] as usize;
                let node = d["skins"][0]["joints"][joint].as_u64().unwrap() as usize;
                skinned += (world[node]
                    * Mat4::from_cols_array(ib[joint].as_slice().try_into().unwrap()))
                .transform_point3(Vec3::from_slice(&positions[v]))
                    * w[v][k];
            }
            assert!(skinned.distance(Vec3::from_slice(&positions[v])) < 1e-6);
            assert!((w[v].iter().sum::<f32>() - 1.).abs() < 1e-6);
        }
        assert_eq!(j[0][0], 0.);
        assert_eq!(w[0][0], 1.);
        assert_eq!(j[2][0], 1.);
        assert_eq!(w[2][0], 1.);
        assert_eq!(distal_hair(&mut d, &mut b).unwrap(), 0);
    }
    #[test]
    fn lateral_geometry_generates_a_lateral_lever_instead_of_world_down() {
        let dir = std::env::temp_dir().join(format!("gravity-lever-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,1]}],"nodes":[{"name":"c_sprhirTest_jnt"},{"name":"hair","mesh":0,"skin":0}],"meshes":[],"materials":[{}],"skins":[],"buffers":[{"byteLength":0}],"bufferViews":[],"accessors":[]});
        let mut b = Vec::new();
        let p = accessor::floats(
            &mut d,
            &mut b,
            &[vec![0.2, 0., 0.], vec![0.3, 0., 0.01], vec![0.25, 0.01, 0.]],
            3,
        )
        .unwrap();
        let j = accessor::shorts(&mut d, &mut b, &[[0; 4]; 3]);
        let w = accessor::floats(&mut d, &mut b, &vec![vec![1., 0., 0., 0.]; 3], 4).unwrap();
        let ib = accessor::floats(
            &mut d,
            &mut b,
            &[Mat4::IDENTITY.to_cols_array().to_vec()],
            16,
        )
        .unwrap();
        let i = accessor::shorts(&mut d, &mut b, &[[0, 1, 2, 0]]);
        d["accessors"][i]["type"] = json!("SCALAR");
        d["accessors"][i]["count"] = json!(3);
        d["meshes"] = json!([{"primitives":[{"attributes":{"POSITION":p,"JOINTS_0":j,"WEIGHTS_0":w},"indices":i,"material":0}]}]);
        d["skins"] = json!([{"joints":[0],"inverseBindMatrices":ib}]);
        d["buffers"][0]["byteLength"] = json!(b.len());
        let input = dir.join("in.glb");
        let output = dir.join("out.glb");
        glb_io::write(&input, &d, &b).unwrap();
        let report = annotate(&input, &output).unwrap();
        assert_eq!(report["joints"], 1);
        let (d, _) = glb_io::read(&output).unwrap();
        let tail = &d["nodes"][0]["extras"]["secretarySecondary"]["tail"];
        assert!(tail[0].as_f64().unwrap() > 0.24);
        assert!(tail[1].as_f64().unwrap().abs() < 0.01);
        assert!(
            (d["nodes"][0]["extras"]["secretarySecondary"]["parameters"][2]
                .as_f64()
                .unwrap()
                - 9.81)
                .abs()
                < 1e-6
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
