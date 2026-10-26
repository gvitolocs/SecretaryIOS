//! Shared native-to-canonical world deformation, used by still and clip exports.
use crate::{
    animation::{globals, Animation},
    compose::scene_frames,
    invalid,
    model::Model,
    Result,
};
use glam::{Mat4, Quat, Vec3};
use serde_json::Value;
use std::collections::HashMap;
pub struct PoseMap {
    pub parents: Vec<i32>,
    pub local: Vec<Mat4>,
    pub mapped: Vec<Option<usize>>,
    rest: Vec<Mat4>,
    source_parents: Vec<i32>,
    source_rest: Vec<Mat4>,
}
struct Frame<'a> {
    map: &'a PoseMap,
    deform: &'a [Mat4],
}
impl Frame<'_> {
    fn walk(&self, i: usize, out: &mut [Mat4], state: &mut [u8]) -> Result<Mat4> {
        if state[i] == 2 {
            return Ok(out[i]);
        }
        if state[i] == 1 {
            return Err(invalid("target pose cycle"));
        }
        state[i] = 1;
        let parent = if self.map.parents[i] >= 0 {
            self.walk(self.map.parents[i] as usize, out, state)?
        } else {
            Mat4::IDENTITY
        };
        out[i] = if let Some(source) = self.map.mapped[i] {
            self.deform[source] * self.map.rest[i]
        } else {
            parent * self.map.local[i]
        };
        state[i] = 2;
        Ok(out[i])
    }
}
impl PoseMap {
    pub fn new(model: &Model, doc: &Value) -> Result<Self> {
        let (parents, local, rest) = scene_frames(doc)?;
        let source_parents = model.bones.iter().map(|b| b.parent).collect::<Vec<_>>();
        let source_local = model
            .bones
            .iter()
            .map(|b| {
                Mat4::from_scale_rotation_translation(
                    Vec3::from_array(b.scale),
                    Quat::from_array(b.rotation).normalize(),
                    Vec3::from_array(b.translation),
                )
            })
            .collect::<Vec<_>>();
        let source_rest = globals(&source_parents, &source_local)?;
        let names = model
            .bones
            .iter()
            .enumerate()
            .map(|(i, b)| (b.name.as_str(), i))
            .collect::<HashMap<_, _>>();
        let mapped = doc["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| names.get(n["name"].as_str().unwrap_or("")).copied())
            .collect();
        Ok(Self {
            parents,
            local,
            rest,
            source_parents,
            source_rest,
            mapped,
        })
    }
    pub fn sample(&self, model: &Model, clip: &Animation, time: f32) -> Result<Vec<Mat4>> {
        if !time.is_finite() || time < 0. {
            return Err(invalid("pose time"));
        }
        let source = globals(&self.source_parents, &clip.local_pose(model, time))?;
        let reflection = Mat4::from_scale(Vec3::new(1., 1., -1.));
        let deform = source
            .iter()
            .zip(&self.source_rest)
            .map(|(p, r)| reflection * (*p) * r.inverse() * reflection)
            .collect::<Vec<_>>();
        let mut world = vec![Mat4::IDENTITY; self.parents.len()];
        let mut state = vec![0; world.len()];
        let frame = Frame {
            map: self,
            deform: &deform,
        };
        for i in 0..world.len() {
            frame.walk(i, &mut world, &mut state)?;
        }
        let result = world
            .iter()
            .enumerate()
            .map(|(i, m)| {
                if self.parents[i] >= 0 {
                    world[self.parents[i] as usize].inverse() * (*m)
                } else {
                    *m
                }
            })
            .collect::<Vec<_>>();
        if result.iter().any(|m| !m.is_finite()) {
            return Err(invalid("nonfinite retarget pose"));
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        animation::BoneCurves,
        model::Bone,
        physics::{Curve, CurveKey},
    };
    use serde_json::json;
    #[test]
    fn rest_axes_do_not_change_animation_deformation() {
        let model = Model {
            version: 2001,
            name: "test".into(),
            root: "Root".into(),
            bones: vec![Bone {
                name: "Root".into(),
                parent: -1,
                scale_helper: false,
                translation: [0.; 3],
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
            }],
            skin_bones: vec![],
            inverse_bind: vec![],
            vertices: vec![],
            tangents: vec![],
            skin: vec![],
            submeshes: vec![],
            materials: vec![],
            morphs: vec![],
        };
        let target =
            Mat4::from_scale_rotation_translation(Vec3::ONE, Quat::from_rotation_y(0.37), Vec3::Y);
        let doc = json!({"nodes":[{"name":"Root","matrix":target.to_cols_array()}]});
        let map = PoseMap::new(&model, &doc).unwrap();
        let key = |v| Curve {
            keys: vec![CurveKey {
                time: 0.,
                value: v,
                incoming: Some(0.),
                outgoing: Some(0.),
            }],
        };
        let clip = Animation {
            version: 2000,
            bones: vec![BoneCurves {
                path: "Root".into(),
                properties: [
                    (2, key(std::f32::consts::FRAC_1_SQRT_2)),
                    (3, key(std::f32::consts::FRAC_1_SQRT_2)),
                ]
                .into_iter()
                .collect(),
            }],
            bust_left: false,
            bust_right: false,
        };
        let actual = map.sample(&model, &clip, 0.).unwrap()[0];
        let expected = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2) * target;
        for (a, b) in actual.to_cols_array().iter().zip(expected.to_cols_array()) {
            assert!((a - b).abs() < 1e-6);
        }
    }
}
