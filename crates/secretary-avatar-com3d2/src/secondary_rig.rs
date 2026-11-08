//! Bind soft skirt panels to the native skeleton. Rigid shoes/gloves stay rigid.
use crate::{accessor, compose, invalid, Result};
use glam::{Mat4, Vec3};
use serde_json::{json, Value};
pub fn garment(doc: &mut Value, data: &mut Vec<u8>, category: &str, prefix: &str) -> Result<usize> {
    if category == "original-skirt" {
        return crate::legacy_cloth::bind(doc, data, prefix);
    }
    let hanging = category == "hanging-accessory";
    if hanging && doc["extras"]["secretaryClothComponentSizes"].is_array() {
        return ribbon_chains(doc, data, prefix);
    }
    let hair = ["hairf", "hairfull"].contains(&category);
    if !hair && !hanging && !["skirt", "onepiece", "original-skirt"].contains(&category) {
        return Ok(0);
    }
    let (_, _, world) = compose::scene_frames(doc)?;
    let Some(parent) = doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .position(|n| n["name"] == if hair { "Bip01 Head" } else { "Bip01 Pelvis" })
    else {
        return Ok(0);
    };
    let waist = world[parent].w_axis.y + if hair { 0.06 } else { 0. };
    let center = world[parent].w_axis.truncate();
    let mut panels = Vec::new();
    let mut panel_world = Vec::new();
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.;
        let p = Vec3::new(
            center.x + (if hair { 0.045 } else { 0.12 }) * angle.sin(),
            waist,
            center.z + (if hair { 0.045 } else { 0.10 }) * angle.cos(),
        );
        let w = Mat4::from_translation(p);
        let (scale, rotation, translation) =
            (world[parent].inverse() * w).to_scale_rotation_translation();
        let n = doc["nodes"].as_array().unwrap().len();
        doc["nodes"].as_array_mut().unwrap().push(json!({"name":format!("SecretarySecondary_{prefix}_{i}"),"translation":translation.to_array(),"rotation":rotation.to_array(),"scale":scale.to_array(),"extras":{"secretarySecondary":{"kind":if hair{"hair"}else{"cloth"},"tail":[0,-0.26,0]}}}));
        if !doc["nodes"][parent]["children"].is_array() {
            doc["nodes"][parent]["children"] = json!([]);
        }
        doc["nodes"][parent]["children"]
            .as_array_mut()
            .unwrap()
            .push(json!(n));
        panels.push(n);
        panel_world.push(w);
    }
    let skins = doc["skins"]
        .as_array()
        .ok_or_else(|| invalid("cloth skins"))?
        .len();
    let mut offsets = Vec::new();
    for si in 0..skins {
        let count = doc["skins"][si]["joints"].as_array().unwrap().len();
        offsets.push(count);
        let ib = doc["skins"][si]["inverseBindMatrices"].as_u64().unwrap() as usize;
        let mut binds = accessor::read(doc, data, ib)?;
        for w in &panel_world {
            binds.push(w.inverse().to_cols_array().to_vec());
        }
        let ib = accessor::floats(doc, data, &binds, 16)?;
        doc["skins"][si]["inverseBindMatrices"] = json!(ib);
        doc["skins"][si]["joints"]
            .as_array_mut()
            .unwrap()
            .extend(panels.iter().map(|n| json!(n)));
    }
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let mut changed = 0;
    for (ni, n) in nodes.iter().enumerate() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let mesh = mesh as usize;
        let skin = skin as usize;
        let primitives = doc["meshes"][mesh]["primitives"]
            .as_array()
            .unwrap()
            .clone();
        for (pi, p) in primitives.iter().enumerate() {
            let read = |name: &str| {
                accessor::read(doc, data, p["attributes"][name].as_u64().unwrap() as usize)
            };
            let positions = read("POSITION")?;
            let eligible = if hanging {
                Some(hanging_vertices(doc, data, p, world[ni], waist)?)
            } else {
                None
            };
            let mut joints = read("JOINTS_0")?;
            let mut weights = read("WEIGHTS_0")?;
            for (v, p) in positions.iter().enumerate() {
                if category == "original-skirt" {
                    let material = primitives[pi]["material"]
                        .as_u64()
                        .and_then(|i| doc["materials"][i as usize]["name"].as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if material.contains("shoe")
                        || material.contains("hat")
                        || material.contains("headband")
                    {
                        continue;
                    }
                    // Original boots, cuffs and gloves share clothing materials.
                    // A low/crouched bind pose must not turn them into waist cloth.
                    let limb_weight = (0..4)
                        .filter(|&k| {
                            let j = joints[v][k] as usize;
                            let n = doc["skins"][skin]["joints"][j].as_u64().unwrap() as usize;
                            let name = doc["nodes"][n]["name"].as_str().unwrap_or("");
                            name.contains("Calf")
                                || name.contains("Foot")
                                || name.contains("Toe")
                                || name.contains("Arm")
                                || name.contains("Hand")
                                || name.contains("Finger")
                                || name.contains("Thigh")
                                || name.contains("twist")
                        })
                        .map(|k| weights[v][k])
                        .sum::<f32>();
                    if limb_weight > 0.15 {
                        continue;
                    }
                }

                if category == "original-skirt"
                    && (0..4).any(|k| {
                        let joint = joints[v][k] as usize;
                        let node = doc["skins"][skin]["joints"][joint].as_u64().unwrap() as usize;
                        weights[v][k] > 0.01
                            && doc["nodes"][node]["name"]
                                .as_str()
                                .is_some_and(|n| n.contains("sprmnt") || n.starts_with("mant_"))
                    })
                {
                    continue;
                }

                if eligible.as_ref().is_some_and(|set| !set.contains(&v)) {
                    continue;
                }
                let p = world[ni].transform_point3(Vec3::from_slice(p));
                let fall = ((waist - (if hair { 0.02 } else { 0.03 }) - p.y)
                    / (if hair { 0.18 } else { 0.24 }))
                .clamp(0., 1.);
                if fall <= 0.
                    || p.y < waist - (if hair || hanging { 0.9 } else { 0.44 })
                    || p.x.abs() > 0.38
                {
                    continue;
                }
                if category == "original-skirt"
                    && Vec3::new(p.x - center.x, 0., p.z - center.z).length() < 0.14
                {
                    continue;
                }
                let angle = (p.x - center.x)
                    .atan2(p.z - center.z)
                    .rem_euclid(std::f32::consts::TAU);
                let sector = angle / std::f32::consts::TAU * 8.;
                let panel = sector.floor() as usize % 8;
                let fraction = sector.fract();
                let blend = fall * if hanging { 1. } else { 0.7 };
                let mut old = (0..4)
                    .map(|k| (joints[v][k] as u16, weights[v][k]))
                    .collect::<Vec<_>>();
                old.sort_by(|a, b| b.1.total_cmp(&a.1));
                old.truncate(2);
                let sum = old.iter().map(|v| v.1).sum::<f32>();
                if sum < 1e-6 {
                    continue;
                }
                for k in 0..2 {
                    joints[v][k] = old[k].0 as f32;
                    weights[v][k] = old[k].1 / sum * (1. - blend);
                }
                joints[v][2] = (offsets[skin] + panel) as f32;
                weights[v][2] = blend * (1. - fraction);
                joints[v][3] = (offsets[skin] + (panel + 1) % 8) as f32;
                weights[v][3] = blend * fraction;
                changed += 1;
            }
            let j = joints
                .iter()
                .map(|v| std::array::from_fn(|i| v[i] as u16))
                .collect::<Vec<[u16; 4]>>();
            let j = accessor::shorts(doc, data, &j);
            let w = accessor::floats(doc, data, &weights, 4)?;
            doc["meshes"][mesh]["primitives"][pi]["attributes"]["JOINTS_0"] = json!(j);
            doc["meshes"][mesh]["primitives"][pi]["attributes"]["WEIGHTS_0"] = json!(w);
        }
    }
    doc["buffers"][0]["byteLength"] = json!(data.len());
    Ok(changed)
}
/// Select long connected hanging surfaces. Small rigid bags, buckles and shoes
/// must not acquire a soft-cloth weight merely because they share a material.
fn hanging_vertices(
    doc: &Value,
    data: &[u8],
    p: &Value,
    world: Mat4,
    waist: f32,
) -> Result<std::collections::HashSet<usize>> {
    let positions = accessor::read(
        doc,
        data,
        p["attributes"]["POSITION"]
            .as_u64()
            .ok_or_else(|| invalid("hanging positions"))? as usize,
    )?;
    let indices = accessor::read(
        doc,
        data,
        p["indices"]
            .as_u64()
            .ok_or_else(|| invalid("hanging triangles"))? as usize,
    )?;
    let mut parent = (0..positions.len()).collect::<Vec<_>>();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut used = std::collections::HashSet::new();
    let (triangles, remainder) = indices.as_chunks::<3>();
    if !remainder.is_empty() {
        return Err(invalid("incomplete hanging triangle"));
    }
    for triangle in triangles {
        let ids = triangle.iter().map(|v| v[0] as usize).collect::<Vec<_>>();
        if ids.iter().any(|i| *i >= positions.len()) {
            return Err(invalid("hanging index bounds"));
        }
        let a = root(&mut parent, ids[0]);
        for i in ids {
            let b = root(&mut parent, i);
            parent[b] = a;
            used.insert(i);
        }
    }
    // Exported cloth has duplicated UV/normal seam vertices. Treat coincident
    // points as connected for classification; do not alter the render topology.
    let mut coincident = std::collections::HashMap::new();
    for i in &used {
        let p = Vec3::from_slice(&positions[*i]);
        if !p.is_finite() {
            return Err(invalid("nonfinite hanging vertex"));
        }
        let key = (
            (p.x * 10000.).round() as i64,
            (p.y * 10000.).round() as i64,
            (p.z * 10000.).round() as i64,
        );
        if let Some(other) = coincident.insert(key, *i) {
            let a = root(&mut parent, *i);
            let b = root(&mut parent, other);
            parent[b] = a;
        }
    }
    let mut bounds = std::collections::HashMap::<usize, (Vec3, Vec3)>::new();
    let mut counts = std::collections::HashMap::<usize, usize>::new();

    for i in &used {
        let key = root(&mut parent, *i);
        *counts.entry(key).or_default() += 1;
        let v = world.transform_point3(Vec3::from_slice(&positions[*i]));
        let b = bounds.entry(key).or_insert((v, v));
        b.0 = b.0.min(v);
        b.1 = b.1.max(v);
    }
    Ok(used
        .into_iter()
        .filter(|i| {
            let component = root(&mut parent, *i);
            if let Some(sizes) = doc["extras"]["secretaryClothComponentSizes"].as_array() {
                return sizes
                    .iter()
                    .any(|n| n.as_u64() == Some(counts[&component] as u64));
            }
            let (lo, hi) = bounds[&component];
            hi.y - lo.y > 0.35
                && lo.y < waist - 0.35
                && (hi.y - waist).abs() < 0.25
                && hi.x - lo.x < 0.35
        })
        .collect())
}

