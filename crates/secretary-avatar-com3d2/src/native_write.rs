//! Bounded native COM3D2 serialization. No game scripts are evaluated.
use crate::{
    invalid,
    menu::Menu,
    model::{Material, Model},
    Result,
};
use serde_json::Value;
#[derive(Default)]
pub struct Writer {
    pub bytes: Vec<u8>,
}
impl Writer {
    pub fn string(&mut self, s: &str) -> Result<()> {
        if s.len() > 1_048_576 {
            return Err(invalid("native string limit"));
        }
        let mut n = s.len() as u32;
        while n >= 128 {
            self.bytes.push((n as u8) | 128);
            n >>= 7;
        }
        self.bytes.push(n as u8);
        self.bytes.extend(s.as_bytes());
        Ok(())
    }
    pub fn int(&mut self, n: i32) {
        self.bytes.extend(n.to_le_bytes());
    }
    pub fn count(&mut self, n: usize) -> Result<()> {
        self.int(i32::try_from(n).map_err(|_| invalid("native count overflow"))?);
        Ok(())
    }
    pub fn float(&mut self, n: f32) -> Result<()> {
        if !n.is_finite() {
            return Err(invalid("native nonfinite value"));
        }
        self.bytes.extend(n.to_le_bytes());
        Ok(())
    }
    pub fn floats(&mut self, a: &[f32]) -> Result<()> {
        for &v in a {
            self.float(v)?;
        }
        Ok(())
    }
    fn values(&mut self, v: &Value, n: usize) -> Result<()> {
        let a = v
            .as_array()
            .filter(|a| a.len() == n)
            .ok_or_else(|| invalid("native property vector"))?;
        for v in a {
            self.float(
                v.as_f64()
                    .ok_or_else(|| invalid("native property number"))? as f32,
            )?;
        }
        Ok(())
    }
    pub fn material(&mut self, m: &Material) -> Result<()> {
        self.string(&m.name)?;
        self.string(&m.shader)?;
        self.string(&m.shader_file)?;
        for p in &m.properties {
            let kind = p["kind"]
                .as_str()
                .ok_or_else(|| invalid("native property type"))?;
            self.string(kind)?;
            self.string(
                p["name"]
                    .as_str()
                    .ok_or_else(|| invalid("native property name"))?,
            )?;
            let v = &p["value"];
            match kind {
                "tex" => {
                    if v.is_null() {
                        self.string("null")?;
                    } else {
                        let sub = v["subtype"].as_str().unwrap_or("tex2d");
                        self.string(sub)?;
                        match sub {
                            "tex2d" | "cube" => {
                                self.string(
                                    v["texture"]
                                        .as_str()
                                        .ok_or_else(|| invalid("native texture name"))?,
                                )?;
                                self.string(v["sourcePath"].as_str().unwrap_or(""))?;
                                self.values(&v["offset"], 2)?;
                                self.values(&v["scale"], 2)?;
                            }
                            _ => return Err(invalid("unsupported writable texture subtype")),
                        }
                    }
                }
                "col" | "vec" => self.values(v, 4)?,
                "f" | "range" => self.float(
                    v.as_f64()
                        .ok_or_else(|| invalid("native property scalar"))?
                        as f32,
                )?,
                "tex_offset" | "tex_scale" => self.values(v, 2)?,
                _ => return Err(invalid("unsupported writable material property")),
            }
        }
        self.string("end")
    }
}
pub fn model(m: &Model) -> Result<Vec<u8>> {
    if m.version != 2001
        || m.vertices.len() > 65535
        || m.vertices.len() != m.skin.len()
        || m.skin_bones.len() != m.inverse_bind.len()
        || m.submeshes.len() != m.materials.len()
        || (!m.tangents.is_empty() && m.tangents.len() != m.vertices.len())
    {
        return Err(invalid("native model output shape/version"));
    }
    let mut w = Writer::default();
    w.string("CM3D2_MESH")?;
    w.int(m.version);
    w.string(&m.name)?;
    w.string(&m.root)?;
    w.count(m.bones.len())?;
    for b in &m.bones {
        w.string(&b.name)?;
        w.bytes.push(u8::from(b.scale_helper));
    }
    for b in &m.bones {
        w.int(b.parent);
    }
    for b in &m.bones {
        w.floats(&b.translation)?;
        w.floats(&b.rotation)?;
        let scaled = b.scale != [1.; 3];
        w.bytes.push(u8::from(scaled));
        if scaled {
            w.floats(&b.scale)?;
        }
    }
    w.count(m.vertices.len())?;
    w.count(m.submeshes.len())?;
    w.count(m.skin_bones.len())?;
    for n in &m.skin_bones {
        w.string(n)?;
    }
    for m in &m.inverse_bind {
        w.floats(m)?;
    }
    for v in &m.vertices {
        w.floats(&v.position)?;
        w.floats(&v.normal)?;
        w.floats(&v.uv)?;
    }
    w.count(m.tangents.len())?;
    for t in &m.tangents {
        w.floats(t)?;
    }
    for s in &m.skin {
        for j in s.joints {
            w.bytes.extend(j.to_le_bytes());
        }
        w.floats(&s.weights)?;
    }
    for sub in &m.submeshes {
        w.count(sub.len())?;
        for j in sub {
            w.bytes.extend(j.to_le_bytes());
        }
    }
    w.count(m.materials.len())?;
    for m in &m.materials {
        w.material(m)?;
    }
    for morph in &m.morphs {
        if morph.indices.len() != morph.positions.len()
            || morph.indices.len() != morph.normals.len()
        {
            return Err(invalid("native morph output shape"));
        }
        w.string("morph")?;
        w.string(&morph.name)?;
        w.count(morph.indices.len())?;
        for ((i, p), n) in morph
            .indices
            .iter()
            .zip(&morph.positions)
            .zip(&morph.normals)
        {
            w.bytes.extend(i.to_le_bytes());
            w.floats(p)?;
            w.floats(n)?;
        }
    }
    w.string("end")?;
    Model::read(&w.bytes)?;
    Ok(w.bytes)
}
pub fn material(m: &Material) -> Result<Vec<u8>> {
    let mut w = Writer::default();
    w.string("CM3D2_MATERIAL")?;
    w.int(1000);
    w.string(&m.name)?;
    w.material(m)?;
    crate::mate::read(&w.bytes)?;
    Ok(w.bytes)
}
pub fn texture(name: &str, png: &[u8]) -> Result<Vec<u8>> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(invalid("native texture output requires PNG"));
    }
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    let mut w = Writer::default();
    w.string("CM3D2_TEX")?;
    w.int(1010);
    w.string(name)?;
    w.count(width as usize)?;
    w.count(height as usize)?;
    w.int(4);
    w.count(png.len())?;
    w.bytes.extend(png);
    crate::texture::Texture::read(&w.bytes)?;
    Ok(w.bytes)
}
pub fn menu(m: &Menu) -> Result<Vec<u8>> {
    let mut s = Writer::default();
    for c in &m.commands {
        if c.is_empty() || c.len() > 255 {
            return Err(invalid("native menu argument count"));
        }
        s.bytes.push(c.len() as u8);
        for a in c {
            s.string(a)?;
        }
    }
    s.bytes.push(0);
    let mut w = Writer::default();
    w.string("CM3D2_MENU")?;
    w.int(m.version);
    for a in [&m.source, &m.name, &m.category, &m.description] {
        w.string(a)?;
    }
    w.count(s.bytes.len())?;
    w.bytes.extend(s.bytes);
    Menu::read(&w.bytes)?;
    Ok(w.bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strings_roundtrip_utf8_and_seven_bit_boundaries() {
        for s in ["着物".to_owned(), "x".repeat(128), "y".repeat(16384)] {
            let mut w = Writer::default();
            w.string(&s).unwrap();
            assert_eq!(crate::binary::Reader::new(&w.bytes).string().unwrap(), s);
        }
    }
    #[test]
    fn model_roundtrip_retains_skin_morphs_and_native_frames() {
        use crate::model::*;
        let m = Model {
            version: 2001,
            name: "衣服".into(),
            root: "body".into(),
            bones: vec![Bone {
                name: "body".into(),
                scale_helper: true,
                parent: -1,
                translation: [0.; 3],
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
            }],
            skin_bones: vec!["body".into()],
            inverse_bind: vec![glam::Mat4::IDENTITY.to_cols_array()],
            vertices: vec![Vertex {
                position: [0.; 3],
                normal: [0., 1., 0.],
                uv: [0.5; 2],
            }],
            tangents: vec![],
            skin: vec![Skin {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            }],
            submeshes: vec![vec![0, 0, 0]],
            materials: vec![Material {
                name: "服".into(),
                shader: "CM3D2/Toony_Lighted".into(),
                shader_file: "".into(),
                properties: vec![],
            }],
            morphs: vec![Morph {
                name: "munel".into(),
                indices: vec![0],
                positions: vec![[0.1, 0., 0.]],
                normals: vec![[0.; 3]],
            }],
        };
        let b = model(&m).unwrap();
        let out = Model::read(&b).unwrap();
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::to_value(&out).unwrap()
        );
        let mut bad = m;
        bad.skin[0].joints[0] = 1;
        assert!(model(&bad).is_err());
    }
}
