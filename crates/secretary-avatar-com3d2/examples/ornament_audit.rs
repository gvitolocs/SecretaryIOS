use glam::{Mat4, Vec3};
use secretary_avatar_com3d2::{accessor, compose, glb_io, secondary_rig, Result};
use serde_json::json;
use std::path::Path;
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let (d, b) = glb_io::read(Path::new(&args[1]))?;
    let (_, _, world) = compose::scene_frames(&d)?;
    let tips = d["extras"]["secretaryRibbonTips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let p = t["position"].as_array().unwrap();
            Vec3::new(
                p[0].as_f64().unwrap() as f32,
                p[1].as_f64().unwrap() as f32,
                p[2].as_f64().unwrap() as f32,
            )
        })
        .collect::<Vec<_>>();
    let mut report = Vec::new();
    for n in d["nodes"].as_array().unwrap() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        if n["extras"]["secretaryRibbonSizes"].is_array() {
            continue;
        }
        let s = &d["skins"][skin as usize];
        let ib = accessor::read(&d, &b, s["inverseBindMatrices"].as_u64().unwrap() as usize)?;
        let m = s["joints"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, j)| {
                world[j.as_u64().unwrap() as usize]
                    * Mat4::from_cols_array(ib[i].as_slice().try_into().unwrap())
            })
            .collect::<Vec<_>>();
        for p in d["meshes"][mesh as usize]["primitives"].as_array().unwrap() {
            let a = &p["attributes"];
            let pos = accessor::read(&d, &b, a["POSITION"].as_u64().unwrap() as usize)?;
            let j = accessor::read(&d, &b, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let w = accessor::read(&d, &b, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let points = pos
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let mut v = Vec3::ZERO;
                    for k in 0..4 {
                        v += m[j[i][k] as usize].transform_point3(Vec3::from_slice(p)) * w[i][k];
                    }
                    v
                })
                .collect::<Vec<_>>();
            let ix = accessor::read(&d, &b, p["indices"].as_u64().unwrap() as usize)?;
            for group in secondary_rig::component_groups(&points, &ix)? {
                let mut lo = Vec3::splat(f32::INFINITY);
                let mut hi = Vec3::splat(f32::NEG_INFINITY);
                let mut center = Vec3::ZERO;
                for i in &group {
                    lo = lo.min(points[*i]);
                    hi = hi.max(points[*i]);
                    center += points[*i];
                }
                center /= group.len() as f32;
                let distance = tips
                    .iter()
                    .map(|p| p.distance(center))
                    .fold(f32::INFINITY, f32::min);
                if distance < 0.3 {
                    report.push(json!({"mesh":n["name"],"material":d["materials"][p["material"].as_u64().unwrap_or(0) as usize]["name"],"vertices":group.len(),"extent":(hi-lo).to_array(),"center":center.to_array(),"distance":distance}));
                }
            }
        }
    }
    println!("{}", json!(report));
    Ok(())
}
