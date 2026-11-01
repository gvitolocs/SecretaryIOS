//! `glbgraft TARGET.glb SOURCE.glb OUT.glb BONE[,BONE...]`: a hairstyle's own bones, copied into
//! another outfit.
//!
//! Each bone is added (with its local transform) under the target node that has the same name as
//! its parent in SOURCE, so list parents first. Existing indices, skins and the binary chunk are
//! untouched; bones already in TARGET are skipped. The grafted bones are also listed in an extra
//! joints-only skin `hairstyle-bones` (no inverse bind matrices, no mesh uses it): glTF loaders then
//! load them as bones (three.js marks every joint of every skin), so a hairstyle part re-binds to
//! them and its spring chain can run, even on clients with an older stage.
//! The Gym ponytail's bones into the uniform:
//! `glbgraft lucina.glb lucina-gym.glb lucina.glb.new ptail_0,ptail_1,ptail_2,ptail_3`.

use serde_json::{json, Value};
use std::{env, path::Path, process::ExitCode};
use wardrobe_glb::Glb;

const SKIN: &str = "hairstyle-bones";

fn graft(target: &mut Glb, source: &Glb, names: &[&str]) -> Result<(), String> {
    for &name in names {
        if target.node_index(name).is_some() {
            eprintln!("{name}: already there");
            continue;
        }
        let si = source.node_index(name).ok_or_else(|| format!("{name}: not in the source"))?;
        let sp = source.parent_of(si).ok_or_else(|| format!("{name}: no parent in the source"))?;
        let pname = source.json["nodes"][sp]["name"].as_str().unwrap_or_default().to_owned();
        let pi = target.node_index(&pname).ok_or_else(|| format!("{name}: parent {pname} not in the target"))?;
        let mut node = json!({ "name": name });
        for k in ["translation", "rotation", "scale", "matrix"] {
            if let Some(v) = source.json["nodes"][si].get(k) {
                node[k] = v.clone();
            }
        }
        let nodes = target.json["nodes"].as_array_mut().unwrap();
        nodes.push(node);
        let ni = nodes.len() - 1;
        let parent = nodes[pi].as_object_mut().unwrap();
        parent.entry("children").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!(ni));
        eprintln!("{name}: under {pname}");
    }
    let joints: Vec<Value> = names.iter().filter_map(|n| target.node_index(n)).map(|i| json!(i)).collect();
    let skins = target.json.as_object_mut().unwrap().entry("skins").or_insert_with(|| json!([]));
    let skins = skins.as_array_mut().ok_or("skins is not an array")?;
    match skins.iter_mut().find(|s| s["name"] == SKIN) {
        Some(s) => s["joints"] = json!(joints),
        None => skins.push(json!({ "name": SKIN, "joints": joints })),
    }
    Ok(())
}

fn main() -> ExitCode {
    let a: Vec<String> = env::args().collect();
    if a.len() != 5 {
        eprintln!("usage: glbgraft TARGET.glb SOURCE.glb OUT.glb BONE[,BONE...]");
        return ExitCode::from(2);
    }
    let names: Vec<&str> = a[4].split(',').collect();
    let run = || -> Result<(), String> {
        let mut target = Glb::read(Path::new(&a[1])).map_err(|e| e.to_string())?;
        let source = Glb::read(Path::new(&a[2])).map_err(|e| e.to_string())?;
        graft(&mut target, &source, &names)?;
        target.write(Path::new(&a[3])).map_err(|e| format!("{}: {e}", a[3]))
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
