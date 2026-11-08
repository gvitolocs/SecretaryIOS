use secretary_avatar_com3d2::{glb_io, Result};
use serde_json::json;
use std::path::Path;
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let (d, _) = glb_io::read(Path::new(&args[1]))?;
    let nodes = d["nodes"].as_array().unwrap();
    let chains = nodes
        .iter()
        .filter(|n| n["extras"]["secretarySecondary"]["chain"] == true)
        .collect::<Vec<_>>();
    println!(
        "{}",
        json!({"chainJoints":chains.len(),"authoredAxesRetained":chains.iter().all(|n|n["extras"]["secretarySecondary"]["tailSource"]=="authored-chain"),"ornamentVertices":d["extras"]["secretaryRibbonOrnamentVertices"],"tips":d["extras"]["secretaryRibbonTips"],"secondary":d["extras"]["secondaryGravityBindings"]})
    );
    Ok(())
}
