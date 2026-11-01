//! Reference-pose layer fitting for the original closed Uniform. The tunic,
//! trousers and cuffs share a material, so material-wide offsets are unsafe.
//! Connectivity and skin roles distinguish the actual clothing layers.
use crate::{accessor, invalid, surface::Surface, Result};
use glam::{Mat4, Vec3};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
fn root(p: &mut [usize], i: usize) -> usize {
    if p[i] != i {
        p[i] = root(p, p[i]);
    }
    p[i]
}
fn join(p: &mut [usize], a: usize, b: usize) {
    let a = root(p, a);
    let b = root(p, b);
    p[b] = a;
}
/// Compress only a knee garment's medial side into the available inter-leg
/// space. The monotone Hermite map keeps the outer silhouette, has unit slope
/// at the centre line, and never folds a triangle over itself.
fn medial(point: Vec3, centre: Vec3, extent: f32, factor: f32, side: f32) -> (Vec3, f32, f32) {
    let d = (point.x - centre.x) * side;
    if d <= 0. || extent <= 1e-6 {
        return (point, 1., 0.);
    }
    let dy = (point.y - centre.y).abs();
    let (w, dw) = if dy <= 0.12 {
        (1., 0.)
    } else if dy >= 0.20 {
        (0., 0.)
    } else {
        let t = (dy - 0.12) / 0.08;
        (
            1. - t * t * (3. - 2. * t),
            -6. * t * (1. - t) / 0.08 * (point.y - centre.y).signum(),
        )
    };
    if w == 0. {
        return (point, 1., 0.);
    }
    let u = (d / extent).clamp(0., 1.);
    let smooth = u * u * (3. - 2. * u);
    let change = (factor - 1.) * smooth;
    let mut out = point;
    out.x = centre.x + side * (d + extent * change * w);
    (
        out,
        1. + (factor - 1.) * 6. * u * (1. - u) * w,
        side * extent * change * dw,
    )
}
struct MeshData {
    mesh: usize,
    primitive: usize,
    points: Vec<Vec3>,
    normals: Vec<Vec3>,
    deltas: Vec<Vec3>,
    matrices: Vec<Mat4>,
    joints: Vec<Vec<f32>>,
    weights: Vec<Vec<f32>>,
    skin: usize,
    triangles: Vec<[usize; 3]>,
    attributes: Vec<(String, Vec<Vec<f32>>)>,
}
pub fn uniform(
    doc: &mut Value,
    data: &mut Vec<u8>,
    bind: &[Mat4],
    reference: &[Mat4],
) -> Result<Value> {
    let nodes = doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("uniform nodes"))?;
    let find = |name: &str| {
        nodes
            .iter()
            .position(|n| n["name"] == name)
            .ok_or_else(|| invalid(format!("uniform {name}")))
    };
    let knees = [
        reference[find("Bip01 L Calf")?].w_axis.truncate(),
        reference[find("Bip01 R Calf")?].w_axis.truncate(),
    ];
    let waist = reference[find("Bip01 Pelvis")?].w_axis.y;
    let pelvis_z = reference[find("Bip01 Pelvis")?].w_axis.z;
    let mut meshes = Vec::new();
    for node in nodes {
        let (Some(mesh), Some(skin)) = (node["mesh"].as_u64(), node["skin"].as_u64()) else {
            continue;
        };
        let (mesh, skin) = (mesh as usize, skin as usize);
        let palette = doc["skins"][skin]["joints"].as_array().unwrap();
        for (pi, p) in doc["meshes"][mesh]["primitives"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let mat = p["material"].as_u64().unwrap_or(0) as usize;
            if doc["materials"][mat]["name"] != "Dress" {
                continue;
            }
            let read = |key: &str| {
                accessor::read(doc, data, p["attributes"][key].as_u64().unwrap() as usize)
            };
            let ps = read("POSITION")?;
            let ns = read("NORMAL")?;
            let joints = read("JOINTS_0")?;
            let weights = read("WEIGHTS_0")?;
            let deltas = accessor::read(
                doc,
                data,
                p["targets"][0]["POSITION"]
                    .as_u64()
                    .ok_or_else(|| invalid("uniform size target"))? as usize,
            )?;
            let mut matrices = Vec::new();
            let mut points = Vec::new();
            let mut normals = Vec::new();
            let mut ds = Vec::new();
            for v in 0..ps.len() {
                let mut m = Mat4::ZERO;
                for k in 0..4 {
                    if weights[v][k] <= 0. {
                        continue;
                    }
                    let j = palette[joints[v][k] as usize].as_u64().unwrap() as usize;
                    m += (reference[j] * bind[j].inverse()) * weights[v][k];
                }
                if !m.is_finite() || m.determinant().abs() < 1e-7 {
                    return Err(invalid("uniform skin inverse"));
                }
                matrices.push(m);
                points.push(m.transform_point3(Vec3::from_slice(&ps[v])));
                normals.push(
                    m.inverse()
                        .transpose()
                        .transform_vector3(Vec3::from_slice(&ns[v]))
                        .normalize_or_zero(),
                );
                ds.push(m.transform_vector3(Vec3::from_slice(&deltas[v])));
            }
            let raw = accessor::read(doc, data, p["indices"].as_u64().unwrap() as usize)?;
            let triangles = raw
                .as_chunks::<3>()
                .0
                .iter()
                .map(|t| [t[0][0] as usize, t[1][0] as usize, t[2][0] as usize])
                .collect();
            let mut attributes = Vec::new();
            for (name, index) in p["attributes"].as_object().unwrap() {
                if ["POSITION", "NORMAL", "JOINTS_0", "WEIGHTS_0"].contains(&name.as_str()) {
                    continue;
                }
                attributes.push((
                    name.clone(),
                    accessor::read(doc, data, index.as_u64().unwrap() as usize)?,
                ));
            }
            meshes.push(MeshData {
                attributes,

                mesh,
                primitive: pi,
                points,
                normals,
                deltas: ds,
                matrices,
                joints,
                weights,
                skin,
                triangles,
            });
        }
    }
    let mut knee_vertices = 0;
    let mut outer_vertices = 0;
    let mut max_move = 0f32;
    let mut role_counts = Vec::new();
    for m in &mut meshes {
        let count = m.points.len();
        let palette = doc["skins"][m.skin]["joints"].as_array().unwrap();
        let leg_weights = m
            .joints
            .iter()
            .zip(&m.weights)
            .map(|(js, ws)| {
                let mut out = [0.; 2];
                for k in 0..4 {
                    let j = palette[js[k] as usize].as_u64().unwrap() as usize;
                    let name = doc["nodes"][j]["name"].as_str().unwrap_or("");
                    if !["Thigh", "Calf", "Foot", "Toe"]
                        .iter()
                        .any(|s| name.contains(s))
                    {
                        continue;
                    }
                    for (side, tag) in ["Bip01 L ", "Bip01 R "].iter().enumerate() {
                        if name.starts_with(tag) {
                            out[side] += ws[k];
                        }
                    }
                }
                out
            })
            .collect::<Vec<_>>();
        let mut extents = [0f32; 2];
        for (v, p) in m.points.iter().enumerate() {
            for side in 0..2 {
                if leg_weights[v][side] > 0.9 && (p.y - knees[side].y).abs() < 0.20 {
                    let direction = if knees[side].x < 0. { 1. } else { -1. };
                    extents[side] = extents[side].max((p.x - knees[side].x) * direction);
                }
            }
        }
        let factors = std::array::from_fn::<_, 2, _>(|side| {
            let available = (knees[side].x.abs() - 0.003).max(0.);
            if extents[side] <= available {
                1.
            } else {
                (available / extents[side]).clamp(0.35, 1.)
            }
        });
        let mut changed = vec![false; count];
        for (v, leg_weight) in leg_weights.iter().enumerate() {
            for side in 0..2 {
                if leg_weight[side] <= 0.9 || factors[side] >= 1. {
                    continue;
                }
                let direction = if knees[side].x < 0. { 1. } else { -1. };
                let old = m.points[v];
                let (next, dx, dy) =
                    medial(old, knees[side], extents[side], factors[side], direction);
                if old.distance(next) < 1e-7 {
                    continue;
                }
                let endpoint = medial(
                    old + m.deltas[v],
                    knees[side],
                    extents[side],
                    factors[side],
                    direction,
                )
                .0;
                m.deltas[v] = endpoint - next;
                m.points[v] = next;
                let n = m.normals[v];
                m.normals[v] = Vec3::new(n.x / dx, n.y - dy * n.x / dx, n.z).normalize_or_zero();
                max_move = max_move.max(old.distance(next));
                knee_vertices += 1;
                changed[v] = true;
            }
        }
        // Weld for classification only. Original render topology/UVs remain intact.
        let mut parents = (0..count).collect::<Vec<_>>();
        let mut weld = HashMap::new();
        for (v, p) in m.points.iter().enumerate() {
            let key = (
                (p.x * 100000.).round() as i64,
                (p.y * 100000.).round() as i64,
                (p.z * 100000.).round() as i64,
            );
            if let Some(&other) = weld.get(&key) {
                join(&mut parents, v, other);
            } else {
                weld.insert(key, v);
            }
        }
        for t in &m.triangles {
            join(&mut parents, t[0], t[1]);
            join(&mut parents, t[0], t[2]);
        }
        let mut components = HashMap::<usize, HashSet<usize>>::new();
        for t in &m.triangles {
            let id = root(&mut parents, t[0]);
            for &v in t {
                components.entry(id).or_default().insert(v);
            }
        }
        let mut pants = HashSet::new();
        let mut tunic = HashSet::new();
        for vertices in components.values() {
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            let mut leg = 0.;
            let mut torso = 0.;
            for &v in vertices {
                lo = lo.min(m.points[v]);
                hi = hi.max(m.points[v]);
                leg += leg_weights[v][0] + leg_weights[v][1];
                for k in 0..4 {
                    let n = palette[m.joints[v][k] as usize].as_u64().unwrap() as usize;
                    let name = doc["nodes"][n]["name"].as_str().unwrap_or("");
                    if name.contains("Spine") || name.contains("Pelvis") {
                        torso += m.weights[v][k];
                    }
                }
            }
            leg /= vertices.len() as f32;
            torso /= vertices.len() as f32;
            if hi.y > waist && lo.y < knees[0].y + 0.06 && hi.y - lo.y > 0.3 && leg > 0.25 {
                pants.extend(vertices.iter().copied());
            }
            if lo.y > knees[0].y + 0.10
                && lo.y < waist - 0.05
                && hi.y > waist + 0.35
                && vertices.len() > 300
                && torso > 0.45
            {
                tunic.extend(vertices.iter().copied());
            }
        }
        let pant_triangles = m
            .triangles
            .iter()
            .filter(|t| t.iter().all(|v| pants.contains(v)))
            .copied()
            .collect::<Vec<_>>();
        let refined = refine(m, &mut tunic, waist, palette, bind, reference)?;
        let count = m.points.len();
        changed.resize(count, true);
        role_counts.push(json!
({"pantsVertices":pants.len(),"tunicVertices":tunic.len(),"medialFactors":factors,"kneeExtents":extents,"refinedVertices":refined}));
        if !pant_triangles.is_empty() && !tunic.is_empty() {
            let surface = Surface::geometry(&m.points, &m.normals, &pant_triangles)?;
            let original = m.points.clone();
            let radial = |point: Vec3| {
                let centre = Vec3::new(0., point.y, pelvis_z);
                let direction = (point - centre).normalize_or_zero();
                let radius = point.distance(centre);
                surface.radial(centre, direction).map(|hit| {
                    (
                        (hit.point.distance(centre) + 0.014 - radius).max(0.),
                        direction,
                    )
                })
            };
            // Include edge midpoints and triangle interiors. Vertex-only
            // clearance still lets a coarse triangle cut through a curved leg.
            for _ in 0..10 {
                let mut moves = vec![0f32; count];
                for &v in &tunic {
                    if m.points[v].y <= waist - 0.02 {
                        if let Some((depth, _)) = radial(m.points[v]) {
                            moves[v] = moves[v].max(depth);
                        }
                    }
                }
                for t in &m.triangles {
                    if !t.iter().all(|v| tunic.contains(v))
                        || t.iter().all(|&v| m.points[v].y > waist - 0.02)
                    {
                        continue;
                    }
                    let p = t.map(|v| m.points[v]);
                    let probes = [
                        (p[0] + p[1]) * 0.5,
                        (p[1] + p[2]) * 0.5,
                        (p[2] + p[0]) * 0.5,
                        (p[0] + p[1] + p[2]) / 3.,
                    ];
                    let mut required = 0f32;
                    for q in probes {
                        if q.y <= waist - 0.02 {
                            if let Some((depth, _)) = radial(q) {
                                required = required.max(depth);
                            }
                        }
                    }
                    if required > 0.0001 {
                        for &v in t {
                            moves[v] = moves[v].max(required * 1.1);
                        }
                    }
                }
                if moves.iter().all(|&d| d < 0.0001) {
                    break;
                }
                for (v, depth) in moves.iter().enumerate() {
                    if *depth <= 0.0001 {
                        continue;
                    }
                    let centre = Vec3::new(0., m.points[v].y, pelvis_z);
                    let direction = (m.points[v] - centre).normalize_or_zero();
                    m.points[v] += direction * depth.min(0.012);
                }
            }
            for &v in &tunic {
                let distance = original[v].distance(m.points[v]);
                if distance > 0.065 {
                    return Err(invalid(format!(
                        "uniform clearance {distance}m vertex {v}: {:?} to {:?}",
                        original[v], m.points[v]
                    )));
                }
                if distance > 1e-6 {
                    changed[v] = true;
                    outer_vertices += 1;
                    max_move = max_move.max(distance);
                }
            }
        }
        // Recompute only affected smoothing vertices; retain the existing UV splits.
        let mut sums = vec![Vec3::ZERO; count];
        let mut neighbours = changed.clone();
        for t in &m.triangles {
            let n = (m.points[t[1]] - m.points[t[0]]).cross(m.points[t[2]] - m.points[t[0]]);
            for &v in t {
                sums[v] += n;
            }
            if t.iter().any(|&v| changed[v]) {
                for &v in t {
                    neighbours[v] = true;
                }
            }
        }
        for v in 0..count {
            if neighbours[v] && sums[v].length_squared() > 1e-12 {
                let mut n = sums[v].normalize();
                if n.dot(m.normals[v]) < 0. {
                    n = -n;
                }
                m.normals[v] = n;
            }
        }
        let mut ps = Vec::new();
        let mut ns = Vec::new();
        let mut ds = Vec::new();
        for v in 0..count {
            let inverse = m.matrices[v].inverse();
            ps.push(inverse.transform_point3(m.points[v]).to_array().to_vec());
            ns.push(
                m.matrices[v]
                    .transpose()
                    .transform_vector3(m.normals[v])
                    .normalize_or_zero()
                    .to_array()
                    .to_vec(),
            );
            ds.push(inverse.transform_vector3(m.deltas[v]).to_array().to_vec());
        }
        let p = accessor::floats(doc, data, &ps, 3)?;
        let n = accessor::floats(doc, data, &ns, 3)?;
        let d = accessor::floats(doc, data, &ds, 3)?;
        let joints = m
            .joints
            .iter()
            .map(|v| std::array::from_fn(|i| v[i] as u16))
            .collect::<Vec<[u16; 4]>>();
        let j = accessor::shorts(doc, data, &joints);
        let w = accessor::floats(doc, data, &m.weights, 4)?;
        let indices = write_triangles(doc, data, &m.triangles);
        for (name, values) in &m.attributes {
            let a = accessor::floats(doc, data, values, values[0].len())?;
            doc["meshes"][m.mesh]["primitives"][m.primitive]["attributes"][name] = json!(a);
        }
        let primitive = &mut doc["meshes"][m.mesh]["primitives"][m.primitive];
        primitive["attributes"]["JOINTS_0"] = json!(j);
        primitive["attributes"]["WEIGHTS_0"] = json!(w);
        primitive["indices"] = json!(indices);

        primitive["attributes"]["POSITION"] = json!(p);
        primitive["attributes"]["NORMAL"] = json!(n);
        primitive["targets"][0]["POSITION"] = json!(d);
    }
    let report = json!({"revision":1,"referencePose":true,"kneeVertices":knee_vertices,"outerLayerVertices":outer_vertices,"maxMoveMetres":max_move,"roles":role_counts});
    doc["extras"]["uniformLayerFit"] = report.clone();
    doc["buffers"][0]["byteLength"] = json!(data.len());
    Ok(report)
}
fn split_pattern(t: [usize; 3], mid: [Option<usize>; 3]) -> Vec<[usize; 3]> {
    let [a, b, c] = t;
    let [ab, bc, ca] = mid;
    match (ab, bc, ca) {
        (None, None, None) => vec![t],
        (Some(x), None, None) => vec![[a, x, c], [x, b, c]],
        (None, Some(y), None) => vec![[b, y, a], [y, c, a]],
        (None, None, Some(z)) => vec![[c, z, b], [z, a, b]],
        (Some(x), Some(y), None) => vec![[x, b, y], [a, x, c], [x, y, c]],
        (None, Some(y), Some(z)) => vec![[y, c, z], [b, y, a], [y, z, a]],
        (Some(x), None, Some(z)) => vec![[z, a, x], [c, z, b], [z, x, b]],
        (Some(x), Some(y), Some(z)) => vec![[a, x, z], [x, b, y], [z, y, c], [x, y, z]],
    }
}
fn refine(
    m: &mut MeshData,
    outer: &mut HashSet<usize>,
    waist: f32,
    palette: &[Value],
    bind: &[Mat4],
    reference: &[Mat4],
) -> Result<usize> {
    let start = m.points.len();
    for _ in 0..4 {
        let mut edges = std::collections::BTreeSet::new();
        for t in &m.triangles {
            if !t.iter().all(|v| outer.contains(v))
                || t.iter().all(|&v| m.points[v].y > waist - 0.02)
            {
                continue;
            }
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if m.points[a].distance(m.points[b]) > 0.025 {
                    edges.insert((a.min(b), a.max(b)));
                }
            }
        }
        if edges.is_empty() {
            break;
        }
        if m.points.len() + edges.len() > 100000 {
            return Err(invalid("uniform refinement limit"));
        }
        let mut mids = HashMap::new();
        for (a, b) in edges {
            let v = m.points.len();
            mids.insert((a, b), v);
            m.points.push((m.points[a] + m.points[b]) * 0.5);
            m.normals
                .push((m.normals[a] + m.normals[b]).normalize_or_zero());
            m.deltas.push((m.deltas[a] + m.deltas[b]) * 0.5);
            let mut weights = HashMap::<usize, f32>::new();
            for p in [a, b] {
                for k in 0..4 {
                    *weights.entry(m.joints[p][k] as usize).or_default() += m.weights[p][k] * 0.5;
                }
            }
            let mut weights = weights
                .into_iter()
                .filter(|(_, w)| *w > 1e-6)
                .collect::<Vec<_>>();
            weights.sort_by(|a, b| b.1.total_cmp(&a.1));
            weights.truncate(4);
            let sum = weights.iter().map(|(_, w)| w).sum::<f32>();
            let (mut js, mut ws) = (vec![0.; 4], vec![0.; 4]);
            let mut matrix = Mat4::ZERO;
            for (k, (j, w)) in weights.iter().enumerate() {
                js[k] = *j as f32;
                ws[k] = *w / sum;
                let n = palette[*j].as_u64().unwrap() as usize;
                matrix += (reference[n] * bind[n].inverse()) * ws[k];
            }
            m.joints.push(js);
            m.weights.push(ws);
            m.matrices.push(matrix);
            for (_, values) in &mut m.attributes {
                values.push(
                    values[a]
                        .iter()
                        .zip(&values[b])
                        .map(|(a, b)| (a + b) * 0.5)
                        .collect(),
                );
            }
            if outer.contains(&a) && outer.contains(&b) {
                outer.insert(v);
            }
        }
        let mut triangles = Vec::new();
        for &t in &m.triangles {
            let mid = [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])]
                .map(|(a, b)| mids.get(&(a.min(b), a.max(b))).copied());
            triangles.extend(split_pattern(t, mid));
        }
        m.triangles = triangles;
    }
    Ok(m.points.len() - start)
}
fn write_triangles(doc: &mut Value, data: &mut Vec<u8>, triangles: &[[usize; 3]]) -> usize {
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let start = data.len();
    for t in triangles {
        for &v in t {
            data.extend((v as u32).to_le_bytes());
        }
    }
    let view = doc["bufferViews"].as_array().unwrap().len();
    doc["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":start,"byteLength":data.len()-start}));
    let a = doc["accessors"].as_array().unwrap().len();
    doc["accessors"].as_array_mut().unwrap().push(
        json!({"bufferView":view,"componentType":5125,"type":"SCALAR","count":triangles.len()*3}),
    );
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn medial_fit_is_monotone_keeps_outer_shape_and_leaves_other_heights_alone() {
        let centre = Vec3::new(-0.05, 0.55, 0.);
        let mut last = f32::NEG_INFINITY;
        for i in 0..101 {
            let p = Vec3::new(centre.x + 0.08 * i as f32 / 100., 0.55, 0.);
            let (q, dx, _) = medial(p, centre, 0.08, 0.5, 1.);
            assert!(q.x >= last);
            assert!(dx > 0.);
            last = q.x;
        }
        assert!((last + 0.01).abs() < 1e-6);
        for p in [Vec3::new(-0.1, 0.55, 0.), Vec3::new(0.03, 0.9, 0.)] {
            assert_eq!(medial(p, centre, 0.08, 0.5, 1.).0, p);
        }
    }
}
#[cfg(test)]
mod refinement_tests {
    use super::*;
    #[test]
    fn every_conforming_split_preserves_winding_and_area() {
        let points = [
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            Vec3::X * 0.5,
            (Vec3::X + Vec3::Y) * 0.5,
            Vec3::Y * 0.5,
        ];
        for mask in 0..8 {
            let mids = std::array::from_fn(|i| {
                if mask & (1 << i) != 0 {
                    Some(3 + i)
                } else {
                    None
                }
            });
            let children = split_pattern([0, 1, 2], mids);
            let mut area = 0.;
            for t in children {
                let z = (points[t[1]] - points[t[0]])
                    .cross(points[t[2]] - points[t[0]])
                    .z;
                assert!(z > 0.);
                area += z * 0.5;
            }
            assert!((area - 0.5).abs() < 1e-6, "mask {mask}");
        }
    }
}