/// Author-reviewed hanging strips use an unbranched chain, not radial skirt
/// panels. Otherwise the same narrow ribbon is pulled around unrelated roots.
fn ribbon_chains(doc: &mut Value, data: &mut Vec<u8>, prefix: &str) -> Result<usize> {
    let (_, _, world) = compose::scene_frames(doc)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let waist = nodes
        .iter()
        .position(|n| n["name"] == "Bip01 Pelvis")
        .map(|i| world[i].w_axis.y)
        .ok_or_else(|| invalid("ribbon pelvis"))?;
    let mut changed = 0;
    for (ni, n) in nodes.iter().enumerate() {
        if doc["extras"]["secretaryRibbonNode"]
            .as_u64()
            .is_some_and(|target| target as usize != ni)
        {
            continue;
        }
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let (mesh, skin) = (mesh as usize, skin as usize);
        let primitives = doc["meshes"][mesh]["primitives"]
            .as_array()
            .unwrap()
            .clone();
        for (pi, p) in primitives.iter().enumerate() {
            let eligible = hanging_vertices(doc, data, p, world[ni], waist)?;
            if eligible.is_empty() {
                continue;
            }
            let a = &p["attributes"];
            let pos = accessor::read(doc, data, a["POSITION"].as_u64().unwrap() as usize)?;
            let mut joints = accessor::read(doc, data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let mut weights = accessor::read(doc, data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let points = pos
                .iter()
                .map(|p| world[ni].transform_point3(Vec3::from_slice(p)))
                .collect::<Vec<_>>();
            let mut parent = (0..pos.len()).collect::<Vec<_>>();
            fn root(p: &mut [usize], mut i: usize) -> usize {
                while p[i] != i {
                    p[i] = p[p[i]];
                    i = p[i];
                }
                i
            }
            let ix = accessor::read(doc, data, p["indices"].as_u64().unwrap() as usize)?;
            for t in ix.as_chunks::<3>().0 {
                let first = t[0][0] as usize;
                if !eligible.contains(&first) {
                    continue;
                }
                let a = root(&mut parent, first);
                for v in t {
                    let i = v[0] as usize;
                    if eligible.contains(&i) {
                        let b = root(&mut parent, i);
                        parent[b] = a;
                    }
                }
            }
            let mut welded = std::collections::HashMap::new();
            for i in &eligible {
                let p = points[*i];
                let key = (
                    (p.x * 10000.).round() as i64,
                    (p.y * 10000.).round() as i64,
                    (p.z * 10000.).round() as i64,
                );
                if let Some(other) = welded.insert(key, *i) {
                    let a = root(&mut parent, *i);
                    let b = root(&mut parent, other);
                    parent[b] = a;
                }
            }
            let mut groups = std::collections::BTreeMap::<usize, Vec<usize>>::new();
            let mut ordered = eligible.into_iter().collect::<Vec<_>>();
            ordered.sort_unstable();
            for i in ordered {
                groups.entry(root(&mut parent, i)).or_default().push(i);
            }
            for group in groups.values() {
                let hi = group
                    .iter()
                    .map(|i| points[*i].y)
                    .fold(f32::NEG_INFINITY, f32::max);
                let lo = group
                    .iter()
                    .map(|i| points[*i].y)
                    .fold(f32::INFINITY, f32::min);
                let height = hi - lo;
                if height < 0.05 {
                    continue;
                }
                let mut control = Vec::new();
                for t in [0., 0.4, 0.8] {
                    let y = hi - height * t;
                    let band = group
                        .iter()
                        .copied()
                        .filter(|i| (points[*i].y - y).abs() < (height * 0.12).max(0.015))
                        .collect::<Vec<_>>();
                    let point =
                        band.iter().map(|i| points[*i]).sum::<Vec3>() / band.len().max(1) as f32;
                    if band.is_empty() {
                        return Err(invalid("ribbon chain lacks a control band"));
                    }
                    control.push(point);
                }
                let mut support = std::collections::BTreeMap::<usize, f32>::new();
                for v in group.iter().filter(|v| points[**v].y > hi - 0.025) {
                    for k in 0..4 {
                        let joint = joints[*v][k] as usize;
                        *support.entry(joint).or_default() += weights[*v][k];
                    }
                }
                let support = support
                    .into_iter()
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .ok_or_else(|| invalid("ribbon attachment"))?
                    .0;
                let attachment = doc["skins"][skin]["joints"][support].as_u64().unwrap() as usize;
                let offset = doc["skins"][skin]["joints"].as_array().unwrap().len();
                let mut previous = attachment;
                let mut parent_world = world[attachment];
                let mut ib = accessor::read(
                    doc,
                    data,
                    doc["skins"][skin]["inverseBindMatrices"].as_u64().unwrap() as usize,
                )?;
                let end_band = group
                    .iter()
                    .copied()
                    .filter(|i| points[*i].y < lo + 0.025)
                    .collect::<Vec<_>>();
                let endpoint = end_band.iter().map(|i| points[*i]).sum::<Vec3>()
                    / end_band.len().max(1) as f32;
                for (segment, point) in control.iter().enumerate() {
                    let tail = if segment + 1 < control.len() {
                        control[segment + 1] - *point
                    } else {
                        endpoint - *point
                    };
                    let w = Mat4::from_translation(*point);
                    let (scale, rotation, translation) =
                        (parent_world.inverse() * w).to_scale_rotation_translation();
                    let node = doc["nodes"].as_array().unwrap().len();
                    doc["nodes"].as_array_mut().unwrap().push(json!({"name":format!("SecretaryRibbon_{prefix}_{offset}_{segment}"),"translation":translation.to_array(),"rotation":rotation.to_array(),"scale":scale.to_array(),"extras":{"secretarySecondary":{"kind":"cloth","chain":true,"tail":tail.to_array()}}}));
                    if !doc["nodes"][previous]["children"].is_array() {
                        doc["nodes"][previous]["children"] = json!([]);
                    }
                    doc["nodes"][previous]["children"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(node));
                    doc["skins"][skin]["joints"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(node));
                    ib.push(w.inverse().to_cols_array().to_vec());
                    previous = node;
                    parent_world = w;
                }
                if !doc["extras"]["secretaryRibbonTips"].is_array() {
                    doc["extras"]["secretaryRibbonTips"] = json!([]);
                }
                doc["extras"]["secretaryRibbonTips"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"position":endpoint.to_array(),"bone":previous}));
                let ib = accessor::floats(doc, data, &ib, 16)?;
                doc["skins"][skin]["inverseBindMatrices"] = json!(ib);
                for v in group {
                    let distance = hi - points[*v].y;
                    let blend = ((distance - 0.02) / 0.025).clamp(0., 1.);
                    let blend = blend * blend * (3. - 2. * blend);
                    let sector = (distance / height / 0.4).clamp(0., 2.);
                    let segment = sector.floor() as usize;
                    let mix = sector.fract();
                    let mut old = (0..4)
                        .map(|k| (joints[*v][k], weights[*v][k]))
                        .collect::<Vec<_>>();
                    old.sort_by(|a, b| b.1.total_cmp(&a.1));
                    let sum = old[0].1 + old[1].1;
                    if sum < 1e-6 {
                        return Err(invalid("ribbon weights"));
                    }
                    joints[*v] = vec![
                        old[0].0,
                        old[1].0,
                        (offset + segment) as f32,
                        (offset + (segment + 1).min(2)) as f32,
                    ];
                    weights[*v] = vec![
                        old[0].1 / sum * (1. - blend),
                        old[1].1 / sum * (1. - blend),
                        blend * (1. - mix),
                        blend * mix,
                    ];
                    changed += 1;
                }
            }
            let j = accessor::shorts(
                doc,
                data,
                &joints
                    .iter()
                    .map(|v| std::array::from_fn(|k| v[k] as u16))
                    .collect::<Vec<[u16; 4]>>(),
            );
            let w = accessor::floats(doc, data, &weights, 4)?;
            doc["meshes"][mesh]["primitives"][pi]["attributes"]["JOINTS_0"] = json!(j);
            doc["meshes"][mesh]["primitives"][pi]["attributes"]["WEIGHTS_0"] = json!(w);
        }
    }
    doc["buffers"][0]["byteLength"] = json!(data.len());
    Ok(changed)
}

