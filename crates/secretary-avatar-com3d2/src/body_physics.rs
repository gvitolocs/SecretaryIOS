//! Rust implementation of the observed body-spring controls, at a 60 Hz reference
//! tick. Coordinates are the right-handed, Z-reflected game frame used by GLB.
//! This is separate from the game's DynamicBone clothing/hair solver.
use crate::{invalid, Result};
use glam::{Mat3, Mat4, Quat, Vec3};
const TICK: f64 = 1.0 / 60.0;
/// The native solver also drives the child deformation joint. Apply its
/// absolute game-local roll through the authored bind-frame corrections.
pub fn child_matrix(parent_correction:Mat4,child_correction:Mat4,translation:Vec3,scale:Vec3,roll:f32)->Result<Mat4>{
    if !parent_correction.is_finite()||!child_correction.is_finite()||child_correction.determinant().abs()<1e-8||!translation.is_finite()||!scale.is_finite()||!roll.is_finite(){return Err(invalid("body child-joint input"));}
    Ok(parent_correction*Mat4::from_scale_rotation_translation(scale,Quat::from_rotation_z(roll),translation)*child_correction.inverse())
}
#[derive(Debug, Clone, Copy)]
pub struct Parameters {
    pub size: f32,
    pub softness: f32,
    pub supported: bool,
    pub blend: f32,
    pub inward: f32,
    pub elevation: f32,
    pub side: f32,
    pub gravity: f32,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            size: 0.5,
            softness: 0.5,
            supported: true,
            blend: 1.,
            inward: 0.5,
            elevation: 0.5,
            side: 1.,
            gravity: 0.1,
        }
    }
}
impl Parameters {
    fn validate(&self) -> Result<()> {
        for (v, max) in [
            (self.size, 1.3),
            (self.softness, 1.),
            (self.blend, 1.),
            (self.inward, 1.),
            (self.elevation, 1.),
            (self.gravity, 10.),
        ] {
            if !v.is_finite() || v < 0. || v > max {
                return Err(invalid("body spring parameter range"));
            }
        }
        if self.side != 1. && self.side != -1. {
            return Err(invalid("body spring side"));
        }
        Ok(())
    }
    pub fn coefficients(&self) -> (f32, f32, f32, f32) {
        let soft = ((self.softness - 0.5) * 2.).max(0.);
        let mass = 0.4 + 1.1 * soft;
        let damping = 0.3 - 0.29 * soft;
        let blend = (self.softness * 2.).min(1.);
        let (small, large, power) = if self.supported {
            (0.27, 0.07, 0.7)
        } else {
            (0.22, 0.055, 0.5)
        };
        let x = (self.size / 1.3).powf(power);
        (mass, damping, small + (large - small) * x, blend)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Input {
    pub base_world: Mat4,
    pub parent_world: Mat4,
    /// target-rest-bone inverse * source-game-rest-bone (both in GLB space).
    pub bone_correction: Mat4,
    pub parent_correction: Mat4,
}
#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub local_matrix: Mat4,
    pub angle_degrees: f32,
    pub sub_roll: f32,
    pub steps: u32,
}
#[derive(Debug, Clone, Copy)]
pub struct BodySpring {
    tail: Option<Vec3>,
    velocity: Vec3,
    last_pivot: Vec3,
    last_soft_blend: f32,
    accumulator: f64,
}
impl Default for BodySpring {
    fn default() -> Self {
        Self::new()
    }
}
impl BodySpring {
    pub const fn new() -> Self {
        Self {
            tail: None,
            velocity: Vec3::ZERO,
            last_pivot: Vec3::ZERO,
            last_soft_blend: -1.,
            accumulator: 0.,
        }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
    pub fn impulse(&mut self, velocity: Vec3) -> Result<()> {
        if !velocity.is_finite() || velocity.length() > 0.1 {
            return Err(invalid("body impulse range"));
        }
        self.velocity += velocity;
        Ok(())
    }
    pub fn advance(&mut self, input: Input, p: Parameters, dt: f32) -> Result<Output> {
        p.validate()?;
        if !dt.is_finite() || !(0.0..=1.0).contains(&dt) {
            return Err(invalid("body spring dt"));
        }
        for matrix in [
            input.base_world,
            input.parent_world,
            input.bone_correction,
            input.parent_correction,
        ] {
            if !matrix.is_finite() || matrix.determinant().abs() < 1e-8 {
                return Err(invalid("body spring matrix"));
            }
        }
        let game = input.base_world * input.bone_correction;
        let (scale, base_rotation, pivot) = game.to_scale_rotation_translation();
        let target = game.transform_point3(Vec3::new(-0.7, 0., 0.));
        let (mass, damping, stiffness, soft_blend) = p.coefficients();
        let mut steps = 0;
        let warp = self.tail.is_none() || pivot.distance(self.last_pivot) > 0.5 || p.blend == 0.;
        if warp {
            self.tail = Some(target);
            self.velocity = Vec3::ZERO;
            self.accumulator = 0.;
            self.last_soft_blend = soft_blend;
        } else {
            self.accumulator += (dt as f64).min(0.1);
            while self.accumulator + 1e-9 >= TICK {
                self.accumulator = (self.accumulator - TICK).max(0.);
                steps += 1;
                let tail = self.tail.unwrap();
                let mut force = (target - tail) * stiffness - Vec3::Y * (p.gravity / 10.);
                self.velocity += force * ((1. - damping) / mass);
                if self.last_soft_blend != soft_blend {
                    self.velocity = Vec3::ZERO;
                    force = Vec3::ZERO;
                    self.last_soft_blend = soft_blend;
                }
                if (tail + force + self.velocity - pivot).dot(target - pivot) < 0.3 {
                    force *= 0.8;
                    self.velocity *= 0.8;
                }
                self.tail = Some(vector_slerp(
                    target,
                    tail + force + self.velocity,
                    soft_blend,
                ));
            }
        }
        self.last_pivot = pivot;
        let tail = self.tail.unwrap();
        if !tail.is_finite() || tail.distance(target) > 10. {
            self.reset();
            return Err(invalid("body spring diverged"));
        }
        let up = game.transform_vector3(Vec3::Y).normalize_or_zero();
        let z = -(tail - pivot).normalize_or_zero();
        let x = up.cross(z).normalize_or_zero();
        let y = z.cross(x).normalize_or_zero();
        let aim = if x.length_squared() > 0.5 {
            Quat::from_mat3(&Mat3::from_cols(x, y, z))
                * Quat::from_rotation_y((-90. + (p.inward - 0.5) * 25. * p.size).to_radians())
                * Quat::from_rotation_z((-(p.elevation - 0.5) * 60. * p.side * p.size).to_radians())
        } else {
            base_rotation
        };
        let rotation = base_rotation.slerp(aim.normalize(), p.blend).normalize();
        let distance = tail.distance(target);
        let stretch = Vec3::new(
            1. + distance * 0.3,
            1. + distance * 0.2,
            1. + distance * 0.2,
        );
        let desired = Mat4::from_scale_rotation_translation(scale * stretch, rotation, pivot)
            * input.bone_correction.inverse();
        let local = input.parent_world.inverse() * desired;
        let angle = 2.
            * base_rotation
                .dot(rotation)
                .abs()
                .clamp(0., 1.)
                .acos()
                .to_degrees();
        let parent_up = (input.parent_world * input.parent_correction)
            .transform_vector3(Vec3::Y)
            .normalize_or_zero();
        let sub_roll = std::f32::consts::PI
            + ((1. - parent_up.dot(-Vec3::Y).abs()) * (-p.side) * 0.8
                + (p.elevation - 0.5) * (p.size * 0.5))
                * p.size.min(1.)
                * p.blend;
        Ok(Output {
            local_matrix: local,
            angle_degrees: angle,
            sub_roll,
            steps,
        })
    }
}
fn vector_slerp(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    let (la, lb) = (a.length(), b.length());
    if la < 1e-6 || lb < 1e-6 {
        return a.lerp(b, t);
    }
    let (na, nb) = (a / la, b / lb);
    let angle = na.dot(nb).clamp(-1., 1.).acos();
    if angle.abs() < 1e-5 || angle.sin().abs() < 1e-5 {
        return a.lerp(b, t);
    }
    (na * ((1. - t) * angle).sin() + nb * (t * angle).sin()) / angle.sin() * (la + (lb - la) * t)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> Input {
        Input {
            base_world: Mat4::from_translation(Vec3::new(0., 1.2, 0.)),
            parent_world: Mat4::IDENTITY,
            bone_correction: Mat4::IDENTITY,
            parent_correction: Mat4::IDENTITY,
        }
    }
    fn error(a: Mat4, b: Mat4) -> f32 {
        a.to_cols_array()
            .iter()
            .zip(b.to_cols_array())
            .map(|(a, b)| (a - b).abs())
            .fold(0., f32::max)
    }
    #[test]
    fn rest_pose_is_preserved_with_rotated_bone_axes() {
        let mut i = input();
        i.bone_correction = Mat4::from_rotation_z(1.1);
        let p = Parameters {
            gravity: 0.,
            ..Default::default()
        };
        let mut state = BodySpring::new();
        state.advance(i, p, 0.).unwrap();
        for _ in 0..120 {
            let o = state.advance(i, p, 1. / 60.).unwrap();
            assert!(error(o.local_matrix, i.base_world) < 0.00001);
        }
    }
    #[test]
    fn constant_input_matches_at_30_60_120_hz() {
        let p = Parameters::default();
        let mut outputs = Vec::new();
        for hz in [30, 60, 120] {
            let mut state = BodySpring::new();
            state.advance(input(), p, 0.).unwrap();
            for _ in 0..hz * 3 {
                state.advance(input(), p, 1. / hz as f32).unwrap();
            }
            outputs.push(state.advance(input(), p, 0.).unwrap().local_matrix);
        }
        assert!(error(outputs[0], outputs[1]) < 0.00001);
        assert!(error(outputs[1], outputs[2]) < 0.00001);
    }
    #[test]
    fn an_impulse_settles_and_a_teleport_resets() {
        let p = Parameters {
            gravity: 0.,
            ..Default::default()
        };
        let i = input();
        let mut state = BodySpring::new();
        state.advance(i, p, 0.).unwrap();
        state.impulse(Vec3::new(0., 0.015, 0.)).unwrap();
        let first = state.advance(i, p, 1. / 60.).unwrap();
        assert!(first.angle_degrees > 0.1);
        for _ in 0..600 {
            state.advance(i, p, 1. / 60.).unwrap();
        }
        assert!(state.advance(i, p, 0.).unwrap().angle_degrees < 0.05);
        let j = Input {
            base_world: Mat4::from_translation(Vec3::new(2., 1.2, 0.)),
            ..i
        };
        assert!(
            error(
                state.advance(j, p, 1. / 60.).unwrap().local_matrix,
                j.base_world
            ) < 0.00001
        );
    }
    #[test]
    fn malformed_inputs_are_rejected() {
        let mut state = BodySpring::new();
        assert!(state
            .advance(
                input(),
                Parameters {
                    softness: f32::NAN,
                    ..Default::default()
                },
                0.
            )
            .is_err());
        assert!(state.advance(input(), Parameters::default(), -1.).is_err());
        assert!(state.impulse(Vec3::splat(f32::INFINITY)).is_err());
    }
}
