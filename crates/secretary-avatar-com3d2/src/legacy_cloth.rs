//! A coupled hem sheet for old complete outfits. Only pelvis-weighted free
//! fabric is eligible: boots, sleeves, gloves, the authored cape and rigid
//! accessories must keep their existing skinning.
use crate::{accessor, compose, invalid, Result};
use glam::{Mat4, Vec3};
use serde_json::{json, Value};
const COLS: usize = 12;
const ROWS: usize = 4;
fn eligible(
    doc: &Value,
    skin: usize,
    j: &[f32],
    w: &[f32],
    point: Vec3,
    center: Vec3,
    waist: f32,
) -> bool {
    let radius = Vec3::new(point.x - center.x, 0., point.z - center.z).length();
    if point.y >= waist - 0.035 || point.y < waist - 0.5 || !(0.14..0.42).contains(&radius) {
        return false;
    }
    let mut limbs = 0.;
    let mut torso = 0.;
    for k in 0..4 {
        if w[k] < 0.001 {
            continue;
        }
        let Some(n) = doc["skins"][skin]["joints"][j[k] as usize].as_u64() else {
            return false;
        };
        let name = doc["nodes"][n as usize]["name"].as_str().unwrap_or("");
        if name.contains("sprmnt") || name.starts_with("mant_") || name.contains("sprblt") {
            return false;
        }
        if [
            "Calf", "Foot", "Toe", "Arm", "Hand", "Finger", "Thigh", "twist", "Head", "Neck",
        ]
        .iter()
        .any(|s| name.contains(s))
        {
            limbs += w[k];
        } else if name.contains("Pelvis") || name.contains("Spine") || name.starts_with("Hip_") {
            torso += w[k];
        }
    }
    limbs < 0.15 && torso > 0.75
}
struct Primitive {
    mesh: usize,
    primitive: usize,
    skin: usize,
    positions: Vec<Vec<f32>>,
    joints: Vec<Vec<f32>>,
    weights: Vec<Vec<f32>>,
    selected: Vec<usize>,
    world: Mat4,
}
pub fn bind(doc: &mut Value, data: &mut Vec<u8>, prefix: &str) -> Result<usize> {
    let (_, _, world) = compose::scene_frames(doc)?;
    let nodes = doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("legacy cloth nodes"))?
        .clone();
    let Some(parent) = nodes.iter().position(|n| n["name"] == "Bip01 Pelvis") else {
        return Ok(0);
    };
    let center = world[parent].w_axis.truncate();
    let waist = center.y;
    let mut candidates = Vec::new();
    let mut sectors = [0usize; COLS];
    let mut radius = vec![Vec::<f32>::new(); COLS];
    let mut bottom = waist;
    let mut selected_count = 0;
    for (ni, n) in nodes.iter().enumerate() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let (mesh, skin) = (mesh as usize, skin as usize);
        for (pi, p) in doc["meshes"][mesh]["primitives"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let material = p["material"]
                .as_u64()
                .and_then(|i| doc["materials"][i as usize]["name"].as_str())
                .unwrap_or("")
                .to_lowercase();
            if ["shoe", "hat", "headband", "accessory"]
                .iter()
                .any(|s| material.contains(s))
            {
                continue;
            }
            let read = |key: &str| {
                accessor::read(doc, data, p["attributes"][key].as_u64().unwrap() as usize)
            };
            let positions = read("POSITION")?;
            let joints = read("JOINTS_0")?;
            let weights = read("WEIGHTS_0")?;
            let used = accessor::read(doc, data, p["indices"].as_u64().unwrap() as usize)?
                .into_iter()
                .map(|v| v[0] as usize)
                .collect::<std::collections::HashSet<_>>();
            let mut selected = Vec::new();
            for v in used {
                let point = world[ni].transform_point3(Vec3::from_slice(&positions[v]));
                if !eligible(doc, skin, &joints[v], &weights[v], point, center, waist) {
                    continue;
                }
                let a = (point.x - center.x)
                    .atan2(point.z - center.z)
                    .rem_euclid(std::f32::consts::TAU);
                let col = (a / std::f32::consts::TAU * COLS as f32) as usize % COLS;
                sectors[col] += 1;
                radius[col].push(Vec3::new(point.x - center.x, 0., point.z - center.z).length());
                bottom = bottom.min(point.y);
                selected.push(v);
                selected_count += 1;
            }
            if !selected.is_empty() {
                candidates.push(Primitive {
                    mesh,
                    primitive: pi,
                    skin,
                    positions,
                    joints,
                    weights,
                    selected,
                    world: world[ni],
                });
            }
        }
    }
    // An apron/open panel cannot be treated as a closed skirt. Leave unsupported
    // sparse surfaces unchanged until their own seam layout has been authored.
    if selected_count < 60
        || sectors.iter().filter(|&&n| n >= 3).count() < 10
        || waist - bottom < 0.08
    {
        return Ok(0);
    }
    let root_radius = 0.13;
    let mut mean = 0.;
    for r in &mut radius {
        r.sort_by(f32::total_cmp);
        if !r.is_empty() {
            mean += r[r.len() * 3 / 4];
        }
    }
    mean /= radius.iter().filter(|r| !r.is_empty()).count() as f32;
    let mut grid_nodes = Vec::new();
    let mut grid_world = Vec::new();
    for row in 0..ROWS {
        for (col, samples) in radius.iter().enumerate() {
            let t = row as f32 / (ROWS - 1) as f32;
            let r = if samples.is_empty() {
                mean
            } else {
                samples[samples.len() * 3 / 4]
            };
            let r = root_radius + (r - root_radius) * t;
            let a = col as f32 * std::f32::consts::TAU / COLS as f32;
            let point = Vec3::new(
                center.x + r * a.sin(),
                waist - (waist - bottom) * t,
                center.z + r * a.cos(),
            );
            let global = Mat4::from_translation(point);
            let i = row * COLS + col;
            let ancestor = if row == 0 {
                parent
            } else {
                grid_nodes[i - COLS]
            };
            let parent_world = if row == 0 {
                world[parent]
            } else {
                grid_world[i - COLS]
            };
            let node = doc["nodes"].as_array().unwrap().len();
            doc["nodes"].as_array_mut().unwrap().push(json!({"name":format!("SecretaryHem_{prefix}_{row}_{col}"),"matrix":(parent_world.inverse()*global).to_cols_array(),"extras":{"secretarySecondary":{"grid":{"id":format!("legacy-hem-{prefix}"),"column":col,"row":row,"columns":COLS,"rows":ROWS,"closed":true,"parameters":[18,13.4,0.0001,0.0000014,0.002,0.6,0.008,-0.03],"gravityWeight":1,"source":"inferred-pelvis-weighted-hem"}}}}));
            if !doc["nodes"][ancestor]["children"].is_array() {
                doc["nodes"][ancestor]["children"] = json!([]);
            }
            doc["nodes"][ancestor]["children"]
                .as_array_mut()
                .unwrap()
                .push(json!(node));
            grid_nodes.push(node);
            grid_world.push(global);
        }
    }
    let skins = doc["skins"].as_array().unwrap().len();
    let mut offsets = Vec::new();
    for si in 0..skins {
        offsets.push(doc["skins"][si]["joints"].as_array().unwrap().len());
        let mut inverse = accessor::read(
            doc,
            data,
            doc["skins"][si]["inverseBindMatrices"].as_u64().unwrap() as usize,
        )?;
        inverse.extend(
            grid_world
                .iter()
                .map(|w| w.inverse().to_cols_array().to_vec()),
        );
        let ib = accessor::floats(doc, data, &inverse, 16)?;
        doc["skins"][si]["inverseBindMatrices"] = json!(ib);
        doc["skins"][si]["joints"]
            .as_array_mut()
            .unwrap()
            .extend(grid_nodes.iter().map(|i| json!(i)));
    }
    for p in &mut candidates {
        for &v in &p.selected {
            let point = p.world.transform_point3(Vec3::from_slice(&p.positions[v]));
            let col = (point.x - center.x)
                .atan2(point.z - center.z)
                .rem_euclid(std::f32::consts::TAU)
                / std::f32::consts::TAU
                * COLS as f32;
            let row = ((waist - point.y) / (waist - bottom) * (ROWS - 1) as f32)
                .clamp(0., (ROWS - 1) as f32 - 0.00001);
            let (c, r) = (col.floor() as usize, row.floor() as usize);
            let (u, t) = (col.fract(), row.fract());
            p.joints[v] = [
                r * COLS + c,
                r * COLS + (c + 1) % COLS,
                (r + 1) * COLS + c,
                (r + 1) * COLS + (c + 1) % COLS,
            ]
            .iter()
            .map(|i| (offsets[p.skin] + i) as f32)
            .collect();
            p.weights[v] = vec![(1. - u) * (1. - t), u * (1. - t), (1. - u) * t, u * t];
        }
        let j = p
            .joints
            .iter()
            .map(|v| std::array::from_fn(|i| v[i] as u16))
            .collect::<Vec<[u16; 4]>>();
        let j = accessor::shorts(doc, data, &j);
        let w = accessor::floats(doc, data, &p.weights, 4)?;
        doc["meshes"][p.mesh]["primitives"][p.primitive]["attributes"]["JOINTS_0"] = json!(j);
        doc["meshes"][p.mesh]["primitives"][p.primitive]["attributes"]["WEIGHTS_0"] = json!(w);
    }
    doc["buffers"][0]["byteLength"] = json!(data.len());
    doc["extras"]["secretaryLegacyHem"] = json!({"nodes":COLS*ROWS,"vertices":selected_count,"method":"coupled-sheet; pelvis-weighted free hem only","individualFitReviewRequired":true});
    Ok(selected_count)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crouched_boots_and_gloves_do_not_become_a_skirt() {
        let doc = json!({"nodes":[{"name":"Bip01 Pelvis"},{"name":"Bip01 L Calf"},{"name":"Bip01 L Hand"},{"name":"c_sprmnt2_jnt"}],"skins":[{"joints":[0,1,2,3]}]});
        let p = Vec3::new(0.2, 0.6, 0.);
        let c = Vec3::new(0., 0.9, 0.);
        assert!(eligible(
            &doc,
            0,
            &[0., 0., 0., 0.],
            &[1., 0., 0., 0.],
            p,
            c,
            0.9
        ));
        for j in [1., 2., 3.] {
            assert!(!eligible(
                &doc,
                0,
                &[j, 0., 0., 0.],
                &[1., 0., 0., 0.],
                p,
                c,
                0.9
            ));
        }
    }
}
#[cfg(test)]
mod contract_tests {
    use super::*;
    #[test]
    fn coupled_hem_preserves_bind_surface_and_excludes_boot_in_same_material() {
        let mut d = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,2]}],"nodes":[{"name":"Bip01 Pelvis","translation":[0,0.9,0],"children":[1]},{"name":"Bip01 L Calf","translation":[0.2,-0.3,0]},{"name":"dress","mesh":0,"skin":0}],"skins":[],"meshes":[],"materials":[{"name":"Clothes"}],"accessors":[],"bufferViews":[],"buffers":[{"byteLength":0}]});
        let mut b = Vec::new();
        let mut points = Vec::new();
        for row in 0..6 {
            for col in 0..12 {
                let a = (col as f32 + 0.3) * std::f32::consts::TAU / 12.;
                points.push(vec![
                    0.23 * a.sin(),
                    0.84 - row as f32 * 0.05,
                    0.23 * a.cos(),
                ]);
            }
        }
        points.push(vec![0.23, 0.6, 0.]);
        let count = points.len();
        let p = accessor::floats(&mut d, &mut b, &points, 3).unwrap();
        let mut joints = vec![[0; 4]; count];
        joints[count - 1][0] = 1;
        let j = accessor::shorts(&mut d, &mut b, &joints);
        let w = accessor::floats(&mut d, &mut b, &vec![vec![1., 0., 0., 0.]; count], 4).unwrap();
        let ib = accessor::floats(
            &mut d,
            &mut b,
            &[
                Mat4::from_translation(Vec3::new(0., -0.9, 0.))
                    .to_cols_array()
                    .to_vec(),
                Mat4::from_translation(Vec3::new(-0.2, -0.6, 0.))
                    .to_cols_array()
                    .to_vec(),
            ],
            16,
        )
        .unwrap();
        let mut indices = (0..count as u16).collect::<Vec<_>>();
        while !indices.len().is_multiple_of(4) {
            indices.push(0);
        }
        let index = accessor::shorts(&mut d, &mut b, indices.as_chunks::<4>().0);
        d["accessors"][index]["type"] = json!("SCALAR");
        d["accessors"][index]["count"] = json!(count);
        d["skins"] = json!([{"joints":[0,1],"inverseBindMatrices":ib}]);
        d["meshes"] = json!([{"primitives":[{"attributes":{"POSITION":p,"JOINTS_0":j,"WEIGHTS_0":w},"indices":index,"material":0}]}]);
        assert_eq!(bind(&mut d, &mut b, "test").unwrap(), 72);
        assert_eq!(d["nodes"].as_array().unwrap().len(), 51);
        let at = &d["meshes"][0]["primitives"][0]["attributes"];
        let js = accessor::read(&d, &b, at["JOINTS_0"].as_u64().unwrap() as usize).unwrap();
        let ws = accessor::read(&d, &b, at["WEIGHTS_0"].as_u64().unwrap() as usize).unwrap();
        assert_eq!(js[count - 1][0], 1.);
        assert_eq!(ws[count - 1][0], 1.);
        let (_, _, world) = compose::scene_frames(&d).unwrap();
        let inverse = accessor::read(
            &d,
            &b,
            d["skins"][0]["inverseBindMatrices"].as_u64().unwrap() as usize,
        )
        .unwrap();
        for v in 0..count {
            let mut actual = Vec3::ZERO;
            for k in 0..4 {
                let j = js[v][k] as usize;
                let n = d["skins"][0]["joints"][j].as_u64().unwrap() as usize;
                actual += (world[n]
                    * Mat4::from_cols_array(inverse[j].as_slice().try_into().unwrap()))
                .transform_point3(Vec3::from_slice(&points[v]))
                    * ws[v][k];
            }
            assert!(
                actual.distance(Vec3::from_slice(&points[v])) < 1e-5,
                "vertex {v}"
            );
        }
    }
}
