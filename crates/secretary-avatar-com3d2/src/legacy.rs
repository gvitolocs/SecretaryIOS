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
/// How far cloth stays outside the native skin.
const CLEARANCE: f32 = 0.005;
/// How far a shoe sole sits below the lowest native toe.
const SOLE: f32 = 0.012;
/// Smoothstep from `a` (0) to `b` (1); `a > b` runs downhill.
fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}
/// Connected piece of each vertex, numbered by its smallest vertex: triangles join their corners,
/// and vertices at the same place (split along a UV seam) are one.
fn pieces(points: &[Vec3], indices: &[Vec<f32>]) -> Vec<usize> {
    let count = points.len();
    let mut parent = (0..count).collect::<Vec<_>>();
    fn root(parent: &mut [usize], mut v: usize) -> usize {
        while parent[v] != v {
            parent[v] = parent[parent[v]];
            v = parent[v];
        }
        v
    }
    let join = |parent: &mut Vec<usize>, a: usize, b: usize| {
        let (a, b) = (root(parent, a), root(parent, b));
        parent[a.max(b)] = a.min(b);
    };
    for t in indices.chunks(3) {
        let ids = t.iter().map(|i| i[0] as usize).filter(|&i| i < count).collect::<Vec<_>>();
        for w in ids.windows(2) {
            join(&mut parent, w[0], w[1]);
        }
    }
    let mut seen = HashMap::new();
    for (v, p) in points.iter().enumerate() {
        let key = (*p * 1e4).round().as_ivec3();
        if let Some(&first) = seen.get(&key) {
            join(&mut parent, first, v);
        } else {
            seen.insert(key, v);
        }
    }
    (0..count).map(|v| root(&mut parent, v)).collect()
}
/// A garment vertex mapped onto the native skeleton, before the body fit.
struct Fitting {
    point: Vec3,
    normal: Vec3,
    skin: HashMap<usize, f32>,
    bust: f32,
    /// The native foot a shoe point rides on, and how fully.
    shoe: Option<(usize, f32)>,
}
/// Outward push per point that keeps stacked layers apart: each point moves as far as the deepest
/// point of the stack it sits in (along that point's body normal, within `RADIUS` sideways and
/// `DEPTH` along it), fading with the sideways distance. Snapping every penetrating point onto
/// the skin instead collapsed shirt, vest and apron onto one depth, where they z-fought.
fn layered_push(points: &[Vec3], normals: &[Vec3], need: &[f32]) -> Vec<f32> {
    const RADIUS: f32 = 0.02;
    const DEPTH: f32 = 0.04;
    let cell = |p: Vec3| (p / DEPTH).floor().as_ivec3();
    let mut grid = HashMap::<glam::IVec3, Vec<usize>>::new();
    for (j, &r) in need.iter().enumerate() {
        if r > 0. {
            grid.entry(cell(points[j])).or_default().push(j);
        }
    }
    points
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let c = cell(p);
            let mut best = need[i];
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for &j in grid.get(&(c + glam::IVec3::new(dx, dy, dz))).into_iter().flatten() {
                            let d = p - points[j];
                            let along = d.dot(normals[j]);
                            if along.abs() > DEPTH || normals[i].dot(normals[j]) < 0.5 {
                                continue;
                            }
                            let side = (d - normals[j] * along).length();
                            if side < RADIUS {
                                best = best.max(need[j] * (1. - side / RADIUS));
                            }
                        }
                    }
                }
            }
            best
        })
        .collect()
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

    // Body surfaces at the three bust sizes, each also without the left and/or right arm: a torso
    // point must not be pushed out of the hands clasped in front of it, only a sleeve may.
    let arm = |side: &str, j: usize| {
        target_nodes[j]["name"].as_str().is_some_and(|n| {
            n.contains(&format!(" {side} "))
                && ["UpperArm", "Forearm", "Hand", "Finger"]
                    .iter()
                    .any(|p| n.contains(p))
        })
    };
    let surfaces = if hair {
        None
    } else {
        let mut out = Vec::new();
        for size in [0., 0.5, 1.] {
            // The uniform keeps its reviewed bind-pose fit; everything else fits the posed body.
            let full = if preserve_shell {
                Surface::body_at_size(&reference, &base_data, size)?
            } else {
                Surface::posed_at_size(&reference, &base_data, size)?
            };
            let pick = |left: bool, right: bool| {
                full.without(|j| (!left && arm("L", j)) || (!right && arm("R", j)))
                    .ok_or_else(|| invalid("native body surface without arms is empty"))
            };
            out.push([pick(false, false)?, pick(true, false)?, pick(false, true)?, full]);
        }
        Some(out)
    };
    let surface_for = |size: usize, skin: &HashMap<usize, f32>| {
        let side = |s: &str| skin.iter().any(|(j, w)| *w > 0.05 && arm(s, *j));
        let all = surfaces.as_ref().unwrap();
        &all[size][match (side("L"), side("R")) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        }]
    };
    // Loose skirt cloth keeps the old body's pelvis frame: mapped through each thigh, a hem that
    // hung between the old, wider-set legs bent into wedges and points.
    let pelvis = target_names
        .get("Bip01 Pelvis")
        .copied()
        .and_then(|t| direct.iter().position(|d| *d == Some(t)));
    let leg = |j: usize| {
        target_nodes[j]["name"]
            .as_str()
            .is_some_and(|n| n.contains("Thigh") || n.contains("Calf"))
    };
    // Shoes ride on the native foot as one piece. Mapped bone by bone, ankle straps weighted to
    // the old calf landed under the floor and the toe box sat off the raised native foot.
    let lower = |j: usize| {
        target_nodes[j]["name"]
            .as_str()
            .is_some_and(|n| n.contains("Foot") || n.contains("Toe") || n.contains("Calf"))
    };
    let feet = ["leftFoot", "rightFoot"]
        .iter()
        .filter_map(|n| source_names.get(*n).copied())
        .filter_map(|i| direct[i].map(|t| (i, t, source_world[i].w_axis.x)))
        .collect::<Vec<_>>();
    let mut converted = Vec::new();
    let mut pending = Vec::new();
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
            let mut mapped_vertices = Vec::with_capacity(ps.len());
            for v in 0..ps.len() {
                let mut point = Vec3::ZERO;
                let mut rest = Vec3::ZERO;
                let mut normal = Vec3::ZERO;
                let mut skinweights = HashMap::<usize, f32>::new();
                let mut bust = 0.;
                let mut legs = 0.;
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
                    let bind = source_world[source]
                        * Mat4::from_cols_array(inv.as_slice().try_into().unwrap());
                    let matrix = matrices[source] * bind;
                    point += matrix.transform_point3(Vec3::from_slice(&ps[v])) * w;
                    rest += bind.transform_point3(Vec3::from_slice(&ps[v])) * w;
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
                    if leg(target) {
                        legs += w;
                    }
                    sum += w;
                }
                if sum < 0.999 {
                    return Err(invalid(format!(
                        "unmapped legacy skin influence at vertex {v}"
                    )));
                }
                point /= sum;
                rest /= sum;
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
                mapped_vertices.push((point, rest, normal, skinweights, bust, legs / sum));
            }
            // Heights (1 cm bands, front and back apart) where a connected piece of cloth crosses
            // the gap between the old legs: a skirt, apron or loincloth there. Trouser legs, boots
            // and leg drapes never cross it, even behind a loincloth.
            let rests = mapped_vertices.iter().map(|v| v.1).collect::<Vec<_>>();
            let piece = match primitive["indices"].as_u64() {
                Some(i) => pieces(&rests, &accessor::read(&doc, &data, i as usize)?),
                None => vec![0; rests.len()],
            };
            let spans = mapped_vertices
                .iter()
                .zip(&piece)
                .filter(|(v, _)| v.1.x.abs() < 0.015)
                .map(|(v, &c)| (c, (v.1.y * 100.).floor() as i32, v.1.z > 0.))
                .collect::<HashSet<_>>();
            let crosses = |c: usize, rest: Vec3| {
                let band = (rest.y * 100.).floor() as i32;
                (-1..=1).any(|d| spans.contains(&(c, band + d, rest.z > 0.)))
            };
            let mut vertices = Vec::with_capacity(mapped_vertices.len());
            for ((mut point, rest, normal, mut skinweights, bust, legs), &c) in
                mapped_vertices.into_iter().zip(&piece)
            {
                if let (Some(pelvis), true, false) = (pelvis, legs > 0., hair || preserve_shell) {
                    if crosses(c, rest) {
                        let away = surface_for(1, &skinweights).nearest(point).distance;
                        let loose = smooth(0.025, 0.06, away) * legs;
                        point = point.lerp(matrices[pelvis].transform_point3(rest), loose);
                    }
                }
                let mut shoe = None;
                let nearest_foot = feet
                    .iter()
                    .min_by(|a, b| (a.2 - rest.x).abs().total_cmp(&(b.2 - rest.x).abs()));
                if let (false, Some(&(foot, target, x))) = (hair || preserve_shell, nearest_foot) {
                    let sum = skinweights.values().sum::<f32>();
                    let low = skinweights
                        .iter()
                        .filter(|(j, _)| lower(**j))
                        .map(|(_, w)| w)
                        .sum::<f32>()
                        / sum;
                    let on = smooth(0.17, 0.12, rest.y)
                        * smooth(0.3, 0.5, low)
                        * smooth(0.1, 0.07, (rest.x - x).abs());
                    if on > 0. {
                        point = point.lerp(matrices[foot].transform_point3(rest), on);
                        for w in skinweights.values_mut() {
                            *w *= 1. - on;
                        }
                        *skinweights.entry(target).or_default() += on * sum;
                        shoe = Some((target, on));
                    }
                }
                vertices.push(Fitting {
                    point,
                    normal,
                    skin: skinweights,
                    bust,
                    shoe,
                });
            }
            pending.push((converted.len(), primitive.clone(), vertices));
        }
        out_mesh["weights"] = json!([0]);
        out_mesh["extras"]["targetNames"] = json!(["munel"]);
        converted.push(out_mesh);
    }

    // Each shoe centred on the native foot (toes included) and its sole set on the floor under
    // the toes: the old foot is longer and flatter than the raised native one.
    if let Some(all) = surfaces.as_ref() {
        for &(_, target, _) in &feet {
            let side = target_nodes[target]["name"].as_str().unwrap_or("").replace("Foot", "");
            let Some((body, floor)) = all[1][3].region(|j| {
                target_nodes[j]["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with(&side) && (n.contains("Foot") || n.contains("Toe")))
            }) else {
                continue;
            };
            let solid = pending
                .iter()
                .flat_map(|p| p.2.iter())
                .filter(|f| f.shoe.is_some_and(|(t, on)| t == target && on > 0.99))
                .map(|f| f.point)
                .collect::<Vec<_>>();
            if solid.is_empty() {
                continue;
            }
            let center = solid.iter().sum::<Vec3>() / solid.len() as f32;
            let sole = solid.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
            let shift = Vec3::new(body.x - center.x, floor - SOLE - sole, body.z - center.z)
                .clamp(Vec3::splat(-0.05), Vec3::splat(0.05));
            for f in pending.iter_mut().flat_map(|p| p.2.iter_mut()) {
                if let Some((t, on)) = f.shoe {
                    if t == target {
                        f.point += shift * on;
                    }
                }
            }
        }
    }
    // Size endpoints and clearance over the whole garment at once, so stacked layers move together.
    let all: Vec<&Fitting> = pending.iter().flat_map(|p| p.2.iter()).collect();
    let mut placed = vec![(Vec3::ZERO, Vec3::ZERO); all.len()];
    let mut skins_out = Vec::with_capacity(all.len());
    if hair {
        for (i, f) in all.iter().enumerate() {
            placed[i] = (f.point, Vec3::ZERO);
            skins_out.push(f.skin.clone());
        }
    } else if preserve_shell {
        for (i, f) in all.iter().enumerate() {
            // The uniform keeps its reviewed fit: the whole body, arms included.
            let hit = surfaces.as_ref().unwrap()[1][3].nearest(f.point);
            placed[i] = if f.bust > 0.05 && hit.distance < 0.15 {
                (f.point - hit.delta * 0.5, hit.delta)
            } else {
                (f.point, Vec3::ZERO)
            };
            skins_out.push(f.skin.clone());
        }
    } else {
        let mut ends = [Vec::new(), Vec::new()];
        let mut needs = [Vec::new(), Vec::new()];
        let mut outward = [Vec::new(), Vec::new()];
        for f in &all {
            let mid = surface_for(1, &f.skin).nearest(f.point);
            let grow = if mid.distance < 0.15 { mid.delta } else { Vec3::ZERO };
            for (e, (size, offset)) in [(0, -0.5), (2, 0.5)].into_iter().enumerate() {
                let base = f.point + grow * offset;
                let body = surface_for(size, &f.skin);
                let hit = body.nearest(base);
                let depth = (base - hit.point).dot(hit.normal);
                let facing = f.normal.normalize_or_zero();
                let near = if hit.distance < 0.15
                    && (depth > -0.03 || facing.dot(hit.normal) > 0.3)
                {
                    (CLEARANCE - depth).max(0.)
                } else {
                    0.
                };
                // Inside the larger native bust the nearest skin is often its side or underside,
                // which a push along its normal does not clear: cloth facing front or back also
                // clears the front or back envelope, whichever takes more.
                let envelope = if hit.distance >= 0.15 {
                    None
                } else if facing.z < -0.1 {
                    body.front(base)
                        .map(|h| ((base.z - (h.point.z - CLEARANCE)).clamp(0., 0.2), -Vec3::Z))
                } else if facing.z > 0.1 {
                    body.back(base)
                        .map(|h| (((h.point.z + CLEARANCE) - base.z).clamp(0., 0.2), Vec3::Z))
                } else {
                    None
                };
                let (need, out) = match envelope {
                    Some((n, o)) if n > near => (n, o),
                    _ => (near, hit.normal),
                };
                needs[e].push(need);
                ends[e].push(base);
                outward[e].push(out);
            }
            // Bust cloth takes the skin's own weights, so it moves exactly as the breast physics
            // moves the skin under it; a partial share left the sides of the bust behind and the
            // skin showed through. Fades out away from the body.
            let native_bust = mid
                .skin
                .iter()
                .filter(|(j, _)| {
                    target_nodes[*j]["name"]
                        .as_str()
                        .is_some_and(|n| n.starts_with("Mune_"))
                })
                .map(|(_, w)| w)
                .sum::<f32>();
            let mix = smooth(0.005, 0.05, native_bust.max(f.bust)) * smooth(0.15, 0.08, mid.distance);
            let mut skin = f.skin.clone();
            if mix > 0. {
                for w in skin.values_mut() {
                    *w *= 1. - mix;
                }
                for (j, w) in mid.skin {
                    *skin.entry(j).or_default() += w * mix;
                }
                fitted += 1;
            }
            skins_out.push(skin);
        }
        let pushes = [0, 1].map(|e| layered_push(&ends[e], &outward[e], &needs[e]));
        for i in 0..all.len() {
            let low = ends[0][i] + outward[0][i] * pushes[0][i];
            let high = ends[1][i] + outward[1][i] * pushes[1][i];
            if pushes[0][i] > 0. || pushes[1][i] > 0. {
                fitted += 1;
            }
            placed[i] = (low, high - low);
        }
    }
    let mut next = 0;
    for (mesh, primitive, vertices) in pending {
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut indices = Vec::new();
        let mut weights = Vec::new();
        let mut size_delta = Vec::new();
        for f in &vertices {
            let (point, delta) = placed[next];
            let packed = compact_weights(skins_out[next].clone());
            next += 1;
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
            let (point, normal, delta) = encode_reference(point, f.normal, delta, deformation)?;
            positions.push(point.to_array().to_vec());
            normals.push(normal.normalize_or_zero().to_array().to_vec());
            indices.push(ids);
            weights.push(values);
            size_delta.push(delta.to_array().to_vec());
            total += 1;
        }
        let mut p = primitive;
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
        converted[mesh]["primitives"].as_array_mut().unwrap().push(p);
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
    fn pieces_join_triangles_and_seam_copies_but_not_separate_cloth() {
        // Triangle 0-1-2; vertex 3 is a UV-seam copy of vertex 2 in triangle 3-4-5; 6-7-8 apart.
        let points = [
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            Vec3::Y,
            Vec3::new(1., 1., 0.),
            Vec3::new(0., 2., 0.),
            Vec3::new(5., 0., 0.),
            Vec3::new(6., 0., 0.),
            Vec3::new(5., 1., 0.),
        ];
        let indices = [0, 1, 2, 3, 4, 5, 6, 7, 8].map(|i| vec![i as f32]);
        assert_eq!(pieces(&points, &indices), [0, 0, 0, 0, 0, 0, 6, 6, 6]);
    }
    #[test]
    fn stacked_layers_move_out_together_and_keep_their_spacing() {
        // An inner layer 1 cm inside the skin, an outer one 0.5 cm outside it over the same spot,
        // a point 1.5 cm to the side, one 5 cm out and one on the far side (normal reversed).
        let points = [
            Vec3::new(0., 1., -0.01),
            Vec3::new(0., 1., 0.005),
            Vec3::new(0.015, 1., 0.005),
            Vec3::new(0., 1., 0.05),
            Vec3::new(0., 1., -0.012),
        ];
        let normals = [Vec3::Z, Vec3::Z, Vec3::Z, Vec3::Z, -Vec3::Z];
        let need = [0.015, 0., 0., 0., 0.];
        let push = layered_push(&points, &normals, &need);
        assert!((push[0] - 0.015).abs() < 1e-6);
        // The outer layer moves as far as the inner one, so they stay 1.5 cm apart.
        assert!((push[1] - 0.015).abs() < 1e-6);
        assert!(push[2] > 0. && push[2] < 0.005);
        assert_eq!(push[3], 0.);
        assert_eq!(push[4], 0.);
        assert!((smooth(0.15, 0.08, 0.08) - 1.).abs() < 1e-6 && smooth(0.15, 0.08, 0.2) == 0.);
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
