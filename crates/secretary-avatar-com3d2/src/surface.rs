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
    fn envelope(
        &self,
        p: Vec3,
        triangles: &[Triangle],
        best: &mut Option<(f32, usize, [f32; 3])>,
        front: bool,
    ) {
        if p.x < self.lo.x || p.x > self.hi.x || p.y < self.lo.y || p.y > self.hi.y {
            return;
        }
        if let Some((a, b)) = &self.children {
            a.envelope(p, triangles, best, front);
            b.envelope(p, triangles, best, front);
            return;
        }
        for &id in &self.ids {
            let t = &triangles[id];
            let a = t.p[0];
            let b = t.p[1];
            let c = t.p[2];
            let det = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
            if det.abs() < 1e-12 {
                continue;
            }
            let u = ((b.y - c.y) * (p.x - c.x) + (c.x - b.x) * (p.y - c.y)) / det;
            let v = ((c.y - a.y) * (p.x - c.x) + (a.x - c.x) * (p.y - c.y)) / det;
            let w = 1. - u - v;
            if u < -1e-5 || v < -1e-5 || w < -1e-5 {
                continue;
            }
            let z = u * a.z + v * b.z + w * c.z;
            if best
                .as_ref()
                .is_none_or(|b| if front { z < b.0 } else { z > b.0 })
            {
                *best = Some((z, id, [u, v, w]));
            }
        }
    }
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
    fn ray(
        &self,
        o: Vec3,
        d: Vec3,
        triangles: &[Triangle],
        keep: &impl Fn(usize) -> bool,
        best: &mut f32,
        found: &mut bool,
    ) {
        // Slab test against the box, then Möller–Trumbore against each triangle.
        let mut near = 0f32;
        let mut far = *best;
        for a in 0..3 {
            if d[a].abs() < 1e-12 {
                if o[a] < self.lo[a] || o[a] > self.hi[a] {
                    return;
                }
                continue;
            }
            let (t0, t1) = ((self.lo[a] - o[a]) / d[a], (self.hi[a] - o[a]) / d[a]);
            near = near.max(t0.min(t1));
            far = far.min(t0.max(t1));
            if near > far {
                return;
            }
        }
        if let Some((a, b)) = &self.children {
            a.ray(o, d, triangles, keep, best, found);
            b.ray(o, d, triangles, keep, best, found);
            return;
        }
        for &id in &self.ids {
            let t = &triangles[id];
            if !t.skin[0].first().is_none_or(|s| keep(s.0)) {
                continue;
            }
            let e1 = t.p[1] - t.p[0];
            let e2 = t.p[2] - t.p[0];
            let h = d.cross(e2);
            let det = e1.dot(h);
            if det.abs() < 1e-10 {
                continue;
            }
            let s = o - t.p[0];
            let u = s.dot(h) / det;
            let q = s.cross(e1);
            let v = d.dot(q) / det;
            if u < 0. || v < 0. || u + v > 1. {
                continue;
            }
            let r = e2.dot(q) / det;
            if r >= 0. && r <= *best {
                *best = r;
                *found = true;
            }
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
    /// The body in its bind pose (node transforms only).
    pub fn body_at_size(doc: &Value, data: &[u8], size: f32) -> Result<Self> {
        Self::build_body(doc, data, size, false)
    }
    /// The body skinned into the pose the document's nodes hold (a posed reference). The bind-pose
    /// surface has the legs spread wide and the arms out, so a garment fitted in the standing pose
    /// was compared with limbs that were not there.
    pub fn posed_at_size(doc: &Value, data: &[u8], size: f32) -> Result<Self> {
        Self::build_body(doc, data, size, true)
    }
    fn build_body(doc: &Value, data: &[u8], size: f32, posed: bool) -> Result<Self> {
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
                // Posed: each vertex follows its blended joints into the document's pose.
                let joints = if posed {
                    let ib = accessor::read(
                        doc,
                        data,
                        doc["skins"][s as usize]["inverseBindMatrices"]
                            .as_u64()
                            .ok_or_else(|| invalid("surface inverse binds"))?
                            as usize,
                    )?;
                    skin.iter()
                        .zip(&ib)
                        .map(|(j, m)| {
                            let n = j.as_u64().ok_or_else(|| invalid("surface skin joint"))?;
                            let m: [f32; 16] =
                                m.as_slice().try_into().map_err(|_| invalid("surface inverse bind"))?;
                            Ok(world
                                .get(n as usize)
                                .ok_or_else(|| invalid("surface skin joint"))?
                                .mul_mat4(&glam::Mat4::from_cols_array(&m)))
                        })
                        .collect::<Result<Vec<_>>>()?
                } else {
                    Vec::new()
                };
                let place = |v: usize| -> Result<glam::Mat4> {
                    if !posed {
                        return Ok(world[i]);
                    }
                    let mut m = glam::Mat4::ZERO;
                    for k in 0..4 {
                        if ws[v][k] > 0. {
                            m += *joints
                                .get(js[v][k] as usize)
                                .ok_or_else(|| invalid("surface skin joint"))?
                                * ws[v][k];
                        }
                    }
                    Ok(m)
                };
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
                    let [ma, mb, mc] = [place(ids[0])?, place(ids[1])?, place(ids[2])?];
                    let m = [ma, mb, mc];
                    let point = std::array::from_fn::<Vec3, 3, _>(|c| {
                        m[c].transform_point3(Vec3::from_slice(&ps[ids[c]]))
                    });
                    if (point[1] - point[0])
                        .cross(point[2] - point[0])
                        .length_squared()
                        < 1e-16
                    {
                        continue;
                    }
                    let norms = std::array::from_fn::<Vec3, 3, _>(|c| {
                        m[c].inverse()
                            .transpose()
                            .transform_vector3(Vec3::from_slice(&ns[ids[c]]))
                            .normalize_or_zero()
                    });
                    let delta = std::array::from_fn::<Vec3, 3, _>(|c| {
                        m[c].transform_vector3(Vec3::from_slice(&ds[ids[c]]))
                    });
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
    pub fn geometry(points: &[Vec3], normals: &[Vec3], faces: &[[usize; 3]]) -> Result<Self> {
        let mut triangles = Vec::new();
        for t in faces {
            if t.iter().any(|&v| v >= points.len() || v >= normals.len()) {
                return Err(invalid("garment surface index"));
            }
            triangles.push(Triangle {
                p: t.map(|v| points[v]),
                n: t.map(|v| normals[v]),
                delta: [Vec3::ZERO; 3],
                skin: std::array::from_fn(|_| vec![(0, 1.)]),
            });
        }
        if triangles.is_empty() {
            return Err(invalid("empty garment surface"));
        }
        let root = Node::build(&triangles, (0..triangles.len()).collect());
        Ok(Self { triangles, root })
    }
    /// The same surface without the triangles whose strongest joint `drop` rejects; `None` if
    /// nothing is left.
    pub fn without(&self, drop: impl Fn(usize) -> bool) -> Option<Self> {
        let triangles = self
            .triangles
            .iter()
            .filter(|t| {
                let strongest = t
                    .skin
                    .iter()
                    .flatten()
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|s| s.0);
                !strongest.is_some_and(&drop)
            })
            .cloned()
            .collect::<Vec<_>>();
        if triangles.is_empty() {
            return None;
        }
        let root = Node::build(&triangles, (0..triangles.len()).collect());
        Some(Self { triangles, root })
    }
    /// Distance to the first triangle a ray from `origin` along `direction` meets within `max`.
    pub fn ray(&self, origin: Vec3, direction: Vec3, max: f32) -> Option<f32> {
        self.ray_where(origin, direction, max, |_| true)
    }
    /// As `ray`, over the triangles whose tag (see `tagged`) `keep` accepts.
    pub fn ray_where(
        &self,
        origin: Vec3,
        direction: Vec3,
        max: f32,
        keep: impl Fn(usize) -> bool,
    ) -> Option<f32> {
        let d = direction.normalize_or_zero();
        let mut best = max;
        let mut found = false;
        self.root
            .ray(origin, d, &self.triangles, &keep, &mut best, &mut found);
        found.then_some(best)
    }
    /// A plain geometry surface whose triangles carry a tag each (in place of skin weights).
    pub fn tagged(points: &[Vec3], normals: &[Vec3], faces: &[[usize; 3]], tags: &[usize]) -> Result<Self> {
        let mut s = Self::geometry(points, normals, faces)?;
        if tags.len() != s.triangles.len() {
            return Err(invalid("garment surface tags"));
        }
        for (t, &tag) in s.triangles.iter_mut().zip(tags) {
            t.skin = std::array::from_fn(|_| vec![(tag, 1.)]);
        }
        Ok(s)
    }
    /// Mean corner and lowest corner height of the triangles whose strongest joint `keep` accepts.
    pub fn region(&self, keep: impl Fn(usize) -> bool) -> Option<(Vec3, f32)> {
        let mut sum = Vec3::ZERO;
        let mut low = f32::INFINITY;
        let mut n = 0;
        for t in &self.triangles {
            let strongest = t.skin.iter().flatten().max_by(|a, b| a.1.total_cmp(&b.1));
            if strongest.is_some_and(|s| keep(s.0)) {
                for p in t.p {
                    sum += p;
                    low = low.min(p.y);
                    n += 1;
                }
            }
        }
        (n > 0).then(|| (sum / n as f32, low))
    }
    /// Outermost intersection of an authoring ray, rather than a nearest
    /// underside/crotch surface. Used to keep a whole outer garment outside
    /// both trouser legs at the same height.
    pub fn radial(&self, origin: Vec3, direction: Vec3) -> Option<Hit> {
        let d = direction.normalize_or_zero();
        let mut best = None;
        let mut distance = 0.;
        for (i, t) in self.triangles.iter().enumerate() {
            let e1 = t.p[1] - t.p[0];
            let e2 = t.p[2] - t.p[0];
            let h = d.cross(e2);
            let det = e1.dot(h);
            if det.abs() < 1e-8 {
                continue;
            }
            let inv = 1. / det;
            let s = origin - t.p[0];
            let u = s.dot(h) * inv;
            if !(-1e-5..=1.00001).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = d.dot(q) * inv;
            if v < -1e-5 || u + v > 1.00001 {
                continue;
            }
            let r = e2.dot(q) * inv;
            if r > distance && r < 0.5 {
                distance = r;
                best = Some((r * r, i, [1. - u - v, u, v]));
            }
        }
        best.map(|v| self.hit(v))
    }
    pub fn nearest(&self, p: Vec3) -> Hit {
        let mut best = (f32::INFINITY, 0, [1., 0., 0.]);
        self.root.query(p, &self.triangles, &mut best);
        self.hit(best)
    }
    /// Front envelope at fixed lateral/vertical coordinates. This prevents a
    /// fitting query inside a curved chest from selecting the underside.
    pub fn front(&self, p: Vec3) -> Option<Hit> {
        let mut best = None;
        self.root.envelope(p, &self.triangles, &mut best, true);
        best.map(|(z, id, w)| self.hit(((z - p.z).powi(2), id, w)))
    }
    pub fn back(&self, p: Vec3) -> Option<Hit> {
        let mut best = None;
        self.root.envelope(p, &self.triangles, &mut best, false);
        best.map(|(z, id, w)| self.hit(((z - p.z).powi(2), id, w)))
    }
    fn hit(&self, best: (f32, usize, [f32; 3])) -> Hit {
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
    fn front_envelope_keeps_xy_and_selects_front_instead_of_nearest_underside() {
        let triangles = [-0.12, 0.1]
            .into_iter()
            .map(|z| Triangle {
                p: [
                    Vec3::new(-1., 1., z),
                    Vec3::new(1., 1., z),
                    Vec3::new(0., 2., z),
                ],
                n: [-Vec3::Z; 3],
                delta: [Vec3::ZERO; 3],
                skin: std::array::from_fn(|_| vec![(0, 1.)]),
            })
            .collect::<Vec<_>>();
        let root = Node::build(&triangles, vec![0, 1]);
        let surface = Surface { triangles, root };
        let hit = surface.front(Vec3::new(0.1, 1.3, 0.09)).unwrap();
        assert!(hit.point.distance(Vec3::new(0.1, 1.3, -0.12)) < 1e-6);
        assert!(surface.front(Vec3::new(2., 1.3, 0.)).is_none());
        assert_eq!(hit.skin, vec![(0, 1.)]);
        let back = surface.back(Vec3::new(0.1, 1.3, -0.09)).unwrap();
        assert!(back.point.distance(Vec3::new(0.1, 1.3, 0.1)) < 1e-6);
    }
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
#[cfg(test)]
mod garment_surface_tests {
    use super::*;
    #[test]
    fn outermost_radial_layer_is_selected_above_a_nearer_underside() {
        let p = vec![
            Vec3::new(-0.2, -0.2, 0.05),
            Vec3::new(0.2, -0.2, 0.05),
            Vec3::new(0., 0.2, 0.05),
            Vec3::new(-0.2, -0.2, 0.1),
            Vec3::new(0.2, -0.2, 0.1),
            Vec3::new(0., 0.2, 0.1),
        ];
        let s = Surface::geometry(&p, &[Vec3::Z; 6], &[[0, 1, 2], [3, 4, 5]]).unwrap();
        assert!((s.nearest(Vec3::new(0., 0., 0.06)).point.z - 0.05).abs() < 1e-6);
        assert!((s.radial(Vec3::ZERO, Vec3::Z).unwrap().point.z - 0.1).abs() < 1e-6);
        assert!(Surface::geometry(&p, &[], &[[0, 1, 2]]).is_err());
    }
}
