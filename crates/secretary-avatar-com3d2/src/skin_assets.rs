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
pub(crate) fn png(encoded: &[u8]) -> Result<Vec<u8>> {
    let pixels = Pixels::decode(encoded)?;
    if encoded.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(encoded.to_vec())
    } else {
        pixels.encode()
    }
}
/// Desktop Unity AG normal encoding. Keep the original RGB texture separately
/// for the Rust/PBR renderer and platforms that use RGB normals.
pub(crate) fn normal_ag(encoded: &[u8]) -> Result<Vec<u8>> {
    let mut p = Pixels::decode(encoded)?;
    for pixel in p.rgba.as_chunks_mut::<4>().0 {
        let x = pixel[0];
        let y = 255 - pixel[1];
        *pixel = [255, y, 0, x];
    }
    p.encode()
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
    n == "faceskin" || (n.contains("skin") && !n.contains("black"))
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
    let (mut doc, mut data) = glb_io::read(input)?; // Lucina's FaceColor primitive holds sclera, teeth and mouth features;
                                                    // actual face skin is FaceSkin. Shared skin tint must not color features.
    for i in 0..doc["materials"]
        .as_array()
        .ok_or_else(|| invalid("skin materials"))?
        .len()
    {
        if doc["materials"][i]["name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case("FaceColor"))
        {
            if doc["materials"][i]["extras"]["secretarySkin"]["version"] == 2 {
                if !doc["materials"][i]["extras"]["nativePack"].is_null() {
                    return Err(invalid("restore FaceColor from the pre-native source: original texture indices were remapped"));
                }
                let original =
                    doc["materials"][i]["extras"]["secretarySkin"]["originalAlbedo"].clone();
                if !original.is_object() {
                    return Err(invalid("missing original face feature albedo"));
                }
                if let Some(t) = original["baseColorTexture"]["index"].as_u64() {
                    embedded(&doc, &data, t as usize)?;
                }
                doc["materials"][i]["pbrMetallicRoughness"] = original;
            }
            let m = &mut doc["materials"][i];
            m["pbrMetallicRoughness"]["baseColorFactor"] = json!([1, 1, 1, 1]);
            m["pbrMetallicRoughness"]["metallicFactor"] = json!(0);
            if let Some(extras) = m["extras"].as_object_mut() {
                extras.remove("secretarySkin");
            }
            m["extras"]["secretaryFeatureColor"] = json!({"version":1,"surface":"eye whites, teeth and mouth features","skinTint":false});
            m["extensions"]["KHR_materials_unlit"] = json!({});
            if !doc["extensionsUsed"].is_array() {
                doc["extensionsUsed"] = json!([]);
            }
            if !doc["extensionsUsed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "KHR_materials_unlit")
            {
                doc["extensionsUsed"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("KHR_materials_unlit"));
            }
        }
    }

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
    // body surface. Its UV islands do not match the main skin atlas, so use a
    // neutral detail texel rather than sampling a dark atlas island.
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
        let flat = (skin_color::srgb(1. / DETAIL_GAIN) * 255.).round() as u8;
        let safe_texture = texture(
            &mut doc,
            &mut data,
            &Pixels {
                w: 1,
                h: 1,
                rgba: vec![flat, flat, flat, 255],
            },
            "safe_surface_shared_skin",
        )?;
        for m in doc["materials"].as_array_mut().unwrap() {
            if m["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("skin_safe_black"))
            {
                m["pbrMetallicRoughness"] = main["pbrMetallicRoughness"].clone();
                m["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index":safe_texture});
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn different_source_tints_share_color_and_safe_patch_without_tinting_hair() {
        let dir = std::env::temp_dir().join(format!("skin-uniform-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("input.glb");
        let output = dir.join("output.glb");
        let mut doc = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[]}],"nodes":[],"meshes":[],"buffers":[{"byteLength":0}],"bufferViews":[],"materials":[]});
        let mut data = Vec::new();
        for (name, rgb) in [
            ("skin_safe3", [230, 170, 130]),
            ("FaceSkin", [200, 190, 185]),
            ("skin_safe_black3", [0, 0, 0]),
            ("Hair", [25, 30, 80]),
        ] {
            let t = texture(
                &mut doc,
                &mut data,
                &Pixels {
                    w: 2,
                    h: 2,
                    rgba: rgb.into_iter().chain([255]).cycle().take(16).collect(),
                },
                name,
            )
            .unwrap();
            doc["materials"].as_array_mut().unwrap().push(json!({"name":name,"pbrMetallicRoughness":{"baseColorTexture":{"index":t},"baseColorFactor":[1,1,1,1]}}));
        }
        doc["buffers"][0]["byteLength"] = json!(data.len());
        let hair = doc["materials"][3].clone();
        glb_io::write(&input, &doc, &data).unwrap();
        let report = normalize(&input, &output, None).unwrap();
        assert_eq!(report["blackSurfacesRecolored"], 1);
        let (out, bytes) = glb_io::read(&output).unwrap();
        for i in 0..3 {
            assert_eq!(
                out["materials"][i]["extras"]["secretarySkin"]["uniform"],
                "skinColor"
            );
            assert_eq!(
                out["materials"][i]["pbrMetallicRoughness"]["baseColorFactor"],
                out["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"]
            );
            let t = out["materials"][i]["pbrMetallicRoughness"]["baseColorTexture"]["index"]
                .as_u64()
                .unwrap() as usize;
            let px = Pixels::decode(&embedded(&out, &bytes, t).unwrap()).unwrap();
            let reference = skin_color::linear(px.rgba[0] as f32 / 255.) * DETAIL_GAIN;
            assert!((reference - 1.).abs() < 0.015);
        }
        assert_ne!(
            out["materials"][2]["pbrMetallicRoughness"]["baseColorTexture"],
            out["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]
        );
        assert_eq!(out["materials"][3], hair);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[cfg(test)]
mod eye_tests {
    use super::*;
    #[test]
    fn eye_whites_restore_source_pixels_and_never_join_shared_skin_tint() {
        let dir = std::env::temp_dir().join(format!("lucina-sclera-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut doc = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[]}],"nodes":[],"meshes":[],"buffers":[{"byteLength":0}],"bufferViews":[],"materials":[]});
        let mut data = Vec::new();
        let white = Pixels {
            w: 1,
            h: 1,
            rgba: vec![255; 4],
        };
        let white_tex = texture(&mut doc, &mut data, &white, "original_sclera").unwrap();
        let pink_tex = texture(
            &mut doc,
            &mut data,
            &Pixels {
                w: 1,
                h: 1,
                rgba: vec![230, 170, 150, 255],
            },
            "old_tinted_features",
        )
        .unwrap();
        doc["materials"] = json!([{"name":"FaceColor","pbrMetallicRoughness":{"baseColorTexture":{"index":pink_tex},"baseColorFactor":[0.8,0.5,0.4,1]},"extras":{"secretarySkin":{"version":2,"originalAlbedo":{"baseColorTexture":{"index":white_tex}}}}},{"name":"FaceSkin","pbrMetallicRoughness":{"baseColorTexture":{"index":white_tex}}}]);
        doc["buffers"][0]["byteLength"] = json!(data.len());
        let input = dir.join("input.glb");
        let output = dir.join("output.glb");
        glb_io::write(&input, &doc, &data).unwrap();
        normalize(&input, &output, None).unwrap();
        let (out, bytes) = glb_io::read(&output).unwrap();
        let eye = &out["materials"][0];
        assert!(eye["extras"]["secretarySkin"].is_null());
        assert_eq!(
            eye["pbrMetallicRoughness"]["baseColorFactor"],
            json!([1, 1, 1, 1])
        );
        assert_eq!(
            eye["pbrMetallicRoughness"]["baseColorTexture"]["index"],
            white_tex
        );
        assert_eq!(
            Pixels::decode(&embedded(&out, &bytes, white_tex).unwrap())
                .unwrap()
                .rgba,
            vec![255; 4]
        );
        assert_eq!(
            out["materials"][1]["extras"]["secretarySkin"]["uniform"],
            "skinColor"
        );
        assert!(!is_skin("FaceColor"));
        assert!(is_skin("FaceSkin"));
        normalize(&output, &output, None).unwrap();
        let (again, _) = glb_io::read(&output).unwrap();
        assert_eq!(again["materials"][0], *eye);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Match visible frontal skin patches to one neutral albedo anchor. Geometry,
/// UVs and the original maps remain intact; feature/eye materials are excluded.
pub fn match_tone(input: &Path, output: &Path) -> Result<Value> {
    let (mut doc, mut data) = glb_io::read(input)?;
    let (_, _, world) = scene_frames(&doc)?;
    let mut samples = HashMap::<usize, Vec<[f32; 3]>>::new();
    let mut decoded = HashMap::<usize, Pixels>::new();
    for (ni, node) in doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("tone nodes"))?
        .iter()
        .enumerate()
    {
        let Some(mesh) = node["mesh"].as_u64() else {
            continue;
        };
        for primitive in doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .ok_or_else(|| invalid("tone primitives"))?
        {
            let Some(material) = primitive["material"].as_u64() else {
                continue;
            };
            let material = material as usize;
            let m = &doc["materials"][material];
            let name = m["name"].as_str().unwrap_or("");
            if !["FaceSkin", "skin_safe3"].contains(&name)
                || m["extras"]["secretarySkin"]["version"] != 2
                || m["extras"]["secretarySkin"]["toneAnchor"]["version"] == 1
            {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = decoded.entry(material) {
                let t = m["pbrMetallicRoughness"]["baseColorTexture"]["index"]
                    .as_u64()
                    .ok_or_else(|| invalid("tone texture"))? as usize;
                entry.insert(Pixels::decode(&embedded(&doc, &data, t)?)?);
            }
            let read = |key: &str| {
                accessor::read(
                    &doc,
                    &data,
                    primitive["attributes"][key]
                        .as_u64()
                        .ok_or_else(|| invalid("tone attribute"))? as usize,
                )
            };
            let ps = read("POSITION")?;
            let ns = read("NORMAL")?;
            let uv = read("TEXCOORD_0")?;
            let indices = accessor::read(
                &doc,
                &data,
                primitive["indices"]
                    .as_u64()
                    .ok_or_else(|| invalid("tone indices"))? as usize,
            )?;
            for tri in indices.as_chunks::<3>().0 {
                let ids = std::array::from_fn::<_, 3, _>(|i| tri[i][0] as usize);
                if ids
                    .iter()
                    .any(|i| *i >= ps.len() || *i >= ns.len() || *i >= uv.len())
                {
                    return Err(invalid("tone triangle bounds"));
                }
                let point = ids
                    .map(|i| world[ni].transform_point3(Vec3::from_slice(&ps[i])))
                    .iter()
                    .sum::<Vec3>()
                    / 3.;
                let normal = ids
                    .map(|i| {
                        world[ni]
                            .inverse()
                            .transpose()
                            .transform_vector3(Vec3::from_slice(&ns[i]))
                    })
                    .iter()
                    .sum::<Vec3>()
                    .normalize_or_zero();
                let region = if name == "FaceSkin" {
                    point.y > 1.47 && point.y < 1.58 && point.x.abs() < 0.07
                } else {
                    point.y > 1.10 && point.y < 1.30 && point.x.abs() < 0.15
                };
                if !region || point.z >= -0.04 || normal.z >= -0.65 {
                    continue;
                }
                let coord =
                    std::array::from_fn(|a| ids.iter().map(|i| uv[*i][a]).sum::<f32>() / 3.);
                let rgb = decoded[&material].at(coord);
                if rgb.iter().any(|c| *c < 0.05) {
                    continue;
                }
                samples.entry(material).or_default().push(rgb);
            }
        }
    }
    let mut report = Vec::new();
    for (material, colors) in samples {
        if colors.is_empty() {
            continue;
        }
        let reference = std::array::from_fn::<_, 3, _>(|c| {
            let mut values = colors.iter().map(|v| v[c]).collect::<Vec<_>>();
            values.sort_by(f32::total_cmp);
            values[values.len() / 2] * DETAIL_GAIN
        });
        if reference
            .iter()
            .any(|v| !v.is_finite() || !(0.25..=2.5).contains(v))
        {
            return Err(invalid("skin tone reference outside calibration range"));
        }
        let mut image = decoded.remove(&material).unwrap();
        for p in image.rgba.as_chunks_mut::<4>().0 {
            for c in 0..3 {
                p[c] = (skin_color::srgb(
                    (skin_color::linear(p[c] as f32 / 255.) / reference[c]).clamp(0., 1.),
                ) * 255.)
                    .round() as u8;
            }
        }
        let name = doc["materials"][material]["name"]
            .as_str()
            .unwrap()
            .to_owned();
        let t = texture(&mut doc, &mut data, &image, &format!("{name}_matched_tone"))?;
        doc["materials"][material]["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index":t});
        doc["materials"][material]["extras"]["secretarySkin"]["toneAnchor"] = json!({"version":1,"reference":reference,"sampleCount":colors.len(),"method":"median frontal skin albedo in linear light"});
        report.push(json!({"material":name,"reference":reference,"samples":colors.len()}));
    }
    doc["buffers"][0]["byteLength"] = json!(data.len());
    glb_io::write(output, &doc, &data)?;
    Ok(json!({"materials":report,"output":output}))
}
#[cfg(test)]
mod tone_tests {
    use super::*;
    #[test]
    fn different_face_and_body_bases_match_without_recoloring_eyes() {
        let dir = std::env::temp_dir().join(format!("skin-tone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"name":"skin","mesh":0}],"meshes":[{"primitives":[]}],"materials":[],"accessors":[],"buffers":[{"byteLength":0}],"bufferViews":[]});
        let mut bytes = Vec::new();
        for (name, y, rgb) in [
            ("FaceSkin", 1.5, [0.88, 0.92, 0.96]),
            ("skin_safe3", 1.2, [1.04, 1.08, 1.12]),
        ] {
            let pixels = Pixels {
                w: 1,
                h: 1,
                rgba: rgb
                    .map(|v| (skin_color::srgb(v / DETAIL_GAIN) * 255.).round() as u8)
                    .into_iter()
                    .chain([255])
                    .collect(),
            };
            let t = texture(&mut d, &mut bytes, &pixels, name).unwrap();
            let material = d["materials"].as_array().unwrap().len();
            d["materials"].as_array_mut().unwrap().push(json!({"name":name,"pbrMetallicRoughness":{"baseColorTexture":{"index":t}},"extras":{"secretarySkin":{"version":2,"uniform":"skinColor"}}}));
            let p = accessor::floats(
                &mut d,
                &mut bytes,
                &[
                    vec![-0.02, y, -0.06],
                    vec![0.02, y, -0.06],
                    vec![0., y + 0.01, -0.06],
                ],
                3,
            )
            .unwrap();
            let n = accessor::floats(&mut d, &mut bytes, &vec![vec![0., 0., -1.]; 3], 3).unwrap();
            let uv = accessor::floats(&mut d, &mut bytes, &vec![vec![0.5, 0.5]; 3], 2).unwrap();
            let i = accessor::shorts(&mut d, &mut bytes, &[[0, 1, 2, 0]]);
            d["accessors"][i]["type"] = json!("SCALAR");
            d["accessors"][i]["count"] = json!(3);
            d["meshes"][0]["primitives"].as_array_mut().unwrap().push(json!({"attributes":{"POSITION":p,"NORMAL":n,"TEXCOORD_0":uv},"indices":i,"material":material}));
        }
        let eye = json!({"name":"FaceColor","pbrMetallicRoughness":{"baseColorFactor":[1,1,1,1]},"extras":{"secretaryFeatureColor":{"skinTint":false}}});
        d["materials"].as_array_mut().unwrap().push(eye.clone());
        d["buffers"][0]["byteLength"] = json!(bytes.len());
        let input = dir.join("input.glb");
        let output = dir.join("out.glb");
        glb_io::write(&input, &d, &bytes).unwrap();
        let report = match_tone(&input, &output).unwrap();
        assert_eq!(report["materials"].as_array().unwrap().len(), 2);
        let (out, data) = glb_io::read(&output).unwrap();
        let mut samples = Vec::new();
        for i in 0..2 {
            let t = out["materials"][i]["pbrMetallicRoughness"]["baseColorTexture"]["index"]
                .as_u64()
                .unwrap() as usize;
            samples.push(
                Pixels::decode(&embedded(&out, &data, t).unwrap())
                    .unwrap()
                    .rgba,
            );
        }
        assert_eq!(samples[0], samples[1]);
        assert_eq!(out["materials"][2], eye);
        assert_eq!(out["meshes"], d["meshes"]);
        match_tone(&output, &output).unwrap();
        let (again, _) = glb_io::read(&output).unwrap();
        assert_eq!(again["materials"], out["materials"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