/// Freeze the current skinned pose into an individual wearable before authoring
/// new cloth chains. Original native body joints remain animated; their current
/// world frames become this wearable's new inverse-bind reference.
fn bake_wearable(doc: &mut Value, data: &mut Vec<u8>, ni: usize) -> Result<()> {
    let (parents, _, world) = compose::scene_frames(doc)?;
    let mesh = doc["nodes"][ni]["mesh"]
        .as_u64()
        .ok_or_else(|| invalid("posed wearable mesh"))? as usize;
    let old_skin = doc["nodes"][ni]["skin"]
        .as_u64()
        .ok_or_else(|| invalid("posed wearable skin"))? as usize;
    let mut skin = doc["skins"][old_skin].clone();
    let inverse = accessor::read(
        doc,
        data,
        skin["inverseBindMatrices"].as_u64().unwrap() as usize,
    )?;
    let joints = skin["joints"].as_array().unwrap();
    let transforms = joints
        .iter()
        .enumerate()
        .map(|(i, j)| {
            world[j.as_u64().unwrap() as usize]
                * Mat4::from_cols_array(inverse[i].as_slice().try_into().unwrap())
        })
        .collect::<Vec<_>>();
    let binds = joints
        .iter()
        .map(|j| {
            world[j.as_u64().unwrap() as usize]
                .inverse()
                .to_cols_array()
                .to_vec()
        })
        .collect::<Vec<_>>();
    let primitives = doc["meshes"][mesh]["primitives"]
        .as_array()
        .unwrap()
        .clone();
    for (pi, p) in primitives.iter().enumerate() {
        let a = &p["attributes"];
        let ids = accessor::read(doc, data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
        let weights = accessor::read(doc, data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
        let matrices = ids
            .iter()
            .zip(&weights)
            .map(|(j, w)| {
                let mut m = Mat4::ZERO;
                for k in 0..4 {
                    m += transforms[j[k] as usize] * w[k];
                }
                m
            })
            .collect::<Vec<_>>();
        for name in ["POSITION", "NORMAL", "TANGENT"] {
            let Some(index) = a[name].as_u64() else {
                continue;
            };
            let mut values = accessor::read(doc, data, index as usize)?;
            for (v, m) in values.iter_mut().zip(&matrices) {
                let p = Vec3::from_slice(v);
                let value = if name == "POSITION" {
                    m.transform_point3(p)
                } else {
                    m.transform_vector3(p).normalize_or_zero()
                };
                v[..3].copy_from_slice(&value.to_array());
            }
            let dims = if name == "TANGENT" { 4 } else { 3 };
            let index = accessor::floats(doc, data, &values, dims)?;
            doc["meshes"][mesh]["primitives"][pi]["attributes"][name] = json!(index);
        }
        if let Some(targets) = p["targets"].as_array() {
            for (ti, target) in targets.iter().enumerate() {
                for name in ["POSITION", "NORMAL", "TANGENT"] {
                    let Some(index) = target[name].as_u64() else {
                        continue;
                    };
                    let mut values = accessor::read(doc, data, index as usize)?;
                    for (v, m) in values.iter_mut().zip(&matrices) {
                        let p = m.transform_vector3(Vec3::from_slice(v));
                        v[..3].copy_from_slice(&p.to_array());
                    }
                    let index = accessor::floats(doc, data, &values, 3)?;
                    doc["meshes"][mesh]["primitives"][pi]["targets"][ti][name] = json!(index);
                }
            }
        }
    }
    let ib = accessor::floats(doc, data, &binds, 16)?;
    skin["inverseBindMatrices"] = json!(ib);
    let new_skin = doc["skins"].as_array().unwrap().len();
    doc["skins"].as_array_mut().unwrap().push(skin);
    doc["nodes"][ni]["skin"] = json!(new_skin);
    // Geometry is now in world space. Keep the mesh's world frame at identity,
    // including when it is nested under an armature object.
    let matrix = if parents[ni] >= 0 {
        world[parents[ni] as usize].inverse()
    } else {
        Mat4::IDENTITY
    };
    let node = doc["nodes"][ni].as_object_mut().unwrap();
    node.remove("translation");
    node.remove("rotation");
    node.remove("scale");
    node.insert("matrix".into(), json!(matrix.to_cols_array()));
    doc["buffers"][0]["byteLength"] = json!(data.len());
    Ok(())
}
pub fn posed_ribbons(path: &std::path::Path) -> Result<usize> {
    let (mut doc, mut data) = crate::glb_io::read(path)?;
    let targets = doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, n)| n["mesh"].is_number() && n["extras"]["secretaryRibbonSizes"].is_array())
        .map(|(i, n)| (i, n["extras"]["secretaryRibbonSizes"].clone()))
        .collect::<Vec<_>>();
    let mut changed = 0;
    for (ni, sizes) in targets {
        bake_wearable(&mut doc, &mut data, ni)?;
        doc["extras"]["secretaryRibbonNode"] = json!(ni);
        doc["extras"]["secretaryClothComponentSizes"] = sizes;
        let count = ribbon_chains(&mut doc, &mut data, &format!("posed_{ni}"))?;
        if count == 0 {
            return Err(invalid("reviewed ribbon components did not match"));
        }
        changed += count;
    }
    if let Some(extra) = doc["extras"].as_object_mut() {
        extra.remove("secretaryRibbonNode");
        extra.remove("secretaryClothComponentSizes");
    }
    if changed > 0 {
        attach_tip_ornaments(&mut doc, &mut data)?;
        crate::glb_io::write(path, &doc, &data)?;
    }
    Ok(changed)
}

