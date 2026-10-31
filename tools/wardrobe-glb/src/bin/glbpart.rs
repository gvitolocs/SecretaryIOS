//! `glbpart IN.glb OUT.glb MATERIAL[,MATERIAL...]`: a hairstyle part cut out of an outfit GLB.
//!
//! Keeps only the primitives whose material is listed, and the materials, textures and images
//! they use; drops animations and empty meshes. Nodes, skins and the binary chunk stay as they
//! are, so the part has the outfit's exact bind pose and bone names (HairRig re-binds it by name).
//! The Gym outfit's own ponytail: `glbpart lucina-gym.glb lucina-hair-ponytail.glb Hair,Clothes_HairTie`.

use serde_json::{json, Value};
use std::{collections::BTreeMap, env, path::Path, process::ExitCode};
use wardrobe_glb::Glb;

/// Rewrites every `"index"` of a `*Texture` object under `v` through `f`.
fn remap_textures(v: &mut Value, f: &mut dyn FnMut(usize) -> usize) {
    match v {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if k.ends_with("Texture") {
                    if let Some(i) = x.get("index").and_then(Value::as_u64) {
                        x["index"] = json!(f(i as usize));
                    }
                }
                remap_textures(x, f);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| remap_textures(x, f)),
        _ => {}
    }
}

/// Keeps the entries of `list` at the indices `f` asks for, in first-use order.
struct Keep<'a> {
    list: &'a [Value],
    map: BTreeMap<usize, usize>,
    out: Vec<Value>,
}

impl<'a> Keep<'a> {
    fn new(list: &'a [Value]) -> Self {
        Self { list, map: BTreeMap::new(), out: Vec::new() }
    }
    fn index(&mut self, i: usize) -> usize {
        *self.map.entry(i).or_insert_with(|| {
            self.out.push(self.list[i].clone());
            self.out.len() - 1
        })
    }
}

fn main() -> ExitCode {
    let a: Vec<String> = env::args().collect();
    if a.len() != 4 {
        eprintln!("usage: glbpart IN.glb OUT.glb MATERIAL[,MATERIAL...]");
        return ExitCode::from(2);
    }
    let keep: Vec<&str> = a[3].split(',').collect();
    let mut glb = match Glb::read(Path::new(&a[1])) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let materials = glb.list("materials");
    let textures = glb.list("textures");
    let images = glb.list("images");
    let wanted = |p: &Value| {
        p["material"].as_u64().and_then(|k| materials.get(k as usize)).and_then(|m| m["name"].as_str()).is_some_and(|n| keep.contains(&n))
    };

    // Meshes: only the wanted primitives; meshes left empty go, with their nodes' mesh/skin.
    let mut mesh_map = BTreeMap::new();
    let mut meshes = Vec::new();
    for (i, m) in glb.list("meshes").into_iter().enumerate() {
        let prims: Vec<Value> = m["primitives"].as_array().into_iter().flatten().filter(|p| wanted(p)).cloned().collect();
        if prims.is_empty() {
            continue;
        }
        let mut m = m;
        m["primitives"] = json!(prims);
        mesh_map.insert(i, meshes.len());
        meshes.push(m);
    }
    if meshes.is_empty() {
        eprintln!("no primitive uses {}", a[3]);
        return ExitCode::FAILURE;
    }
    for n in glb.json["nodes"].as_array_mut().into_iter().flatten() {
        let Some(mi) = n.get("mesh").and_then(Value::as_u64) else { continue };
        match mesh_map.get(&(mi as usize)) {
            Some(k) => n["mesh"] = json!(k),
            None => {
                let o = n.as_object_mut().unwrap();
                o.remove("mesh");
                o.remove("skin");
            }
        }
    }

    // Materials, then the textures they use, then those textures' images.
    let mut mats = Keep::new(&materials);
    for m in &mut meshes {
        for p in m["primitives"].as_array_mut().unwrap() {
            p["material"] = json!(mats.index(p["material"].as_u64().unwrap() as usize));
        }
    }
    let mut new_mats = mats.out;
    let mut texs = Keep::new(&textures);
    for m in &mut new_mats {
        remap_textures(m, &mut |t| texs.index(t));
    }
    let mut new_texs = texs.out;
    let mut imgs = Keep::new(&images);
    for t in &mut new_texs {
        if let Some(s) = t.get("source").and_then(Value::as_u64) {
            t["source"] = json!(imgs.index(s as usize));
        }
        // EXT_texture_webp and friends name their own source.
        for e in t.get_mut("extensions").and_then(Value::as_object_mut).into_iter().flat_map(|o| o.values_mut()) {
            if let Some(s) = e.get("source").and_then(Value::as_u64) {
                e["source"] = json!(imgs.index(s as usize));
            }
        }
    }

    let names: Vec<Value> = new_mats.iter().map(|m| m["name"].clone()).collect();
    let o = glb.json.as_object_mut().unwrap();
    o.insert("meshes".into(), json!(meshes));
    o.insert("materials".into(), json!(new_mats));
    o.remove("animations");
    if new_texs.is_empty() {
        for k in ["textures", "images", "samplers"] {
            o.remove(k);
        }
    } else {
        o.insert("textures".into(), json!(new_texs));
        o.insert("images".into(), json!(imgs.out));
    }
    if let Err(e) = glb.write(Path::new(&a[2])) {
        eprintln!("{}: {e}", a[2]);
        return ExitCode::FAILURE;
    }
    eprintln!("{}: materials {}", a[2], json!(names));
    ExitCode::SUCCESS
}
