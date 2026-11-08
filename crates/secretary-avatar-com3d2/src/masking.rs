//! Native body visibility rules. A menu is data, never executable script.
//! The game hides a triangle if any of its vertices uses a hidden skin joint.
use crate::{compose::scene_frames, glb_io, invalid, menu::Menu, Result};
use serde_json::{json, Value};
use std::path::Path;

fn numbers(doc: &Value, data: &[u8], index: usize, width: usize) -> Result<Vec<[f32; 4]>> {
    let a = doc["accessors"]
        .get(index)
        .ok_or_else(|| invalid("mask accessor"))?;
    if a["sparse"].is_object() {
        return Err(invalid("sparse skin attributes unsupported"));
    }
    let component = a["componentType"]
        .as_u64()
        .ok_or_else(|| invalid("skin component"))?;
    let bytes = match component {
        5121 => 1,
        5123 => 2,
        5125 | 5126 => 4,
        _ => return Err(invalid("skin component type")),
    };
    let count = a["count"]
        .as_u64()
        .filter(|n| *n <= 1_000_000)
        .ok_or_else(|| invalid("skin count"))? as usize;
    let view = a["bufferView"]
        .as_u64()
        .and_then(|i| doc["bufferViews"].get(i as usize))
        .ok_or_else(|| invalid("skin view"))?;
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + a["byteOffset"].as_u64().unwrap_or(0) as usize;
    let stride = view["byteStride"]
        .as_u64()
        .unwrap_or((bytes * width) as u64) as usize;
    if stride < bytes * width {
        return Err(invalid("skin stride"));
    }
    let normalized = a["normalized"] == true;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut value = [0.; 4];
        for (j, v) in value.iter_mut().enumerate().take(width) {
            let p = start
                .checked_add(
                    i.checked_mul(stride)
                        .ok_or_else(|| invalid("skin offset"))?,
                )
                .and_then(|n| n.checked_add(j * bytes))
                .ok_or_else(|| invalid("skin offset"))?;
            let b = data
                .get(p..p + bytes)
                .ok_or_else(|| invalid("skin data bounds"))?;
            *v = match component {
                5121 => b[0] as f32 / if normalized { 255. } else { 1. },
                5123 => {
                    u16::from_le_bytes(b.try_into().unwrap()) as f32
                        / if normalized { 65535. } else { 1. }
                }
                5125 => u32::from_le_bytes(b.try_into().unwrap()) as f32,
                5126 => f32::from_le_bytes(b.try_into().unwrap()),
                _ => unreachable!(),
            };
            if !v.is_finite() {
                return Err(invalid("skin numeric value"));
            }
        }
        out.push(value);
    }
    Ok(out)
}

pub fn visibility(doc: &Value, menu: &Menu) -> Result<Vec<bool>> {
    let (parents, _, _) = scene_frames(doc)?;
    let nodes = doc["nodes"].as_array().unwrap();
    let mut visible = vec![true; nodes.len()];
    for command in &menu.commands {
        let Some(kind) = command.first().map(String::as_str) else {
            continue;
        };
        let flag = match kind {
            "node消去" => false,
            "node表示" => true,
            _ => continue,
        };
        if command.len() != 2 {
            return Err(invalid("body visibility rule arguments"));
        }
        for (i, visibility) in visible.iter_mut().enumerate() {
            let mut current = i as i32;
            while current >= 0 {
                let name = nodes[current as usize]["name"].as_str().unwrap_or("");
                if command[1] == "_ALL_" || name.contains(&command[1]) {
                    *visibility = flag;
                    break;
                }
                current = parents[current as usize];
            }
        }
    }
    Ok(visible)
}

