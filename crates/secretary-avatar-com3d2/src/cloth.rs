//! Coupled, pinned cloth sheets in world metres. XPBD stretch/bend constraints
//! connect neighbouring chains; each strand is no longer an isolated pendulum.
use crate::{invalid, secondary_physics::Capsule, Result};
use glam::{Mat4, Quat, Vec3};
pub const MAX_NODES: usize = 1152;
#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub closed: bool,
    pub iterations: usize,
    pub damping: f32,
    pub horizontal_compliance: f32,
    pub vertical_compliance: f32,
    pub bend_compliance: f32,
    pub support: f32,
    pub radius: f32,
    pub floor: f32,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            closed: true,
            iterations: 16,
            damping: 13.4,
            horizontal_compliance: 0.0001,
            vertical_compliance: 0.0000014,
            bend_compliance: 0.002,
            support: 0.5,
            radius: 0.008,
            floor: -0.03,
        }
    }
}
#[derive(Clone, Debug)]
struct Link {
    a: usize,
    b: usize,
    length: f32,
    compliance: f32,
    lambda: f32,
    stretch_only: bool,
}
#[derive(Debug)]
pub struct Grid {
    pub columns: usize,
    pub rows: usize,
    pub parameters: Parameters,
    worlds: Vec<Mat4>,
    external: Vec<Mat4>,
    parents: Vec<i32>,
    gravity_weights: Vec<f32>,
    points: Vec<Vec3>,
    previous: Vec<Vec3>,
    velocities: Vec<Vec3>,
    links: Vec<Link>,
    output_world: Vec<Mat4>,
    output: Vec<Mat4>,
    initialized: bool,
    accumulator: f64,
    pub contacts: u32,
    pub max_drop: f32,
    pub steps: u32,
}
impl Grid {
    pub fn new(columns: usize, rows: usize, parameters: Parameters) -> Result<Self> {
        let count = columns
            .checked_mul(rows)
            .filter(|n| *n <= MAX_NODES)
            .ok_or_else(|| invalid("cloth dimensions"))?;
        if !(3..=24).contains(&columns)
            || !(2..=48).contains(&rows)
            || !(1..=32).contains(&parameters.iterations)
            || ![
                parameters.damping,
                parameters.horizontal_compliance,
                parameters.vertical_compliance,
                parameters.bend_compliance,
                parameters.support,
                parameters.radius,
                parameters.floor,
            ]
            .iter()
            .all(|v| v.is_finite())
            || !(0.0..=40.).contains(&parameters.damping)
            || !(0.0..=0.1).contains(&parameters.horizontal_compliance)
            || !(0.0..=0.1).contains(&parameters.vertical_compliance)
            || !(0.0..=0.1).contains(&parameters.bend_compliance)
            || !(0.0..=20.).contains(&parameters.support)
            || !(0.0..=0.05).contains(&parameters.radius)
        {
            return Err(invalid("cloth parameters"));
        }
        Ok(Self {
            columns,
            rows,
            parameters,
            worlds: vec![Mat4::IDENTITY; count],
            external: vec![Mat4::IDENTITY; count],
            parents: vec![-1; count],
            gravity_weights: vec![1.; count],
            points: vec![Vec3::ZERO; count],
            previous: vec![Vec3::ZERO; count],
            velocities: vec![Vec3::ZERO; count],
            links: Vec::new(),
            output_world: vec![Mat4::IDENTITY; count],
            output: vec![Mat4::IDENTITY; count],
            initialized: false,
            accumulator: 0.,
            contacts: 0,
            max_drop: 0.,
            steps: 0,
        })
    }
    pub fn len(&self) -> usize {
        self.points.len()
    }
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
    pub fn set_pose(
        &mut self,
        i: usize,
        world: Mat4,
        parent: Mat4,
        parent_index: i32,
        gravity_weight: f32,
    ) -> Result<()> {
        if i >= self.len()
            || parent_index < -1
            || parent_index >= i as i32
            || !world.is_finite()
            || !parent.is_finite()
            || world.determinant().abs() < 1e-8
            || parent.determinant().abs() < 1e-8
            || !gravity_weight.is_finite()
            || !(0.0..=4.).contains(&gravity_weight)
        {
            return Err(invalid("cloth pose"));
        }
        self.worlds[i] = world;
        self.external[i] = parent;
        self.parents[i] = parent_index;
        self.gravity_weights[i] = gravity_weight;
        Ok(())
    }
    pub fn reset(&mut self) {
        self.initialized = false;
        self.accumulator = 0.;
    }
    pub fn impulse(&mut self, velocity: Vec3) -> Result<()> {
        if !velocity.is_finite() || velocity.length() > 2. {
            return Err(invalid("cloth impulse"));
        }
        for v in &mut self.velocities[self.columns..] {
            *v += velocity;
        }
        Ok(())
    }
    fn goal(&self, i: usize) -> Vec3 {
        self.worlds[i].w_axis.truncate()
    }
    fn initialize(&mut self) -> Result<()> {
        for i in 0..self.len() {
            self.points[i] = self.goal(i);
            self.previous[i] = self.points[i];
            self.velocities[i] = Vec3::ZERO;
        }
        self.links.clear();
        let cols = self.columns;
        for row in 0..self.rows {
            for col in 0..cols {
                let a = row * cols + col;
                let mut pairs = Vec::new();
                if self.parameters.closed || col + 1 < cols {
                    pairs.push((
                        row * cols + (col + 1) % cols,
                        self.parameters.horizontal_compliance,
                        true,
                    ));
                }
                if row + 1 < self.rows {
                    pairs.push((a + cols, self.parameters.vertical_compliance, true));
                }
                if row + 2 < self.rows {
                    pairs.push((a + 2 * cols, self.parameters.bend_compliance, false));
                }
                if self.parameters.closed || col + 2 < cols {
                    pairs.push((
                        row * cols + (col + 2) % cols,
                        self.parameters.bend_compliance * 2.,
                        true,
                    ));
                }
                for (b, compliance, stretch_only) in pairs {
                    let length = self.points[a].distance(self.points[b]);
                    if !(0.00001..=5.).contains(&length) {
                        return Err(invalid("cloth degenerate rest edge"));
                    }
                    self.links.push(Link {
                        a,
                        b,
                        length,
                        compliance,
                        lambda: 0.,
                        stretch_only,
                    });
                }
            }
        }
        self.initialized = true;
        self.accumulator = 0.;
        Ok(())
    }
    pub fn advance(&mut self, capsules: &[Capsule], dt: f32, gravity: bool) -> Result<&[Mat4]> {
        if !dt.is_finite()
            || !(0.0..=0.25).contains(&dt)
            || capsules.len() > 16
            || capsules.iter().any(|c| {
                !c.a.is_finite()
                    || !c.b.is_finite()
                    || !c.radius.is_finite()
                    || !(0.0..=0.5).contains(&c.radius)
            })
        {
            return Err(invalid("cloth frame"));
        }
        if !self.initialized || self.points[0].distance(self.goal(0)) > 0.5 {
            self.initialize()?;
        }
        self.contacts = 0;
        self.steps = 0;
        self.max_drop = 0.;
        self.accumulator += f64::from(dt);
        let tick = 1. / 60.;
        while self.accumulator + 1e-9 >= tick {
            self.accumulator -= tick;
            self.steps += 1;
            let h = tick as f32;
            for i in 0..self.len() {
                self.previous[i] = self.points[i];
                if i < self.columns {
                    self.points[i] = self.goal(i);
                    self.velocities[i] = Vec3::ZERO;
                    continue;
                }
                let acceleration = if gravity {
                    -Vec3::Y * 9.81 * self.gravity_weights[i]
                } else {
                    Vec3::ZERO
                } + (self.goal(i) - self.points[i]) * self.parameters.support;
                self.velocities[i] =
                    (self.velocities[i] + acceleration * h) * (-self.parameters.damping * h).exp();
                self.points[i] += self.velocities[i] * h;
            }
            for link in &mut self.links {
                link.lambda = 0.;
            }
            for _ in 0..self.parameters.iterations {
                for link in &mut self.links {
                    let delta = self.points[link.b] - self.points[link.a];
                    let distance = delta.length();
                    if distance < 1e-8 {
                        continue;
                    }
                    let error = distance - link.length;
                    let wa = if link.a < self.columns { 0. } else { 1. };
                    let wb = if link.b < self.columns { 0. } else { 1. };
                    let alpha = link.compliance / (h * h);
                    if wa + wb == 0. {
                        continue;
                    }
                    let dlambda = (-error - alpha * link.lambda) / (wa + wb + alpha);
                    let next = if link.stretch_only {
                        (link.lambda + dlambda).min(0.)
                    } else {
                        link.lambda + dlambda
                    };
                    let correction = delta / distance * (next - link.lambda);
                    link.lambda = next;
                    self.points[link.a] -= correction * wa;
                    self.points[link.b] += correction * wb;
                }
                for i in self.columns..self.len() {
                    for c in capsules {
                        let ab = c.b - c.a;
                        let t = if ab.length_squared() > 1e-10 {
                            ((self.points[i] - c.a).dot(ab) / ab.length_squared()).clamp(0., 1.)
                        } else {
                            0.
                        };
                        let center = c.a + ab * t;
                        let offset = self.points[i] - center;
                        let radius = c.radius + self.parameters.radius;
                        if offset.length_squared() < radius * radius {
                            let normal = if offset.length_squared() > 1e-12 {
                                offset.normalize()
                            } else {
                                (self.goal(i) - center).normalize_or_zero()
                            };
                            self.points[i] = center + normal * radius;
                            self.contacts += 1;
                        }
                    }
                    self.points[i].y = self.points[i].y.max(self.parameters.floor);
                }
                for i in 0..self.columns {
                    self.points[i] = self.goal(i);
                }
            }
            for i in self.columns..self.len() {
                self.velocities[i] = (self.points[i] - self.previous[i]) / h;
                let speed = self.velocities[i].length();
                if speed > 5. {
                    self.velocities[i] *= 5. / speed;
                }
            }
        }
        let alpha = (self.accumulator / tick).clamp(0., 1.) as f32;
        for i in 0..self.len() {
            let point = if i < self.columns {
                self.goal(i)
            } else {
                self.previous[i].lerp(self.points[i], alpha)
            };
            let (a, b) = if i + self.columns < self.len() {
                (i, i + self.columns)
            } else {
                (i - self.columns, i)
            };
            let rest = (self.goal(b) - self.goal(a)).normalize_or_zero();
            let pa = if a < self.columns {
                self.goal(a)
            } else {
                self.previous[a].lerp(self.points[a], alpha)
            };
            let pb = self.previous[b].lerp(self.points[b], alpha);
            let direction = (pb - pa).normalize_or_zero();
            let rotation = if rest.length_squared() > 0.5 && direction.length_squared() > 0.5 {
                Quat::from_rotation_arc(rest, direction)
            } else {
                Quat::IDENTITY
            };
            let mut target = Mat4::from_quat(rotation) * self.worlds[i];
            target.w_axis = point.extend(1.);
            self.output_world[i] = target;
            let parent = if self.parents[i] >= 0 {
                self.output_world[self.parents[i] as usize]
            } else {
                self.external[i]
            };
            self.output[i] = parent.inverse() * target;
            self.max_drop = self.max_drop.max(self.goal(i).y - point.y);
        }
        Ok(&self.output)
    }
    pub fn positions(&self) -> &[Vec3] {
        &self.points
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Grid {
        let cols = 12;
        let rows = 5;
        let mut grid = Grid::new(cols, rows, Parameters::default()).unwrap();
        for row in 0..rows {
            for col in 0..cols {
                let t = row as f32 / (rows - 1) as f32;
                let a = col as f32 * std::f32::consts::TAU / cols as f32;
                let radius = 0.14 + 0.20 * t;
                let point = Vec3::new(radius * a.sin(), 1. - 0.5 * t, radius * a.cos());
                let i = row * cols + col;
                grid.set_pose(
                    i,
                    Mat4::from_translation(point),
                    Mat4::IDENTITY,
                    if row == 0 { -1 } else { (i - cols) as i32 },
                    1.,
                )
                .unwrap();
            }
        }
        grid
    }
    #[test]
    fn gravity_drapes_flared_sheet_without_moving_the_waist_or_stretching_seams() {
        let mut g = fixture();
        g.advance(&[], 0., true).unwrap();
        let before = g.points.clone();
        for _ in 0..600 {
            g.advance(&[], 1. / 60., true).unwrap();
        }
        let cols = g.columns;
        for (i, p) in before.iter().enumerate().take(cols) {
            assert!(g.points[i].distance(*p) < 1e-6);
        }
        let bottom = (g.rows - 1) * cols;
        let drop = (bottom..g.len())
            .map(|i| before[i].y - g.points[i].y)
            .sum::<f32>()
            / cols as f32;
        assert!(drop > 0.02, "drop={drop}");
        for l in &g.links {
            if l.stretch_only {
                assert!(g.points[l.a].distance(g.points[l.b]) < l.length * 1.06 + 0.002);
            }
        }
        assert!(g
            .points
            .iter()
            .all(|p| p.is_finite() && p.y >= g.parameters.floor));
    }
    #[test]
    fn zero_gravity_preserves_authored_shape_and_fixed_ticks_agree() {
        let mut zero = fixture();
        zero.advance(&[], 0., false).unwrap();
        let start = zero.points.clone();
        for _ in 0..120 {
            zero.advance(&[], 1. / 60., false).unwrap();
        }
        assert!(zero
            .points
            .iter()
            .zip(&start)
            .all(|(a, b)| a.distance(*b) < 1e-5));
        let run = |hz| {
            let mut g = fixture();
            for _ in 0..hz * 2 {
                g.advance(&[], 1. / hz as f32, true).unwrap();
            }
            g.points
        };
        let at30 = run(30);
        for other in [run(60), run(120)] {
            assert!(other.iter().zip(&at30).all(|(a, b)| a.distance(*b) < 1e-4));
        }
    }
    #[test]
    fn body_contacts_floor_and_teleport_are_bounded() {
        let mut g = fixture();
        let capsule = Capsule {
            a: Vec3::new(0., 0.15, 0.),
            b: Vec3::new(0., 0.9, 0.),
            radius: 0.17,
        };
        let mut contacts = 0;
        for _ in 0..180 {
            g.advance(&[capsule], 1. / 60., true).unwrap();
            contacts += g.contacts;
        }
        assert!(contacts > 0);
        for p in &g.points[g.columns..] {
            let t = ((p.y - capsule.a.y) / (capsule.b.y - capsule.a.y)).clamp(0., 1.);
            assert!(
                p.distance(capsule.a.lerp(capsule.b, t))
                    >= capsule.radius + g.parameters.radius - 1e-4
            );
        }
        for m in &mut g.worlds {
            m.w_axis.x += 2.;
        }
        g.advance(&[], 0., true).unwrap();
        assert!(g.points[0].distance(g.goal(0)) < 1e-6);
    }
    #[test]
    fn full_matrices_keep_scaled_parent_frames_and_invalid_inputs_are_rejected() {
        let mut g = fixture();
        let parent = Mat4::from_scale_rotation_translation(
            Vec3::new(1.2, 0.8, 1.1),
            Quat::from_rotation_z(0.2),
            Vec3::ZERO,
        );
        g.external[0] = parent;
        let root = g.worlds[0];
        let out = g.advance(&[], 0., false).unwrap();
        assert!((parent * out[0]).abs_diff_eq(root, 1e-5));
        assert!(Grid::new(0, 4, Parameters::default()).is_err());
        assert!(Grid::new(24, 49, Parameters::default()).is_err());
        assert!(g.advance(&[], f32::NAN, true).is_err());
        assert!(g.set_pose(0, Mat4::NAN, Mat4::IDENTITY, -1, 1.).is_err());
    }
}
