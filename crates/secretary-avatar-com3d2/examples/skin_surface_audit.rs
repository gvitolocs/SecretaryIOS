use glam::Vec3;
use secretary_avatar_com3d2::{accessor, glb_io, skin_color};
use std::collections::HashMap;
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let (d, b) = glb_io::read(std::path::Path::new(&args[1])).unwrap();
    let mut images = HashMap::new();
    for (i, m) in d["materials"].as_array().unwrap().iter().enumerate() {
        if m["extras"]["secretarySkin"]["version"] != 2 {
            continue;
        }
        let t = m["pbrMetallicRoughness"]["baseColorTexture"]["index"]
            .as_u64()
            .unwrap() as usize;
        let im = d["textures"][t]["source"].as_u64().unwrap() as usize;
        let v = &d["bufferViews"][d["images"][im]["bufferView"].as_u64().unwrap() as usize];
        let s = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        let n = v["byteLength"].as_u64().unwrap() as usize;
        let mut reader = png::Decoder::new(std::io::Cursor::new(&b[s..s + n]))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(frame.color_type, png::ColorType::Rgba);
        images.insert(i, (frame.width, frame.height, pixels));
    }
    for m in d["meshes"].as_array().unwrap() {
        for p in m["primitives"].as_array().unwrap() {
            let id = p["material"].as_u64().unwrap() as usize;
            let Some(image) = images.get(&id) else {
                continue;
            };
            let name = d["materials"][id]["name"].as_str().unwrap();
            let read = |a: &str| {
                accessor::read(&d, &b, p["attributes"][a].as_u64().unwrap() as usize).unwrap()
            };
            let ps = read("POSITION");
            let ns = read("NORMAL");
            let uv = read("TEXCOORD_0");
            let indices = accessor::read(&d, &b, p["indices"].as_u64().unwrap() as usize).unwrap();
            let mut count = 0.;
            let mut normal = Vec3::ZERO;
            let mut color = Vec3::ZERO;
            let mut dot = 0.;
            for tri in indices.as_chunks::<3>().0 {
                let ii = std::array::from_fn::<_, 3, _>(|k| tri[k][0] as usize);
                let point = ii.map(|i| Vec3::from_slice(&ps[i])).iter().sum::<Vec3>() / 3.;
                let face = name == "FaceSkin"
                    && point.y > 1.47
                    && point.y < 1.58
                    && point.x.abs() < 0.07
                    && point.z < -0.04;
                let body = name == "skin_safe3"
                    && point.y > 1.10
                    && point.y < 1.3
                    && point.x.abs() < 0.15
                    && point.z < -0.04;
                if !face && !body {
                    continue;
                }
                let n = ii
                    .map(|i| Vec3::from_slice(&ns[i]))
                    .iter()
                    .sum::<Vec3>()
                    .normalize_or_zero();
                let geo = (Vec3::from_slice(&ps[ii[1]]) - Vec3::from_slice(&ps[ii[0]]))
                    .cross(Vec3::from_slice(&ps[ii[2]]) - Vec3::from_slice(&ps[ii[0]]))
                    .normalize_or_zero();
                let u = ii.iter().map(|&i| uv[i][0]).sum::<f32>() / 3.;
                let v = ii.iter().map(|&i| uv[i][1]).sum::<f32>() / 3.;
                let x = ((u.rem_euclid(1.) * image.0 as f32) as u32).min(image.0 - 1);
                let y = ((v.rem_euclid(1.) * image.1 as f32) as u32).min(image.1 - 1);
                let offset = ((y * image.0 + x) * 4) as usize;
                let p = &image.2[offset..offset + 4];
                color += Vec3::new(
                    skin_color::linear(p[0] as f32 / 255.),
                    skin_color::linear(p[1] as f32 / 255.),
                    skin_color::linear(p[2] as f32 / 255.),
                ) * skin_color::DETAIL_GAIN;
                normal += n;
                dot += n.dot(geo);
                count += 1.;
            }
            if count > 0. {
                println!(
                    "{name} triangles={count} mean_normal={:?} winding_dot={} neutral_detail={:?}",
                    normal / count,
                    dot / count,
                    color / count
                );
            }
        }
    }
}
