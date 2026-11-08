//! Avatar interchange exporter. Unity shader semantics remain in material extras.
use crate::resources::ResourceSource;
use crate::{arc::Archive, invalid, model::Model, texture::Texture, Result};
use glam::{Mat4, Quat, Vec3};
use serde_json::{json, Value};
use std::{collections::HashMap, fs, path::Path};
struct Buffer {
    bytes: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}
impl Buffer {
    fn view(&mut self, data: &[u8]) -> usize {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        let offset = self.bytes.len();
        self.bytes.extend(data);
        let i = self.views.len();
        self.views
            .push(json!({"buffer":0,"byteOffset":offset,"byteLength":data.len()}));
        i
    }
    fn accessor(&mut self, data: &[u8], component: u32, kind: &str, count: usize) -> usize {
        let view = self.view(data);
        let i = self.accessors.len();
        self.accessors
            .push(json!({"bufferView":view,"componentType":component,"type":kind,"count":count}));
        i
    }
    fn floats(&mut self, items: &[f32], kind: &str, width: usize) -> usize {
        let data = items
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect::<Vec<_>>();
        self.accessor(&data, 5126, kind, items.len() / width)
    }
    fn shorts(&mut self, items: &[u16], kind: &str, width: usize) -> usize {
        let data = items
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect::<Vec<_>>();
        self.accessor(&data, 5123, kind, items.len() / width)
    }
}
fn reflect(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[1], -p[2]]
}
fn texture_entry(arc: &impl ResourceSource, model_path: &str, name: &str) -> Result<String> {
    let target = if name.to_lowercase().ends_with(".tex") {
        name.to_owned()
    } else {
        format!("{name}.tex")
    };
    arc.resolve(&target, model_path)
}
pub fn export(arc: &mut Archive, path: &str, out: &Path) -> Result<Value> {
    let model = Model::read(&arc.read(path)?)?;
    export_model(arc, path, &model, out)
}
pub fn export_model(
    arc: &mut impl ResourceSource,
    path: &str,
    model: &Model,
    out: &Path,
) -> Result<Value> {
    let root_index = model
        .bones
        .iter()
        .position(|b| b.name == model.root)
        .ok_or_else(|| invalid("model root bone missing"))?;
    let mut chain = Vec::new();
    let mut next = root_index as i32;
    while next >= 0 {
        chain.push(next as usize);
        next = model.bones[next as usize].parent;
    }
    let mut mesh_transform = Mat4::IDENTITY;
    for i in chain.into_iter().rev() {
        let bone = &model.bones[i];
        mesh_transform *= Mat4::from_scale_rotation_translation(
            Vec3::from_array(bone.scale),
            Quat::from_array(bone.rotation).normalize(),
            Vec3::from_array(bone.translation),
        );
    }
    if mesh_transform.determinant().abs() < 1e-8 {
        return Err(invalid("singular model-root transform"));
    }
    let root_inverse = mesh_transform.inverse();
    let normal_transform = root_inverse.transpose();
    let mut b = Buffer {
        bytes: Vec::new(),
        views: Vec::new(),
        accessors: Vec::new(),
    };
    let mut materials = Vec::new();
    let mut textures = Vec::new();
    let mut images = Vec::new();
    let mut image_cache = HashMap::new();
    let mut unlit = false;
    for m in &model.materials {
        let colour = m
            .properties
            .iter()
            .rev()
            .find(|p| p["name"] == "_Color")
            .map(|p| p["value"].clone())
            .unwrap_or(json!([1, 1, 1, 1]));
        let mut material = json!({"name":m.name,"doubleSided":true,"pbrMetallicRoughness":{"baseColorFactor":colour,"metallicFactor":0,"roughnessFactor":1},"extras":{"com3d2":m,"shaderPreview":"PBR approximation; source shader preserved"}});
        if let Some(p) = m
            .properties
            .iter()
            .rev()
            .find(|p| p["name"] == "_MainTex" && p["value"]["texture"].is_string())
        {
            let name = p["value"]["texture"].as_str().unwrap();
            let entry = texture_entry(arc, path, name)?;
            let texture = if let Some(i) = image_cache.get(&entry) {
                *i
            } else {
                let tex = Texture::read(&arc.read_resource(&entry)?)?;
                let view = b.view(&tex.encoded);
                let i = images.len();
                images.push(json!({"name":tex.name,"bufferView":view,"mimeType":tex.mime,"extras":{"sourceVersion":tex.version,"sourcePath":entry}}));
                textures.push(json!({"source":i,"sampler":0}));
                image_cache.insert(entry, i);
                i
            };
            material["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index":texture});
        }
        let shader = m.shader.to_lowercase();
        if shader.contains("cutout") || shader.contains("alphatest") {
            material["alphaMode"] = json!("MASK");
            material["alphaCutoff"] = m
                .properties
                .iter()
                .rev()
                .find(|p| p["name"] == "_Cutoff")
                .map(|p| p["value"].clone())
                .unwrap_or(json!(0.5));
        } else if shader.contains("trans")
            || material["pbrMetallicRoughness"]["baseColorFactor"][3]
                .as_f64()
                .is_some_and(|a| a < 0.999)
        {
            material["alphaMode"] = json!("BLEND");
        }
        if m.shader.starts_with("Unlit/") {
            unlit = true;
            material["extensions"] = json!({"KHR_materials_unlit":{}});
        }
        materials.push(material);
    }
    let mut nodes = Vec::new();
    let mut roots = Vec::new();
    let mut helper = HashMap::new();
    for (i, bone) in model.bones.iter().enumerate() {
        let q = Quat::from_array([
            -bone.rotation[0],
            -bone.rotation[1],
            bone.rotation[2],
            bone.rotation[3],
        ])
        .normalize();
        nodes.push(json!({"name":bone.name,"translation":reflect(bone.translation),"rotation":q.to_array(),"scale":bone.scale}));
        if bone.parent < 0 {
            roots.push(i);
        }
    }
    for (i, bone) in model.bones.iter().enumerate() {
        if bone.parent >= 0 {
            let parent = &mut nodes[bone.parent as usize];
            if !parent["children"].is_array() {
                parent["children"] = json!([]);
            }
            parent["children"].as_array_mut().unwrap().push(json!(i));
        }
        if bone.scale_helper {
            let index = nodes.len();
            helper.insert(i, index);
            nodes.push(
                json!({"name":format!("{}_SCL_",bone.name),"extras":{"com3d2ScaleHelper":true}}),
            );
            if !nodes[i]["children"].is_array() {
                nodes[i]["children"] = json!([]);
            }
            nodes[i]["children"]
                .as_array_mut()
                .unwrap()
                .push(json!(index));
        }
    }
    let mut joints = model
        .skin_bones
        .iter()
        .map(|name| {
            let i = model.bones.iter().position(|x| &x.name == name).unwrap();
            helper.get(&i).copied().unwrap_or(i)
        })
        .collect::<Vec<_>>();
    let reflection = Mat4::from_scale(Vec3::new(1., 1., -1.));
    let mut matrices = model
        .inverse_bind
        .iter()
        .flat_map(|m| {
            (reflection * Mat4::from_cols_array(m) * root_inverse * reflection).to_cols_array()
        })
        .collect::<Vec<_>>();
    // Retain the entire canonical skeleton, including currently unweighted
    // attachment and physics joints. Wearables can legitimately reference them.
    for i in 0..model.bones.len() {
        let mut chain = Vec::new();
        let mut parent = i as i32;
        while parent >= 0 {
            chain.push(parent as usize);
            parent = model.bones[parent as usize].parent;
        }
        let mut world = Mat4::IDENTITY;
        for j in chain.into_iter().rev() {
            let bone = &model.bones[j];
            world *= Mat4::from_scale_rotation_translation(
                Vec3::from_array(bone.scale),
                Quat::from_array(bone.rotation).normalize(),
                Vec3::from_array(bone.translation),
            );
        }
        for node in [Some(i), helper.get(&i).copied()].into_iter().flatten() {
            if !joints.contains(&node) {
                joints.push(node);
                matrices.extend((reflection * world.inverse() * reflection).to_cols_array());
            }
        }
    }
    let ib = b.floats(&matrices, "MAT4", 16);
    let ps = model
        .vertices
        .iter()
        .flat_map(|v| {
            reflect(
                mesh_transform
                    .transform_point3(Vec3::from_array(v.position))
                    .to_array(),
            )
        })
        .collect::<Vec<_>>();
    let ns = model
        .vertices
        .iter()
        .flat_map(|v| {
            reflect(
                normal_transform
                    .transform_vector3(Vec3::from_array(v.normal))
                    .normalize_or_zero()
                    .to_array(),
            )
        })
        .collect::<Vec<_>>();
    let uv = model
        .vertices
        .iter()
        .flat_map(|v| [v.uv[0], 1. - v.uv[1]])
        .collect::<Vec<_>>();
    let position = b.floats(&ps, "VEC3", 3);
    let normal = b.floats(&ns, "VEC3", 3);
    let texcoord = b.floats(&uv, "VEC2", 2);
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in ps.as_chunks::<3>().0.iter() {
        for j in 0..3 {
            min[j] = min[j].min(p[j]);
            max[j] = max[j].max(p[j]);
        }
    }
    b.accessors[position]["min"] = json!(min);
    b.accessors[position]["max"] = json!(max);
    let mut joint_values = Vec::new();
    let mut weight_values = Vec::new();
    for s in &model.skin {
        let sum = s.weights.iter().sum::<f32>();
        if sum < 1e-6 {
            return Err(invalid(
                "unweighted vertex needs an explicit attachment policy",
            ));
        }
        for j in 0..4 {
            joint_values.push(if s.weights[j] > 0. { s.joints[j] } else { 0 });
            weight_values.push(s.weights[j] / sum);
        }
    }
    let skin_joints = b.shorts(&joint_values, "VEC4", 4);
    let skin_weights = b.floats(&weight_values, "VEC4", 4);
    let mut targets = Vec::new();
    for morph in &model.morphs {
        let mut ps = vec![0.; model.vertices.len() * 3];
        let mut ns = ps.clone();
        for (j, i) in morph.indices.iter().enumerate() {
            let i = *i as usize;
            ps[i * 3..i * 3 + 3].copy_from_slice(&reflect(
                mesh_transform
                    .transform_vector3(Vec3::from_array(morph.positions[j]))
                    .to_array(),
            ));
            ns[i * 3..i * 3 + 3].copy_from_slice(&reflect(
                normal_transform
                    .transform_vector3(Vec3::from_array(morph.normals[j]))
                    .to_array(),
            ));
        }
        let pos = b.floats(&ps, "VEC3", 3);
        let norm = b.floats(&ns, "VEC3", 3);
        targets.push(json!({"POSITION":pos,"NORMAL":norm}));
    }
    let mut primitives = Vec::new();
    for (i, sub) in model.submeshes.iter().enumerate() {
        let indices = sub
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|tri| [tri[0], tri[2], tri[1]])
            .collect::<Vec<_>>();
        let index = b.shorts(&indices, "SCALAR", 1);
        let mut primitive = json!({"attributes":{"POSITION":position,"NORMAL":normal,"TEXCOORD_0":texcoord,"JOINTS_0":skin_joints,"WEIGHTS_0":skin_weights},"indices":index,"material":i,"mode":4});
        if !targets.is_empty() {
            primitive["targets"] = json!(targets);
        }
        primitives.push(primitive);
    }
    // Bake the native mesh root into geometry and remove it from inverse binds.
    // This preserves world-space skinning and gives authoring tools a true
    // identity rest deformation, instead of a hidden 90-degree correction.
    let mesh = nodes.len();
    nodes.push(json!({"name":format!("COM3D2_{}",model.name),"mesh":0,"skin":0}));
    roots.push(mesh);
    while !b.bytes.len().is_multiple_of(4) {
        b.bytes.push(0);
    }
    let mut doc = json!({"asset":{"version":"2.0","generator":"Secretary Rust COM3D2 compatibility"},"scene":0,"scenes":[{"nodes":roots}],"nodes":nodes,"skins":[{"joints":joints,"inverseBindMatrices":ib}],"meshes":[{"name":model.name,"primitives":primitives,"weights":vec![0.;model.morphs.len()],"extras":{"targetNames":model.morphs.iter().map(|m|&m.name).collect::<Vec<_>>()}}],"materials":materials,"bufferViews":b.views,"accessors":b.accessors,"buffers":[{"byteLength":b.bytes.len()}],"extras":{"com3d2":{"source":path,"version":model.version,"root":model.root,"nativeMorphs":true,"unityRuntimeRequired":false,"shaderParity":false}}});
    if !images.is_empty() {
        doc["images"] = json!(images);
        doc["textures"] = json!(textures);
        doc["samplers"] = json!([{"magFilter":9729,"minFilter":9987,"wrapS":10497,"wrapT":10497}]);
    }
    if unlit {
        doc["extensionsUsed"] = json!(["KHR_materials_unlit"]);
    }
    let mut json = serde_json::to_vec(&doc).map_err(|e| invalid(e.to_string()))?;
    while !json.len().is_multiple_of(4) {
        json.push(32);
    }
    let size = 12 + 8 + json.len() + 8 + b.bytes.len();
    let mut bytes = Vec::with_capacity(size);
    bytes.extend(b"glTF");
    bytes.extend(2u32.to_le_bytes());
    bytes.extend((size as u32).to_le_bytes());
    bytes.extend((json.len() as u32).to_le_bytes());
    bytes.extend(b"JSON");
    bytes.extend(json);
    bytes.extend((b.bytes.len() as u32).to_le_bytes());
    bytes.extend(b"BIN\0");
    bytes.extend(b.bytes);
    fs::write(out, bytes)?;
    Ok(
        json!({"source":path,"output":out,"vertices":model.vertices.len(),"nodes":nodes.len(),"morphs":model.morphs.len(),"bytes":size,"shaderParity":false}),
    )
}