pub fn component_groups(points: &[Vec3], indices: &[Vec<f32>]) -> Result<Vec<Vec<usize>>> {
    let mut parent = (0..points.len()).collect::<Vec<_>>();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut used = std::collections::BTreeSet::new();
    for triangle in indices.as_chunks::<3>().0 {
        let ids = triangle.iter().map(|v| v[0] as usize).collect::<Vec<_>>();
        if ids.iter().any(|i| *i >= points.len()) {
            return Err(invalid("ornament index"));
        }
        let a = root(&mut parent, ids[0]);
        for i in ids {
            let b = root(&mut parent, i);
            parent[b] = a;
            used.insert(i);
        }
    }
    let mut welded = std::collections::HashMap::new();
    for i in &used {
        let p = points[*i];
        let key = (
            (p.x * 10000.).round() as i64,
            (p.y * 10000.).round() as i64,
            (p.z * 10000.).round() as i64,
        );
        if let Some(other) = welded.insert(key, *i) {
            let a = root(&mut parent, *i);
            let b = root(&mut parent, other);
            parent[b] = a;
        }
    }
    let mut groups = std::collections::BTreeMap::<usize, Vec<usize>>::new();
    for i in used {
        groups.entry(root(&mut parent, i)).or_default().push(i);
    }
    Ok(groups.into_values().collect())
}
fn tip_attachment(points: &[Vec3], group: &[usize], tips: &[(Vec3, usize)]) -> Option<usize> {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let mut center = Vec3::ZERO;
    for i in group {
        lo = lo.min(points[*i]);
        hi = hi.max(points[*i]);
        center += points[*i];
    }
    if (hi - lo).max_element() > 0.18 {
        return None;
    }
    center /= group.len() as f32;
    tips.iter()
        .filter_map(|(point, bone)| {
            let distance = point.distance(center);
            (distance < 0.11).then_some((distance, *bone))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, bone)| bone)
}
fn attach_tip_ornaments(doc: &mut Value, data: &mut Vec<u8>) -> Result<usize> {
    let tips = doc["extras"]["secretaryRibbonTips"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| {
            let p = t["position"].as_array().unwrap();
            (
                Vec3::new(
                    p[0].as_f64().unwrap() as f32,
                    p[1].as_f64().unwrap() as f32,
                    p[2].as_f64().unwrap() as f32,
                ),
                t["bone"].as_u64().unwrap() as usize,
            )
        })
        .collect::<Vec<_>>();
    let (_, _, world) = compose::scene_frames(doc)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let mut attached = 0;
    for (ni, n) in nodes.iter().enumerate() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let (mesh, skin) = (mesh as usize, skin as usize);
        if n["extras"]["secretaryRibbonSizes"].is_array() {
            continue;
        }
        let primitives = doc["meshes"][mesh]["primitives"]
            .as_array()
            .unwrap()
            .clone();
        if primitives.iter().any(|p| {
            doc["materials"][p["material"].as_u64().unwrap_or(0) as usize]["extras"]
                ["secretarySkin"]["version"]
                == 2
        }) {
            continue;
        }
        let old_skin = &doc["skins"][skin];
        let inverse = accessor::read(
            doc,
            data,
            old_skin["inverseBindMatrices"].as_u64().unwrap() as usize,
        )?;
        let transforms = old_skin["joints"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, j)| {
                world[j.as_u64().unwrap() as usize]
                    * Mat4::from_cols_array(inverse[i].as_slice().try_into().unwrap())
            })
            .collect::<Vec<_>>();
        let mut matches = Vec::new();
        for (pi, p) in primitives.iter().enumerate() {
            let a = &p["attributes"];
            let pos = accessor::read(doc, data, a["POSITION"].as_u64().unwrap() as usize)?;
            let j = accessor::read(doc, data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let w = accessor::read(doc, data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let points = pos
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let mut v = Vec3::ZERO;
                    for k in 0..4 {
                        v += transforms[j[i][k] as usize].transform_point3(Vec3::from_slice(p))
                            * w[i][k];
                    }
                    v
                })
                .collect::<Vec<_>>();
            let ix = accessor::read(doc, data, p["indices"].as_u64().unwrap() as usize)?;
            for group in component_groups(&points, &ix)? {
                if let Some(bone) = tip_attachment(&points, &group, &tips) {
                    matches.push((pi, group, bone));
                }
            }
        }
        if matches.is_empty() {
            continue;
        }
        bake_wearable(doc, data, ni)?;
        let skin = doc["nodes"][ni]["skin"].as_u64().unwrap() as usize;
        for (pi, group, bone) in matches {
            let offset = doc["skins"][skin]["joints"].as_array().unwrap().len();
            doc["skins"][skin]["joints"]
                .as_array_mut()
                .unwrap()
                .push(json!(bone));
            let mut ib = accessor::read(
                doc,
                data,
                doc["skins"][skin]["inverseBindMatrices"].as_u64().unwrap() as usize,
            )?;
            ib.push(world[bone].inverse().to_cols_array().to_vec());
            let ib = accessor::floats(doc, data, &ib, 16)?;
            doc["skins"][skin]["inverseBindMatrices"] = json!(ib);
            let a = doc["meshes"][mesh]["primitives"][pi]["attributes"].clone();
            let mut j = accessor::read(doc, data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let mut w = accessor::read(doc, data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            for v in &group {
                j[*v] = vec![offset as f32, 0., 0., 0.];
                w[*v] = vec![1., 0., 0., 0.];
            }
            let j = accessor::shorts(
                doc,
                data,
                &j.iter()
                    .map(|v| std::array::from_fn(|k| v[k] as u16))
                    .collect::<Vec<[u16; 4]>>(),
            );
            let w = accessor::floats(doc, data, &w, 4)?;
            doc["meshes"][mesh]["primitives"][pi]["attributes"]["JOINTS_0"] = json!(j);
            doc["meshes"][mesh]["primitives"][pi]["attributes"]["WEIGHTS_0"] = json!(w);
            attached += group.len();
        }
    }
    doc["buffers"][0]["byteLength"] = json!(data.len());
    doc["extras"]["secretaryRibbonOrnamentVertices"] = json!(attached);
    Ok(attached)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn end_ornament_follows_chain_without_dragging_shoes_or_remote_props() {
        let mut d = json!({"scene":0,"scenes":[{"nodes":[0,2]}],"nodes":[{"name":"Bip01 Pelvis","translation":[0,0.9,0],"children":[1]},{"name":"tip","translation":[0.1,-0.6,0]},{"mesh":0,"skin":0}],"meshes":[],"skins":[],"materials":[{}],"accessors":[],"bufferViews":[],"buffers":[{"byteLength":0}],"extras":{"secretaryRibbonTips":[{"position":[0.1,0.3,0],"bone":1}]}});
        let mut b = Vec::new();
        let positions = vec![
            vec![0.08, 0.3, 0.],
            vec![0.12, 0.3, 0.],
            vec![0.1, 0.27, 0.],
            vec![0.48, 0.1, 0.],
            vec![0.52, 0.1, 0.],
            vec![0.5, 0.07, 0.],
            vec![-0.05, 0.3, 0.],
            vec![0.25, 0.3, 0.],
            vec![0.1, 0.1, 0.],
        ];
        let p = accessor::floats(&mut d, &mut b, &positions, 3).unwrap();
        let j = accessor::shorts(&mut d, &mut b, &[[0; 4]; 9]);
        let w = accessor::floats(&mut d, &mut b, &vec![vec![1., 0., 0., 0.]; 9], 4).unwrap();
        let ix = accessor::shorts(&mut d, &mut b, &[[0, 1, 2, 3], [4, 5, 6, 7], [8, 0, 0, 0]]);
        d["accessors"][ix]["type"] = json!("SCALAR");
        d["accessors"][ix]["count"] = json!(9);
        let ib = accessor::floats(
            &mut d,
            &mut b,
            &[Mat4::from_translation(Vec3::new(0., -0.9, 0.))
                .to_cols_array()
                .to_vec()],
            16,
        )
        .unwrap();
        d["skins"] = json!([{"joints":[0],"inverseBindMatrices":ib}]);
        d["meshes"] = json!([{"primitives":[{"attributes":{"POSITION":p,"JOINTS_0":j,"WEIGHTS_0":w},"indices":ix,"material":0}]}]);
        assert_eq!(attach_tip_ornaments(&mut d, &mut b).unwrap(), 3);
        d["nodes"][1]["translation"][0] = json!(0.2);
        let (_, _, world) = compose::scene_frames(&d).unwrap();
        let a = &d["meshes"][0]["primitives"][0]["attributes"];
        let pos = accessor::read(&d, &b, a["POSITION"].as_u64().unwrap() as usize).unwrap();
        let j = accessor::read(&d, &b, a["JOINTS_0"].as_u64().unwrap() as usize).unwrap();
        let w = accessor::read(&d, &b, a["WEIGHTS_0"].as_u64().unwrap() as usize).unwrap();
        let skin = d["nodes"][2]["skin"].as_u64().unwrap() as usize;
        let ib = accessor::read(
            &d,
            &b,
            d["skins"][skin]["inverseBindMatrices"].as_u64().unwrap() as usize,
        )
        .unwrap();
        for v in 0..9 {
            let mut actual = Vec3::ZERO;
            for k in 0..4 {
                let joint = j[v][k] as usize;
                let node = d["skins"][skin]["joints"][joint].as_u64().unwrap() as usize;
                actual += (world[node]
                    * Mat4::from_cols_array(ib[joint].as_slice().try_into().unwrap()))
                .transform_point3(Vec3::from_slice(&pos[v]))
                    * w[v][k];
            }
            let expected =
                Vec3::from_slice(&positions[v]) + if v < 3 { Vec3::X * 0.1 } else { Vec3::ZERO };
            assert!(actual.distance(expected) < 1e-6);
        }
    }

    #[test]
    fn pose_bake_preserves_world_surface_uvs_and_morph_direction() {
        let rotation = glam::Quat::from_rotation_z(0.45);
        let mut d = json!({"scene":0,"scenes":[{"nodes":[0,1]}],"nodes":[{"name":"Bip01 Pelvis","translation":[0,0.9,0],"rotation":rotation.to_array()},{"mesh":0,"skin":0}],"meshes":[],"skins":[],"accessors":[],"bufferViews":[],"buffers":[{"byteLength":0}]});
        let mut b = Vec::new();
        let positions = vec![vec![0.1, 0.9, 0.1], vec![-0.1, 1., 0.1], vec![0., 0.9, 0.2]];
        let p = accessor::floats(&mut d, &mut b, &positions, 3).unwrap();
        let j = accessor::shorts(&mut d, &mut b, &[[0; 4]; 3]);
        let w = accessor::floats(&mut d, &mut b, &vec![vec![1., 0., 0., 0.]; 3], 4).unwrap();
        let uv = accessor::floats(
            &mut d,
            &mut b,
            &[vec![0., 0.], vec![1., 0.], vec![0., 1.]],
            2,
        )
        .unwrap();
        let delta = accessor::floats(&mut d, &mut b, &vec![vec![0.01, 0., 0.]; 3], 3).unwrap();
        let ib = accessor::floats(
            &mut d,
            &mut b,
            &[Mat4::from_translation(Vec3::new(0., -0.9, 0.))
                .to_cols_array()
                .to_vec()],
            16,
        )
        .unwrap();
        d["skins"] = json!([{"joints":[0],"inverseBindMatrices":ib}]);
        d["meshes"] = json!([{"primitives":[{"attributes":{"POSITION":p,"JOINTS_0":j,"WEIGHTS_0":w,"TEXCOORD_0":uv},"targets":[{"POSITION":delta}]}]}]);
        let (_, _, old) = compose::scene_frames(&d).unwrap();
        let transform = old[0] * Mat4::from_translation(Vec3::new(0., -0.9, 0.));
        bake_wearable(&mut d, &mut b, 1).unwrap();
        let a = &d["meshes"][0]["primitives"][0]["attributes"];
        let actual = accessor::read(&d, &b, a["POSITION"].as_u64().unwrap() as usize).unwrap();
        for (p, v) in positions.iter().zip(actual) {
            assert!(
                Vec3::from_slice(&v).distance(transform.transform_point3(Vec3::from_slice(p)))
                    < 1e-6
            );
        }
        let index = d["meshes"][0]["primitives"][0]["targets"][0]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let actual = accessor::read(&d, &b, index).unwrap();
        assert!(Vec3::from_slice(&actual[0]).distance(rotation * Vec3::X * 0.01) < 1e-6);
        assert_eq!(a["TEXCOORD_0"], json!(uv));
        let (_, _, now) = compose::scene_frames(&d).unwrap();
        let skin = d["nodes"][1]["skin"].as_u64().unwrap() as usize;
        let ib = accessor::read(
            &d,
            &b,
            d["skins"][skin]["inverseBindMatrices"].as_u64().unwrap() as usize,
        )
        .unwrap();
        assert!(
            (now[0] * Mat4::from_cols_array(ib[0].as_slice().try_into().unwrap()))
                .abs_diff_eq(Mat4::IDENTITY, 1e-6)
        );
    }

    #[test]
    fn ribbon_chain_preserves_bind_shape_and_attachment() {
        let mut d = json!({"scene":0,"scenes":[{"nodes":[0,1]}],"nodes":[{"name":"Bip01 Pelvis","translation":[0,0.9,0]},{"mesh":0,"skin":0}],"meshes":[],"skins":[],"materials":[{}],"accessors":[],"bufferViews":[],"buffers":[{"byteLength":0}],"extras":{"secretaryClothComponentSizes":[8]}});
        let mut b = Vec::new();
        let positions = vec![
            vec![0.10, 0.9, 0.16],
            vec![0.14, 0.9, 0.16],
            vec![0.10, 0.66, 0.20],
            vec![0.14, 0.66, 0.20],
            vec![0.10, 0.42, 0.24],
            vec![0.14, 0.42, 0.24],
            vec![0.10, 0.30, 0.28],
            vec![0.14, 0.30, 0.28],
        ];
        let p = accessor::floats(&mut d, &mut b, &positions, 3).unwrap();
        let j = accessor::shorts(&mut d, &mut b, &[[0; 4]; 8]);
        let w = accessor::floats(&mut d, &mut b, &vec![vec![1., 0., 0., 0.]; 8], 4).unwrap();
        let i = accessor::shorts(
            &mut d,
            &mut b,
            &[
                [0, 1, 2, 1],
                [3, 2, 2, 3],
                [4, 3, 5, 4],
                [4, 5, 6, 5],
                [7, 6, 0, 0],
            ],
        );
        d["accessors"][i]["type"] = json!("SCALAR");
        d["accessors"][i]["count"] = json!(18);
        let ib = accessor::floats(
            &mut d,
            &mut b,
            &[Mat4::from_translation(Vec3::new(0., -0.9, 0.))
                .to_cols_array()
                .to_vec()],
            16,
        )
        .unwrap();
        d["skins"] = json!([{"joints":[0],"inverseBindMatrices":ib}]);
        d["meshes"] = json!([{"primitives":[{"attributes":{"POSITION":p,"JOINTS_0":j,"WEIGHTS_0":w},"indices":i,"material":0}]}]);
        assert_eq!(
            garment(&mut d, &mut b, "hanging-accessory", "fixture").unwrap(),
            8
        );
        assert_eq!(d["skins"][0]["joints"].as_array().unwrap().len(), 4);
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
        for v in 0..positions.len() {
            let mut point = Vec3::ZERO;
            for k in 0..4 {
                let joint = j[v][k] as usize;
                let node = d["skins"][0]["joints"][joint].as_u64().unwrap() as usize;
                point += (world[node]
                    * Mat4::from_cols_array(ib[joint].as_slice().try_into().unwrap()))
                .transform_point3(Vec3::from_slice(&positions[v]))
                    * w[v][k];
            }
            assert!(point.distance(Vec3::from_slice(&positions[v])) < 1e-6);
            assert!((w[v].iter().sum::<f32>() - 1.).abs() < 1e-6);
        }
        assert_eq!(j[0][0], 0.);
        assert_eq!(w[0][0], 1.);
        assert_eq!(j[1][0], 0.);
        assert_eq!(w[1][0], 1.);
        let end = d["skins"][0]["joints"][3].as_u64().unwrap() as usize;
        let middle = d["skins"][0]["joints"][2].as_u64().unwrap() as usize;
        assert!(d["nodes"][middle]["children"]
            .as_array()
            .unwrap()
            .contains(&json!(end)));
        for joint in 1..4 {
            let node = d["skins"][0]["joints"][joint].as_u64().unwrap() as usize;
            assert!(
                d["nodes"][node]["extras"]["secretarySecondary"]["tail"][1]
                    .as_f64()
                    .unwrap()
                    < 0.
            );
        }
        d["asset"] = json!({"version":"2.0"});
        let path = std::env::temp_dir().join(format!("ribbon-axis-{}.glb", std::process::id()));
        crate::glb_io::write(&path, &d, &b).unwrap();
        crate::secondary_bindings::annotate(&path, &path).unwrap();
        let (checked, _) = crate::glb_io::read(&path).unwrap();
        let tail = &checked["nodes"][end]["extras"]["secretarySecondary"];
        assert_eq!(tail["tailSource"], "authored-chain");
        assert!(tail["tail"][1].as_f64().unwrap() < 0.);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn long_ribbon_is_soft_but_a_disconnected_bag_stays_rigid() {
        let mut d = json!({"accessors":[],"bufferViews":[],"buffers":[{"byteLength":0}]});
        let mut b = Vec::new();
        let p = accessor::floats(
            &mut d,
            &mut b,
            &[
                vec![0.10, 0.91, 0.16],
                vec![0.14, 0.90, 0.16],
                vec![0.14, 0.60, 0.22],
                vec![0.10, 0.60, 0.22],
                vec![0.10, 0.60, 0.22],
                vec![0.14, 0.60, 0.22],
                vec![0.14, 0.30, 0.28],
                vec![0.10, 0.30, 0.28],
                vec![-0.15, 0.90, 0.14],
                vec![-0.05, 0.90, 0.14],
                vec![-0.05, 0.75, 0.20],
                vec![-0.15, 0.75, 0.20],
            ],
            3,
        )
        .unwrap();
        let i = accessor::shorts(
            &mut d,
            &mut b,
            &[
                [0, 1, 2, 0],
                [2, 3, 4, 5],
                [6, 4, 6, 7],
                [8, 9, 10, 8],
                [10, 11, 0, 0],
            ],
        );
        d["accessors"][i]["type"] = json!("SCALAR");
        d["accessors"][i]["count"] = json!(18);
        let p = json!({"attributes":{"POSITION":p},"indices":i});
        let eligible = hanging_vertices(&d, &b, &p, Mat4::IDENTITY, 0.9).unwrap();
        assert_eq!(
            eligible,
            std::collections::HashSet::from([0, 1, 2, 3, 4, 5, 6, 7])
        );
    }
    #[test]
    fn skirt_rig_preserves_bind_shape_and_pins_waist() {
        let mut doc = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,1]}],"nodes":[{"name":"Bip01 Pelvis","translation":[0,0.9,0]},{"name":"skirt","mesh":0,"skin":0}],"meshes":[],"skins":[],"accessors":[],"bufferViews":[],"buffers":[{"byteLength":0}]});
        let mut data = Vec::new();
        let positions = vec![
            vec![0.1, 0.9, 0.12],
            vec![0.12, 0.68, 0.12],
            vec![-0.1, 0.6, 0.12],
        ];
        let p = accessor::floats(&mut doc, &mut data, &positions, 3).unwrap();
        let j = accessor::shorts(&mut doc, &mut data, &[[0; 4]; 3]);
        let w = accessor::floats(&mut doc, &mut data, &vec![vec![1., 0., 0., 0.]; 3], 4).unwrap();
        let ib = accessor::floats(
            &mut doc,
            &mut data,
            &[Mat4::from_translation(Vec3::new(0., -0.9, 0.))
                .to_cols_array()
                .to_vec()],
            16,
        )
        .unwrap();
        doc["skins"] = json!([{"joints":[0],"inverseBindMatrices":ib}]);
        doc["meshes"] =
            json!([{"primitives":[{"attributes":{"POSITION":p,"JOINTS_0":j,"WEIGHTS_0":w}}]}]);
        assert_eq!(garment(&mut doc, &mut data, "skirt", "test").unwrap(), 2);
        let (_, _, world) = compose::scene_frames(&doc).unwrap();
        let a = &doc["meshes"][0]["primitives"][0]["attributes"];
        let joints = accessor::read(&doc, &data, a["JOINTS_0"].as_u64().unwrap() as usize).unwrap();
        let weights =
            accessor::read(&doc, &data, a["WEIGHTS_0"].as_u64().unwrap() as usize).unwrap();
        let ib = accessor::read(
            &doc,
            &data,
            doc["skins"][0]["inverseBindMatrices"].as_u64().unwrap() as usize,
        )
        .unwrap();
        for (v, p) in positions.iter().enumerate() {
            let mut point = Vec3::ZERO;
            for k in 0..4 {
                let joint = joints[v][k] as usize;
                let node = doc["skins"][0]["joints"][joint].as_u64().unwrap() as usize;
                let inverse = Mat4::from_cols_array(ib[joint].as_slice().try_into().unwrap());
                point +=
                    (world[node] * inverse).transform_point3(Vec3::from_slice(p)) * weights[v][k];
            }
            assert!(point.distance(Vec3::from_slice(p)) < 1e-6);
            assert!((weights[v].iter().sum::<f32>() - 1.).abs() < 1e-6);
        }
        assert_eq!(joints[0][0], 0.);
        assert_eq!(weights[0][0], 1.);
        assert!(weights[2][2] + weights[2][3] > 0.69);
    }
}
