//! Constrained secondary motion for authored hair and loose garment joints.
//! This is a Rust spring/particle solver, not native DynamicBone parity.
use crate::{invalid, Result};
use glam::{Mat4, Quat, Vec3};
#[derive(Clone, Copy, Debug)]
pub struct Capsule {
    pub a: Vec3,
    pub b: Vec3,
    pub radius: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub world: Mat4,
    pub parent: Mat4,
    pub tail: Vec3,
    pub stiffness: f32,
    pub damping: f32,
    pub gravity: f32,
    pub max_angle: f32,
    pub radius: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct Output {
    pub local: Mat4,
    pub angle: f32,
    pub contacts: u32,
    pub drop_meters: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    tail: Option<Vec3>,
    velocity: Vec3,
    pivot: Vec3,
    accumulator: f64,
    previous: Vec3,
}
impl Default for Spring {
    fn default() -> Self {
        Self::new()
    }
}
impl Spring {
    pub const fn new() -> Self {
        Self {
            tail: None,
            velocity: Vec3::ZERO,
            pivot: Vec3::ZERO,
            accumulator: 0.,
            previous: Vec3::ZERO,
        }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
    pub fn impulse(&mut self, v: Vec3) -> Result<()> {
        if !v.is_finite() || v.length() > 2. {
            return Err(invalid("secondary impulse"));
        }
        self.velocity += v;
        Ok(())
    }
    pub fn advance(&mut self, input: Input, capsules: &[Capsule], dt: f32) -> Result<Output> {
        if !dt.is_finite()
            || !(0.0..=0.25).contains(&dt)
            || !input.world.is_finite()
            || !input.parent.is_finite()
            || input.world.determinant().abs() < 1e-8
            || input.parent.determinant().abs() < 1e-8
            || !input.tail.is_finite()
            || ![
                input.stiffness,
                input.damping,
                input.gravity,
                input.max_angle,
                input.radius,
            ]
            .iter()
            .all(|v| v.is_finite())
            || !(0.0..=200.).contains(&input.stiffness)
            || !(0.0..=40.).contains(&input.damping)
            || !(0.0..=20.).contains(&input.gravity)
            || !(0.01..=3.0).contains(&input.max_angle)
            || !(0.0..=0.05).contains(&input.radius)
            || capsules.len() > 16
            || capsules.iter().any(|c| {
                !c.a.is_finite()
                    || !c.b.is_finite()
                    || !c.radius.is_finite()
                    || !(0.0..=0.5).contains(&c.radius)
            })
        {
            return Err(invalid("secondary spring input"));
        }
        let pivot = input.world.w_axis.truncate();
        let goal = input.world.transform_point3(input.tail);
        let vector = goal - pivot;
        let length = vector.length();
        if !(0.005..=2.).contains(&length) {
            return Err(invalid("secondary tail length"));
        }
        if self.tail.is_none() || pivot.distance(self.pivot) > 0.5 {
            self.reset();
            self.tail = Some(goal);
            self.previous = goal;
            self.pivot = pivot;
        }
        self.pivot = pivot;
        self.accumulator += f64::from(dt);
        let tick = 1. / 60.;
        let mut tail = self.tail.unwrap();
        let mut contacts = 0;
        while self.accumulator + 1e-9 >= tick {
            self.accumulator -= tick;
            let h = tick as f32;
            let force = (goal - tail) * input.stiffness - Vec3::Y * input.gravity;
            self.velocity = (self.velocity + force * h) * (-input.damping * h).exp();
            let old = tail;
            self.previous = tail;
            tail += self.velocity * h;
            // Alternate length, cone and capsule projections. Root position is pinned.
            for _ in 0..4 {
                let direction = (tail - pivot).normalize_or_zero();
                let axis = vector / length;
                let angle = axis.dot(direction).clamp(-1., 1.).acos();
                let direction = if angle > input.max_angle {
                    Quat::IDENTITY.slerp(
                        Quat::from_rotation_arc(axis, direction),
                        input.max_angle / angle,
                    ) * axis
                } else {
                    direction
                };
                tail = pivot + direction * length;
                for c in capsules {
                    // A clear tip does not mean a clear lock: the shaft can cut
                    // straight through a shoulder. Keep the root pinned and
                    // project intermediate samples back onto the length sphere.
                    for fraction in [0.4, 0.7, 1.] {
                        let sample = pivot.lerp(tail, fraction);
                        let ab = c.b - c.a;
                        let t = if ab.length_squared() > 1e-10 {
                            (sample - c.a).dot(ab) / ab.length_squared()
                        } else {
                            0.
                        };
                        let center = c.a + ab * t.clamp(0., 1.);
                        let offset = sample - center;
                        let radius = c.radius + input.radius;
                        if offset.length_squared() < radius * radius {
                            let normal = if offset.length_squared() > 1e-12 {
                                offset.normalize()
                            } else {
                                vector.normalize()
                            };
                            let push = normal * (radius - offset.length()) / fraction;
                            tail = pivot + (tail + push - pivot).normalize_or_zero() * length;
                            contacts += 1;
                        }
                    }
                }
            }
            self.velocity = (tail - old) / h;
            let speed = self.velocity.length();
            if speed > 3. {
                self.velocity *= 3. / speed;
            }
        }
        self.tail = Some(tail);
        let alpha = (self.accumulator / tick).clamp(0., 1.) as f32;
        let rendered = self.previous.lerp(tail, alpha);
        let direction = (rendered - pivot).normalize_or_zero();
        let axis = vector / length;
        let delta = Quat::from_rotation_arc(axis, direction);
        let mut world = Mat4::from_quat(delta) * input.world;
        world.w_axis = input.world.w_axis;
        Ok(Output {
            local: input.parent.inverse() * world,
            angle: axis.dot(direction).clamp(-1., 1.).acos(),
            contacts,
            drop_meters: goal.y - rendered.y,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> Input {
        Input {
            world: Mat4::IDENTITY,
            parent: Mat4::IDENTITY,
            tail: Vec3::new(0., -0.3, 0.),
            stiffness: 45.,
            damping: 9.,
            gravity: 1.,
            max_angle: 0.7,
            radius: 0.005,
        }
    }
    #[test]
    fn clear_tip_cannot_hide_a_shaft_intersection() {
        let mut s = Spring::new();
        let mut i = input();
        i.gravity = 0.;
        i.max_angle = 1.4;
        let c = Capsule {
            a: Vec3::new(0., -0.15, 0.01),
            b: Vec3::new(0., -0.15, 0.01),
            radius: 0.06,
        };
        // Both endpoints are outside this sphere, but the middle crosses it.
        assert!(i.tail.distance(c.a) > c.radius);
        let mut contacts = 0;
        for _ in 0..300 {
            contacts += s.advance(i, &[c], 1. / 60.).unwrap().contacts;
        }
        let tail = s.tail.unwrap();
        let t = ((c.a.dot(tail)) / tail.length_squared()).clamp(0., 1.);
        assert!(contacts > 0);
        assert!((tail * t).distance(c.a) > c.radius);
        assert!((tail.length() - i.tail.length()).abs() < 1e-5);
    }
    #[test]
    fn scaled_native_parents_do_not_shear_or_move_the_pinned_root() {
        let parent = Mat4::from_scale_rotation_translation(
            Vec3::new(1.2, 0.8, 1.1),
            Quat::from_rotation_z(0.4),
            Vec3::new(0.1, 1.4, 0.),
        );
        let local = Mat4::from_rotation_y(0.5);
        let mut i = input();
        i.parent = parent;
        i.world = parent * local;
        i.gravity = 0.;
        let mut s = Spring::new();
        let rest = s.advance(i, &[], 0.).unwrap();
        assert!(rest.local.abs_diff_eq(local, 1e-5));
        s.impulse(Vec3::X * 0.2).unwrap();
        for _ in 0..60 {
            let out = s.advance(i, &[], 1. / 60.).unwrap();
            let w = parent * out.local;
            assert!(w.w_axis.abs_diff_eq(i.world.w_axis, 1e-5));
            let actual = w.transform_vector3(i.tail).length();
            let authored = i.world.transform_vector3(i.tail).length();
            assert!((actual - authored).abs() < 1e-5);
        }
    }

    #[test]
    fn moving_root_preserves_length_and_settles_without_wind() {
        let mut s = Spring::new();
        s.advance(input(), &[], 1. / 60.).unwrap();
        s.impulse(Vec3::new(0.6, 0., 0.)).unwrap();
        let mut peak = 0f32;
        let mut out = None;
        for _ in 0..600 {
            let r = s.advance(input(), &[], 1. / 60.).unwrap();
            peak = peak.max(r.angle);
            out = Some(r);
        }
        assert!(peak > 0.01 && peak < 0.71);
        assert!(out.unwrap().angle < 0.005);
        assert!((s.tail.unwrap().length() - 0.3).abs() < 1e-4);
    }
    #[test]
    fn capsule_contact_and_teleport_reset() {
        let mut s = Spring::new();
        let mut i = input();
        i.tail = Vec3::new(0., -0.25, 0.12);
        let c = Capsule {
            a: Vec3::new(0., -0.3, 0.),
            b: Vec3::new(0., 0., 0.),
            radius: 0.13,
        };
        let mut contacts = 0;
        for _ in 0..120 {
            contacts += s.advance(i, &[c], 1. / 60.).unwrap().contacts;
        }
        assert!(contacts > 0);
        assert!(s.tail.unwrap().z >= 0.134);
        i.world = Mat4::from_translation(Vec3::X * 2.);
        let out = s.advance(i, &[], 0.).unwrap();
        assert!(out.angle < 0.001);
    }
    #[test]
    fn fixed_ticks_agree_at_30_60_120_hz() {
        let run = |hz| {
            let mut s = Spring::new();
            s.advance(input(), &[], 0.).unwrap();
            s.impulse(Vec3::X * 0.3).unwrap();
            for _ in 0..hz {
                s.advance(input(), &[], 1. / hz as f32).unwrap();
            }
            s.tail.unwrap()
        };
        assert!(run(30).distance(run(120)) < 1e-5);
        assert!(run(60).distance(run(120)) < 1e-5);
    }
    #[test]
    fn rejects_nonfinite_and_excessive_inputs() {
        let mut s = Spring::new();
        assert!(s.advance(input(), &[], f32::NAN).is_err());
        let mut i = input();
        i.tail = Vec3::ZERO;
        assert!(s.advance(i, &[], 0.).is_err());
        assert!(s.impulse(Vec3::X * 3.).is_err());
    }
}
#[cfg(test)]
mod gravity_tests {
    use super::*;
    #[test]
    fn gravity_drops_a_horizontal_strand_without_an_impulse_and_zero_gravity_does_not() {
        let run = |gravity| {
            let mut s = Spring::new();
            let i = Input {
                world: Mat4::from_rotation_y(0.7),
                parent: Mat4::IDENTITY,
                tail: Vec3::X * 0.3,
                stiffness: 22.,
                damping: 12.,
                gravity,
                max_angle: 1.65,
                radius: 0.005,
            };
            let mut out = None;
            for _ in 0..300 {
                out = Some(s.advance(i, &[], 1. / 60.).unwrap());
            }
            out.unwrap()
        };
        let without = run(0.);
        let with = run(9.81);
        assert!(without.drop_meters.abs() < 1e-5);
        assert!(
            with.drop_meters > 0.20,
            "measured drop: {}",
            with.drop_meters
        );
        assert!(with.angle > 0.7);
    }
}
