//! Retarget the existing fitted wardrobe onto the native body. No legacy body,
//! head or synthetic body-fill mesh is copied into the canonical avatar.
use crate::{accessor, compose::scene_frames, glb_io, invalid, surface::Surface, Result};
use glam::{Mat3, Mat4, Vec3};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
fn alias(name: &str) -> String {
    let direct = match name {
        "hips" => "Bip01 Pelvis",
        "spine" => "Bip01 Spine",
        "chest" => "Bip01 Spine1a",
        "upperChest" => "Bip01 Spine1a",
        "neck" => "Bip01 Neck",
        "head" => "Bip01 Head",
        "breast_left" => "Mune_L",
        "breast_right" => "Mune_R",
        "breast_left_sub" => "Mune_L_sub",
        "breast_right_sub" => "Mune_R_sub",
        _ => "",
    };
    if !direct.is_empty() {
        return direct.into();
    }
    for (side, code) in [("left", "L"), ("right", "R")] {
        if let Some(rest) = name.strip_prefix(side) {
            let suffix = match rest {
                "Shoulder" => "Clavicle",
                "UpperArm" => "UpperArm",
                "LowerArm" => "Forearm",
                "Hand" => "Hand",
                "UpperLeg" => "Thigh",
                "LowerLeg" => "Calf",
                "Foot" => "Foot",
                "Toes" => "Toe0",
                _ => "",
            };
            if !suffix.is_empty() {
                return format!("Bip01 {code} {suffix}");
            }
            for (finger, n) in [
                ("Thumb", 0),
                ("Index", 1),
                ("Middle", 2),
                ("Ring", 3),
                ("Little", 4),
            ] {
                if let Some(joint) = rest.strip_prefix(finger) {
                    let digit = match joint {
                        "Proximal" => "",
                        "Intermediate" => "1",
                        "Distal" => "2",
                        _ => continue,
                    };
                    return format!("Bip01 {code} Finger{n}{digit}");
                }
            }
        }
    }
    name.into()
}
fn next(name: &str) -> Option<String> {
    for (a, b) in [
        ("hips", "spine"),
        ("spine", "chest"),
        ("chest", "neck"),
        ("upperChest", "neck"),
        ("neck", "head"),
    ] {
        if name == a {
            return Some(b.into());
        }
    }
    for (a, b) in [
        ("Shoulder", "UpperArm"),
        ("UpperArm", "LowerArm"),
        ("LowerArm", "Hand"),
        ("Hand", "MiddleProximal"),
        ("UpperLeg", "LowerLeg"),
        ("LowerLeg", "Foot"),
        ("Foot", "Toes"),
        ("Proximal", "Intermediate"),
        ("Intermediate", "Distal"),
    ] {
        if let Some(p) = name.strip_suffix(a) {
            return Some(format!("{p}{b}"));
        }
    }
    None
}
fn segment(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> Mat4 {
    let from = b - a;
    let to = d - c;
    if from.length() < 1e-5 || to.length() < 1e-5 {
        return Mat4::from_translation(c - a);
    }
    let direction = from.normalize();
    let ratio = (to.length() / from.length()).clamp(0.5, 2.);
    let stretch = Mat3::IDENTITY
        + Mat3::from_cols(
            direction * direction.x,
            direction * direction.y,
            direction * direction.z,
        ) * (ratio - 1.);
    Mat4::from_translation(c)
        * Mat4::from_quat(glam::Quat::from_rotation_arc(direction, to.normalize()))
        * Mat4::from_mat3(stretch)
        * Mat4::from_translation(-a)
}
fn compact_weights(mut values: HashMap<usize, f32>) -> Vec<(usize, f32)> {
    values.retain(|_, w| *w > 1e-6);
    let mut out = values.into_iter().collect::<Vec<_>>();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.truncate(4);
    let sum = out.iter().map(|v| v.1).sum::<f32>();
    for v in &mut out {
        v.1 /= sum;
    }
    out
}
pub fn retarget(input: &Path, canonical: &Path, output: &Path, hair: bool) -> Result<Value> {
    let (mut doc, mut data) = glb_io::read(input)?;
    let (base, base_data) = glb_io::read(canonical)?;
    let (parents, _, source_world) = scene_frames(&doc)?;
    let (_, _, target_world) = scene_frames(&base)?;
    let source_nodes = doc["nodes"].as_array().unwrap().clone();
    let target_nodes = base["nodes"].as_array().unwrap();
    let source_names = source_nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n["name"].as_str().map(|n| (n.to_owned(), i)))
        .collect::<HashMap<_, _>>();
    let target_names = target_nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n["name"].as_str().map(|n| (n.to_owned(), i)))
        .collect::<HashMap<_, _>>();
    let target_joints = base["skins"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["joints"].as_array().unwrap())
        .filter_map(Value::as_u64)
        .map(|i| i as usize)
        .collect::<HashSet<_>>();
    let mut target_joints = target_joints.into_iter().collect::<Vec<_>>();
    target_joints.sort();
    let joint_index = target_joints
        .iter()
        .enumerate()
        .map(|(i, &j)| (j, i))
        .collect::<HashMap<_, _>>();
    let rotation = Mat4::from_rotation_y(std::f32::consts::PI);
    let direct = source_nodes
        .iter()
        .map(|n| {
            target_names
                .get(&alias(n["name"].as_str().unwrap_or("")))
                .copied()
                .filter(|j| joint_index.contains_key(j))
        })
        .collect::<Vec<_>>();
    let mut mapped = direct.clone();
    let mut matrices = vec![rotation; source_nodes.len()];
    for i in 0..source_nodes.len() {
        if hair {
            continue;
        }
        if let Some(t) = direct[i] {
            let name = source_nodes[i]["name"].as_str().unwrap_or("");
            let from = rotation.transform_point3(source_world[i].w_axis.truncate());
            let to = target_world[t].w_axis.truncate();
            matrices[i] = if let Some(next) = next(name)
                .and_then(|n| source_names.get(&n).copied())
                .filter(|&j| direct[j].is_some())
            {
                segment(
                    from,
                    rotation.transform_point3(source_world[next].w_axis.truncate()),
                    to,
                    target_world[direct[next].unwrap()].w_axis.truncate(),
                ) * rotation
            } else {
                Mat4::from_translation(to - from) * rotation
            };
        }
    }
    for i in 0..mapped.len() {
        if mapped[i].is_none() {
            let mut parent = parents[i];
            while parent >= 0 {
                if direct[parent as usize].is_some() {
                    mapped[i] = direct[parent as usize];
                    matrices[i] = matrices[parent as usize];
                    break;
                }
                parent = parents[parent as usize];
            }
        }
    }
    let surface = if hair {
        None
    } else {
        Some(Surface::body_at_size(&base, &base_data, 0.5)?)
    };
    let mut converted = Vec::new();
    let mut fitted = 0;
    let mut unmapped = 0;
    let mut total = 0;
    let skins = doc["skins"]
        .as_array()
        .ok_or_else(|| invalid("legacy skin missing"))?
        .clone();
    let meshes = doc["meshes"]
        .as_array()
        .ok_or_else(|| invalid("legacy meshes"))?
        .clone();
    for (source_node, node) in source_nodes.iter().enumerate() {
        let Some(mesh) = node["mesh"].as_u64() else {
            continue;
        };
        let source_mesh = &meshes[mesh as usize];
        let rigid = node["skin"].as_u64().is_none();
        let (sj, ib) = if let Some(skin) = node["skin"].as_u64() {
            let skin = &skins[skin as usize];
            (
                skin["joints"]
                    .as_array()
                    .ok_or_else(|| invalid("legacy joints"))?
                    .clone(),
                accessor::read(
                    &doc,
                    &data,
                    skin["inverseBindMatrices"]
                        .as_u64()
                        .ok_or_else(|| invalid("legacy inverse binds"))?
                        as usize,
                )?,
            )
        } else {
            let target = target_names
                .get("Bip01 Head")
                .copied()
                .ok_or_else(|| invalid("native head"))?;
            mapped[source_node] = Some(target);
            matrices[source_node] = rotation;
            (
                vec![json!(source_node)],
                vec![Mat4::IDENTITY.to_cols_array().to_vec()],
            )
        };
        let mut out_mesh = source_mesh.clone();
        out_mesh["primitives"] = json!([]);
        for primitive in source_mesh["primitives"]
            .as_array()
            .ok_or_else(|| invalid("legacy primitives"))?
        {
            let get = |k: &str| {
                primitive["attributes"][k]
                    .as_u64()
                    .map(|i| i as usize)
                    .ok_or_else(|| invalid("legacy skin attribute"))
            };
            let ps = accessor::read(&doc, &data, get("POSITION")?)?;
            let ns = accessor::read(&doc, &data, get("NORMAL")?)?;
            let js = if rigid {
                vec![vec![0.; 4]; ps.len()]
            } else {
                accessor::read(&doc, &data, get("JOINTS_0")?)?
            };
            let ws = if rigid {
                vec![vec![1., 0., 0., 0.]; ps.len()]
            } else {
                accessor::read(&doc, &data, get("WEIGHTS_0")?)?
            };
            if [ns.len(), js.len(), ws.len()]
                .iter()
                .any(|n| *n != ps.len())
            {
                return Err(invalid("legacy attribute count"));
            }
            let mut positions = Vec::new();
            let mut normals = Vec::new();
            let mut indices = Vec::new();
            let mut weights = Vec::new();
            let mut size_delta = Vec::new();
            for v in 0..ps.len() {
                let mut point = Vec3::ZERO;
                let mut normal = Vec3::ZERO;
                let mut skinweights = HashMap::<usize, f32>::new();
                let mut bust = 0.;
                let mut sum = 0.;
                for k in 0..4 {
                    let w = ws[v][k];
                    if w <= 0. {
                        continue;
                    }
                    let j = js[v][k] as usize;
                    let source = sj
                        .get(j)
                        .and_then(Value::as_u64)
                        .ok_or_else(|| invalid("legacy joint index"))?
                        as usize;
                    let Some(target) = mapped.get(source).copied().flatten() else {
                        unmapped += 1;
                        continue;
                    };
                    let inv = ib
                        .get(j)
                        .filter(|m| m.len() == 16)
                        .ok_or_else(|| invalid("legacy inverse matrix"))?;
                    let matrix = matrices[source]
                        * source_world[source]
                        * Mat4::from_cols_array(inv.as_slice().try_into().unwrap());
                    point += matrix.transform_point3(Vec3::from_slice(&ps[v])) * w;
                    normal += matrix
                        .inverse()
                        .transpose()
                        .transform_vector3(Vec3::from_slice(&ns[v]))
                        * w;
                    *skinweights.entry(target).or_default() += w;
                    if target_nodes[target]["name"]
                        .as_str()
                        .is_some_and(|n| n.starts_with("Mune_"))
                    {
                        bust += w;
                    }
                    sum += w;
                }
                if sum < 0.999 {
                    return Err(invalid(format!(
                        "unmapped legacy skin influence at vertex {v}"
                    )));
                }
                point /= sum;
                let mut delta = Vec3::ZERO;
                if let Some(surface) = &surface {
                    let hit = surface.nearest(point);
                    let close = hit.distance < 0.15;
                    if close {
                        delta = hit.delta;
                        if bust > 0.05 && delta.length() > 0.001 {
                            let mix = (bust / 0.3).min(1.);
                            for w in skinweights.values_mut() {
                                *w *= 1. - mix;
                            }
                            for (j, w) in hit.skin {
                                *skinweights.entry(j).or_default() += w * mix;
                            }
                            fitted += 1;
                        }
                    }
                    // Existing garments are authored at their neutral fitting size. Center
                    // the native size curve there; never collapse a cup onto one nearest point.
                    point -= delta * 0.5;
                }
                let packed = compact_weights(skinweights);
                let mut ids = [0u16; 4];
                let mut values = vec![0.; 4];
                for (k, (j, w)) in packed.iter().enumerate() {
                    ids[k] = *joint_index
                        .get(j)
                        .ok_or_else(|| invalid("native joint index"))?
                        as u16;
                    values[k] = *w;
                }
                positions.push(point.to_array().to_vec());
                normals.push(normal.normalize_or_zero().to_array().to_vec());
                indices.push(ids);
                weights.push(values);
                size_delta.push(delta.to_array().to_vec());
                total += 1;
            }
            let mut p = primitive.clone();
            p.as_object_mut().unwrap().remove("targets");
            let attr = p["attributes"]
                .as_object_mut()
                .ok_or_else(|| invalid("legacy attributes"))?;
            attr.remove("TANGENT");
            attr.insert(
                "POSITION".into(),
                json!(accessor::floats(&mut doc, &mut data, &positions, 3)?),
            );
            attr.insert(
                "NORMAL".into(),
                json!(accessor::floats(&mut doc, &mut data, &normals, 3)?),
            );
            attr.insert(
                "JOINTS_0".into(),
                json!(accessor::shorts(&mut doc, &mut data, &indices)),
            );
            attr.insert(
                "WEIGHTS_0".into(),
                json!(accessor::floats(&mut doc, &mut data, &weights, 4)?),
            );
            let a = accessor::floats(&mut doc, &mut data, &size_delta, 3)?;
            p["targets"] = json!([{"POSITION":a}]);
            out_mesh["primitives"].as_array_mut().unwrap().push(p);
        }
        out_mesh["weights"] = json!([0]);
        out_mesh["extras"]["targetNames"] = json!(["munel"]);
        converted.push(out_mesh);
    }
    if converted.is_empty() {
        return Err(invalid("legacy item has no garment or hair geometry"));
    }
    let matrices = target_joints
        .iter()
        .map(|&j| target_world[j].inverse().to_cols_array().to_vec())
        .collect::<Vec<_>>();
    let ib = accessor::floats(&mut doc, &mut data, &matrices, 16)?;
    doc["skins"] = json!([{"joints":target_joints,"inverseBindMatrices":ib}]);
    let mut nodes = target_nodes.clone();
    for n in &mut nodes {
        let o = n.as_object_mut().unwrap();
        o.remove("mesh");
        o.remove("skin");
        o.remove("camera");
    }
    let mut roots = base["scenes"][base["scene"].as_u64().unwrap_or(0) as usize]["nodes"]
        .as_array()
        .cloned()
        .ok_or_else(|| invalid("canonical roots"))?;
    for (i, mesh) in converted.iter().enumerate() {
        let index = nodes.len();
        nodes.push(json!({"name":mesh["name"],"mesh":i,"skin":0}));
        roots.push(json!(index));
    }
    doc["nodes"] = json!(nodes);
    doc["meshes"] = json!(converted);
    doc["scenes"] = json!([{"nodes":roots}]);
    doc["scene"] = json!(0);
    doc.as_object_mut().unwrap().remove("animations");
    doc["buffers"][0]["byteLength"] = json!(data.len());
    doc["extras"]["nativeRetarget"] = json!({"version":1,"source":input.file_name().and_then(|n|n.to_str()),"mappedJoints":direct.iter().filter(|j|j.is_some()).count(),"vertices":total,"surfaceFittedVertices":fitted,"unmappedInfluences":unmapped,"canonicalBodyCopied":false,"fitReviewRequired":true});
    glb_io::write(output, &doc, &data)?;
    Ok(
        json!({"output":output,"vertices":total,"fitted":fitted,"unmapped":unmapped,"meshes":converted.len()}),
    )
}
