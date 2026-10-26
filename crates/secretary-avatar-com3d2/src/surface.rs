//! Nearest-surface queries on the native body, including barycentric morph and
//! skin weights. Garments follow the same body surface under sizing and motion.
use crate::{accessor, compose::scene_frames, invalid, Result};
use glam::Vec3;
use serde_json::Value;
use std::collections::HashMap;
#[derive(Clone)]
struct Triangle {
    p: [Vec3; 3],
    n: [Vec3; 3],
    delta: [Vec3; 3],
    skin: [Vec<(usize, f32)>; 3],
}
struct Node {
    lo: Vec3,
    hi: Vec3,
    children: Option<(Box<Node>, Box<Node>)>,
    ids: Vec<usize>,
}
pub struct Surface {
    triangles: Vec<Triangle>,
    root: Node,
}
pub struct Hit {
    pub point: Vec3,
    pub normal: Vec3,
    pub delta: Vec3,
    pub skin: Vec<(usize, f32)>,
    pub distance: f32,
}
fn closest(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> [f32; 3] {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0. && d2 <= 0. {
        return [1., 0., 0.];
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0. && d4 <= d3 {
        return [0., 1., 0.];
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0. && d1 >= 0. && d3 <= 0. {
        let v = d1 / (d1 - d3);
        return [1. - v, v, 0.];
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0. && d5 <= d6 {
        return [0., 0., 1.];
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0. && d2 >= 0. && d6 <= 0. {
        let w = d2 / (d2 - d6);
        return [1. - w, 0., w];
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0. && (d4 - d3) >= 0. && (d5 - d6) >= 0. {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return [0., 1. - w, w];
    }
    let denom = va + vb + vc;
    if denom.abs() < 1e-16 {
        return [1., 0., 0.];
    }
    let v = vb / denom;
    let w = vc / denom;
    [1. - v - w, v, w]
}
impl Node {
    fn build(triangles: &[Triangle], mut ids: Vec<usize>) -> Self {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for &i in &ids {
            for p in triangles[i].p {
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        if ids.len() <= 12 {
            return Self {
                lo,
                hi,
                children: None,
                ids,
            };
        }
        let span = hi - lo;
        let axis = if span.x > span.y && span.x > span.z {
            0
        } else if span.y > span.z {
            1
        } else {
            2
        };
        ids.sort_by(|&a, &b| {
            triangles[a]
                .p
                .iter()
                .map(|p| p[axis])
                .sum::<f32>()
                .total_cmp(&triangles[b].p.iter().map(|p| p[axis]).sum::<f32>())
        });
        let right = ids.split_off(ids.len() / 2);
        Self {
            lo,
            hi,
            children: Some((
                Box::new(Self::build(triangles, ids)),
                Box::new(Self::build(triangles, right)),
            )),
            ids: Vec::new(),
        }
    }
    fn distance(&self, p: Vec3) -> f32 {
        (p - p.clamp(self.lo, self.hi)).length_squared()
    }
    fn query(&self, p: Vec3, triangles: &[Triangle], best: &mut (f32, usize, [f32; 3])) {
        if self.distance(p) > best.0 {
            return;
        }
        if let Some((a, b)) = &self.children {
            if a.distance(p) < b.distance(p) {
                a.query(p, triangles, best);
                b.query(p, triangles, best);
            } else {
                b.query(p, triangles, best);
                a.query(p, triangles, best);
            }
        } else {
            for &i in &self.ids {
                let t = &triangles[i];
                let w = closest(p, t.p[0], t.p[1], t.p[2]);
                let q = t.p[0] * w[0] + t.p[1] * w[1] + t.p[2] * w[2];
                let d = p.distance_squared(q);
                if d < best.0 {
                    *best = (d, i, w);
                }
            }
        }
    }
}
impl Surface {
    pub fn body(doc: &Value, data: &[u8]) -> Result<Self> {
        Self::body_at_size(doc, data, 0.)
    }
    pub fn body_at_size(doc: &Value, data: &[u8], size: f32) -> Result<Self> {
        let (_, _, world) = scene_frames(doc)?;
        let mut triangles = Vec::new();
        for (i, node) in doc["nodes"].as_array().unwrap().iter().enumerate() {
            let (Some(m), Some(s)) = (node["mesh"].as_u64(), node["skin"].as_u64()) else {
                continue;
            };
            let mesh = &doc["meshes"][m as usize];
            let skin = doc["skins"][s as usize]["joints"]
                .as_array()
                .ok_or_else(|| invalid("surface skin"))?;
            let morph = mesh["extras"]["targetNames"]
                .as_array()
                .and_then(|a| a.iter().position(|n| n == "munel"));
            for p in mesh["primitives"]
                .as_array()
                .ok_or_else(|| invalid("body primitives"))?
            {
                let mat = p["material"]
                    .as_u64()
                    .ok_or_else(|| invalid("body material"))? as usize;
                let name = doc["materials"][mat]["name"].as_str().unwrap_or("");
                if !name.starts_with("skin_") {
                    continue;
                }
                let get = |name: &str| {
                    p["attributes"][name]
                        .as_u64()
                        .ok_or_else(|| invalid("body surface attribute"))
                };
                let ps = accessor::read(doc, data, get("POSITION")? as usize)?;
                let ns = accessor::read(doc, data, get("NORMAL")? as usize)?;
                let js = accessor::read(doc, data, get("JOINTS_0")? as usize)?;
                let ws = accessor::read(doc, data, get("WEIGHTS_0")? as usize)?;
                let ds = if let Some(a) = morph.and_then(|j| p["targets"][j]["POSITION"].as_u64()) {
                    accessor::read(doc, data, a as usize)?
                } else {
                    vec![vec![0.; 3]; ps.len()]
                };
                let indices = accessor::read(
                    doc,
                    data,
                    p["indices"]
                        .as_u64()
                        .ok_or_else(|| invalid("surface indices"))? as usize,
                )?;
                let normal = world[i].inverse().transpose();
                for tri in indices.as_chunks::<3>().0 {
                    let ids = [tri[0][0] as usize, tri[1][0] as usize, tri[2][0] as usize];
                    if ids.iter().any(|&v| {
                        v >= ps.len()
                            || v >= ns.len()
                            || v >= ds.len()
                            || v >= js.len()
                            || v >= ws.len()
                    }) {
                        return Err(invalid("surface vertex bounds"));
                    }
                    let point = ids.map(|v| world[i].transform_point3(Vec3::from_slice(&ps[v])));
                    if (point[1] - point[0])
                        .cross(point[2] - point[0])
                        .length_squared()
                        < 1e-16
                    {
                        continue;
                    }
                    let norms = ids.map(|v| {
                        normal
                            .transform_vector3(Vec3::from_slice(&ns[v]))
                            .normalize_or_zero()
                    });
                    let delta = ids.map(|v| world[i].transform_vector3(Vec3::from_slice(&ds[v])));
                    let weights = ids.map(|v| {
                        (0..4)
                            .filter(|&k| ws[v][k] > 0.)
                            .map(|k| {
                                let n = skin
                                    .get(js[v][k] as usize)
                                    .and_then(Value::as_u64)
                                    .ok_or_else(|| invalid("surface skin joint"))?;
                                Ok((n as usize, ws[v][k]))
                            })
                            .collect::<Result<Vec<_>>>()
                    });
                    let [a, b, c] = weights;
                    triangles.push(Triangle {
                        p: point,
                        n: norms,
                        delta,
                        skin: [a?, b?, c?],
                    });
                }
            }
        }
        if triangles.is_empty() {
            return Err(invalid("native body surface is empty"));
        }
        for t in &mut triangles {
            for i in 0..3 {
                t.p[i] += t.delta[i] * size;
            }
        }
        let root = Node::build(&triangles, (0..triangles.len()).collect());
        Ok(Self { triangles, root })
    }
    pub fn nearest(&self, p: Vec3) -> Hit {
        let mut best = (f32::INFINITY, 0, [1., 0., 0.]);
        self.root.query(p, &self.triangles, &mut best);
        let t = &self.triangles[best.1];
        let mut point = Vec3::ZERO;
        let mut normal = Vec3::ZERO;
        let mut delta = Vec3::ZERO;
        let mut skin = HashMap::<usize, f32>::new();
        for i in 0..3 {
            point += t.p[i] * best.2[i];
            normal += t.n[i] * best.2[i];
            delta += t.delta[i] * best.2[i];
            for &(j, w) in &t.skin[i] {
                *skin.entry(j).or_default() += w * best.2[i];
            }
        }
        let mut skin = skin.into_iter().collect::<Vec<_>>();
        skin.sort_by(|a, b| b.1.total_cmp(&a.1));
        skin.truncate(4);
        let sum = skin.iter().map(|p| p.1).sum::<f32>();
        for (_, w) in &mut skin {
            *w /= sum;
        }
        Hit {
            point,
            normal: normal.normalize_or_zero(),
            delta,
            skin,
            distance: best.0.sqrt(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_surface_handles_inside_edges_and_degenerate_triangles() {
        let a = Vec3::ZERO;
        let b = Vec3::X;
        let c = Vec3::Y;
        let w = closest(Vec3::new(0.2, 0.3, 0.4), a, b, c);
        assert!((w[0] - 0.5).abs() < 1e-6);
        assert!((w[1] - 0.2).abs() < 1e-6);
        assert!((w[2] - 0.3).abs() < 1e-6);
        assert_eq!(closest(-Vec3::X, a, b, c), [1., 0., 0.]);
        assert!(closest(Vec3::ONE, a, a, a).iter().all(|v| v.is_finite()));
    }
}
