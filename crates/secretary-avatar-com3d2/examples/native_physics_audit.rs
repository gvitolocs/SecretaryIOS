use secretary_avatar_com3d2::{
    menu::Menu,
    model::Model,
    physics::Physics,
    resources::{ArchiveSet, ResourceSource},
    Result,
};
use serde_json::{json, Value};
use std::fs;
fn main() -> Result<()> {
    let cfg: Value = serde_json::from_slice(&fs::read(
        "/home/nez/.local/state/lucina-nte-preview/wardrobe-native-v3/library-config.json",
    )?)
    .unwrap();
    let archives = cfg["archives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let mut resources = ArchiveSet::open(&archives)?;
    let paths = resources
        .paths()
        .filter(|p| {
            p.ends_with(".phy")
                && (p.contains("onepiece003") || p.contains("dress218") || p.contains("default"))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut physics = Vec::new();
    for path in paths {
        let result = resources
            .read_resource(&path)
            .and_then(|b| Physics::read(&b));
        physics.push(match result {
            Ok(p) => json!({"path":path,"physics":p}),
            Err(e) => json!({"path":path,"error":e.to_string()}),
        });
    }
    let mut models = Vec::new();
    for id in ["com-288d0149e4d24a86", "com-8f102903bef85d87"] {
        let item = cfg["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap();
        let menu = item["source"]["menu"].as_str().unwrap();
        let m = Menu::read(&resources.read_resource(menu)?)?;
        let mut model_info = Vec::new();
        for command in &m.commands {
            if command.first().is_some_and(|s| s == "additem") {
                let path = resources.resolve(&command[1], menu)?;
                let model = Model::read(&resources.read_resource(&path)?)?;
                let bones = model
                    .bones
                    .iter()
                    .filter(|b| {
                        let n = b.name.to_lowercase();
                        n.contains("skirt")
                            || n.contains("hair")
                            || n.contains("jiggle")
                            || n.contains("coat")
                    })
                    .collect::<Vec<_>>();
                model_info.push(json!({"path":path,"root":model.root,"bones":bones}));
            }
        }
        models.push(json!({"id":id,"commands":m.commands,"models":model_info}));
    }
    println!("{}", json!({"physics":physics,"models":models}));
    Ok(())
}
