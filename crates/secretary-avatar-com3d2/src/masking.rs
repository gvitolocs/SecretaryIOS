//! Native body visibility rules. A menu is data, never executable script.
//! The game hides a triangle if any of its vertices uses a hidden skin joint.
use crate::{compose::scene_frames, glb_io, invalid, menu::Menu, Result};
use serde_json::{json, Value};
use std::path::Path;

fn numbers(doc: &Value, data: &[u8], index: usize, width: usize) -> Result<Vec<[f32; 4]>> {
    let a = doc["accessors"]
        .get(index)
        .ok_or_else(|| invalid("mask accessor"))?;
    if a["sparse"].is_object() {
        return Err(invalid("sparse skin attributes unsupported"));
    }
    let component = a["componentType"]
        .as_u64()
        .ok_or_else(|| invalid("skin component"))?;
    let bytes = match component {
        5121 => 1,
        5123 => 2,
        5125 | 5126 => 4,
        _ => return Err(invalid("skin component type")),
    };
    let count = a["count"]
        .as_u64()
        .filter(|n| *n <= 1_000_000)
        .ok_or_else(|| invalid("skin count"))? as usize;
    let view = a["bufferView"]
        .as_u64()
        .and_then(|i| doc["bufferViews"].get(i as usize))
        .ok_or_else(|| invalid("skin view"))?;
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + a["byteOffset"].as_u64().unwrap_or(0) as usize;
    let stride = view["byteStride"]
        .as_u64()
        .unwrap_or((bytes * width) as u64) as usize;
    if stride < bytes * width {
        return Err(invalid("skin stride"));
    }
    let normalized = a["normalized"] == true;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut value = [0.; 4];
        for (j, v) in value.iter_mut().enumerate().take(width) {
            let p = start
                .checked_add(
                    i.checked_mul(stride)
                        .ok_or_else(|| invalid("skin offset"))?,
                )
                .and_then(|n| n.checked_add(j * bytes))
                .ok_or_else(|| invalid("skin offset"))?;
            let b = data
                .get(p..p + bytes)
                .ok_or_else(|| invalid("skin data bounds"))?;
            *v = match component {
                5121 => b[0] as f32 / if normalized { 255. } else { 1. },
                5123 => {
                    u16::from_le_bytes(b.try_into().unwrap()) as f32
                        / if normalized { 65535. } else { 1. }
                }
                5125 => u32::from_le_bytes(b.try_into().unwrap()) as f32,
                5126 => f32::from_le_bytes(b.try_into().unwrap()),
                _ => unreachable!(),
            };
            if !v.is_finite() {
                return Err(invalid("skin numeric value"));
            }
        }
        out.push(value);
    }
    Ok(out)
}

pub fn visibility(doc: &Value, menu: &Menu) -> Result<Vec<bool>> {
    let (parents, _, _) = scene_frames(doc)?;
    let nodes = doc["nodes"].as_array().unwrap();
    let mut visible = vec![true; nodes.len()];
    for command in &menu.commands {
        let Some(kind) = command.first().map(String::as_str) else {
            continue;
        };
        let flag = match kind {
            "node消去" => false,
            "node表示" => true,
            _ => continue,
        };
        if command.len() != 2 {
            return Err(invalid("body visibility rule arguments"));
        }
        for (i, visibility) in visible.iter_mut().enumerate() {
            let mut current = i as i32;
            while current >= 0 {
                let name = nodes[current as usize]["name"].as_str().unwrap_or("");
                if command[1] == "_ALL_" || name.contains(&command[1]) {
                    *visibility = flag;
                    break;
                }
                current = parents[current as usize];
            }
        }
    }
    Ok(visible)
}

