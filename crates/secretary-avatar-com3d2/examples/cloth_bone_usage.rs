use secretary_avatar_com3d2::{accessor, glb_io, Result};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};
fn main() -> Result<()> {
    let a = std::env::args().collect::<Vec<_>>();
    let (d, b) = glb_io::read(Path::new(&a[1]))?;
    let mut sums = BTreeMap::<String, f32>::new();
    for n in d["nodes"].as_array().unwrap() {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let bones = d["skins"][skin as usize]["joints"].as_array().unwrap();
        for p in d["meshes"][mesh as usize]["primitives"].as_array().unwrap() {
            let material = d["materials"][p["material"].as_u64().unwrap_or(0) as usize]["name"]
                .as_str()
                .unwrap_or("");
            if material != "Dress" && !material.starts_with("Clothes") {
                continue;
            }
            let at = &p["attributes"];
            let j = accessor::read(&d, &b, at["JOINTS_0"].as_u64().unwrap() as usize)?;
            let w = accessor::read(&d, &b, at["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let used = accessor::read(&d, &b, p["indices"].as_u64().unwrap() as usize)?
                .into_iter()
                .map(|v| v[0] as usize)
                .collect::<HashSet<_>>();
            for i in used {
                for k in 0..4 {
                    if w[i][k] < 0.01 {
                        continue;
                    }
                    let node = bones[j[i][k] as usize].as_u64().unwrap() as usize;
                    let name = d["nodes"][node]["name"].as_str().unwrap_or("");
                    if name.contains("mnt") || name.contains("mant_") {
                        *sums.entry(name.into()).or_default() += w[i][k];
                    }
                }
            }
        }
    }
    println!("{}", json!(sums));
    Ok(())
}
