use secretary_avatar_com3d2::arc::Archive;
use std::{env, fs, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("match-skin-tone") if args.len()==4=>{println!("{}",secretary_avatar_com3d2::skin_assets::match_tone(Path::new(&args[2]),Path::new(&args[3]))?);}
        Some("secondary-bindings") if args.len()==4=>{println!("{}",secretary_avatar_com3d2::secondary_bindings::annotate(Path::new(&args[2]),Path::new(&args[3]))?);}
        Some("export-native-head") if args.len()==5=>{let body=secretary_avatar_com3d2::model::Model::read(&fs::read(&args[3])?)?;println!("{}",secretary_avatar_com3d2::native_pack::export_head(Path::new(&args[2]),&body,Path::new(&args[4]))?);}
        Some("export-native-pack") if args.len()==6 => {let body=secretary_avatar_com3d2::model::Model::read(&fs::read(&args[3])?)?;println!("{}",secretary_avatar_com3d2::native_pack::export(Path::new(&args[2]),&body,Path::new(&args[4]),&args[5])?);}
        Some("glb-summary") if args.len()==3=>{let(d,_)=secretary_avatar_com3d2::glb_io::read(Path::new(&args[2]))?;println!("{}",serde_json::json!({"materials":d["materials"],"meshes":d["meshes"].as_array().unwrap().iter().map(|m|serde_json::json!({"name":m["name"],"targets":m["extras"]["targetNames"],"primitives":m["primitives"].as_array().unwrap().iter().map(|p|serde_json::json!({"material":p["material"],"vertices":d["accessors"][p["attributes"]["POSITION"].as_u64().unwrap() as usize]["count"]})).collect::<Vec<_>>()})).collect::<Vec<_>>(),"skins":d["skins"].as_array().map(Vec::len)}));}
        Some("index") if args.len()==3 => {let a=Archive::open(Path::new(&args[2]))?;println!("{}",serde_json::to_string(&a.entries)?);}
        Some("library-init") if args.len()==9||args.len()==10 => {println!("{}",secretary_avatar_com3d2::wardrobe::init(Path::new(&args[2]),Path::new(&args[3]),Path::new(&args[4]),Path::new(&args[5]),&[args[6].clone(),args[7].clone()],Path::new(&args[8]),args.get(9).map(Path::new))?);}
        Some("library-compose") if args.len()==4 => {let mut library=secretary_avatar_com3d2::wardrobe::Library::open(Path::new(&args[2]))?;let ids=if args[3].is_empty(){Vec::new()}else{args[3].split(',').map(str::to_owned).collect::<Vec<_>>()};println!("{}",library.compose(&ids)?);}
        Some("library-warm") if args.len()==3||args.len()==4 => {let mut library=secretary_avatar_com3d2::wardrobe::Library::open(Path::new(&args[2]))?;println!("{}",library.warm(args.get(3).is_some_and(|v|v=="all"))?);}
        Some("retarget-legacy") if args.len()==5||args.len()==6 => {println!("{}",secretary_avatar_com3d2::legacy::retarget(Path::new(&args[2]),Path::new(&args[3]),Path::new(&args[4]),args.get(5).is_some_and(|v|v=="hair"))?);}
        Some("scene-inspect") if args.len()==3 => {let(doc,_)=secretary_avatar_com3d2::glb_io::read(Path::new(&args[2]))?;let(_,_,world)=secretary_avatar_com3d2::compose::scene_frames(&doc)?;println!("{}",serde_json::json!(doc["nodes"].as_array().unwrap().iter().enumerate().map(|(i,n)|serde_json::json!({"name":n["name"],"head":world[i].w_axis.to_array()})).collect::<Vec<_>>()));}
        Some("catalog") if args.len()==4 => {let mut archive=Archive::open(Path::new(&args[2]))?;let catalog=secretary_avatar_com3d2::catalog::build(&mut archive)?;fs::write(&args[3],serde_json::to_vec(&catalog)?)?;println!("{}",serde_json::json!({"parsed":catalog["parsed"],"failures":catalog["failures"].as_array().map_or(0,Vec::len),"categories":catalog["categories"]}));}
        Some("extract") if args.len()==5 => {let mut a=Archive::open(Path::new(&args[2]))?;let b=a.read(&args[3])?;fs::write(&args[4],b)?;}
        Some("model-inspect") if args.len()==3 => {let m=secretary_avatar_com3d2::model::Model::read(&fs::read(&args[2])?)?;println!("{}",m.summary());}
        Some("bind-audit") if args.len()==3 => {let m=secretary_avatar_com3d2::model::Model::read(&fs::read(&args[2])?)?;println!("{}",m.bind_audit());}
        Some("model-json") if args.len()==4 => {let m=secretary_avatar_com3d2::model::Model::read(&fs::read(&args[2])?)?;fs::write(&args[3],serde_json::to_vec(&m)?)?;}
        Some("export-model") if args.len()==5 => {let mut a=Archive::open(Path::new(&args[2]))?;let report=secretary_avatar_com3d2::gltf::export(&mut a,&args[3],Path::new(&args[4]))?;println!("{report}");}
        Some("menu-inspect") if args.len()==3 => {println!("{}",serde_json::to_string(&secretary_avatar_com3d2::menu::Menu::read(&fs::read(&args[2])?)?)?);}
        Some("skirt-inspect") if args.len()==3 => {println!("{}",serde_json::to_string(&secretary_avatar_com3d2::physics::SkirtPhysics::read(&fs::read(&args[2])?)?)?);}
        Some("physics-inspect") if args.len()==3 => {println!("{}",serde_json::to_string(&secretary_avatar_com3d2::physics::Physics::read(&fs::read(&args[2])?)?)?);}
        Some("animation-inspect") if args.len()==3 => {let clip=secretary_avatar_com3d2::animation::Animation::read(&fs::read(&args[2])?)?;println!("{}",serde_json::json!({"version":clip.version,"duration":clip.duration(),"bones":clip.bones.len(),"paths":clip.bones.iter().map(|b|&b.path).collect::<Vec<_>>()}));}
        Some("animation-json") if args.len()==4 => {let clip=secretary_avatar_com3d2::animation::Animation::read(&fs::read(&args[2])?)?;fs::write(&args[3],serde_json::to_vec(&clip)?)?;}
        Some("pose-asset") if args.len()==6 => {let model=secretary_avatar_com3d2::model::Model::read(&fs::read(&args[2])?)?;let clip=secretary_avatar_com3d2::animation::Animation::read(&fs::read(&args[3])?)?;println!("{}",secretary_avatar_com3d2::pose_asset::standing(&model,&clip,Path::new(&args[4]),Path::new(&args[5]),0.)?);}
        Some("animate-asset") if args.len()==6 => {let model=secretary_avatar_com3d2::model::Model::read(&fs::read(&args[2])?)?;let clip=secretary_avatar_com3d2::animation::Animation::read(&fs::read(&args[3])?)?;println!("{}",secretary_avatar_com3d2::clip_asset::export(&model,&clip,Path::new(&args[4]),Path::new(&args[5]))?);}
        Some("attach") if args.len()==5 => {println!("{}",secretary_avatar_com3d2::compose::attach(Path::new(&args[2]),Path::new(&args[3]),Path::new(&args[4]))?);}
        Some("body-bindings") if args.len()==5 => {let model=secretary_avatar_com3d2::model::Model::read(&fs::read(&args[2])?)?;let bindings=secretary_avatar_com3d2::bindings::export(&model,Path::new(&args[3]))?;fs::write(&args[4],serde_json::to_vec_pretty(&bindings)?)?;}
        Some("mask-body") if args.len()==5 => {let menu=secretary_avatar_com3d2::menu::Menu::read(&fs::read(&args[3])?)?;println!("{}",secretary_avatar_com3d2::masking::body(Path::new(&args[2]),&menu,Path::new(&args[4]))?);}
        Some("serve") if args.len()==4 => {secretary_avatar_com3d2::preview::serve(Path::new(&args[2]),Path::new(&args[3]))?;}
        Some("audit-models") if args.len()==3 => {
            let mut archive=Archive::open(Path::new(&args[2]))?;
            let paths=archive.entries.iter().filter(|e|e.path.ends_with(".model")).map(|e|e.path.clone()).collect::<Vec<_>>();
            let(mut passed,mut vertices,mut morphs)=(0usize,0usize,0usize);let mut failures=Vec::new();
            for path in &paths{
                match archive.read(path).and_then(|bytes|secretary_avatar_com3d2::model::Model::read(&bytes)){
                    Ok(model)=>{passed+=1;vertices+=model.vertices.len();morphs+=model.morphs.len();},
                    Err(error)=>failures.push(serde_json::json!({"path":path,"error":error.to_string()})),
                }
            }
            println!("{}",serde_json::json!({"models":paths.len(),"passed":passed,"vertices":vertices,"morphs":morphs,"failures":failures}));
        }
        Some("skin-preview") if args.len()==4 => {println!("{}",secretary_avatar_com3d2::skin_preview::harmonize(Path::new(&args[2]),Path::new(&args[3]))?);}
        Some("unify-skin") if args.len()==4||args.len()==5 => {println!("{}",secretary_avatar_com3d2::skin_assets::normalize(Path::new(&args[2]),Path::new(&args[3]),args.get(4).map(Path::new))?);}
        _ => return Err("Commands: index, extract, model-inspect, model-json, bind-audit, export-model, audit-models, menu-inspect, physics-inspect, animation-inspect, animation-json, pose-asset, animate-asset, attach, mask-body, catalog, body-bindings, skin-preview, serve. See crate README for arguments.".into()),
    }
    Ok(())
}
