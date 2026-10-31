//! Retarget the existing fitted wardrobe onto the native body. No legacy body,
//! head or synthetic body-fill mesh is copied into the canonical avatar.
use crate::{accessor, compose::scene_frames, glb_io, invalid, surface::Surface, Result};
use glam::{Mat3, Mat4, Vec3};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FitProfile {
    pub chest_vertical_scale: f32,
    pub chest_offset_y: f32,
    #[serde(default)]
    pub waist_offset_y: f32,
}
impl FitProfile {
    fn validate(&self) -> Result<()> {
        if !self.chest_vertical_scale.is_finite()
            || !(0.75..=2.).contains(&self.chest_vertical_scale)
            || !self.chest_offset_y.is_finite()
            || self.chest_offset_y.abs() > 0.06
            || !self.waist_offset_y.is_finite()
            || self.waist_offset_y.abs() > 0.15
        {
            return Err(invalid("garment fit profile range"));
        }
        Ok(())
    }
    fn point(&self, p: Vec3, n: Vec3, center: f32, waist: f32, anchor_weight: f32) -> Vec3 {
        let smooth = |a: f32, b: f32, x: f32| {
            let t = ((x - a) / (b - a)).clamp(0., 1.);
            t * t * (3. - 2. * t)
        };
        let p = if p.x.abs() < 0.28 {
            p + Vec3::Y
                * self.waist_offset_y
                * anchor_weight
                * smooth(waist - 0.55, waist - 0.4, p.y)
                * (1. - smooth(waist + 0.05, waist + 0.15, p.y))
        } else {
            p
        };
        if n.normalize_or_zero().z >= -0.2 || p.z >= 0.02 || p.x.abs() >= 0.24 {
            return p;
        }
        let t = (1. - ((p.y - center).abs() / 0.17)).clamp(0., 1.);
        let weight = t * t * (3. - 2. * t);
        p + Vec3::Y
            * ((p.y - center) * (self.chest_vertical_scale - 1.) + self.chest_offset_y)
            * weight
    }
}
fn semantic(name: &str) -> String {
    if name == "ctr_spine_upper" {
        return "chest".into();
    }
    if name == "lowerBody" {
        return "hips".into();
    }
    for side in ["left", "right"] {
        for (pattern, target) in [
            (format!("leg_{side}_knee_ctr"), "LowerLeg"),
            (format!("leg_{side}_thigh_ctr"), "UpperLeg"),
            (format!("arm_{side}_shoulder_2_ctr"), "UpperArm"),
            (format!("arm_{side}_elbow_ctr"), "LowerArm"),
        ] {
            if name == pattern || name == format!("{pattern}_2") {
                return format!("{side}{target}");
            }
        }
        if name == format!("collar_{side}") {
            return format!("{side}Shoulder");
        }
    }
    name.into()
}
fn alias(name: &str) -> String {
    let normalized = semantic(name);
    let name = normalized.as_str();

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
fn clearance(point: Vec3, normal: Vec3, hit: &crate::surface::Hit) -> Vec3 {
    // Push only penetrating, similarly oriented torso cloth out along the body
    // normal. Preserve tangent coordinates instead of collapsing onto a nearest
    // point, and leave loose cloth, open necklines and accessories in place.
    if !(0.95..1.42).contains(&point.y)
        || point.x.abs() > 0.24
        || hit.distance > 0.08
        || normal.normalize_or_zero().dot(hit.normal) < 0.25
    {
        return point;
    }
    let depth = (point - hit.point).dot(hit.normal);
    if depth < 0.006 {
        point + hit.normal * (0.006 - depth)
    } else {
        point
    }
}
pub fn retarget(input: &Path, canonical: &Path, output: &Path, hair: bool) -> Result<Value> {
    retarget_with_fit(input, canonical, output, hair, None)
}
pub fn retarget_with_fit(
    input: &Path,
    canonical: &Path,
    output: &Path,
    hair: bool,
    profile: Option<&FitProfile>,
) -> Result<Value> {
    retarget_reference(input, canonical, None, output, hair, profile, false)
}
/// Fit in an explicit posed reference and encode the result through the inverse
/// blended target skin. Blending bind-pose fits then posing again introduces
/// cross terms at multi-weight knees, gloves and the hem.
pub fn retarget_reference(
    input: &Path,
    canonical: &Path,
    reference: Option<&Path>,
    output: &Path,
    hair: bool,
    profile: Option<&FitProfile>,
    preserve_shell: bool,
) -> Result<Value> {
    if let Some(p) = profile {
        p.validate()?;
        if hair {
            return Err(invalid("torso fitting cannot apply to hair"));
        }
    }
    let (mut doc, mut data) = glb_io::read(input)?;
    let (base, base_data) = glb_io::read(canonical)?;
    let (parents, _, source_world) = scene_frames(&doc)?;
    let (_, _, mut target_world) = scene_frames(&base)?;
    let reference = match reference {
        Some(path) => glb_io::read(path)?.0,
        None => base.clone(),
    };
    if reference["nodes"].as_array().map(Vec::len) != base["nodes"].as_array().map(Vec::len)
        || reference["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .zip(base["nodes"].as_array().unwrap())
            .any(|(a, b)| a["name"] != b["name"])
    {
        return Err(invalid("reference pose must share canonical node order"));
    }
    let (_, _, mut reference_world) = scene_frames(&reference)?;
    let source_nodes = doc["nodes"].as_array().unwrap().clone();

    let mut target_nodes = base["nodes"].as_array().unwrap().clone();
    let source_names = source_nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n["name"].as_str().map(|n| (n.to_owned(), i)))
        .collect::<HashMap<_, _>>();
    let mut target_names = target_nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n["name"].as_str().map(|n| (n.to_owned(), i)))
        .collect::<HashMap<_, _>>();
    let chest_center = ["Mune_L_sub", "Mune_R_sub"]
        .iter()
        .filter_map(|n| target_names.get(*n))
        .map(|&i| reference_world[i].w_axis.y)
        .sum::<f32>()
        / 2.;
    let waist_center = target_names
        .get("Bip01 Spine")
        .map(|&i| reference_world[i].w_axis.y)
        .unwrap_or(chest_center - 0.28);
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
    let mut joint_index = target_joints
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
            let normalized = semantic(name);
            let reference = source_names.get(&normalized).copied().unwrap_or(i);
            let from = rotation.transform_point3(source_world[reference].w_axis.truncate());
            let to = reference_world[t].w_axis.truncate();
            matrices[i] = if let Some(next) = next(&normalized)
                .and_then(|n| source_names.get(&n).copied())
                .filter(|&j| direct[j].is_some())
            {
                segment(
                    from,
                    rotation.transform_point3(source_world[next].w_axis.truncate()),
                    to,
                    reference_world[direct[next].unwrap()].w_axis.truncate(),
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
    // Keep the original cape/belt controls as extra native-compatible joints.
    // Their old ancestor transform still bakes the garment vertices into the
    // target body frame; the new joints retain independent downstream motion.
    let mut preserved = Vec::new();
    if !hair {
        for (source, node) in source_nodes.iter().enumerate() {
            let name = node["name"].as_str().unwrap_or("");
            let cloth = name.contains("sprmnt")
                || name.starts_with("mant_")
                || name == "c_mngmnt"
                || name.contains("sprblt")
                || name.contains("mngblt");
            if !cloth || direct[source].is_some() {
                continue;
            }
            let target = target_nodes.len();
            let ancestor = mapped[source].ok_or_else(|| invalid("cloth ancestor"))?;
            let deformation = reference_world[ancestor] * target_world[ancestor].inverse();
            let global = deformation.inverse() * matrices[source] * source_world[source];

            let (_, rotation, position) = global.to_scale_rotation_translation();
            target_world.push(Mat4::from_rotation_translation(
                rotation.normalize(),
                position,
            ));
            reference_world.push(deformation * target_world[target]);
            target_nodes.push(json!({"name":name}));
            target_names.insert(name.to_owned(), target);
            mapped[source] = Some(target);
            joint_index.insert(target, target_joints.len());
            target_joints.push(target);
            preserved.push((source, target));
        }
    }
    let mut attachments = Vec::new();
    for (source, target) in &preserved {
        let p = parents[*source];
        let parent = if p >= 0 { mapped[p as usize] } else { None }
            .or_else(|| target_names.get("Bip01 Spine1a").copied())
            .ok_or_else(|| invalid("secondary garment attachment"))?;
        let local = target_world[parent].inverse() * target_world[*target];
        target_nodes[*target] = json!({"name":source_nodes[*source]["name"],"matrix":local.to_cols_array(),"extras":{"secretarySecondary":{"source":"preserved-original-cloth-control"}}});
        attachments.push((parent, *target));
    }
    for (parent, child) in attachments {
        if !target_nodes[parent]["children"].is_array() {
            target_nodes[parent]["children"] = json!([]);
        }
        target_nodes[parent]["children"]
            .as_array_mut()
            .unwrap()
            .push(json!(child));
    }

    let surface = if hair {
        None
    } else {
        Some(Surface::body_at_size(&reference, &base_data, 0.5)?)
    };
    let endpoints = if hair {
        None
    } else {
        Some((
            Surface::body_at_size(&reference, &base_data, 0.)?,
            Surface::body_at_size(&reference, &base_data, 1.)?,
        ))
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
                if let Some(p) = profile {
                    let anchor = skinweights
                        .iter()
                        .filter(|(j, _)| {
                            target_nodes[**j]["name"]
                                .as_str()
                                .is_some_and(|n| n == "Bip01 Pelvis")
                        })
                        .map(|(_, w)| w)
                        .sum::<f32>()
                        .clamp(0., 1.);
                    point = p.point(point, normal, chest_center, waist_center, anchor);
                }
                let mut delta = Vec3::ZERO;
                if let Some(surface) = &surface {
                    if preserve_shell {
                        let hit = surface.nearest(point);
                        if bust > 0.05 && hit.distance < 0.15 {
                            delta = hit.delta;
                            point -= delta * 0.5;
                        }
                    } else {
                        let front = if (0.70..1.42).contains(&point.y)
                            && point.x.abs() < 0.24
                            && normal.normalize_or_zero().z < -0.2
                            && point.z < 0.02
                        {
                            surface.front(point)
                        } else {
                            None
                        };
                        let rear = if (0.70..1.18).contains(&point.y)
                            && point.x.abs() < 0.24
                            && normal.normalize_or_zero().z > 0.2
                            && point.z > -0.02
                        {
                            surface.back(point)
                        } else {
                            None
                        };
                        let front = front.filter(|h| h.distance < 0.12);
                        let fitted_front = front.is_some();
                        let rear = rear.filter(|h| h.distance < 0.15);
                        let fitted_rear = rear.is_some();
                        let hit = if let Some(hit) = front {
                            point.z = point.z.min(hit.point.z - 0.012);
                            hit
                        } else if let Some(hit) = rear {
                            point.z = point.z.max(hit.point.z + 0.012);
                            hit
                        } else {
                            let initial = surface.nearest(point);
                            point = clearance(point, normal, &initial);
                            surface.nearest(point)
                        };
                        let close = hit.distance < 0.15;
                        if close {
                            delta = hit.delta;
                            let native_bust = hit
                                .skin
                                .iter()
                                .filter(|(j, _)| {
                                    target_nodes[*j]["name"]
                                        .as_str()
                                        .is_some_and(|n| n.starts_with("Mune_"))
                                })
                                .map(|(_, w)| w)
                                .sum::<f32>();
                            if bust > 0.05
                                || native_bust > 0.05
                                || (fitted_rear && hit.distance < 0.07)
                            {
                                let mix = if fitted_front || fitted_rear {
                                    1.
                                } else {
                                    (native_bust.max(bust) / 0.3).min(1.)
                                };
                                for w in skinweights.values_mut() {
                                    *w *= 1. - mix;
                                }
                                for (j, w) in hit.skin {
                                    *skinweights.entry(j).or_default() += w * mix;
                                }
                                fitted += 1;
                            }
                        }
                        // Calibrate both slider endpoints against the corresponding body,
                        // rather than extrapolating a closest point sampled only at 50%.
                        // A large size can expose a different part of the curved surface.
                        if fitted_front || fitted_rear {
                            let (low, high) = endpoints.as_ref().unwrap();
                            let endpoint = |size: f32, surface: &Surface| {
                                let mut q = point + delta * (size - 0.5);
                                if let Some(h) = if fitted_front {
                                    surface.front(q)
                                } else {
                                    surface.back(q)
                                } {
                                    q.z = if fitted_front {
                                        q.z.min(h.point.z - 0.012)
                                    } else {
                                        q.z.max(h.point.z + 0.012)
                                    };
                                }
                                q
                            };
                            let a = endpoint(0., low);
                            let b = endpoint(1., high);
                            point = a;
                            delta = b - a;
                        } else {
                            point -= delta * 0.5;
                        }
                    }
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
                let deformation = packed.iter().fold(Mat4::ZERO, |m, (j, w)| {
                    m + (reference_world[*j] * target_world[*j].inverse()) * *w
                });
                let encoded = encode_reference(point, normal, delta, deformation)?;
                point = encoded.0;
                normal = encoded.1;
                delta = encoded.2;
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
    doc["extras"]["nativeRetarget"] = json!({"version":2,"poseAwareSkinEncoding":true,"source":input.file_name().and_then(|n|n.to_str()),"mappedJoints":direct.iter().filter(|j|j.is_some()).count(),"vertices":total,"surfaceFittedVertices":fitted,"unmappedInfluences":unmapped,"canonicalBodyCopied":false,"fitReviewRequired":true});
    if preserve_shell {
        crate::garment_fit::uniform(&mut doc, &mut data, &target_world, &reference_world)?;
    }
    glb_io::write(output, &doc, &data)?;
    Ok(
        json!({"output":output,"vertices":total,"fitted":fitted,"unmapped":unmapped,"meshes":converted.len()}),
    )
}

fn encode_reference(
    point: Vec3,
    normal: Vec3,
    delta: Vec3,
    deformation: Mat4,
) -> Result<(Vec3, Vec3, Vec3)> {
    if !deformation.is_finite() || deformation.determinant().abs() < 1e-7 {
        return Err(invalid("singular blended reference skin"));
    }
    let inverse = deformation.inverse();
    Ok((
        inverse.transform_point3(point),
        deformation
            .transpose()
            .transform_vector3(normal)
            .normalize_or_zero(),
        inverse.transform_vector3(delta),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn garment_profile_is_bounded_local_and_keeps_an_ordered_vertical_surface() {
        let p = FitProfile {
            chest_vertical_scale: 1.65,
            chest_offset_y: -0.025,
            waist_offset_y: 0.,
        };
        p.validate().unwrap();
        assert_eq!(
            p.point(Vec3::new(0.3, 1.28, -0.1), -Vec3::Z, 1.28, 1., 0.),
            Vec3::new(0.3, 1.28, -0.1)
        );
        assert_eq!(
            p.point(Vec3::new(0., 1.7, -0.1), -Vec3::Z, 1.28, 1., 0.),
            Vec3::new(0., 1.7, -0.1)
        );
        let mut last = f32::NEG_INFINITY;
        for i in 0..100 {
            let y = 1.1 + i as f32 * 0.004;
            let fitted = p.point(Vec3::new(0.07, y, -0.1), -Vec3::Z, 1.28, 1., 0.).y;
            assert!(fitted > last);
            last = fitted;
        }
        assert!(FitProfile {
            chest_vertical_scale: f32::NAN,
            chest_offset_y: 0.,
            waist_offset_y: 0.,
        }
        .validate()
        .is_err());
    }
    #[test]
    fn waist_profile_moves_pelvis_cloth_without_moving_leg_or_hand_attachments() {
        let p = FitProfile {
            chest_vertical_scale: 1.,
            chest_offset_y: 0.,
            waist_offset_y: 0.1,
        };
        let v = Vec3::new(0.07, 0.85, 0.1);
        assert!((p.point(v, Vec3::Z, 1.28, 1., 1.).y - 0.95).abs() < 1e-6);
        assert_eq!(p.point(v, Vec3::Z, 1.28, 1., 0.), v);
        assert_eq!(
            p.point(Vec3::new(0.4, 0.85, 0.1), Vec3::Z, 1.28, 1., 1.),
            Vec3::new(0.4, 0.85, 0.1)
        );
    }
    #[test]
    fn penetrating_cloth_moves_outward_without_collapsing_tangent_coordinates() {
        let h = crate::surface::Hit {
            point: Vec3::new(0., 1.2, -0.12),
            normal: -Vec3::Z,
            delta: Vec3::ZERO,
            skin: vec![],
            distance: 0.03,
        };
        let p = Vec3::new(0.02, 1.21, -0.1);
        let q = clearance(p, -Vec3::Z, &h);
        assert!((q.x - p.x).abs() < 1e-6 && (q.y - p.y).abs() < 1e-6);
        assert!((q.z + 0.126).abs() < 1e-6);
        assert_eq!(clearance(p, Vec3::Z, &h), p);
        assert_eq!(
            clearance(Vec3::new(0., 0.8, -0.1), -Vec3::Z, &h),
            Vec3::new(0., 0.8, -0.1)
        );
    }
}
#[cfg(test)]
mod reference_tests {
    use super::*;
    use glam::Quat;
    #[test]
    fn inverse_blended_skin_preserves_reference_surface_normal_and_morph() {
        let a =
            Mat4::from_rotation_translation(Quat::from_rotation_x(1.2), Vec3::new(0.2, 0.3, 0.1));
        let b = Mat4::from_rotation_translation(
            Quat::from_rotation_y(-0.8),
            Vec3::new(-0.2, 0.1, -0.1),
        );
        let blend = a * 0.7 + b * 0.3;
        let desired = Vec3::new(0.15, 0.6, 0.3);
        let normal = Vec3::new(0.2, 0.3, 0.9).normalize();
        let delta = Vec3::new(0.03, -0.01, 0.02);
        let (p, n, d) = encode_reference(desired, normal, delta, blend).unwrap();
        assert!(blend.transform_point3(p).distance(desired) < 1e-6);
        assert!(blend.transform_point3(p + d).distance(desired + delta) < 1e-6);
        assert!(
            blend
                .inverse()
                .transpose()
                .transform_vector3(n)
                .normalize()
                .dot(normal)
                > 0.99999
        );
        let naive = a.inverse().transform_point3(desired) * 0.7
            + b.inverse().transform_point3(desired) * 0.3;
        assert!(
            blend.transform_point3(naive).distance(desired) > 0.05,
            "fixture must expose the old cross-term distortion"
        );
        assert!(encode_reference(desired, normal, delta, Mat4::ZERO).is_err());
    }
}
