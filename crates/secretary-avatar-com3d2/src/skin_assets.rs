//! Normalize only skin albedo to a shared neutral reference. The source texture
//! pixels and shader descriptions remain embedded for provenance and recovery.
use crate::{
    accessor,
    compose::scene_frames,
    glb_io, invalid,
    skin_color::{self, DETAIL_GAIN},
    texture::Texture,
    Result,
};
use glam::Vec3;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    path::Path,
};
struct Pixels {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}
impl Pixels {
    fn decode(bytes: &[u8]) -> Result<Self> {
        if !bytes.starts_with(b"\x89PNG") {
            let mut reader = image::ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|e| invalid(e.to_string()))?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(128 * 1024 * 1024);
            reader.limits(limits);
            let decoded = reader
                .decode()
                .map_err(|e| invalid(e.to_string()))?
                .to_rgba8();
            return Ok(Self {
                w: decoded.width() as usize,
                h: decoded.height() as usize,
                rgba: decoded.into_raw(),
            });
        }
        let mut d = png::Decoder::new(Cursor::new(bytes));
        d.set_limits(png::Limits {
            bytes: 128 * 1024 * 1024,
        });
        d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut r = d.read_info().map_err(|e| invalid(e.to_string()))?;
        let mut buffer = vec![
            0;
            r.output_buffer_size()
                .filter(|n| *n <= 128 * 1024 * 1024)
                .ok_or_else(|| invalid("skin image limit"))?
        ];
        let info = r
            .next_frame(&mut buffer)
            .map_err(|e| invalid(e.to_string()))?;
        let (w, h) = (info.width as usize, info.height as usize);
        let mut rgba = Vec::with_capacity(w * h * 4);
        let channels = info.color_type.samples();
        for p in buffer[..info.buffer_size()].chunks_exact(channels) {
            match channels {
                4 => rgba.extend(p),
                3 => rgba.extend([p[0], p[1], p[2], 255]),
                2 => rgba.extend([p[0], p[0], p[0], p[1]]),
                1 => rgba.extend([p[0], p[0], p[0], 255]),
                _ => return Err(invalid("skin PNG channels")),
            }
        }
        Ok(Self { w, h, rgba })
    }
    fn at(&self, uv: [f32; 2]) -> [f32; 3] {
        let x = (uv[0].rem_euclid(1.) * self.w as f32) as usize;
        let y = (uv[1].rem_euclid(1.) * self.h as f32) as usize;
        let p = (y.min(self.h - 1) * self.w + x.min(self.w - 1)) * 4;
        std::array::from_fn(|i| skin_color::linear(self.rgba[p + i] as f32 / 255.))
    }
    fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.w as u32, self.h as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|e| invalid(e.to_string()))?;
            writer
                .write_image_data(&self.rgba)
                .map_err(|e| invalid(e.to_string()))?;
        }
        Ok(out)
    }
}
fn reference(image: &Pixels, samples: &[[f32; 2]]) -> [f32; 3] {
    let mut values = [Vec::new(), Vec::new(), Vec::new()];
    for uv in samples {
        let p = image.at(*uv);
        if p.iter().all(|v| *v > 0.04) {
            for i in 0..3 {
                values[i].push(p[i]);
            }
        }
    }
    if values[0].len() < 4 {
        for p in image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .step_by((image.w * image.h / 10000).max(1))
        {
            if p[3] > 240 && p[0] > 80 && p[1] > 60 && p[2] > 40 {
                for i in 0..3 {
                    values[i].push(skin_color::linear(p[i] as f32 / 255.));
                }
            }
        }
    }
    std::array::from_fn(|i| {
        values[i].sort_by(f32::total_cmp);
        values[i]
            .get(values[i].len() / 2)
            .copied()
            .unwrap_or(1.)
            .max(0.02)
    })
}
pub fn is_skin(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "faceskin" || n == "facecolor" || (n.contains("skin") && !n.contains("black"))
}
fn image_index(doc: &Value, texture: usize) -> Result<usize> {
    doc["textures"][texture]["source"]
        .as_u64()
        .or_else(|| doc["textures"][texture]["extensions"]["EXT_texture_webp"]["source"].as_u64())
        .map(|i| i as usize)
        .ok_or_else(|| invalid("skin image index"))
}
fn embedded(doc: &Value, data: &[u8], texture: usize) -> Result<Vec<u8>> {
    let image = image_index(doc, texture)?;
    let image = &doc["images"][image];
    if !["image/png", "image/jpeg", "image/webp"]
        .contains(&image["mimeType"].as_str().unwrap_or(""))
    {
        return Err(invalid("unsupported skin albedo codec"));
    }
    let view = &doc["bufferViews"][image["bufferView"]
        .as_u64()
        .ok_or_else(|| invalid("skin image view"))? as usize];
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
    let len = view["byteLength"]
        .as_u64()
        .ok_or_else(|| invalid("skin image length"))? as usize;
    Ok(data
        .get(
            start
                ..start
                    .checked_add(len)
                    .ok_or_else(|| invalid("skin image overflow"))?,
        )
        .ok_or_else(|| invalid("skin image bounds"))?
        .to_vec())
}
fn texture(doc: &mut Value, data: &mut Vec<u8>, image: &Pixels, name: &str) -> Result<usize> {
    let png = image.encode()?;
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let start = data.len();
    data.extend(&png);
    let view = doc["bufferViews"].as_array().unwrap().len();
    doc["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":start,"byteLength":png.len()}));
    for key in ["images", "textures"] {
        if !doc[key].is_array() {
            doc[key] = json!([]);
        }
    }
    let i = doc["images"].as_array().unwrap().len();
    doc["images"].as_array_mut().unwrap().push(json!({"name":name,"bufferView":view,"mimeType":"image/png","extras":{"secretaryNormalizedSkin":true}}));
    let t = doc["textures"].as_array().unwrap().len();
    doc["textures"]
        .as_array_mut()
        .unwrap()
        .push(json!({"source":i}));
    Ok(t)
}
pub fn normalize(input: &Path, output: &Path, shadow: Option<&Path>) -> Result<Value> {
    let (mut doc, mut data) = glb_io::read(input)?;
    let (_, _, world) = scene_frames(&doc)?;
    let mut samples = HashMap::<usize, Vec<[f32; 2]>>::new();
    let mut fallback_samples = HashMap::<usize, Vec<[f32; 2]>>::new();
    for (i, node) in doc["nodes"].as_array().unwrap().iter().enumerate() {
        let Some(mesh) = node["mesh"].as_u64() else {
            continue;
        };
        for p in doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .ok_or_else(|| invalid("skin primitive"))?
        {
            let Some(m) = p["material"].as_u64() else {
                continue;
            };
            if !is_skin(doc["materials"][m as usize]["name"].as_str().unwrap_or("")) {
                continue;
            }
            let Some(uv) = p["attributes"]["TEXCOORD_0"].as_u64() else {
                continue;
            };
            let positions = accessor::read(
                &doc,
                &data,
                p["attributes"]["POSITION"]
                    .as_u64()
                    .ok_or_else(|| invalid("skin positions"))? as usize,
            )?;
            let uv = accessor::read(&doc, &data, uv as usize)?;
            let ids = if let Some(a) = p["indices"].as_u64() {
                accessor::read(&doc, &data, a as usize)?
                    .iter()
                    .map(|r| r[0] as usize)
                    .collect::<HashSet<_>>()
            } else {
                (0..positions.len()).collect()
            };
            for v in ids {
                let u = uv
                    .get(v)
                    .filter(|r| r.len() == 2)
                    .ok_or_else(|| invalid("skin UV"))?;
                fallback_samples
                    .entry(m as usize)
                    .or_default()
                    .push([u[0], u[1]]);
                let pt = world[i].transform_point3(Vec3::from_slice(
                    positions.get(v).ok_or_else(|| invalid("skin vertex"))?,
                ));
                if (1.37..1.49).contains(&pt.y) && pt.x.abs() < 0.07 && pt.z.abs() < 0.15 {
                    let u = uv
                        .get(v)
                        .filter(|r| r.len() == 2)
                        .ok_or_else(|| invalid("skin UV"))?;
                    samples.entry(m as usize).or_default().push([u[0], u[1]]);
                }
            }
        }
    }
    for (m, uv) in fallback_samples {
        samples.entry(m).or_insert(uv);
    }
    let mut shared_samples = HashMap::<usize, Vec<[f32; 2]>>::new();
    for (m, uv) in &samples {
        if let Some(t) =
            doc["materials"][*m]["pbrMetallicRoughness"]["baseColorTexture"]["index"].as_u64()
        {
            shared_samples
                .entry(image_index(&doc, t as usize)?)
                .or_default()
                .extend(uv);
        }
    }
    let shadow = shadow
        .map(|path| {
            std::fs::read(path)
                .map_err(Into::into)
                .and_then(|b| Texture::read(&b))
                .and_then(|t| Pixels::decode(&t.encoded))
        })
        .transpose()?;
    let default = skin_color::color(skin_color::DEFAULT_SKIN)?;
    let materials = doc["materials"]
        .as_array()
        .ok_or_else(|| invalid("skin materials"))?
        .clone();
    let mut report = Vec::new();
    for (i, original) in materials.iter().enumerate() {
        let name = original["name"].as_str().unwrap_or("");
        if !is_skin(name) {
            continue;
        }
        if original["extras"]["secretarySkin"]["version"] == 2 {
            continue;
        }
        let flat = (skin_color::srgb(1. / DETAIL_GAIN) * 255.).round() as u8;
        let mut image = if let Some(t) =
            original["pbrMetallicRoughness"]["baseColorTexture"]["index"].as_u64()
        {
            Pixels::decode(&embedded(&doc, &data, t as usize)?)?
        } else {
            Pixels {
                w: 1,
                h: 1,
                rgba: vec![255; 4],
            }
        };
        let original_texture = original["pbrMetallicRoughness"]["baseColorTexture"]["index"]
            .as_u64()
            .map(|t| image_index(&doc, t as usize))
            .transpose()?;
        let sample = original_texture
            .and_then(|t| shared_samples.get(&t))
            .or_else(|| samples.get(&i))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let reference = reference(&image, sample);
        let shadow_reference = shadow
            .as_ref()
            .map(|s| crate::skin_assets::reference(s, sample));
        let use_shadow = name.to_lowercase().starts_with("skin_") && shadow.is_some();
        for (j, pixel) in image.rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let uv = [
                ((j % image.w) as f32 + 0.5) / image.w as f32,
                ((j / image.w) as f32 + 0.5) / image.h as f32,
            ];
            let sh = if use_shadow {
                shadow.as_ref().unwrap().at(uv)
            } else {
                [1.; 3]
            };
            for c in 0..3 {
                let detail = skin_color::linear(pixel[c] as f32 / 255.) / reference[c];
                let shade = if use_shadow {
                    0.85 + 0.15 * (sh[c] / shadow_reference.unwrap()[c]).clamp(0., 1.3)
                } else {
                    1.
                };
                pixel[c] = (skin_color::srgb((detail * shade / DETAIL_GAIN).clamp(0., 1.)) * 255.)
                    .round() as u8;
            }
        }
        if original["pbrMetallicRoughness"]["baseColorTexture"].is_null() {
            image.rgba = vec![flat, flat, flat, 255];
        }
        let t = texture(&mut doc, &mut data, &image, &format!("{name}_shared_skin"))?;
        let m = &mut doc["materials"][i];
        m["pbrMetallicRoughness"]["baseColorFactor"] = json!([
            default[0] * DETAIL_GAIN,
            default[1] * DETAIL_GAIN,
            default[2] * DETAIL_GAIN,
            1.
        ]);
        m["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index":t});
        m["pbrMetallicRoughness"]["metallicFactor"] = json!(0);
        m["pbrMetallicRoughness"]["roughnessFactor"] = json!(0.88);
        if let Some(e) = m["extensions"].as_object_mut() {
            e.remove("KHR_materials_unlit");
        }
        m["extras"]["secretarySkin"] = json!({"version":2,"uniform":"skinColor","gain":DETAIL_GAIN,"reference":reference,"sourceShadowBlend":if use_shadow{0.15}else{0.},"originalAlbedo":original["pbrMetallicRoughness"]});
        report.push(json!({"material":name,"reference":reference,"samples":sample.len(),"shadowDetail":use_shadow}));
    }
    // The safe-body's separate black surface remains a smooth, nongraphic
    // body surface. Reuse the corresponding skin UV map instead of its black map.
    let main = doc["materials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("skin_") && !n.contains("black"))
        })
        .cloned();
    let mut masks = 0;
    if let Some(main) = main {
        for m in doc["materials"].as_array_mut().unwrap() {
            if m["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("skin_safe_black"))
            {
                m["pbrMetallicRoughness"] = main["pbrMetallicRoughness"].clone();
                if let Some(e) = m["extensions"].as_object_mut() {
                    e.remove("KHR_materials_unlit");
                }
                m["extras"]["secretarySkin"] = main["extras"]["secretarySkin"].clone();
                m["extras"]["safeSurfaceRecolored"] = json!(true);
                masks += 1;
            }
        }
    }
    let skin_ids = doc["materials"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, m)| m["extras"]["secretarySkin"]["version"] == 2)
        .map(|(i, _)| i)
        .collect::<HashSet<_>>();
    for mesh in doc["meshes"].as_array_mut().unwrap() {
        for p in mesh["primitives"].as_array_mut().unwrap() {
            if p["material"]
                .as_u64()
                .is_some_and(|m| skin_ids.contains(&(m as usize)))
            {
                if let Some(colors) = p["attributes"].as_object_mut().unwrap().remove("COLOR_0") {
                    p["extras"]["sourceShaderColorMask"] = colors;
                }
            }
        }
    }
    doc["buffers"][0]["byteLength"] = json!(data.len());
    doc["extras"]["sharedSkinColor"] = json!({"version":2,"default":skin_color::DEFAULT_SKIN,"gain":DETAIL_GAIN,"sourceTexturesRetained":true,"blackSurfacesRecolored":masks});
    glb_io::write(output, &doc, &data)?;
    Ok(json!({"output":output,"materials":report,"blackSurfacesRecolored":masks}))
}
