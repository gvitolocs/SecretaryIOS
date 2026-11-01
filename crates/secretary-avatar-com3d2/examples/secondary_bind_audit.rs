use glam::{Mat4, Vec3};
use secretary_avatar_com3d2::{accessor, compose, glb_io, secondary_rig, Result};
use serde_json::{json, Value};
use std::{collections::HashSet, path::Path};
fn points(d: &Value, b: &[u8]) -> Result<Vec<Vec3>> {
    let (_, _, world) = compose::scene_frames(d)?;
    let mut out = Vec::new();
    for n in d["nodes"].as_array().unwrap() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let s = &d["skins"][skin as usize];
        let ib = accessor::read(d, b, s["inverseBindMatrices"].as_u64().unwrap() as usize)?;
        let matrices = s["joints"]
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
            let read = |name: &str| accessor::read(d, b, a[name].as_u64().unwrap() as usize);
            let pos = read("POSITION")?;
            let j = read("JOINTS_0")?;
            let w = read("WEIGHTS_0")?;
            let used = accessor::read(d, b, p["indices"].as_u64().unwrap() as usize)?
                .into_iter()
                .map(|v| v[0] as usize)
                .collect::<HashSet<_>>();
            let mut used = used.into_iter().collect::<Vec<_>>();
            used.sort();
            for v in used {
                let mut pt = Vec3::ZERO;
                for k in 0..4 {
                    pt += matrices[j[v][k] as usize].transform_point3(Vec3::from_slice(&pos[v]))
                        * w[v][k];
                }
                out.push(pt);
            }
        }
    }
    Ok(out)
}
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let path = Path::new(&args[1]);
    let (mut d, mut b) = glb_io::read(path)?;
    let before = points(&d, &b)?;
    let changed = secondary_rig::garment(&mut d, &mut b, &args[2], "audit")?;
    let after = points(&d, &b)?;
    let max = before
        .iter()
        .zip(&after)
        .map(|(a, b)| a.distance(*b))
        .fold(0f32, f32::max);
    println!(
        "{}",
        json!({"input":path,"changed":changed,"vertices":before.len(),"maxBindDisplacementMeters":max})
    );
    if let Some(output) = args.get(3) {
        glb_io::write(Path::new(output), &d, &b)?;
    }
    Ok(())
}
