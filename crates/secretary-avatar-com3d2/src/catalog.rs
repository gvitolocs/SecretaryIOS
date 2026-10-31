//! Private inventory of native menu records. This does not execute menus or
//! mistake a successful parse for a fully compatible outfit.
use crate::{arc::Archive, menu::Menu, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
pub fn build(archive: &mut Archive) -> Result<Value> {
    let paths = archive
        .entries
        .iter()
        .filter(|e| e.path.ends_with(".menu"))
        .map(|e| e.path.clone())
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut failures = Vec::new();
    let mut categories = BTreeMap::<String, usize>::new();
    for path in paths {
        match archive.read(&path).and_then(|bytes| Menu::read(&bytes)) {
            Ok(menu) => {
                *categories.entry(menu.category.clone()).or_default() += 1;
                let assets = menu
                    .commands
                    .iter()
                    .filter(|c| c.first().is_some_and(|s| s == "additem"))
                    .collect::<Vec<_>>();
                let materials = menu
                    .commands
                    .iter()
                    .filter(|c| c.first().is_some_and(|s| s == "マテリアル変更"))
                    .collect::<Vec<_>>();
                let masks = menu
                    .commands
                    .iter()
                    .filter(|c| {
                        c.first().is_some_and(|s| {
                            [
                                "node消去",
                                "node表示",
                                "maskitem",
                                "パーツnode消去",
                                "パーツnode表示",
                            ]
                            .contains(&s.as_str())
                        })
                    })
                    .collect::<Vec<_>>();
                // Stable file identity, deliberately unrelated to array order.
                let id = path.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                    (h ^ b as u64).wrapping_mul(0x100000001b3)
                });
                entries.push(json!({"id":format!("com-{id:016x}"),"source":path,"name":menu.name,"category":menu.category,"description":menu.description,"models":assets,"materials":materials,"visibilityRules":masks,"version":menu.version,"reviewed":false,"runtimeReady":false}));
            }
            Err(error) => failures.push(json!({"source":path,"error":error.to_string()})),
        }
    }
    Ok(
        json!({"version":1,"private":true,"scriptExecution":false,"parsed":entries.len(),"categories":categories,"entries":entries,"failures":failures}),
    )
}