pub fn body(input: &Path, menu: &Menu, output: &Path) -> Result<Value> {
    let (mut doc, mut data) = glb_io::read(input)?;
    let visible = visibility(&doc, menu)?;
    let nodes = doc["nodes"].as_array().unwrap().clone();
    let mut removed = 0;
    let mut kept = 0;
    for node in nodes {
        let (Some(mesh), Some(skin)) = (node["mesh"].as_u64(), node["skin"].as_u64()) else {
            continue;
        };
        // Native round-trip models may rename/merge the body node. Preserve
        // the real face/hair primitives while selecting tagged body materials.
        let legacy_body = node["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("Lucina_COM3D2_Body") || n.starts_with("COM3D2_body"));
        let joints = doc["skins"][skin as usize]["joints"]
            .as_array()
            .ok_or_else(|| invalid("body skin"))?
            .iter()
            .map(|j| {
                j.as_u64()
                    .filter(|i| *i < visible.len() as u64)
                    .map(|i| visible[i as usize])
                    .ok_or_else(|| invalid("body skin node"))
            })
            .collect::<Result<Vec<_>>>()?;
        let primitives = doc["meshes"][mesh as usize]["primitives"]
            .as_array()
            .ok_or_else(|| invalid("body primitives"))?
            .clone();
        for (p, primitive) in primitives.iter().enumerate() {
            if !legacy_body
                && !primitive["material"].as_u64().is_some_and(|m| {
                    doc["materials"][m as usize]["name"]
                        .as_str()
                        .is_some_and(|n| n.starts_with("skin_safe"))
                })
            {
                continue;
            }

            let get = |key: &str| {
                primitive["attributes"][key]
                    .as_u64()
                    .ok_or_else(|| invalid("body skin attribute"))
            };
            let ids = numbers(&doc, &data, get("JOINTS_0")? as usize, 4)?;
            let weights = numbers(&doc, &data, get("WEIGHTS_0")? as usize, 4)?;
            if ids.len() != weights.len() {
                return Err(invalid("skin attribute length"));
            }
            let idx = primitive["indices"]
                .as_u64()
                .ok_or_else(|| invalid("body triangle indices"))? as usize;
            let indices = numbers(&doc, &data, idx, 1)?;
            if indices.len() % 3 != 0 {
                return Err(invalid("body triangle length"));
            }
            let mut result = Vec::new();
            for tri in indices.as_chunks::<3>().0.iter() {
                let mut hide = false;
                for index in tri {
                    let v = index[0] as usize;
                    let is = ids.get(v).ok_or_else(|| invalid("body vertex index"))?;
                    for k in 0..4 {
                        if k == 0 || weights[v][k] > 0. {
                            let flag = joints
                                .get(is[k] as usize)
                                .ok_or_else(|| invalid("body skin joint"))?;
                            hide |= !*flag;
                        }
                    }
                }
                if hide {
                    removed += 1;
                } else {
                    kept += 1;
                    for index in tri {
                        result.extend((index[0] as u32).to_le_bytes());
                    }
                }
            }
            // glTF accessors require a nonzero count. A fully hidden region
            // uses the native renderer's degenerate-triangle convention.
            if result.is_empty() {
                result.extend([0u8; 12]);
            }
            while !data.len().is_multiple_of(4) {
                data.push(0);
            }
            let start = data.len();
            data.extend(&result);
            let view = doc["bufferViews"].as_array().unwrap().len();
            doc["bufferViews"]
                .as_array_mut()
                .unwrap()
                .push(json!({"buffer":0,"byteOffset":start,"byteLength":result.len()}));
            let accessor = doc["accessors"].as_array().unwrap().len();
            doc["accessors"].as_array_mut().unwrap().push(json!({"bufferView":view,"componentType":5125,"count":result.len()/4,"type":"SCALAR"}));
            doc["meshes"][mesh as usize]["primitives"][p]["indices"] = json!(accessor);
        }
    }
    doc["buffers"][0]["byteLength"] = json!(data.len());
    doc["extras"]["bodyVisibility"] =
        json!({"source":menu.source,"removedTriangles":removed,"keptTriangles":kept});
    glb_io::write(output, &doc, &data)?;
    Ok(json!({"removedTriangles":removed,"keptTriangles":kept,"output":output}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn later_show_rule_restores_descendants_without_restoring_siblings() {
        let doc = json!({"nodes":[{"name":"Spine1","children":[1,3]},{"name":"Spine1a","children":[2]},{"name":"Head"},{"name":"UpperArm"}]});
        let menu = Menu {
            version: 2001,
            source: String::new(),
            name: String::new(),
            category: String::new(),
            description: String::new(),
            commands: vec![
                vec!["node消去".into(), "Spine1".into()],
                vec!["node表示".into(), "Spine1a".into()],
            ],
        };
        assert_eq!(
            visibility(&doc, &menu).unwrap(),
            vec![false, true, true, false]
        );
    }
}
