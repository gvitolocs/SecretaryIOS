//! Bind the authored native skirt lattice and retain the source .psk profile.
use crate::{compose, glb_io, invalid, physics::SkirtPhysics, resources::ResourceSource, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::Path,
};
fn coordinate(name: &str) -> Option<(usize, usize)> {
    let name = name.to_lowercase();
    let name = name.strip_prefix("skirt")?;
    let mut parts = name.split('_');
    let column = parts.next()?.parse::<usize>().ok()?.checked_sub(1)?;
    let letters = parts.next()?;
    if !name.contains("_yure_skirt_") || letters.is_empty() {
        return None;
    }
    let mut row = 0usize;
    for c in letters.bytes() {
        if !c.is_ascii_lowercase() {
            return None;
        }
        row = row.checked_mul(26)?.checked_add((c - b'a' + 1) as usize)?;
    }
    Some((column, row.checked_sub(1)?))
}
pub fn annotate_file(
    resources: &mut impl ResourceSource,
    source: &str,
    path: &Path,
) -> Result<usize> {
    let (mut doc, data) = glb_io::read(path)?;
    if doc["extras"]["nativeClothRevision"] == 2 {
        return Ok(doc["extras"]["nativeClothNodes"].as_u64().unwrap_or(0) as usize);
    }
    let mut indices = BTreeMap::new();
    for (i, n) in doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("native cloth nodes"))?
        .iter()
        .enumerate()
    {
        if let Some((column, row)) = n["name"].as_str().and_then(coordinate) {
            if indices.insert((column, row), i).is_some() {
                return Err(invalid("duplicate native cloth coordinate"));
            }
        }
    }
    if indices.is_empty() {
        return Ok(0);
    }
    let columns = indices.keys().map(|v| v.0 + 1).max().unwrap();
    let rows = indices.keys().map(|v| v.1 + 1).max().unwrap();
    if !(3..=24).contains(&columns) || !(2..=48).contains(&rows) || indices.len() != columns * rows
    {
        return Err(invalid("incomplete native cloth lattice"));
    }
    let stem = Path::new(source)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("native cloth source"))?;
    let own = format!("{stem}.psk");
    let has_own = resources.paths().any(|p| {
        p.rsplit('/')
            .next()
            .is_some_and(|n| n.eq_ignore_ascii_case(&own))
    });
    let profile_path =
        resources.resolve(if has_own { &own } else { "default_skirt.psk" }, source)?;
    let profile = SkirtPhysics::read(&resources.read_resource(&profile_path)?)?;
    let (parents, _, world) = compose::scene_frames(&doc)?;
    if let Some(i) = doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .position(|n| n["name"] == "Skirt")
    {
        let parent = if parents[i] >= 0 {
            world[parents[i] as usize]
        } else {
            glam::Mat4::IDENTITY
        };
        doc["nodes"][i]["extras"]["secretaryShapeSource"] =
            json!({"world":world[i].to_cols_array(),"parentWorld":parent.to_cols_array()});
    }

    let mut lengths = vec![0f32; indices.len()];
    for row in 1..rows {
        for col in 0..columns {
            let i = row * columns + col;
            lengths[i] = lengths[i - columns]
                + world[indices[&(col, row)]]
                    .w_axis
                    .truncate()
                    .distance(world[indices[&(col, row - 1)]].w_axis.truncate());
        }
    }
    let damping = -profile.velocity_retention.clamp(0.001, 0.9999).ln() * 60.;
    let parameters = [
        profile.iterations.min(32) as f32,
        damping,
        0.000001 / profile.connection_strength[1].max(0.0001),
        0.000001 / profile.connection_strength[3].max(0.0001),
        0.002,
        (profile.panier_force * 10.).clamp(0., 20.),
        (profile.panier_radius * 0.3).clamp(0.002, 0.02),
        -0.03,
    ];
    let id = format!("native-{stem}");
    for ((column, row), node) in &indices {
        let gravity = profile
            .gravity_distribution
            .sample(lengths[row * columns + column])
            .clamp(0., 4.);
        doc["nodes"][*node]["extras"]["secretarySecondary"] = json!({"kind":"cloth","grid":{"id":id,"column":column,"row":row,"columns":columns,"rows":rows,"closed":true,"parameters":parameters,"gravityWeight":gravity,"source":"native-psk"}});
    }
    doc["extras"]["nativeClothRevision"] = json!(2);
    doc["extras"]["nativeClothNodes"] = json!(indices.len());
    doc["extras"]["nativeSkirtPhysics"] = json!({"resource":profile_path,"parameters":profile,"solver":"Rust XPBD; source units retained, world gravity 9.81 m/s²","nativeSolverParity":false});
    glb_io::write(path, &doc, &data)?;
    Ok(indices.len())
}
/// A small upper-back contact proxy from the actual worn surface. This keeps
/// long hair outside a collar while leaving front locks and the face alone.
pub fn add_back_cage(path: &Path) -> Result<usize> {
    let (mut doc, data) = glb_io::read(path)?;
    let (_, _, world) = compose::scene_frames(&doc)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let find = |name: &str| {
        nodes.iter().position(|n| {
            n["name"]
                .as_str()
                .is_some_and(|n| n.replace('_', " ").eq_ignore_ascii_case(name))
        })
    };
    let (Some(spine), Some(neck)) = (find("Bip01 Spine1a"), find("Bip01 Neck")) else {
        return Ok(0);
    };
    let a = world[spine].w_axis.truncate();
    let b = world[neck].w_axis.truncate();
    let mut back = Vec::new();
    for n in &nodes {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let primitives = doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .unwrap();
        if primitives.iter().any(|p| {
            doc["materials"][p["material"].as_u64().unwrap_or(0) as usize]["extras"]
                ["secretarySkin"]["version"]
                == 2
        }) {
            continue;
        }
        let s = &doc["skins"][skin as usize];
        let ib = crate::accessor::read(
            &doc,
            &data,
            s["inverseBindMatrices"].as_u64().unwrap() as usize,
        )?;
        let transforms = s["joints"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, j)| {
                world[j.as_u64().unwrap() as usize]
                    * glam::Mat4::from_cols_array(ib[i].as_slice().try_into().unwrap())
            })
            .collect::<Vec<_>>();
        for p in primitives {
            let attrs = &p["attributes"];
            let pos =
                crate::accessor::read(&doc, &data, attrs["POSITION"].as_u64().unwrap() as usize)?;
            let j =
                crate::accessor::read(&doc, &data, attrs["JOINTS_0"].as_u64().unwrap() as usize)?;
            let w =
                crate::accessor::read(&doc, &data, attrs["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let used = crate::accessor::read(&doc, &data, p["indices"].as_u64().unwrap() as usize)?
                .into_iter()
                .map(|v| v[0] as usize)
                .collect::<std::collections::HashSet<_>>();
            for i in used {
                let mut point = glam::Vec3::ZERO;
                for k in 0..4 {
                    point += transforms[j[i][k] as usize]
                        .transform_point3(glam::Vec3::from_slice(&pos[i]))
                        * w[i][k];
                }
                if point.y >= a.y - 0.025
                    && point.y <= b.y + 0.025
                    && (point.x - b.x).abs() < 0.09
                    && point.z > a.z + 0.025
                {
                    back.push(point.z);
                }
            }
        }
    }
    for n in doc["nodes"].as_array_mut().unwrap() {
        if let Some(extra) = n["extras"].as_object_mut() {
            extra.remove("secretaryOutfitCapsules");
        }
    }
    if back.len() < 10 {
        return Ok(0);
    }
    back.sort_by(f32::total_cmp);
    let surface = back[(back.len() * 9 / 10).min(back.len() - 1)];
    let radius = 0.035;
    let z = surface + 0.012 - radius;
    let inverse = world[spine].inverse();
    let local_a = inverse.transform_point3(glam::Vec3::new(a.x, a.y - 0.015, z));
    let local_b = inverse.transform_point3(glam::Vec3::new(b.x, b.y + 0.015, z));
    doc["nodes"][spine]["extras"]["secretaryOutfitCapsules"] = json!([{"a":local_a.to_array(),"b":local_b.to_array(),"radius":radius,"source":"worn-upper-back-surface"}]);
    glb_io::write(path, &doc, &data)?;
    Ok(1)
}

/// Skinned bind-pose points of the body skin (materials tagged `secretarySkin` v2).
fn body_skin_points(doc: &Value, data: &[u8], world: &[glam::Mat4]) -> Result<Vec<glam::Vec3>> {
    let mut out = Vec::new();
    for n in doc["nodes"].as_array().unwrap() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let s = &doc["skins"][skin as usize];
        let ib = crate::accessor::read(doc, data, s["inverseBindMatrices"].as_u64().unwrap() as usize)?;
        let transforms = s["joints"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, j)| world[j.as_u64().unwrap() as usize] * glam::Mat4::from_cols_array(ib[i].as_slice().try_into().unwrap()))
            .collect::<Vec<_>>();
        for p in doc["meshes"][mesh as usize]["primitives"].as_array().unwrap() {
            let skin_material = doc["materials"][p["material"].as_u64().unwrap_or(0) as usize]["extras"]["secretarySkin"]["version"] == 2;
            if !skin_material {
                continue;
            }
            let attrs = &p["attributes"];
            let pos = crate::accessor::read(doc, data, attrs["POSITION"].as_u64().unwrap() as usize)?;
            let j = crate::accessor::read(doc, data, attrs["JOINTS_0"].as_u64().unwrap() as usize)?;
            let w = crate::accessor::read(doc, data, attrs["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let used = crate::accessor::read(doc, data, p["indices"].as_u64().unwrap() as usize)?
                .into_iter()
                .map(|v| v[0] as usize)
                .collect::<std::collections::BTreeSet<_>>();
            for i in used {
                let mut point = glam::Vec3::ZERO;
                for k in 0..4 {
                    point += transforms[j[i][k] as usize].transform_point3(glam::Vec3::from_slice(&pos[i])) * w[i][k];
                }
                out.push(point);
            }
        }
    }
    Ok(out)
}

/// Contact capsules for skirts at the hips, fitted to the body skin and anchored to `Hip_L` /
/// `Hip_R`, per side: one across the buttock and one around the upper thigh. The hip size control
/// (`koshi`) scales those bones, so the capsules move and grow with it (`scaled`: the page
/// multiplies the radius by the bone's scale). Without them only the pelvis and the thin fixed
/// thigh capsules held skirts up, and cloth fell through the buttocks and upper thighs.
/// Returns the capsules added (0 or 4); replaces earlier hip capsules on those bones.
pub fn add_hip_cage(path: &Path) -> Result<usize> {
    let (mut doc, data) = glb_io::read(path)?;
    let (_, _, world) = compose::scene_frames(&doc)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let find = |name: &str| nodes.iter().position(|n| n["name"].as_str() == Some(name));
    let (Some(pelvis), Some(left), Some(right)) = (find("Bip01 Pelvis"), find("Hip_L"), find("Hip_R")) else {
        return Ok(0);
    };
    let points = body_skin_points(&doc, &data, &world)?;
    let centre = world[pelvis].w_axis.truncate();
    let mut capsules = Vec::new();
    for (hip, thigh, knee) in [(left, "Bip01 L Thigh", "Bip01 L Calf"), (right, "Bip01 R Thigh", "Bip01 R Calf")] {
        let (Some(thigh), Some(knee)) = (find(thigh), find(knee)) else { return Ok(0) };
        let h = world[hip].w_axis.truncate();
        let side = (h.x - centre.x).signum();
        let (Some(buttock), Some(upper)) = (
            hip_capsule(&points, centre, h, side),
            thigh_capsule(&points, centre, world[thigh].w_axis.truncate(), world[knee].w_axis.truncate(), side),
        ) else {
            return Ok(0);
        };
        capsules.push((hip, [buttock, upper]));
    }
    for (hip, fitted) in capsules {
        let inverse = world[hip].inverse();
        let entries = fitted.iter().map(|&(a, b, radius)| {
            json!({"a": inverse.transform_point3(a).to_array(), "b": inverse.transform_point3(b).to_array(),
                "radius": radius, "scaled": true, "source": "worn-hip-surface"})
        });
        let extras = &mut doc["nodes"][hip]["extras"];
        if !extras["secretaryOutfitCapsules"].is_array() {
            extras["secretaryOutfitCapsules"] = json!([]);
        }
        let list = extras["secretaryOutfitCapsules"].as_array_mut().unwrap();
        list.retain(|c| c["source"] != "worn-hip-surface");
        list.extend(entries);
    }
    glb_io::write(path, &doc, &data)?;
    Ok(4)
}

/// Close-fitting garment points at the back and sides of the hips take the skin weights of the body
/// right under them, so they follow the hip size control (`koshi` scales `Hip_L` / `Hip_R`, which
/// garments are rarely weighted to) instead of the body growing through them. Only points within
/// `REACH` of the body skin whose weights are all on body joints change: cloth-grid and other
/// garment bones keep their own weights. Missing joints are appended to the garment's skin with
/// their bind inverse. Returns the number of points re-weighted.
pub fn follow_body_at_hips(path: &Path) -> Result<usize> {
    const REACH: f32 = 0.05;
    let (mut doc, mut data) = glb_io::read(path)?;
    let (_, _, world) = compose::scene_frames(&doc)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let find = |name: &str| nodes.iter().position(|n| n["name"].as_str() == Some(name));
    let (Some(pelvis), Some(left), Some(right)) = (find("Bip01 Pelvis"), find("Hip_L"), find("Hip_R")) else {
        return Ok(0);
    };
    let body_meshes: std::collections::BTreeSet<u64> = (0..doc["meshes"].as_array().map_or(0, Vec::len) as u64)
        .filter(|&m| {
            doc["meshes"][m as usize]["primitives"].as_array().into_iter().flatten().any(|p| {
                doc["materials"][p["material"].as_u64().unwrap_or(0) as usize]["name"].as_str().is_some_and(|n| n.starts_with("skin_"))
            })
        })
        .collect();
    let is_body_mesh = |mesh: u64| body_meshes.contains(&mesh);
    let mut body_joints = std::collections::BTreeSet::new();
    for n in &nodes {
        if let (Some(m), Some(s)) = (n["mesh"].as_u64(), n["skin"].as_u64()) {
            if is_body_mesh(m) {
                body_joints.extend(doc["skins"][s as usize]["joints"].as_array().unwrap().iter().filter_map(Value::as_u64).map(|j| j as usize));
            }
        }
    }
    if body_joints.is_empty() {
        return Ok(0);
    }
    // A closed outfit (the original Uniform) can hide the whole body: nothing to follow then.
    let Ok(surface) = crate::surface::Surface::body(&doc, &data) else { return Ok(0) };
    let centre = world[pelvis].w_axis.truncate();
    let low = world[left].w_axis.y.min(world[right].w_axis.y) - 0.20;
    let high = centre.y + 0.08;
    let mut changed = 0;
    for n in &nodes {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else { continue };
        if is_body_mesh(mesh) {
            continue;
        }
        let skin = skin as usize;
        let mut joints: Vec<usize> = doc["skins"][skin]["joints"].as_array().unwrap().iter().map(|j| j.as_u64().unwrap() as usize).collect();
        let mut ib = crate::accessor::read(&doc, &data, doc["skins"][skin]["inverseBindMatrices"].as_u64().unwrap() as usize)?;
        let transforms: Vec<glam::Mat4> = joints
            .iter()
            .enumerate()
            .map(|(i, &j)| world[j] * glam::Mat4::from_cols_array(ib[i].as_slice().try_into().unwrap()))
            .collect();
        let added_before = joints.len();
        let primitives = doc["meshes"][mesh as usize]["primitives"].as_array().unwrap().clone();
        for (pi, p) in primitives.iter().enumerate() {
            let attrs = &p["attributes"];
            let pos = crate::accessor::read(&doc, &data, attrs["POSITION"].as_u64().unwrap() as usize)?;
            let mut js = crate::accessor::read(&doc, &data, attrs["JOINTS_0"].as_u64().unwrap() as usize)?;
            let mut ws = crate::accessor::read(&doc, &data, attrs["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let mut touched = false;
            for v in 0..pos.len() {
                let on_body = (0..4).all(|k| ws[v][k] <= 0. || body_joints.contains(&joints[js[v][k] as usize]));
                if !on_body {
                    continue;
                }
                let mut point = glam::Vec3::ZERO;
                for k in 0..4 {
                    point += transforms[js[v][k] as usize].transform_point3(glam::Vec3::from_slice(&pos[v])) * ws[v][k];
                }
                if point.y < low || point.y > high || point.z < centre.z - 0.04 {
                    continue;
                }
                let hit = surface.nearest(point);
                if hit.distance > REACH || !hit.skin.iter().any(|&(j, w)| (j == left || j == right) && w > 0.05) {
                    continue;
                }
                let mut body: Vec<(usize, f32)> = hit.skin.clone();
                body.sort_by(|a, b| b.1.total_cmp(&a.1));
                body.truncate(4);
                let total: f32 = body.iter().map(|b| b.1).sum();
                if total <= 0. {
                    continue;
                }
                let mut row_j = [0f32; 4];
                let mut row_w = [0f32; 4];
                for (k, &(node, w)) in body.iter().enumerate() {
                    let slot = match joints.iter().position(|&j| j == node) {
                        Some(s) => s,
                        None => {
                            joints.push(node);
                            ib.push(world[node].inverse().to_cols_array().to_vec());
                            joints.len() - 1
                        }
                    };
                    row_j[k] = slot as f32;
                    row_w[k] = w / total;
                }
                js[v] = row_j.to_vec();
                ws[v] = row_w.to_vec();
                touched = true;
                changed += 1;
            }
            if touched {
                if joints.len() > u16::MAX as usize {
                    return Err(invalid("garment skin joint count"));
                }
                let rows: Vec<[u16; 4]> = js.iter().map(|r| [r[0] as u16, r[1] as u16, r[2] as u16, r[3] as u16]).collect();
                let j = crate::accessor::shorts(&mut doc, &mut data, &rows);
                let w = crate::accessor::floats(&mut doc, &mut data, &ws, 4)?;
                let attrs = &mut doc["meshes"][mesh as usize]["primitives"][pi]["attributes"];
                attrs["JOINTS_0"] = json!(j);
                attrs["WEIGHTS_0"] = json!(w);
            }
        }
        if joints.len() > added_before {
            let m = crate::accessor::floats(&mut doc, &mut data, &ib, 16)?;
            doc["skins"][skin]["joints"] = json!(joints);
            doc["skins"][skin]["inverseBindMatrices"] = json!(m);
        }
    }
    if changed > 0 {
        doc["buffers"][0]["byteLength"] = json!(data.len());
        glb_io::write(path, &doc, &data)?;
    }
    Ok(changed)
}

/// The buttock behind `hip` on one `side` (+1/-1 in x): a capsule across the buttock's width whose
/// back surface sits a centimetre outside the skin's 90th-percentile depth, its radius half
/// the buttock's height (clamped), at the height where the buttock protrudes most.
fn hip_capsule(points: &[glam::Vec3], centre: glam::Vec3, hip: glam::Vec3, side: f32) -> Option<(glam::Vec3, glam::Vec3, f32)> {
    let region: Vec<glam::Vec3> = points
        .iter()
        .copied()
        .filter(|p| {
            (p.x - centre.x) * side > 0.01
                && (p.x - hip.x).abs() < 0.11
                && p.y > hip.y - 0.18
                && p.y < hip.y + 0.08
                && p.z > centre.z
        })
        .collect();
    if region.len() < 20 {
        return None;
    }
    let pct = |mut v: Vec<f32>, q: f32| {
        v.sort_by(f32::total_cmp);
        v[((v.len() - 1) as f32 * q).round() as usize]
    };
    let surface = pct(region.iter().map(|p| p.z).collect(), 0.9);
    // The protruding part: points within 2.5 cm of the surface give the capsule's extent.
    let back: Vec<glam::Vec3> = region.into_iter().filter(|p| p.z > surface - 0.025).collect();
    let xs: Vec<f32> = back.iter().map(|p| p.x).collect();
    let ys: Vec<f32> = back.iter().map(|p| p.y).collect();
    let (x0, x1) = (pct(xs.clone(), 0.05), pct(xs, 0.95));
    let (y0, y1, yc) = (pct(ys.clone(), 0.05), pct(ys.clone(), 0.95), pct(ys, 0.5));
    let radius = ((y1 - y0) * 0.5).clamp(0.05, 0.08);
    let z = surface + 0.01 - radius;
    let inset = (radius * 0.5).min((x1 - x0) * 0.5);
    Some((glam::Vec3::new(x0 + inset, yc, z), glam::Vec3::new(x1 - inset, yc, z), radius))
}

/// The upper thigh on one side, fitted to the skin around the thigh bone (`thigh` to `knee`): the
/// fixed thigh capsules are thinner than her upper thighs, so short skirts fell inside them.
fn thigh_capsule(points: &[glam::Vec3], centre: glam::Vec3, thigh: glam::Vec3, knee: glam::Vec3, side: f32) -> Option<(glam::Vec3, glam::Vec3, f32)> {
    let axis = knee - thigh;
    let length = axis.length();
    if length < 0.1 {
        return None;
    }
    let dir = axis / length;
    let mut d: Vec<f32> = points
        .iter()
        .filter(|p| (p.x - centre.x) * side > 0.0)
        .filter_map(|p| {
            let t = (*p - thigh).dot(dir) / length;
            (0.08..=0.55).contains(&t).then(|| (*p - thigh - dir * t * length).length())
        })
        .filter(|&r| r < 0.14)
        .collect();
    if d.len() < 20 {
        return None;
    }
    d.sort_by(f32::total_cmp);
    let radius = d[(d.len() - 1) * 85 / 100] + 0.004;
    Some((thigh + dir * (0.08 * length + radius * 0.3), thigh + dir * 0.55 * length, radius))
}

/// The original Lucina cape is an authored open 3x4 lattice. Keep its original
/// weights and anchors instead of replacing it with a radial skirt approximation.
pub fn annotate_mantle_file(path: &Path) -> Result<usize> {
    let (mut doc, data) = glb_io::read(path)?;
    if doc["extras"]["lucinaMantleClothRevision"] == 1 {
        return Ok(12);
    }
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let names = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n["name"].as_str().map(|n| (n.to_owned(), i)))
        .collect::<std::collections::HashMap<_, _>>();
    let mut controls = Vec::new();
    for row in 0..4 {
        for side in ["l", "c", "r"] {
            let name = format!("{side}_sprmnt{}_jnt", row + 1);
            let Some(i) = names.get(&name) else {
                return Ok(0);
            };
            controls.push(*i);
        }
    }
    let mut used = 0f32;
    for n in &nodes {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let joints = doc["skins"][skin as usize]["joints"].as_array().unwrap();
        for p in doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .unwrap()
        {
            let a = &p["attributes"];
            let j = crate::accessor::read(&doc, &data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let w = crate::accessor::read(&doc, &data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let ix = crate::accessor::read(&doc, &data, p["indices"].as_u64().unwrap() as usize)?
                .into_iter()
                .map(|v| v[0] as usize)
                .collect::<std::collections::HashSet<_>>();
            for v in ix {
                for k in 0..4 {
                    if controls.contains(&(joints[j[v][k] as usize].as_u64().unwrap() as usize)) {
                        used += w[v][k];
                    }
                }
            }
        }
    }
    if used < 1. {
        return Ok(0);
    }
    let parameters = [18., 10., 0.00001, 0.000001, 0.003, 0.25, 0.007, -0.03];
    for (index, node) in controls.iter().enumerate() {
        doc["nodes"][*node]["extras"]["secretarySecondary"] = json!({"kind":"cloth","grid":{"id":"lucina-authored-mantle","column":index%3,"row":index/3,"columns":3,"rows":4,"closed":false,"parameters":parameters,"gravityWeight":1,"source":"original-authored-cape"}});
    }
    doc["extras"]["lucinaMantleClothRevision"] = json!(1);
    glb_io::write(path, &doc, &data)?;
    Ok(12)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_lattice_names_have_stable_coordinates() {
        assert_eq!(coordinate("Skirt24_E_yure_skirt_"), Some((23, 4)));
        assert_eq!(coordinate("Skirt01_AA_yure_skirt_"), Some((0, 26)));
        assert_eq!(coordinate("Bip01 L Thigh"), None);
        assert_eq!(coordinate("Skirt00_A_yure_skirt_"), None);
    }
    #[test]
    fn hip_capsule_sits_just_inside_the_buttock_surface_on_its_own_side() {
        use glam::Vec3;
        // A half-ellipsoid buttock behind each hip (+z is the back), plus front points it must ignore.
        let centre = Vec3::new(0., 0.9, 0.);
        let mut points = Vec::new();
        for side in [-1f32, 1.] {
            for i in 0..20 {
                for k in 0..10 {
                    let a = i as f32 / 19. * std::f32::consts::PI;
                    let y = 0.82 + k as f32 * 0.012;
                    points.push(Vec3::new(side * 0.08 + 0.06 * a.cos(), y, 0.02 + 0.09 * a.sin()));
                    points.push(Vec3::new(side * 0.08, y, -0.08));
                }
            }
        }
        let hip = Vec3::new(0.09, 0.9, 0.0);
        let (a, b, r) = hip_capsule(&points, centre, hip, 1.).unwrap();
        let surface = 0.02 + 0.09;
        assert!((a.z + r - surface).abs() < 0.012, "back of the capsule at the skin: {}", a.z + r);
        assert!(a.x > 0.0 && b.x < 0.16 && a.x < b.x, "across the right buttock: {} .. {}", a.x, b.x);
        assert!((a.y - b.y).abs() < 1e-6 && a.y > 0.82 && a.y < 0.82 + 0.108, "level, at the buttock: {}", a.y);
        assert!((0.05..=0.08).contains(&r));
        assert!(hip_capsule(&points, centre, Vec3::new(5., 0.9, 0.), 1.).is_none(), "no skin there: no capsule");
    }
    #[test]
    fn thigh_capsule_wraps_the_upper_thigh_skin() {
        use glam::Vec3;
        // A cylinder of radius 8 cm around the right thigh bone, from hip (0.1, 0.9) to knee (0.1, 0.45).
        let (thigh, knee) = (Vec3::new(0.1, 0.9, 0.), Vec3::new(0.1, 0.45, 0.));
        let mut points = Vec::new();
        for i in 0..36 {
            for k in 0..20 {
                let a = i as f32 / 36. * std::f32::consts::TAU;
                points.push(Vec3::new(0.1 + 0.08 * a.cos(), 0.9 - k as f32 * 0.0225, 0.08 * a.sin()));
            }
        }
        let (a, b, r) = thigh_capsule(&points, Vec3::new(0., 0.95, 0.), thigh, knee, 1.).unwrap();
        assert!((r - 0.084).abs() < 0.006, "radius from the skin: {r}");
        assert!(a.y < 0.9 && b.y > 0.6 && a.y > b.y, "upper half of the thigh: {} .. {}", a.y, b.y);
        assert!(thigh_capsule(&points, Vec3::new(0., 0.95, 0.), thigh, knee, -1.).is_none(), "other side: no skin");
    }
}
