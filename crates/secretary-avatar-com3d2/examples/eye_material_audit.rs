use secretary_avatar_com3d2::{accessor, glb_io};
use std::collections::HashMap;
fn root(p: &mut [usize], i: usize) -> usize {
    if p[i] != i {
        p[i] = root(p, p[i]);
    }
    p[i]
}
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let (d, b) = glb_io::read(std::path::Path::new(&args[1])).unwrap();
    for m in d["meshes"].as_array().unwrap() {
        for p in m["primitives"].as_array().unwrap() {
            let mat = &d["materials"][p["material"].as_u64().unwrap() as usize];
            let name = mat["name"].as_str().unwrap_or("");
            if !["FaceColor", "FaceSkin", "EyeL", "EyeR", "EyeHL", "Lash"].contains(&name) {
                continue;
            }
            let pos = accessor::read(
                &d,
                &b,
                p["attributes"]["POSITION"].as_u64().unwrap() as usize,
            )
            .unwrap();
            let indices = accessor::read(&d, &b, p["indices"].as_u64().unwrap() as usize).unwrap();
            let mut parent = (0..pos.len()).collect::<Vec<_>>();
            for t in indices.as_chunks::<3>().0 {
                let a = root(&mut parent, t[0][0] as usize);
                for v in &t[1..] {
                    let j = root(&mut parent, v[0] as usize);
                    parent[j] = a;
                }
            }
            let mut components = HashMap::<usize, Vec<usize>>::new();
            for v in &indices {
                let v = v[0] as usize;
                let r = root(&mut parent, v);
                components.entry(r).or_default().push(v);
            }
            let mut c = components
                .into_values()
                .map(|v| {
                    let mut lo = [f32::INFINITY; 3];
                    let mut hi = [f32::NEG_INFINITY; 3];
                    for &i in &v {
                        for a in 0..3 {
                            lo[a] = lo[a].min(pos[i][a]);
                            hi[a] = hi[a].max(pos[i][a]);
                        }
                    }
                    (v.len() / 3, lo, hi)
                })
                .collect::<Vec<_>>();
            c.sort_by_key(|v| std::cmp::Reverse(v.0));
            println!("{name}: {c:?}");
            if let Some(i) = mat["pbrMetallicRoughness"]["baseColorTexture"]["index"].as_u64() {
                let image = d["textures"][i as usize]["source"].as_u64().unwrap() as usize;
                let view =
                    &d["bufferViews"][d["images"][image]["bufferView"].as_u64().unwrap() as usize];
                let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
                let len = view["byteLength"].as_u64().unwrap() as usize;
                std::fs::write(
                    format!("/tmp/lucina-eye-{name}.png"),
                    &b[start..start + len],
                )
                .unwrap();
            }
        }
    }
}
