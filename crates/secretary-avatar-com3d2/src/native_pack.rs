//! Convert a bind-pose glTF asset to a native .model/.mate/.tex/.menu pack.
//! Native body bone frames are authoritative; only additional authored facial
//! and accessory joints retain their original world frames. No donor body is added.
use crate::{
    accessor,
    animation::globals,
    compose, glb_io, gltf, invalid,
    menu::Menu,
    model::{Bone, Material, Model, Morph, Skin, Vertex},
    native_write,
    resources::ResourceSource,
    Result,
};
use glam::{Mat4, Quat, Vec3};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
struct Directory {
    root: PathBuf,
    files: Vec<String>,
}
impl ResourceSource for Directory {
    fn paths(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.files.iter().map(String::as_str))
    }
    fn read_resource(&mut self, p: &str) -> Result<Vec<u8>> {
        Ok(fs::read(self.root.join(crate::arc::safe_relative(p)?))?)
    }
}
fn frame(b: &Bone) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::from_array(b.scale),
        Quat::from_array(b.rotation).normalize(),
        Vec3::from_array(b.translation),
    )
}
fn reflection() -> Mat4 {
    Mat4::from_scale(Vec3::new(1., 1., -1.))
}
fn append_png(doc: &mut Value, data: &mut Vec<u8>, png: &[u8]) -> Result<usize> {
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let view = doc["bufferViews"]
        .as_array()
        .ok_or_else(|| invalid("native image views"))?
        .len();
    doc["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":data.len(),"byteLength":png.len()}));
    data.extend(png);
    for key in ["images", "textures"] {
        if !doc[key].is_array() {
            doc[key] = json!([]);
        }
    }
    let image = doc["images"].as_array().unwrap().len();
    doc["images"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bufferView":view,"mimeType":"image/png"}));
    let texture = doc["textures"].as_array().unwrap().len();
    doc["textures"]
        .as_array_mut()
        .unwrap()
        .push(json!({"source":image}));
    Ok(texture)
}
struct Rig {
    bones: Vec<Bone>,
    world: Vec<Mat4>,
    mapped: HashMap<usize, usize>,
    standard: HashMap<String, usize>,
}
impl Rig {
    fn ensure(&mut self, i: usize, doc: &Value, parent: &[i32], world: &[Mat4]) -> Result<usize> {
        if let Some(&j) = self.mapped.get(&i) {
            return Ok(j);
        }
        let name = doc["nodes"][i]["name"]
            .as_str()
            .ok_or_else(|| invalid("unnamed native skin joint"))?;
        let alias = name.strip_suffix("_SCL_").unwrap_or(name);
        if let Some(&j) = self.standard.get(alias) {
            self.mapped.insert(i, j);
            return Ok(j);
        }
        let p = if parent[i] >= 0 {
            Some(self.ensure(parent[i] as usize, doc, parent, world)?)
        } else {
            None
        };
        if self.bones.iter().any(|b| b.name == name) {
            return Err(invalid("duplicate additional native bone"));
        }
        let w = reflection() * world[i] * reflection();
        let local = p.map(|j| self.world[j].inverse() * w).unwrap_or(w);
        let (scale, rotation, translation) = local.to_scale_rotation_translation();
        let rebuilt = Mat4::from_scale_rotation_translation(scale, rotation, translation);
        if rebuilt
            .to_cols_array()
            .iter()
            .zip(local.to_cols_array())
            .any(|(a, b)| (a - b).abs() > 1e-4)
        {
            return Err(invalid("additional bone contains unsupported shear"));
        }
        let j = self.bones.len();
        self.bones.push(Bone {
            name: name.into(),
            parent: p.map(|j| j as i32).unwrap_or(-1),
            scale_helper: false,
            translation: translation.to_array(),
            rotation: rotation.to_array(),
            scale: scale.to_array(),
        });
        self.world.push(w);
        self.mapped.insert(i, j);
        Ok(j)
    }
}
fn embedded(doc: &Value, data: &[u8], texture: usize) -> Result<Vec<u8>> {
    let image = doc["textures"][texture]["source"]
        .as_u64()
        .or_else(|| doc["textures"][texture]["extensions"]["EXT_texture_webp"]["source"].as_u64())
        .ok_or_else(|| invalid("native pack embedded texture source"))? as usize;
    let view = doc["images"][image]["bufferView"]
        .as_u64()
        .ok_or_else(|| invalid("native pack external texture refused"))? as usize;
    let v = &doc["bufferViews"][view];
    let start = v["byteOffset"].as_u64().unwrap_or(0) as usize;
    let n = v["byteLength"]
        .as_u64()
        .ok_or_else(|| invalid("native texture length"))? as usize;
    Ok(data
        .get(
            start
                ..start
                    .checked_add(n)
                    .ok_or_else(|| invalid("native texture overflow"))?,
        )
        .ok_or_else(|| invalid("native texture bounds"))?
        .to_vec())
}
#[derive(Default)]
struct TextureCache {
    rgb: HashMap<usize, String>,
    normal: HashMap<usize, (String, String)>,
}
fn source_texture(
    doc: &Value,
    data: &[u8],
    index: usize,
    location: (&Path, &str),
    files: &mut Vec<String>,
    cache: &mut HashMap<usize, String>,
) -> Result<String> {
    if let Some(n) = cache.get(&index) {
        return Ok(n.clone());
    }
    let (root, prefix) = location;
    let n = format!("{prefix}_tex_{index:04}");
    let png = crate::skin_assets::png(&embedded(doc, data, index)?)?;
    let file = format!("{n}.tex");
    fs::write(root.join(&file), native_write::texture(&n, &png)?)?;
    files.push(file);
    cache.insert(index, n.clone());
    Ok(n)
}
fn extra_texture_infos(value: &Value, path: &str, out: &mut Vec<(String, Value)>) {
    if let Some(object) = value.as_object() {
        for (key, child) in object {
            if key == "extras" {
                continue;
            }
            let pointer = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
            if key.ends_with("Texture") && child["index"].is_number() {
                if pointer != "/normalTexture"
                    && pointer != "/pbrMetallicRoughness/baseColorTexture"
                {
                    out.push((pointer, child.clone()));
                }
            } else {
                extra_texture_infos(child, &pointer, out);
            }
        }
    }
}
fn material(
    doc: &Value,
    data: &[u8],
    index: usize,
    location: (&Path, &str),
    files: &mut Vec<String>,
    texture_cache: &mut TextureCache,
    template: &Material,
) -> Result<(Material, Value)> {
    let (root, prefix) = location;
    let mut original = doc["materials"][index].clone();
    let source = original["extras"]["com3d2"].clone();
    let name = original["name"].as_str().unwrap_or("material").to_owned();
    let mut props = vec![
        json!({"kind":"col","name":"_Color","value":original["pbrMetallicRoughness"]["baseColorFactor"].as_array().cloned().unwrap_or_else(||vec![json!(1);4])}),
    ];
    if let Some(t) = original["pbrMetallicRoughness"]["baseColorTexture"]["index"].as_u64() {
        let name = source_texture(
            doc,
            data,
            t as usize,
            location,
            files,
            &mut texture_cache.rgb,
        )?;
        props.push(json!({"kind":"tex","name":"_MainTex","value":{"subtype":"tex2d","texture":name,"sourcePath":"","offset":[0,0],"scale":[1,1]}}));
    }
    if let Some(t) = original["normalTexture"]["index"].as_u64() {
        if original["normalTexture"]["texCoord"].as_u64().unwrap_or(0) != 0 {
            return Err(invalid("native normal map requires UV0"));
        }
        let t = t as usize;
        let (ag, rgb) = if let Some(names) = texture_cache.normal.get(&t) {
            names.clone()
        } else {
            let rgb = source_texture(doc, data, t, location, files, &mut texture_cache.rgb)?;
            let ag = format!("{prefix}_normal_{t:04}_ag");
            let png = crate::skin_assets::normal_ag(&embedded(doc, data, t)?)?;
            let file = format!("{ag}.tex");
            fs::write(root.join(&file), native_write::texture(&ag, &png)?)?;
            files.push(file);
            texture_cache.normal.insert(t, (ag.clone(), rgb.clone()));
            (ag, rgb)
        };
        let scale = original["normalTexture"]["scale"].as_f64().unwrap_or(1.);
        props.push(json!({"kind":"tex","name":"_BumpMap","value":{"subtype":"tex2d","texture":ag,"sourcePath":"","offset":[0,0],"scale":[1,1]}}));
        props.push(json!({"kind":"f","name":"_BumpScale","value":scale}));
        original["extras"]["nativeNormalSource"] = json!({"rgb":rgb,"ag":ag,"scale":scale,"nativeEncoding":"desktopAG","nativeShaderVerified":false});
    }
    let mut additional = Vec::new();
    extra_texture_infos(&original, "", &mut additional);
    let mut retained = Vec::new();
    for (pointer, info) in additional {
        let index = info["index"].as_u64().unwrap() as usize;
        let name = source_texture(doc, data, index, location, files, &mut texture_cache.rgb)?;
        props.push(json!({"kind":"tex","name":format!("_SecretaryRetainedTexture{}",retained.len()),"value":{"subtype":"tex2d","texture":name,"sourcePath":"","offset":[0,0],"scale":[1,1]}}));
        retained.push(json!({"pointer":pointer,"rgb":name,"info":info}));
    }
    if !retained.is_empty() {
        original["extras"]["nativeAdditionalTextures"] = json!(retained);
    }
    if original["alphaMode"] == "MASK" {
        props.push(json!({"kind":"f","name":"_Cutoff","value":original["alphaCutoff"].as_f64().unwrap_or(0.5)}));
    }
    let shader = source["shader"]
        .as_str()
        .unwrap_or(&template.shader)
        .to_owned();
    let shader_file = source["shader_file"]
        .as_str()
        .unwrap_or(&template.shader_file)
        .to_owned();
    Ok((
        Material {
            name,
            shader,
            shader_file,
            properties: props,
        },
        original,
    ))
}
/// Chunk triangles by unique vertex count, keeping each .model within its U16 limit.
fn chunks(indices: &[usize], limit: usize) -> Result<Vec<(Vec<usize>, Vec<u16>)>> {
    if !indices.len().is_multiple_of(3) || !(3..=65535).contains(&limit) {
        return Err(invalid("native triangle/chunk limit"));
    }
    let mut out = Vec::new();
    let mut map = HashMap::new();
    let mut vertices = Vec::new();
    let mut tri = Vec::new();
    for t in indices.as_chunks::<3>().0 {
        let extra = t
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .iter()
            .filter(|i| !map.contains_key(*i))
            .count();
        if vertices.len() + extra > limit {
            out.push((std::mem::take(&mut vertices), std::mem::take(&mut tri)));
            map.clear();
        }
        for &i in [t[0], t[2], t[1]].iter() {
            let next = map.len() as u16;
            let j = *map.entry(i).or_insert_with(|| {
                vertices.push(i);
                next
            });
            tri.push(j);
        }
    }
    if !tri.is_empty() {
        out.push((vertices, tri));
    }
    Ok(out)
}
pub fn body_masks(category: &str) -> Vec<Vec<String>> {
    if category == "original-uniform" {
        return vec![
            vec!["node消去".into(), "_ALL_".into()],
            vec!["node表示".into(), "Bip01 Neck".into()],
        ];
    }

    if ["skirt", "onepiece", "original-skirt"].contains(&category) {
        ["Hip_L", "Hip_R"]
            .into_iter()
            .map(|n| vec!["node消去".into(), n.into()])
            .collect()
    } else {
        Vec::new()
    }
}
/// Refresh inert visibility records without regenerating unchanged geometry.
pub fn refresh_body_masks(root: &Path, category: &str) -> Result<Vec<Vec<String>>> {
    let rules = body_masks(category);
    if rules.is_empty() {
        return Ok(rules);
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "menu") {
            let mut m = Menu::read(&fs::read(&path)?)?;
            let mut changed = false;
            for rule in &rules {
                if !m.commands.contains(rule) {
                    m.commands.push(rule.clone());
                    changed = true;
                }
            }
            if changed {
                let temp = path.with_extension("menu.pending");
                fs::write(&temp, native_write::menu(&m)?)?;
                fs::rename(temp, path)?;
            }
        }
    }
    Ok(rules)
}
pub fn export(input: &Path, body: &Model, root: &Path, category: &str) -> Result<Value> {
    let requested_category = category;
    let category = match category {
        "original" | "original-skirt" | "original-uniform" => "wear",
        "hairfull" => "hairf",
        "accessories" => "accsenaka",
        "other" => "accude",
        v => v,
    };
    let (mut doc, mut data) = glb_io::read(input)?;
    let rig_key = format!(
        "{:016x}",
        input
            .to_string_lossy()
            .bytes()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b))
                .wrapping_mul(0x100000001b3))
    );
    let secondary_vertices =
        crate::secondary_rig::garment(&mut doc, &mut data, requested_category, &rig_key)?;
    let (parent, _, world) = compose::scene_frames(&doc)?;
    fs::create_dir_all(root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".conversion.lock"))?;
    lock.lock()?;
    if root.join("manifest.json").is_file() {
        fs::rename(
            root.join("manifest.json"),
            root.join("manifest.previous.json"),
        )?;
    }
    fs::create_dir_all(root.join("render"))?;
    let mut rig = Rig {
        bones: body.bones.clone(),
        world: globals(
            &body.bones.iter().map(|b| b.parent).collect::<Vec<_>>(),
            &body.bones.iter().map(frame).collect::<Vec<_>>(),
        )?,
        mapped: HashMap::new(),
        standard: body
            .bones
            .iter()
            .enumerate()
            .map(|(i, b)| (b.name.clone(), i))
            .collect(),
    };
    for s in doc["skins"].as_array().unwrap_or(&Vec::new()) {
        for j in s["joints"]
            .as_array()
            .ok_or_else(|| invalid("native export skin joints"))?
        {
            rig.ensure(
                j.as_u64().ok_or_else(|| invalid("native joint index"))? as usize,
                &doc,
                &parent,
                &world,
            )?;
        }
    }
    let root_index = body
        .bones
        .iter()
        .position(|b| b.name == body.root)
        .ok_or_else(|| invalid("native root missing"))?;
    let root_world = rig.world[root_index];
    let to_native = root_world.inverse() * reflection();
    let prefix = format!(
        "lucina_{:016x}",
        input
            .to_string_lossy()
            .bytes()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b))
                .wrapping_mul(0x100000001b3))
    );
    let mut files = Vec::new();
    let mut assets = Vec::new();
    let mut texture_cache = TextureCache::default();
    let mut metadata = Vec::new();
    let mut expected = Vec::new();
    let mut expressions = HashSet::new();
    let mut unsupported = HashSet::new();
    let template = body
        .materials
        .first()
        .ok_or_else(|| invalid("native shader template"))?;
    for (node, n) in doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("native export nodes"))?
        .iter()
        .enumerate()
    {
        let Some(mesh) = n["mesh"].as_u64() else {
            continue;
        };
        let mesh = &doc["meshes"][mesh as usize];
        let skin = n["skin"].as_u64().map(|s| &doc["skins"][s as usize]);
        let (source_joints, inverse) = if let Some(s) = skin {
            (
                s["joints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|j| j.as_u64().unwrap() as usize)
                    .collect::<Vec<_>>(),
                accessor::read(
                    &doc,
                    &data,
                    s["inverseBindMatrices"]
                        .as_u64()
                        .ok_or_else(|| invalid("native source bind"))? as usize,
                )?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        for p in mesh["primitives"]
            .as_array()
            .ok_or_else(|| invalid("native export primitives"))?
        {
            if p["mode"].as_u64().unwrap_or(4) != 4 {
                return Err(invalid("native export requires triangles"));
            }
            let a = |key: &str| -> Result<Vec<Vec<f32>>> {
                accessor::read(
                    &doc,
                    &data,
                    p["attributes"][key]
                        .as_u64()
                        .ok_or_else(|| invalid(format!("native export requires {key}")))?
                        as usize,
                )
            };
            let positions = a("POSITION")?;
            let normals = a("NORMAL")?;
            let uv = if p["attributes"]["TEXCOORD_0"].is_number() {
                a("TEXCOORD_0")?
            } else {
                vec![vec![0.; 2]; positions.len()]
            };
            let joints = if skin.is_some() {
                a("JOINTS_0")?
            } else {
                vec![vec![0.; 4]; positions.len()]
            };
            let weights = if skin.is_some() {
                a("WEIGHTS_0")?
            } else {
                vec![vec![1., 0., 0., 0.]; positions.len()]
            };
            if [normals.len(), uv.len(), joints.len(), weights.len()]
                .iter()
                .any(|n| *n != positions.len())
            {
                return Err(invalid("native export attribute count"));
            }
            let mut vertices = Vec::new();
            let mut new_skin = Vec::new();
            let mut transforms = Vec::new();
            let mut original_world = Vec::new();
            for v in 0..positions.len() {
                let mut matrix = Mat4::ZERO;
                let mut packed = HashMap::<usize, f32>::new();
                let mut total = 0.;
                if skin.is_some() {
                    for k in 0..4 {
                        let w = weights[v][k];
                        if w <= 0. {
                            continue;
                        }
                        let j = joints[v][k] as usize;
                        let source = *source_joints
                            .get(j)
                            .ok_or_else(|| invalid("native source skin bounds"))?;
                        let target = *rig
                            .mapped
                            .get(&source)
                            .ok_or_else(|| invalid("native mapped skin missing"))?;
                        matrix += (world[source]
                            * Mat4::from_cols_array(
                                inverse
                                    .get(j)
                                    .filter(|m| m.len() == 16)
                                    .ok_or_else(|| invalid("native inverse matrix"))?
                                    .as_slice()
                                    .try_into()
                                    .unwrap(),
                            ))
                            * w;
                        *packed.entry(target).or_default() += w;
                        total += w;
                    }
                } else {
                    matrix = world[node];
                    packed.insert(root_index, 1.);
                    total = 1.;
                }
                if (total - 1.).abs() > 0.001 {
                    return Err(invalid("native source weight sum"));
                }
                matrix *= 1. / total;
                original_world.push(matrix.transform_point3(Vec3::from_slice(&positions[v])));
                let transform = to_native * matrix;
                if transform.determinant().abs() < 1e-8 {
                    return Err(invalid("native export singular vertex transform"));
                }
                vertices.push(Vertex {
                    position: transform
                        .transform_point3(Vec3::from_slice(&positions[v]))
                        .to_array(),
                    normal: transform
                        .inverse()
                        .transpose()
                        .transform_vector3(Vec3::from_slice(&normals[v]))
                        .normalize_or_zero()
                        .to_array(),
                    uv: [uv[v][0], 1. - uv[v][1]],
                });
                transforms.push(transform);
                let mut packed = packed.into_iter().collect::<Vec<_>>();
                packed.sort_by(|a, b| b.1.total_cmp(&a.1));
                packed.truncate(4);
                let sum = packed.iter().map(|p| p.1).sum::<f32>();
                let mut s = Skin {
                    joints: [0; 4],
                    weights: [0.; 4],
                };
                for (k, (j, w)) in packed.iter().enumerate() {
                    s.joints[k] = *j as u16;
                    s.weights[k] = w / sum;
                }
                new_skin.push(s);
            }
            let index = p["indices"]
                .as_u64()
                .map(|i| accessor::read(&doc, &data, i as usize))
                .transpose()?
                .map(|r| r.into_iter().map(|r| r[0] as usize).collect::<Vec<_>>())
                .unwrap_or_else(|| (0..positions.len()).collect());
            if index.iter().any(|i| *i >= positions.len()) {
                return Err(invalid("native source triangle bounds"));
            }
            let mi = p["material"]
                .as_u64()
                .ok_or_else(|| invalid("native source material"))? as usize;
            let (mat, original) = material(
                &doc,
                &data,
                mi,
                (root, &prefix),
                &mut files,
                &mut texture_cache,
                template,
            )?;
            for channel in original["extras"]["nativeAdditionalTextures"]
                .as_array()
                .unwrap_or(&Vec::new())
            {
                unsupported.insert(format!(
                    "{}: native shader mapping unavailable; original texture retained",
                    channel["pointer"].as_str().unwrap()
                ));
            }
            let mut morphs = Vec::new();
            for (i, t) in p["targets"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .enumerate()
            {
                let name = mesh["extras"]["targetNames"][i]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("morph_{i}"));
                expressions.insert(name.clone());
                let d = if let Some(a) = t["POSITION"].as_u64() {
                    accessor::read(&doc, &data, a as usize)?
                } else {
                    vec![vec![0.; 3]; positions.len()]
                };
                let nd = if let Some(a) = t["NORMAL"].as_u64() {
                    accessor::read(&doc, &data, a as usize)?
                } else {
                    vec![vec![0.; 3]; positions.len()]
                };
                if d.len() != positions.len() || nd.len() != positions.len() {
                    return Err(invalid("native morph count"));
                }
                morphs.push((name, d, nd));
            }
            for (source, tri) in chunks(&index, 60000)? {
                let id = assets.len();
                let name = format!("{prefix}_part_{id:04}");
                let mut m = Model {
                    version: 2001,
                    name: name.clone(),
                    root: body.root.clone(),
                    bones: rig.bones.clone(),
                    skin_bones: rig.bones.iter().map(|b| b.name.clone()).collect(),
                    inverse_bind: rig
                        .world
                        .iter()
                        .map(|w| (w.inverse() * root_world).to_cols_array())
                        .collect(),
                    vertices: source.iter().map(|&i| vertices[i].clone()).collect(),
                    skin: source.iter().map(|&i| new_skin[i].clone()).collect(),
                    tangents: vec![],
                    submeshes: vec![tri],
                    materials: vec![mat.clone()],
                    morphs: Vec::new(),
                };
                for (name, d, nd) in &morphs {
                    let mut morph = Morph {
                        name: name.clone(),
                        indices: Vec::new(),
                        positions: Vec::new(),
                        normals: Vec::new(),
                    };
                    for (to, &from) in source.iter().enumerate() {
                        let p = transforms[from].transform_vector3(Vec3::from_slice(&d[from]));
                        let n = transforms[from]
                            .inverse()
                            .transpose()
                            .transform_vector3(Vec3::from_slice(&nd[from]));
                        if p.length_squared() > 1e-18 || n.length_squared() > 1e-18 {
                            morph.indices.push(to as u16);
                            morph.positions.push(p.to_array());
                            morph.normals.push(n.to_array());
                        }
                    }
                    m.morphs.push(morph);
                }
                assets.push(m);
                metadata.push(vec![original.clone()]);
                expected.push(
                    source
                        .iter()
                        .map(|&v| original_world[v])
                        .collect::<Vec<_>>(),
                );
            }
        }
    }
    if assets.is_empty() {
        return Err(invalid("native pack has no geometry"));
    }
    let mut merged = Vec::<Model>::new();
    let mut merged_metadata = Vec::<Vec<Value>>::new();
    let mut merged_expected = Vec::<Vec<Vec3>>::new();
    for ((m, meta), positions) in assets.into_iter().zip(metadata).zip(expected) {
        if merged.last().is_some_and(|last| {
            last.vertices.len() + m.vertices.len() <= 60000
                && last.materials.len() + m.materials.len() <= 512
        }) {
            let last = merged.last_mut().unwrap();
            let offset = last.vertices.len() as u16;
            last.vertices.extend(m.vertices);
            last.skin.extend(m.skin);
            last.submeshes.extend(
                m.submeshes
                    .into_iter()
                    .map(|s| s.into_iter().map(|i| i + offset).collect()),
            );
            last.materials.extend(m.materials);
            for mut morph in m.morphs {
                for i in &mut morph.indices {
                    *i += offset;
                }
                if let Some(existing) = last.morphs.iter_mut().find(|v| v.name == morph.name) {
                    existing.indices.extend(morph.indices);
                    existing.positions.extend(morph.positions);
                    existing.normals.extend(morph.normals);
                } else {
                    last.morphs.push(morph);
                }
            }
            merged_metadata.last_mut().unwrap().extend(meta);
            merged_expected.last_mut().unwrap().extend(positions);
        } else {
            merged.push(m);
            merged_metadata.push(meta);
            merged_expected.push(positions);
        }
    }
    let mut aliases = Vec::new();
    let mut models = Vec::new();
    for (i, m) in merged.iter_mut().enumerate() {
        if category == "head" {
            m.morphs.retain(|v| {
                !body.morphs.iter().any(|b| b.name == v.name)
                    || v.positions.iter().flatten().any(|n| n.abs() > 1e-7)
                    || v.normals.iter().flatten().any(|n| n.abs() > 1e-6)
            });
        }
        for (original, alias) in [
            ("blink", "eyeclose"),
            ("mouth_a", "moutha"),
            ("mouth_i", "mouths"),
            ("mouth_u", "mouthc"),
        ] {
            if !m.morphs.iter().any(|v| v.name == alias) {
                if let Some(source) = m.morphs.iter().find(|v| v.name == original).cloned() {
                    let mut source = source;
                    source.name = alias.into();
                    m.morphs.push(source);
                    expressions.insert(alias.into());
                    aliases.push(json!({"source":original,"native":alias,"phonemeApproximation":alias=="mouths"||alias=="mouthc"}));
                }
            }
        }
        m.name = format!("{prefix}_model_{i:04}");
        let file = format!("{}.model", m.name);
        fs::write(root.join(&file), native_write::model(m)?)?;
        files.push(file.clone());
        models.push(file);
        for (j, mat) in m.materials.iter().enumerate() {
            let file = format!("{}_material_{j:03}.mate", m.name);
            fs::write(root.join(&file), native_write::material(mat)?)?;
            files.push(file);
        }
    }
    let metadata = merged_metadata;
    let expected = merged_expected;
    let mut resources = Directory {
        root: root.into(),
        files: files.clone(),
    };
    let mut render = Vec::new();
    let mut max_error = 0f32;
    let mut vertices = 0;
    for (i, file) in models.iter().enumerate() {
        let model = Model::read(&fs::read(root.join(file))?)?;
        vertices += model.vertices.len();
        let path = root.join("render").join(format!("part-{i:04}.glb"));
        gltf::export_model(&mut resources, file, &model, &path)?;
        let (mut out, mut bytes) = glb_io::read(&path)?;
        let result = accessor::read(
            &out,
            &bytes,
            out["meshes"][0]["primitives"][0]["attributes"]["POSITION"]
                .as_u64()
                .unwrap() as usize,
        )?;
        for (actual, expected) in result.iter().zip(&expected[i]) {
            max_error = max_error.max(Vec3::from_slice(actual).distance(*expected));
        }
        for j in 0..metadata[i].len() {
            let texture = out["materials"][j]["pbrMetallicRoughness"]["baseColorTexture"].clone();
            let native = out["materials"][j]["extras"].clone();
            out["materials"][j] = metadata[i][j].clone();
            if !texture.is_null() {
                out["materials"][j]["pbrMetallicRoughness"]["baseColorTexture"] = texture;
            }
            for field in ["normalTexture", "occlusionTexture", "emissiveTexture"] {
                out["materials"][j].as_object_mut().unwrap().remove(field);
            }
            if let Some(name) = metadata[i][j]["extras"]["nativeNormalSource"]["rgb"].as_str() {
                let png =
                    crate::texture::Texture::read(&fs::read(root.join(format!("{name}.tex")))?)?
                        .encoded;
                let texture = append_png(&mut out, &mut bytes, &png)?;
                out["materials"][j]["normalTexture"] = json!({"index":texture,"scale":metadata[i][j]["extras"]["nativeNormalSource"]["scale"]});
            }
            for channel in metadata[i][j]["extras"]["nativeAdditionalTextures"]
                .as_array()
                .unwrap_or(&Vec::new())
            {
                let pointer = channel["pointer"].as_str().unwrap();
                let name = channel["rgb"].as_str().unwrap();
                let png =
                    crate::texture::Texture::read(&fs::read(root.join(format!("{name}.tex")))?)?
                        .encoded;
                let texture = append_png(&mut out, &mut bytes, &png)?;
                let mut info = channel["info"].clone();
                info["index"] = json!(texture);
                // Occlusion/emissive were removed above; pointer parents remain valid.
                if pointer == "/occlusionTexture" || pointer == "/emissiveTexture" {
                    out["materials"][j][&pointer[1..]] = info;
                } else {
                    *out["materials"][j]
                        .pointer_mut(pointer)
                        .ok_or_else(|| invalid("retained material channel pointer"))? = info;
                }
            }
            out["materials"][j]["extras"]["nativePack"] = native;

            if let Some(e) = metadata[i][j]["extensions"].as_object() {
                for name in e.keys() {
                    if !out["extensionsUsed"].is_array() {
                        out["extensionsUsed"] = json!([]);
                    }
                    if !out["extensionsUsed"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v == name)
                    {
                        out["extensionsUsed"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!(name));
                    }
                }
            }
        }
        for node in out["nodes"].as_array_mut().unwrap() {
            if let Some(source) = doc["nodes"].as_array().unwrap().iter().find(|n| {
                n["name"] == node["name"] && n["extras"]["secretarySecondary"].is_object()
            }) {
                node["extras"]["secretarySecondary"] =
                    source["extras"]["secretarySecondary"].clone();
            }
        }
        out["buffers"][0]["byteLength"] = json!(bytes.len());
        out["extras"]["nativePackRoundtrip"] =
            json!({"model":file,"standardBodyBones":body.bones.len(),"nativeFrames":true});
        glb_io::write(&path, &out, &bytes)?;
        render.push(format!("render/part-{i:04}.glb"));
    }
    for (i, m) in models.iter().enumerate() {
        let menu = Menu {
            version: 2001,
            source: "Secretary Rust native conversion".into(),
            name: input
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Lucina")
                .into(),
            category: category.into(),
            description: "Private native avatar compatibility pack".into(),
            commands: {
                let mut c = vec![vec!["additem".into(), m.clone(), category.into()]];
                c.extend(body_masks(requested_category));
                c
            },
        };
        fs::write(
            root.join(format!("{prefix}_{i:04}.menu")),
            native_write::menu(&menu)?,
        )?;
    }
    let mut expressions = merged
        .iter()
        .flat_map(|m| m.morphs.iter().map(|m| m.name.clone()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    expressions.sort();
    let model_names = &models;
    let report = json!({"version":7,"secondaryRiggedVertices":secondary_vertices,"models":models,"render":render,"vertices":vertices,"standardBodyBones":body.bones.len(),"additionalBones":rig.bones.len()-body.bones.len(),"morphs":expressions,"nativeExpressionAliases":aliases,"maxRestErrorMeters":max_error,"requestedCategory":requested_category,"nativeSlot":category,"nativeFormats":["model2001","mate1000","tex1010","menu2001"],"convertedNormalMaps":texture_cache.normal.len(),"nativeNormalEncoding":"desktopAG; original RGB retained","additionalMaterialTextures":metadata.iter().enumerate().flat_map(|(i,m)|m.iter().enumerate().filter(|(_,m)|m["extras"]["nativeAdditionalTextures"].is_array()).map(move |(j,m)|json!({"model":model_names[i],"material":j,"channels":m["extras"]["nativeAdditionalTextures"]}))).collect::<Vec<_>>(),"unsupportedMaterialChannels":unsupported,"installedGameLoadVerified":false,"shaderParity":false});
    let pending = root.join("manifest.pending.json");
    fs::write(&pending, serde_json::to_vec_pretty(&report).unwrap())?;
    fs::rename(pending, root.join("manifest.json"))?;
    Ok(report)
}
/// Standalone native head, preserving the authored face/eyes/neck connector.
/// Body skin and interchangeable hair/tiara are separate native slots.
pub fn export_head(input: &Path, body: &Model, root: &Path) -> Result<Value> {
    let (mut doc, data) = glb_io::read(input)?;
    let materials = doc["materials"]
        .as_array()
        .ok_or_else(|| invalid("head source materials"))?
        .clone();
    let mut used = HashSet::new();
    for (i, m) in doc["meshes"]
        .as_array_mut()
        .ok_or_else(|| invalid("head source meshes"))?
        .iter_mut()
        .enumerate()
    {
        m["primitives"].as_array_mut().unwrap().retain(|p| {
            let name = materials[p["material"].as_u64().unwrap() as usize]["name"]
                .as_str()
                .unwrap_or("");
            !name.starts_with("skin_safe")
                && name != "Hair"
                && name != "HairHighlight"
                && name != "Tiara"
        });
        if !m["primitives"].as_array().unwrap().is_empty() {
            used.insert(i);
        }
    }
    for n in doc["nodes"].as_array_mut().unwrap() {
        if n["mesh"]
            .as_u64()
            .is_some_and(|m| !used.contains(&(m as usize)))
        {
            n.as_object_mut().unwrap().remove("mesh");
            n.as_object_mut().unwrap().remove("skin");
        }
    }
    fs::create_dir_all(root)?;
    let source = root.join("head-bind-source.glb");
    glb_io::write(&source, &doc, &data)?;
    export(&source, body, root, "head")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn u16_partition_preserves_triangles_winding_and_vertex_identity() {
        let source = [0, 1, 2, 2, 3, 4, 4, 5, 6];
        let parts = chunks(&source, 4).unwrap();
        assert_eq!(parts.len(), 3);
        let restored = parts
            .iter()
            .flat_map(|(v, t)| {
                t.as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|a| [v[a[0] as usize], v[a[2] as usize], v[a[1] as usize]])
            })
            .collect::<Vec<_>>();
        assert_eq!(restored, source);
        assert!(chunks(&source, 65536).is_err());
    }
}
