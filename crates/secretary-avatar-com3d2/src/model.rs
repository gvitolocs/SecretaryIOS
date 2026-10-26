use crate::{binary::Reader, invalid, Error, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
#[derive(Debug, Clone, Serialize)]
pub struct Bone {
    pub name: String,
    pub scale_helper: bool,
    pub parent: i32,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}
#[derive(Debug, Clone, Serialize)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}
#[derive(Debug, Clone, Serialize)]
pub struct Skin {
    pub joints: [u16; 4],
    pub weights: [f32; 4],
}
#[derive(Debug, Clone, Serialize)]
pub struct Material {
    pub name: String,
    pub shader: String,
    pub shader_file: String,
    pub properties: Vec<Value>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Morph {
    pub name: String,
    pub indices: Vec<u16>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Model {
    pub version: i32,
    pub name: String,
    pub root: String,
    pub bones: Vec<Bone>,
    pub skin_bones: Vec<String>,
    pub inverse_bind: Vec<[f32; 16]>,
    pub vertices: Vec<Vertex>,
    pub tangents: Vec<[f32; 4]>,
    pub skin: Vec<Skin>,
    pub submeshes: Vec<Vec<u16>>,
    pub materials: Vec<Material>,
    pub morphs: Vec<Morph>,
}
pub(crate) fn material(r: &mut Reader<'_>) -> Result<Material> {
    let name = r.string()?;
    let shader = r.string()?;
    let shader_file = r.string()?;
    let mut properties = Vec::new();
    loop {
        let kind = r.string()?;
        if kind == "end" {
            break;
        }
        if properties.len() > 1024 {
            return Err(invalid("material property limit"));
        }
        let name = r.string()?;
        let value = match kind.as_str() {
            "tex" => {
                let sub = r.string()?;
                match sub.as_str() {
                    "tex2d" | "cube" => {
                        json!({"subtype":sub,"texture":r.string()?,"sourcePath":r.string()?,"offset":r.array::<2>()?,"scale":r.array::<2>()?})
                    }
                    "null" => Value::Null,
                    "texRT" => json!({"subtype":sub,"unused":[r.string()?,r.string()?]}),
                    _ => return Err(Error::Unsupported(format!("texture subtype {sub}"))),
                }
            }
            "col" | "vec" => json!(r.array::<4>()?),
            "f" | "range" => json!(r.f32()?),
            "tex_offset" | "tex_scale" => json!(r.array::<2>()?),
            _ => return Err(Error::Unsupported(format!("material property {kind}"))),
        };
        properties.push(json!({"kind":kind,"name":name,"value":value}));
    }
    Ok(Material {
        name,
        shader,
        shader_file,
        properties,
    })
}
impl Model {
    pub fn bind_audit(&self) -> Value {
        use glam::{Mat4, Quat, Vec3};
        let global = |index: usize| {
            let mut chain = Vec::new();
            let mut i = index as i32;
            while i >= 0 {
                chain.push(i as usize);
                i = self.bones[i as usize].parent;
            }
            let mut m = Mat4::IDENTITY;
            for i in chain.into_iter().rev() {
                let b = &self.bones[i];
                m *= Mat4::from_scale_rotation_translation(
                    Vec3::from_array(b.scale),
                    Quat::from_array(b.rotation).normalize(),
                    Vec3::from_array(b.translation),
                );
            }
            m
        };
        let root = self
            .bones
            .iter()
            .position(|b| b.name == self.root)
            .map(global)
            .unwrap_or(Mat4::IDENTITY);
        let mut identity_error = 0f32;
        let mut root_error = 0f32;
        let mut samples = Vec::new();
        for (i, name) in self.skin_bones.iter().enumerate() {
            let bone = self.bones.iter().position(|b| &b.name == name).unwrap();
            let matrix = global(bone) * Mat4::from_cols_array(&self.inverse_bind[i]);
            let id = matrix
                .to_cols_array()
                .iter()
                .zip(Mat4::IDENTITY.to_cols_array())
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            let rt = matrix
                .to_cols_array()
                .iter()
                .zip(root.to_cols_array())
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            identity_error = identity_error.max(id);
            root_error = root_error.max(rt);
            if i < 3 {
                samples.push(json!({"bone":name,"matrix":matrix.to_cols_array()}));
            }
        }
        json!({"meshRoot":root.to_cols_array(),"maxBindToIdentity":identity_error,"maxBindToMeshRoot":root_error,"samples":samples})
    }
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.string()? != "CM3D2_MESH" {
            return Err(invalid("model signature"));
        }
        let version = r.i32()?;
        if !(1000..=2001).contains(&version) {
            return Err(Error::Unsupported(format!(
                "model version {version}; supported 1000–2001"
            )));
        }
        let name = r.string()?;
        let root = r.string()?;
        let n = r.count(4096)?;
        let mut bones = Vec::new();
        let mut names = HashSet::new();
        for _ in 0..n {
            let name = r.string()?;
            if !names.insert(name.clone()) {
                return Err(invalid("duplicate bone name"));
            }
            bones.push(Bone {
                name,
                scale_helper: r.u8()? != 0,
                parent: -1,
                translation: [0.; 3],
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
            });
        }
        for (i, b) in bones.iter_mut().enumerate() {
            b.parent = r.i32()?;
            if b.parent >= n as i32 || b.parent == i as i32 {
                return Err(invalid("bone parent index"));
            }
        }
        for b in &mut bones {
            b.translation = r.array()?;
            b.rotation = r.array()?;
            if b.rotation.iter().map(|x| x * x).sum::<f32>() < 1e-8 {
                return Err(invalid("zero bone quaternion"));
            }
            if version >= 2001 && r.u8()? != 0 {
                b.scale = r.array()?;
            }
        }
        for i in 0..n {
            let mut parent = bones[i].parent;
            let mut chain = HashSet::new();
            while parent >= 0 {
                if !chain.insert(parent) {
                    return Err(invalid("bone hierarchy cycle"));
                }
                parent = bones[parent as usize].parent;
            }
        }
        let nv = r.count(65536)?;
        let ns = r.count(512)?;
        let nb = r.count(4096)?;
        let mut skin_bones = Vec::new();
        for _ in 0..nb {
            let name = r.string()?;
            if !names.contains(&name) {
                return Err(invalid(format!("skin bone missing from hierarchy: {name}")));
            }
            skin_bones.push(name);
        }
        let mut inverse_bind = Vec::new();
        for _ in 0..nb {
            inverse_bind.push(r.array()?);
        }
        let mut vertices = Vec::new();
        for _ in 0..nv {
            vertices.push(Vertex {
                position: r.array()?,
                normal: r.array()?,
                uv: r.array()?,
            });
        }
        let nt = r.count(nv)?;
        if nt != 0 && nt != nv {
            return Err(invalid("tangent/vertex count mismatch"));
        }
        let mut tangents = Vec::new();
        for _ in 0..nt {
            tangents.push(r.array()?);
        }
        let mut skin = Vec::new();
        for _ in 0..nv {
            let joints = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
            let weights = r.array::<4>()?;
            for j in 0..4 {
                if weights[j] < 0.
                    || weights[j] > 1.0001
                    || weights[j] > 0. && joints[j] as usize >= nb
                {
                    return Err(invalid("skin weight or joint index"));
                }
            }
            skin.push(Skin { joints, weights });
        }
        let mut submeshes = Vec::new();
        for _ in 0..ns {
            let count = r.count(6_000_000)?;
            if count % 3 != 0 {
                return Err(invalid("triangle index count"));
            }
            let mut indices = Vec::new();
            for _ in 0..count {
                let i = r.u16()?;
                if i as usize >= nv {
                    return Err(invalid("mesh vertex index"));
                }
                indices.push(i);
            }
            submeshes.push(indices);
        }
        let nm = r.count(512)?;
        if nm != ns {
            return Err(invalid("material/submesh count mismatch"));
        }
        let mut materials = Vec::new();
        for _ in 0..nm {
            materials.push(material(&mut r)?);
        }
        let mut morphs = Vec::new();
        loop {
            let tag = r.string()?;
            if tag == "end" {
                break;
            }
            if tag != "morph" {
                return Err(Error::Unsupported(format!("model trailing tag {tag}")));
            }
            if morphs.len() > 2048 {
                return Err(invalid("morph limit"));
            }
            let name = r.string()?;
            let count = r.count(nv)?;
            let mut indices = Vec::new();
            let mut positions = Vec::new();
            let mut normals = Vec::new();
            let mut seen = HashSet::new();
            for _ in 0..count {
                let i = r.u16()?;
                if i as usize >= nv || !seen.insert(i) {
                    return Err(invalid("morph vertex index"));
                }
                indices.push(i);
                positions.push(r.array()?);
                normals.push(r.array()?);
            }
            morphs.push(Morph {
                name,
                indices,
                positions,
                normals,
            });
        }
        if r.left() != 0 {
            return Err(Error::Unsupported(format!(
                "{} trailing model bytes",
                r.left()
            )));
        }
        Ok(Self {
            version,
            name,
            root,
            bones,
            skin_bones,
            inverse_bind,
            vertices,
            tangents,
            skin,
            submeshes,
            materials,
            morphs,
        })
    }
    pub fn summary(&self) -> Value {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut unweighted = 0;
        let mut max_weight_error = 0f32;
        for (v, s) in self.vertices.iter().zip(&self.skin) {
            for j in 0..3 {
                min[j] = min[j].min(v.position[j]);
                max[j] = max[j].max(v.position[j]);
            }
            let sum = s.weights.iter().sum::<f32>();
            if sum < 1e-6 {
                unweighted += 1;
            }
            max_weight_error = max_weight_error.max((sum - 1.).abs());
        }
        json!({"name":self.name,"version":self.version,"root":self.root,"bones":self.bones.len(),"scaleHelpers":self.bones.iter().filter(|b|b.scale_helper).count(),"vertices":self.vertices.len(),"triangles":self.submeshes.iter().map(|s|s.len()/3).sum::<usize>(),"materials":self.materials,"morphs":self.morphs.iter().map(|m|&m.name).collect::<Vec<_>>(),"bounds":[min,max],"unweightedVertices":unweighted,"maxWeightSumError":max_weight_error})
    }
}
