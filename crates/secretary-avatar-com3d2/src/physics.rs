use crate::{binary::Reader, invalid, Result};
use serde::Serialize;
#[derive(Debug, Clone, Serialize)]
pub struct CurveKey {
    pub time: f32,
    pub value: f32,
    pub incoming: Option<f32>,
    pub outgoing: Option<f32>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Curve {
    pub keys: Vec<CurveKey>,
}
impl Curve {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self> {
        let count = r.count(4096)?;
        let mut keys = Vec::new();
        let mut last = f32::NEG_INFINITY;
        for _ in 0..count {
            let time = r.f32()?;
            let value = r.f32()?;
            if time < last {
                return Err(invalid("unsorted physics curve"));
            }
            last = time;
            let mut tangent = || -> Result<Option<f32>> {
                let v = f32::from_bits(r.u32()?);
                if v.is_nan() {
                    return Err(invalid("NaN curve tangent"));
                }
                Ok(if v.is_finite() { Some(v) } else { None })
            };
            keys.push(CurveKey {
                time,
                value,
                incoming: tangent()?,
                outgoing: tangent()?,
            });
        }
        Ok(Self { keys })
    }
    /// Hermite keyframe interpolation; infinite tangents represent held steps.
    pub fn sample(&self, t: f32) -> f32 {
        let Some(first) = self.keys.first() else {
            return 1.;
        };
        if t <= first.time {
            return first.value;
        }
        for pair in self.keys.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if t < b.time {
                let dt = b.time - a.time;
                if dt <= f32::EPSILON {
                    return b.value;
                }
                let (Some(m0), Some(m1)) = (a.outgoing, b.incoming) else {
                    return a.value;
                };
                let x = (t - a.time) / dt;
                let x2 = x * x;
                let x3 = x2 * x;
                return (2. * x3 - 3. * x2 + 1.) * a.value
                    + (x3 - 2. * x2 + x) * dt * m0
                    + (-2. * x3 + 3. * x2) * b.value
                    + (x3 - x2) * dt * m1;
            }
        }
        self.keys.last().unwrap().value
    }
}
#[derive(Debug, Serialize)]
pub struct Parameter {
    pub mode: i32,
    pub per_bone: Vec<(String, f32)>,
    pub value: f32,
    pub distribution: Curve,
}
impl Parameter {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let mode = r.i32()?;
        if !(0..=2).contains(&mode) {
            return Err(invalid("physics partial mode"));
        }
        let mut per_bone = Vec::new();
        if mode == 1 {
            let n = r.count(4096)?;
            for _ in 0..n {
                per_bone.push((r.string()?, r.f32()?));
            }
        }
        Ok(Self {
            mode,
            per_bone,
            value: r.f32()?,
            distribution: Curve::read(r)?,
        })
    }
}
#[derive(Debug, Serialize)]
pub struct Physics {
    pub version: i32,
    pub root: String,
    pub damping: Parameter,
    pub elasticity: Parameter,
    pub stiffness: Parameter,
    pub inertia: Parameter,
    pub radius: Parameter,
    pub end_length: f32,
    pub end_offset: [f32; 3],
    pub gravity: [f32; 3],
    pub force: [f32; 3],
    pub collider_file: String,
    pub collider_count: usize,
    pub exclusions_count: usize,
    pub freeze_axis: i32,
}
impl Physics {
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.string()? != "CM3D21_PHY" {
            return Err(invalid("physics signature"));
        }
        let p = Self {
            version: r.i32()?,
            root: r.string()?,
            damping: Parameter::read(&mut r)?,
            elasticity: Parameter::read(&mut r)?,
            stiffness: Parameter::read(&mut r)?,
            inertia: Parameter::read(&mut r)?,
            radius: Parameter::read(&mut r)?,
            end_length: r.f32()?,
            end_offset: r.array()?,
            gravity: r.array()?,
            force: r.array()?,
            collider_file: r.string()?,
            collider_count: r.count(4096)?,
            exclusions_count: r.count(4096)?,
            freeze_axis: r.i32()?,
        };
        if !(0..=3).contains(&p.freeze_axis) || r.left() != 0 {
            return Err(invalid("physics axis or trailing bytes"));
        }
        Ok(p)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn curves_keep_endpoints_and_step_tangents() {
        let mut c = Curve {
            keys: vec![
                CurveKey {
                    time: 0.,
                    value: 0.,
                    incoming: Some(0.),
                    outgoing: Some(1.),
                },
                CurveKey {
                    time: 1.,
                    value: 1.,
                    incoming: Some(1.),
                    outgoing: Some(0.),
                },
            ],
        };
        assert_eq!(c.sample(-1.), 0.);
        assert!((c.sample(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(c.sample(2.), 1.);
        c.keys[0].outgoing = None;
        assert_eq!(c.sample(0.9), 0.);
        assert_eq!(c.sample(1.), 1.);
    }
}
/// Native skirt-sheet settings. Values are retained in their original units;
/// the Rust solver uses SI gravity and timestep-independent constraint compliance.
#[derive(Debug, Serialize)]
pub struct SkirtPhysics {
    pub version: i32,
    pub panier_radius: f32,
    pub radius_distribution: Curve,
    pub radius_groups: Vec<(String, f32, Curve)>,
    pub panier_force: f32,
    pub force_distribution: Curve,
    pub stress_force: f32,
    pub stress_min_degrees: f32,
    pub stress_max_degrees: f32,
    pub stress_min_scale: f32,
    pub scale_ease_speed: f32,
    pub force_distance_threshold: f32,
    pub iterations: usize,
    pub velocity_retention: f32,
    pub velocity_distribution: Curve,
    pub gravity: [f32; 3],
    pub gravity_distribution: Curve,
    pub connection_strength: [f32; 4],
}
impl SkirtPhysics {
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.string()? != "CM3D21_PSK" {
            return Err(invalid("skirt physics signature"));
        }
        let version = r.i32()?;
        if !(200..=10000).contains(&version) {
            return Err(invalid("skirt physics version"));
        }
        let panier_radius = r.f32()?;
        let radius_distribution = Curve::read(&mut r)?;
        let mut radius_groups = Vec::new();
        if version >= 217 {
            for _ in 0..r.count(4096)? {
                radius_groups.push((r.string()?, r.f32()?, Curve::read(&mut r)?));
            }
        }
        let result = Self {
            version,
            panier_radius,
            radius_distribution,
            radius_groups,
            panier_force: r.f32()?,
            force_distribution: Curve::read(&mut r)?,
            stress_force: r.f32()?,
            stress_min_degrees: r.f32()?,
            stress_max_degrees: r.f32()?,
            stress_min_scale: r.f32()?,
            scale_ease_speed: r.f32()?,
            force_distance_threshold: r.f32()?,
            iterations: r.count(128)?,
            velocity_retention: r.f32()?,
            velocity_distribution: Curve::read(&mut r)?,
            gravity: r.array()?,
            gravity_distribution: Curve::read(&mut r)?,
            connection_strength: r.array()?,
        };
        if r.left() != 0
            || result.iterations == 0
            || !(0.0..=1.).contains(&result.velocity_retention)
            || !(0.0..=0.5).contains(&result.panier_radius)
            || result
                .connection_strength
                .iter()
                .any(|v| !(0.0..=1.).contains(v))
        {
            return Err(invalid("skirt physics parameters"));
        }
        Ok(result)
    }
}
#[cfg(test)]
mod skirt_tests {
    use super::*;
    fn fixture(version: i32, iterations: i32) -> Vec<u8> {
        let signature = b"CM3D21_PSK";
        let mut b = vec![signature.len() as u8];
        b.extend(signature);
        b.extend(version.to_le_bytes());
        b.extend(0.025f32.to_le_bytes());
        b.extend(0i32.to_le_bytes());
        if version >= 217 {
            b.extend(0i32.to_le_bytes());
        }
        b.extend(0.05f32.to_le_bytes());
        b.extend(0i32.to_le_bytes());
        for v in [0.4f32, 45., 90., 0.8, 0.1, 0.1] {
            b.extend(v.to_le_bytes());
        }
        b.extend(iterations.to_le_bytes());
        b.extend(0.8f32.to_le_bytes());
        b.extend(0i32.to_le_bytes());
        for v in [0f32, -0.005, 0.] {
            b.extend(v.to_le_bytes());
        }
        b.extend(0i32.to_le_bytes());
        for v in [0f32, 0.01, 0., 1.] {
            b.extend(v.to_le_bytes());
        }
        b
    }
    #[test]
    fn skirt_versions_keep_native_values_and_reject_partial_or_unsafe_data() {
        for version in [208, 217] {
            let b = fixture(version, 18);
            let p = SkirtPhysics::read(&b).unwrap();
            assert_eq!(p.version, version);
            assert_eq!(p.iterations, 18);
            assert_eq!(p.gravity, [0., -0.005, 0.]);
            assert_eq!(p.connection_strength, [0., 0.01, 0., 1.]);
            assert!(SkirtPhysics::read(&b[..b.len() - 1]).is_err());
            let mut extra = b;
            extra.push(0);
            assert!(SkirtPhysics::read(&extra).is_err());
        }
        assert!(SkirtPhysics::read(&fixture(208, 129)).is_err());
        assert!(SkirtPhysics::read(&fixture(208, 0)).is_err());
    }
}
