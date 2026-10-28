use crate::{binary::Reader, invalid, model::Model, physics::Curve, Error, Result};
use glam::{Mat4, Quat, Vec3};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
#[derive(Debug, Serialize)]
pub struct BoneCurves {
    pub path: String,
    pub properties: BTreeMap<u8, Curve>,
}
#[derive(Debug, Serialize)]
pub struct Animation {
    pub version: i32,
    pub bones: Vec<BoneCurves>,
    pub bust_left: bool,
    pub bust_right: bool,
}
impl Animation {
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.string()? != "CM3D2_ANIM" {
            return Err(invalid("animation signature"));
        }
        let version = r.i32()?;
        if ![1000, 1001, 2000].contains(&version) {
            return Err(Error::Unsupported(format!("animation version {version}")));
        }
        let mut bones: Vec<BoneCurves> = Vec::new();
        let mut current = None;
        loop {
            match r.u8()? {
                0 => break,
                1 => {
                    let path = r.string()?;
                    current = Some(if let Some(i) = bones.iter().position(|b| b.path == path) {
                        i
                    } else {
                        if bones.len() >= 4096 {
                            return Err(invalid("animation bone limit"));
                        }
                        bones.push(BoneCurves {
                            path,
                            properties: BTreeMap::new(),
                        });
                        bones.len() - 1
                    });
                }
                tag @ 100..=106 => {
                    let bone = &mut bones
                        [current.ok_or_else(|| invalid("animation curve without bone"))?];
                    if bone
                        .properties
                        .insert(tag - 100, Curve::read(&mut r)?)
                        .is_some()
                    {
                        return Err(invalid("duplicate animation property"));
                    }
                }
                tag => return Err(Error::Unsupported(format!("animation chunk {tag}"))),
            }
        }
        let bust_left = if r.left() > 0 { r.u8()? != 0 } else { false };
        let bust_right = if r.left() > 0 { r.u8()? != 0 } else { false };
        if r.left() > 0 {
            return Err(invalid("trailing animation bytes"));
        }
        Ok(Self {
            version,
            bones,
            bust_left,
            bust_right,
        })
    }
    pub fn duration(&self) -> f32 {
        self.bones
            .iter()
            .flat_map(|b| b.properties.values())
            .filter_map(|c| c.keys.last())
            .map(|k| k.time)
            .fold(0., f32::max)
    }
    /// Native local-space pose, including unchanged properties from the model.
    pub fn local_pose(&self, model: &Model, time: f32) -> Vec<Mat4> {
        let curves = self
            .bones
            .iter()
            .map(|b| (b.path.rsplit('/').next().unwrap_or(&b.path), b))
            .collect::<HashMap<_, _>>();
        model
            .bones
            .iter()
            .map(|bone| {
                let (mut q, mut p) = (bone.rotation, bone.translation);
                if let Some(curves) = curves.get(bone.name.as_str()) {
                    for (property, curve) in &curves.properties {
                        if curve.keys.is_empty() {
                            continue;
                        }
                        let v = curve.sample(time);
                        if *property < 4 {
                            q[*property as usize] = v;
                        } else {
                            p[*property as usize - 4] = v;
                        }
                    }
                }
                let q = Quat::from_array(q);
                let q = if q.length_squared() > 1e-8 {
                    q.normalize()
                } else {
                    Quat::from_array(bone.rotation).normalize()
                };
                Mat4::from_scale_rotation_translation(
                    Vec3::from_array(bone.scale),
                    q,
                    Vec3::from_array(p),
                )
            })
            .collect()
    }
}
pub fn globals(parents: &[i32], locals: &[Mat4]) -> Result<Vec<Mat4>> {
    fn walk(
        i: usize,
        parents: &[i32],
        locals: &[Mat4],
        out: &mut [Mat4],
        state: &mut [u8],
    ) -> Result<Mat4> {
        if state[i] == 2 {
            return Ok(out[i]);
        }
        if state[i] == 1 {
            return Err(invalid("pose hierarchy cycle"));
        }
        state[i] = 1;
        let parent = parents[i];
        if parent >= parents.len() as i32 {
            return Err(invalid("pose parent"));
        }
        let m = if parent < 0 {
            locals[i]
        } else {
            walk(parent as usize, parents, locals, out, state)? * locals[i]
        };
        out[i] = m;
        state[i] = 2;
        Ok(m)
    }
    if parents.len() != locals.len() {
        return Err(invalid("pose array lengths"));
    }
    let mut out = vec![Mat4::IDENTITY; parents.len()];
    let mut state = vec![0; parents.len()];
    for i in 0..parents.len() {
        walk(i, parents, locals, &mut out, &mut state)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn string(bytes: &mut Vec<u8>, value: &str) {
        bytes.push(value.len() as u8);
        bytes.extend(value.as_bytes());
    }
    fn key(bytes: &mut Vec<u8>, property: u8, value: f32) {
        bytes.push(100 + property);
        bytes.extend(1i32.to_le_bytes());
        for v in [0., value, 0., 0.] {
            bytes.extend(v.to_le_bytes());
        }
    }
    #[test]
    fn separate_rotation_translation_and_empty_records_share_one_bone() {
        let mut bytes = Vec::new();
        string(&mut bytes, "CM3D2_ANIM");
        bytes.extend(2000i32.to_le_bytes());
        bytes.push(1);
        string(&mut bytes, "Bip01");
        key(&mut bytes, 0, 0.3);
        key(&mut bytes, 3, 0.9539392);
        bytes.push(1);
        string(&mut bytes, "Bip01");
        key(&mut bytes, 5, 1.2);
        bytes.push(1);
        string(&mut bytes, "Bip01");
        bytes.extend([0, 0, 0]);
        let clip = Animation::read(&bytes).unwrap();
        assert_eq!(clip.bones.len(), 1);
        assert_eq!(clip.bones[0].properties.len(), 3);
        assert_eq!(clip.bones[0].properties[&5].sample(0.), 1.2);
        assert_eq!(clip.bones[0].properties[&0].sample(0.), 0.3);
        for n in 0..bytes.len() - 3 {
            assert!(Animation::read(&bytes[..n]).is_err(), "truncated at {n}");
        }
    }
}
