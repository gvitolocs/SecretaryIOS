//! Bind the authored native skirt lattice and retain the source .psk profile.
use crate::{compose, glb_io, invalid, physics::SkirtPhysics, resources::ResourceSource, Result};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
fn coordinate(name: &str) -> Option<(usize, usize)> {
    let name = name.to_lowercase();
    let name = name.strip_prefix("skirt")?;
    let mut parts = name.split('_');
    let column = parts.next()?.parse::<usize>().ok()?.checked_sub(1)?;
    let letters = parts.next()?;
    if !name.contains("_yure_skirt_") || letters.is_empty() {
        return None;
    }
    let mut row = 0usize;
    for c in letters.bytes() {
        if !c.is_ascii_lowercase() {
            return None;
        }
        row = row.checked_mul(26)?.checked_add((c - b'a' + 1) as usize)?;
    }
    Some((column, row.checked_sub(1)?))
}
pub fn annotate_file(
    resources: &mut impl ResourceSource,
    source: &str,
    path: &Path,
) -> Result<usize> {
    let (mut doc, data) = glb_io::read(path)?;
    if doc["extras"]["nativeClothRevision"] == 2 {
        return Ok(doc["extras"]["nativeClothNodes"].as_u64().unwrap_or(0) as usize);
    }
    let mut indices = BTreeMap::new();
    for (i, n) in doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("native cloth nodes"))?
        .iter()
        .enumerate()
    {
        if let Some((column, row)) = n["name"].as_str().and_then(coordinate) {
            if indices.insert((column, row), i).is_some() {
                return Err(invalid("duplicate native cloth coordinate"));
            }
        }
    }
    if indices.is_empty() {
        return Ok(0);
    }
    let columns = indices.keys().map(|v| v.0 + 1).max().unwrap();
    let rows = indices.keys().map(|v| v.1 + 1).max().unwrap();
    if !(3..=24).contains(&columns) || !(2..=48).contains(&rows) || indices.len() != columns * rows
    {
        return Err(invalid("incomplete native cloth lattice"));
    }
    let stem = Path::new(source)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("native cloth source"))?;
    let own = format!("{stem}.psk");
    let has_own = resources.paths().any(|p| {
        p.rsplit('/')
            .next()
            .is_some_and(|n| n.eq_ignore_ascii_case(&own))
    });
    let profile_path =
        resources.resolve(if has_own { &own } else { "default_skirt.psk" }, source)?;
    let profile = SkirtPhysics::read(&resources.read_resource(&profile_path)?)?;
    let (parents, _, world) = compose::scene_frames(&doc)?;
    if let Some(i) = doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .position(|n| n["name"] == "Skirt")
    {
        let parent = if parents[i] >= 0 {
            world[parents[i] as usize]
        } else {
            glam::Mat4::IDENTITY
        };
        doc["nodes"][i]["extras"]["secretaryShapeSource"] =
            json!({"world":world[i].to_cols_array(),"parentWorld":parent.to_cols_array()});
    }

    let mut lengths = vec![0f32; indices.len()];
    for row in 1..rows {
        for col in 0..columns {
            let i = row * columns + col;
            lengths[i] = lengths[i - columns]
                + world[indices[&(col, row)]]
                    .w_axis
                    .truncate()
                    .distance(world[indices[&(col, row - 1)]].w_axis.truncate());
        }
    }
    let damping = -profile.velocity_retention.clamp(0.001, 0.9999).ln() * 60.;
    let parameters = [
        profile.iterations.min(32) as f32,
        damping,
        0.000001 / profile.connection_strength[1].max(0.0001),
        0.000001 / profile.connection_strength[3].max(0.0001),
        0.002,
        (profile.panier_force * 10.).clamp(0., 20.),
        (profile.panier_radius * 0.3).clamp(0.002, 0.02),
        -0.03,
    ];
    let id = format!("native-{stem}");
    for ((column, row), node) in &indices {
        let gravity = profile
            .gravity_distribution
            .sample(lengths[row * columns + column])
            .clamp(0., 4.);
        doc["nodes"][*node]["extras"]["secretarySecondary"] = json!({"kind":"cloth","grid":{"id":id,"column":column,"row":row,"columns":columns,"rows":rows,"closed":true,"parameters":parameters,"gravityWeight":gravity,"source":"native-psk"}});
    }
    doc["extras"]["nativeClothRevision"] = json!(2);
    doc["extras"]["nativeClothNodes"] = json!(indices.len());
    doc["extras"]["nativeSkirtPhysics"] = json!({"resource":profile_path,"parameters":profile,"solver":"Rust XPBD; source units retained, world gravity 9.81 m/s²","nativeSolverParity":false});
    glb_io::write(path, &doc, &data)?;
    Ok(indices.len())
}
/// A small upper-back contact proxy from the actual worn surface. This keeps
/// long hair outside a collar while leaving front locks and the face alone.
pub fn add_back_cage(path: &Path) -> Result<usize> {
    let (mut doc, data) = glb_io::read(path)?;
    let (_, _, world) = compose::scene_frames(&doc)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let find = |name: &str| {
        nodes.iter().position(|n| {
            n["name"]
                .as_str()
                .is_some_and(|n| n.replace('_', " ").eq_ignore_ascii_case(name))
        })
    };
    let (Some(spine), Some(neck)) = (find("Bip01 Spine1a"), find("Bip01 Neck")) else {
        return Ok(0);
    };
    let a = world[spine].w_axis.truncate();
    let b = world[neck].w_axis.truncate();
    let mut back = Vec::new();
    for n in &nodes {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let primitives = doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .unwrap();
        if primitives.iter().any(|p| {
            doc["materials"][p["material"].as_u64().unwrap_or(0) as usize]["extras"]
                ["secretarySkin"]["version"]
                == 2
        }) {
            continue;
        }
        let s = &doc["skins"][skin as usize];
        let ib = crate::accessor::read(
            &doc,
            &data,
            s["inverseBindMatrices"].as_u64().unwrap() as usize,
        )?;
        let transforms = s["joints"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, j)| {
                world[j.as_u64().unwrap() as usize]
                    * glam::Mat4::from_cols_array(ib[i].as_slice().try_into().unwrap())
            })
            .collect::<Vec<_>>();
        for p in primitives {
            let attrs = &p["attributes"];
            let pos =
                crate::accessor::read(&doc, &data, attrs["POSITION"].as_u64().unwrap() as usize)?;
            let j =
                crate::accessor::read(&doc, &data, attrs["JOINTS_0"].as_u64().unwrap() as usize)?;
            let w =
                crate::accessor::read(&doc, &data, attrs["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let used = crate::accessor::read(&doc, &data, p["indices"].as_u64().unwrap() as usize)?
                .into_iter()
                .map(|v| v[0] as usize)
                .collect::<std::collections::HashSet<_>>();
            for i in used {
                let mut point = glam::Vec3::ZERO;
                for k in 0..4 {
                    point += transforms[j[i][k] as usize]
                        .transform_point3(glam::Vec3::from_slice(&pos[i]))
                        * w[i][k];
                }
                if point.y >= a.y - 0.025
                    && point.y <= b.y + 0.025
                    && (point.x - b.x).abs() < 0.09
                    && point.z > a.z + 0.025
                {
                    back.push(point.z);
                }
            }
        }
    }
    for n in doc["nodes"].as_array_mut().unwrap() {
        if let Some(extra) = n["extras"].as_object_mut() {
            extra.remove("secretaryOutfitCapsules");
        }
    }
    if back.len() < 10 {
        return Ok(0);
    }
    back.sort_by(f32::total_cmp);
    let surface = back[(back.len() * 9 / 10).min(back.len() - 1)];
    let radius = 0.035;
    let z = surface + 0.012 - radius;
    let inverse = world[spine].inverse();
    let local_a = inverse.transform_point3(glam::Vec3::new(a.x, a.y - 0.015, z));
    let local_b = inverse.transform_point3(glam::Vec3::new(b.x, b.y + 0.015, z));
    doc["nodes"][spine]["extras"]["secretaryOutfitCapsules"] = json!([{"a":local_a.to_array(),"b":local_b.to_array(),"radius":radius,"source":"worn-upper-back-surface"}]);
    glb_io::write(path, &doc, &data)?;
    Ok(1)
}

/// The original Lucina cape is an authored open 3x4 lattice. Keep its original
/// weights and anchors instead of replacing it with a radial skirt approximation.
pub fn annotate_mantle_file(path: &Path) -> Result<usize> {
    let (mut doc, data) = glb_io::read(path)?;
    if doc["extras"]["lucinaMantleClothRevision"] == 1 {
        return Ok(12);
    }
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let names = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n["name"].as_str().map(|n| (n.to_owned(), i)))
        .collect::<std::collections::HashMap<_, _>>();
    let mut controls = Vec::new();
    for row in 0..4 {
        for side in ["l", "c", "r"] {
            let name = format!("{side}_sprmnt{}_jnt", row + 1);
            let Some(i) = names.get(&name) else {
                return Ok(0);
            };
            controls.push(*i);
        }
    }
    let mut used = 0f32;
    for n in &nodes {
        let (Some(mesh), Some(skin)) = (n["mesh"].as_u64(), n["skin"].as_u64()) else {
            continue;
        };
        let joints = doc["skins"][skin as usize]["joints"].as_array().unwrap();
        for p in doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .unwrap()
        {
            let a = &p["attributes"];
            let j = crate::accessor::read(&doc, &data, a["JOINTS_0"].as_u64().unwrap() as usize)?;
            let w = crate::accessor::read(&doc, &data, a["WEIGHTS_0"].as_u64().unwrap() as usize)?;
            let ix = crate::accessor::read(&doc, &data, p["indices"].as_u64().unwrap() as usize)?
                .into_iter()
                .map(|v| v[0] as usize)
                .collect::<std::collections::HashSet<_>>();
            for v in ix {
                for k in 0..4 {
                    if controls.contains(&(joints[j[v][k] as usize].as_u64().unwrap() as usize)) {
                        used += w[v][k];
                    }
                }
            }
        }
    }
    if used < 1. {
        return Ok(0);
    }
    let parameters = [18., 10., 0.00001, 0.000001, 0.003, 0.25, 0.007, -0.03];
    for (index, node) in controls.iter().enumerate() {
        doc["nodes"][*node]["extras"]["secretarySecondary"] = json!({"kind":"cloth","grid":{"id":"lucina-authored-mantle","column":index%3,"row":index/3,"columns":3,"rows":4,"closed":false,"parameters":parameters,"gravityWeight":1,"source":"original-authored-cape"}});
    }
    doc["extras"]["lucinaMantleClothRevision"] = json!(1);
    glb_io::write(path, &doc, &data)?;
    Ok(12)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_lattice_names_have_stable_coordinates() {
        assert_eq!(coordinate("Skirt24_E_yure_skirt_"), Some((23, 4)));
        assert_eq!(coordinate("Skirt01_AA_yure_skirt_"), Some((0, 26)));
        assert_eq!(coordinate("Bip01 L Thigh"), None);
        assert_eq!(coordinate("Skirt00_A_yure_skirt_"), None);
    }
}