pub fn body(input: &Path, menu: &Menu, output: &Path) -> Result<Value> {
    let (mut doc, mut data) = glb_io::read(input)?;
    let visible = visibility(&doc, menu)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let mut removed = 0;
    let mut kept = 0;
    for node in nodes {
        let (Some(mesh), Some(skin)) = (node["mesh"].as_u64(), node["skin"].as_u64()) else {
            continue;
        };
        // Native round-trip models may rename/merge the body node. Preserve
        // the real face/hair primitives while selecting tagged body materials.
        let legacy_body = node["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("Lucina_COM3D2_Body") || n.starts_with("COM3D2_body"));
        let joints = doc["skins"][skin as usize]["joints"]
            .as_array()
            .ok_or_else(|| invalid("body skin"))?
            .iter()
            .map(|j| {
                j.as_u64()
                    .filter(|i| *i < visible.len() as u64)
                    .map(|i| visible[i as usize])
                    .ok_or_else(|| invalid("body skin node"))
            })
            .collect::<Result<Vec<_>>>()?;
        let primitives = doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .ok_or_else(|| invalid("body primitives"))?
            .clone();
        for (p, primitive) in primitives.iter().enumerate() {
            if !legacy_body
                && !primitive["material"].as_u64().is_some_and(|m| {
                    doc["materials"][m as usize]["name"]
                        .as_str()
                        .is_some_and(|n| n.starts_with("skin_safe"))
                })
            {
                continue;
            }

            let get = |key: &str| {
                primitive["attributes"][key]
                    .as_u64()
                    .ok_or_else(|| invalid("body skin attribute"))
            };
            let ids = numbers(&doc, &data, get("JOINTS_0")? as usize, 4)?;
            let weights = numbers(&doc, &data, get("WEIGHTS_0")? as usize, 4)?;
            if ids.len() != weights.len() {
                return Err(invalid("skin attribute length"));
            }
            let idx = primitive["indices"]
                .as_u64()
                .ok_or_else(|| invalid("body triangle indices"))? as usize;
            let indices = numbers(&doc, &data, idx, 1)?;
            if indices.len() % 3 != 0 {
                return Err(invalid("body triangle length"));
            }
            let mut result = Vec::new();
            for tri in indices.as_chunks::<3>().0.iter() {
                let mut hide = false;
                for index in tri {
                    let v = index[0] as usize;
                    let is = ids.get(v).ok_or_else(|| invalid("body vertex index"))?;
                    for k in 0..4 {
                        if k == 0 || weights[v][k] > 0. {
                            let flag = joints
                                .get(is[k] as usize)
                                .ok_or_else(|| invalid("body skin joint"))?;
                            hide |= !*flag;
                        }
                    }
                }
                if hide {
                    removed += 1;
                } else {
                    kept += 1;
                    for index in tri {
                        result.extend((index[0] as u32).to_le_bytes());
                    }
                }
            }
            // glTF accessors require a nonzero count. A fully hidden region
            // uses the native renderer's degenerate-triangle convention.
            if result.is_empty() {
                result.extend([0u8; 12]);
            }
            while !data.len().is_multiple_of(4) {
                data.push(0);
            }
            let start = data.len();
            data.extend(&result);
            let view = doc["bufferViews"].as_array().unwrap().len();
            doc["bufferViews"]
                .as_array_mut()
                .unwrap()
                .push(json!({"buffer":0,"byteOffset":start,"byteLength":result.len()}));
            let accessor = doc["accessors"].as_array().unwrap().len();
            doc["accessors"].as_array_mut().unwrap().push(json!({"bufferView":view,"componentType":5125,"count":result.len()/4,"type":"SCALAR"}));
            doc["meshes"][mesh as usize]["primitives"][p]["indices"] = json!(accessor);
        }
    }
    doc["buffers"][0]["byteLength"] = json!(data.len());
    doc["extras"]["bodyVisibility"] =
        json!({"source":menu.source,"removedTriangles":removed,"keptTriangles":kept});
    glb_io::write(output, &doc, &data)?;
    Ok(json!({"removedTriangles":removed,"keptTriangles":kept,"output":output}))
}


/// Which body vertices a garment surface covers: the garment lies straight out along the skin
/// normal, from `BEHIND` under the skin (where the skin pokes through) to `AHEAD` over it. An
/// opening (neckline, cuff, cut-out) has no cloth along the normal, so the skin there stays.
/// `skip` marks vertices that are never covered.
pub fn covered(points: &[glam::Vec3], normals: &[glam::Vec3], skip: &[bool], garment: &crate::surface::Surface) -> Vec<bool> {
    const BEHIND: f32 = 0.02;
    const AHEAD: f32 = 0.06;
    points
        .iter()
        .zip(normals)
        .zip(skip)
        .map(|((&p, &n), &skip)| {
            !skip && n != glam::Vec3::ZERO && garment.ray(p - n * BEHIND, n, BEHIND + AHEAD).is_some()
        })
        .collect()
}

/// Hides the body triangles that the meshes named in `garments` cover (every vertex covered), for
/// imported outfits that come with no native body rules, as the game's own items hide the body
/// under them: the native body is fuller than the one they were made for. Runs at bind. Returns
/// the number of triangles hidden.
pub fn under_garments(path: &Path, garments: &std::collections::BTreeSet<String>) -> Result<usize> {
    use glam::{Mat4, Vec3};
    let (mut doc, mut data) = glb_io::read(path)?;
    let (_, _, world) = scene_frames(&doc)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let mut points = Vec::new();
    let mut normals = Vec::new();
    let mut faces = Vec::new();
    for n in &nodes {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else { continue };
        if !doc["meshes"][mesh as usize]["name"].as_str().is_some_and(|m| garments.contains(m)) {
            continue;
        }
        let s = &doc["skins"][skin as usize];
        let ib = crate::accessor::read(&doc, &data, s["inverseBindMatrices"].as_u64().unwrap() as usize)?;
        let transforms: Vec<Mat4> = s["joints"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, j)| world[j.as_u64().unwrap() as usize] * Mat4::from_cols_array(ib[i].as_slice().try_into().unwrap()))
            .collect();
        for p in doc["meshes"][mesh as usize]["primitives"].as_array().unwrap() {
            let a = &p["attributes"];
            let (Some(pi), Some(ni), Some(ii)) = (a["POSITION"].as_u64(), a["NORMAL"].as_u64(), p["indices"].as_u64()) else { continue };
            let pos = crate::accessor::read(&doc, &data, pi as usize)?;
            let nor = crate::accessor::read(&doc, &data, ni as usize)?;
            let js = crate::accessor::read(&doc, &data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let ws = crate::accessor::read(&doc, &data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let base = points.len();
            for v in 0..pos.len() {
                let mut m = Mat4::ZERO;
                for k in 0..4 {
                    m += transforms[js[v][k] as usize] * ws[v][k];
                }
                points.push(m.transform_point3(Vec3::from_slice(&pos[v])));
                normals.push(m.transform_vector3(Vec3::from_slice(&nor[v])).normalize_or_zero());
            }
            let idx = crate::accessor::read(&doc, &data, ii as usize)?;
            for t in idx.as_chunks::<3>().0 {
                faces.push([base + t[0][0] as usize, base + t[1][0] as usize, base + t[2][0] as usize]);
            }
        }
    }
    if faces.is_empty() {
        return Ok(0);
    }
    let garment = crate::surface::Surface::geometry(&points, &normals, &faces)?;
    let names: Vec<String> = nodes.iter().map(|n| n["name"].as_str().unwrap_or("").to_owned()).collect();
    let mut hidden = 0;
    for (i, n) in nodes.iter().enumerate() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else { continue };
        let joints: Vec<usize> = doc["skins"][skin as usize]["joints"].as_array().unwrap().iter().map(|j| j.as_u64().unwrap() as usize).collect();
        let primitives = doc["meshes"][mesh as usize]["primitives"].as_array().unwrap().clone();
        for (pi, p) in primitives.iter().enumerate() {
            let skin_material = doc["materials"][p["material"].as_u64().unwrap_or(0) as usize]["name"].as_str().is_some_and(|m| m.starts_with("skin_"));
            if !skin_material {
                continue;
            }
            let a = &p["attributes"];
            let pos = crate::accessor::read(&doc, &data, a["POSITION"].as_u64().unwrap() as usize)?;
            let nor = crate::accessor::read(&doc, &data, a["NORMAL"].as_u64().unwrap() as usize)?;
            let js = crate::accessor::read(&doc, &data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let ws = crate::accessor::read(&doc, &data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let normal = world[i].inverse().transpose();
            let body_points: Vec<Vec3> = pos.iter().map(|q| world[i].transform_point3(Vec3::from_slice(q))).collect();
            let body_normals: Vec<Vec3> = nor.iter().map(|q| normal.transform_vector3(Vec3::from_slice(q)).normalize_or_zero()).collect();
            let skip: Vec<bool> = (0..pos.len())
                .map(|v| {
                    let k = (0..4).max_by(|&a, &b| ws[v][a].total_cmp(&ws[v][b])).unwrap();
                    let name = &names[joints[js[v][k] as usize]];
                    // Hands stay: cuffs end at the wrist.
                    name.contains("Hand") || name.contains("Finger")
                })
                .collect();
            let cover = covered(&body_points, &body_normals, &skip, &garment);
            let idx = crate::accessor::read(&doc, &data, p["indices"].as_u64().unwrap() as usize)?;
            let mut kept = Vec::new();
            for t in idx.as_chunks::<3>().0 {
                let tri = [t[0][0] as usize, t[1][0] as usize, t[2][0] as usize];
                if tri.iter().all(|&v| cover[v]) {
                    hidden += 1;
                } else {
                    kept.extend(tri.iter().flat_map(|&v| (v as u32).to_le_bytes()));
                }
            }
            if kept.is_empty() {
                kept.extend([0u8; 12]);
            }
            while !data.len().is_multiple_of(4) {
                data.push(0);
            }
            let start = data.len();
            data.extend(&kept);
            let view = doc["bufferViews"].as_array().unwrap().len();
            doc["bufferViews"].as_array_mut().unwrap().push(json!({"buffer": 0, "byteOffset": start, "byteLength": kept.len()}));
            let accessor = doc["accessors"].as_array().unwrap().len();
            doc["accessors"].as_array_mut().unwrap().push(json!({"bufferView": view, "componentType": 5125, "count": kept.len() / 4, "type": "SCALAR"}));
            doc["meshes"][mesh as usize]["primitives"][pi]["indices"] = json!(accessor);
        }
    }
    if hidden > 0 {
        doc["buffers"][0]["byteLength"] = json!(data.len());
        glb_io::write(path, &doc, &data)?;
    }
    Ok(hidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn later_show_rule_restores_descendants_without_restoring_siblings() {
        let doc = json!({"nodes":[{"name":"Spine1","children":[1,3]},{"name":"Spine1a","children":[2]},{"name":"Head"},{"name":"UpperArm"}]});
        let menu = Menu {
            version: 2001,
            source: String::new(),
            name: String::new(),
            category: String::new(),
            description: String::new(),
            commands: vec![
                vec!["node消去".into(), "Spine1".into()],
                vec!["node表示".into(), "Spine1a".into()],
            ],
        };
        assert_eq!(
            visibility(&doc, &menu).unwrap(),
            vec![false, true, true, false]
        );
    }
    #[test]
    fn body_points_under_a_close_garment_are_covered_but_not_far_or_skipped_ones() {
        use glam::Vec3;
        // A garment square 1 cm in front of the body plane z = 0 (normals +z), over x, y in [-0.1, 0.1].
        let pts = [Vec3::new(-0.1, -0.1, 0.01), Vec3::new(0.1, -0.1, 0.01), Vec3::new(0.1, 0.1, 0.01), Vec3::new(-0.1, 0.1, 0.01)];
        let garment = crate::surface::Surface::geometry(&pts, &[Vec3::Z; 4], &[[0, 1, 2], [0, 2, 3]]).unwrap();
        let body = [
            Vec3::new(0., 0., 0.),
            Vec3::new(0.05, 0.05, 0.),
            Vec3::new(0.5, 0., 0.),
            Vec3::new(0., 0., 0.),
            Vec3::new(0., 0., 0.02),
            Vec3::new(0., 0., -0.08),
            Vec3::new(0., 0., 0.),
        ];
        let normals = [Vec3::Z, Vec3::Z, Vec3::Z, Vec3::Z, Vec3::Z, Vec3::Z, Vec3::X];
        let skip = [false, false, false, true, false, false, false];
        // Covered; covered; beside the garment (an opening); skipped; skin poking 1 cm through the
        // garment; 9 cm under it; facing sideways, so the cloth is not over it.
        assert_eq!(
            covered(&body, &normals, &skip, &garment),
            [true, true, false, false, true, false, false]
        );
    }
}
