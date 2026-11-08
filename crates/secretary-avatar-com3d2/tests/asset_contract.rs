use glam::{Mat4, Vec3};
use secretary_avatar_com3d2::{compose, glb_io};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "secretary-avatar-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn matrix_bytes(ms: &[Mat4]) -> Vec<u8> {
    ms.iter()
        .flat_map(|m| m.to_cols_array().into_iter().flat_map(f32::to_le_bytes))
        .collect()
}
#[test]
fn wearable_merge_preserves_rest_geometry_across_different_bone_axes() {
    let s = Scratch::new();
    let base = s.0.join("base.glb");
    let part = s.0.join("part.glb");
    let output = s.0.join("out.glb");
    let a = Mat4::from_rotation_z(0.7);
    let b = Mat4::from_rotation_x(-0.4);
    let wrist = b * Mat4::from_translation(Vec3::Y);
    let base_doc = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,1]}],"nodes":[{"name":"Joint","matrix":a.to_cols_array()},{"name":"Base","mesh":0,"skin":0}],"skins":[{"joints":[0],"inverseBindMatrices":0}],"meshes":[{"primitives":[{"attributes":{"POSITION":1}}]}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":64}],"accessors":[{"bufferView":0,"componentType":5126,"type":"MAT4","count":1}],"buffers":[{"byteLength":64}]});
    glb_io::write(&base, &base_doc, &matrix_bytes(&[a.inverse()])).unwrap();
    let doc = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,2]}],"nodes":[{"name":"Joint","matrix":b.to_cols_array(),"children":[1]},{"name":"OwnJoint","translation":[0,1,0]},{"name":"Wearable","mesh":0,"skin":0}],"skins":[{"joints":[0,1],"inverseBindMatrices":0}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"material":0}]}],"materials":[{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}],"textures":[{"source":0}],"images":[{"bufferView":0,"mimeType":"image/png"}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":128}],"accessors":[{"bufferView":0,"componentType":5126,"type":"MAT4","count":2},{"componentType":5126,"type":"VEC3","count":1}],"buffers":[{"byteLength":128}]});
    glb_io::write(&part, &doc, &matrix_bytes(&[b.inverse(), wrist.inverse()])).unwrap();
    let report = compose::attach(&base, &part, &output).unwrap();
    assert_eq!(report["sharedJoints"], 1);
    assert_eq!(report["addedNodes"], 2);
    let (out, data) = glb_io::read(&output).unwrap();
    let (_, _, world) = compose::scene_frames(&out).unwrap();
    let skin = &out["skins"][1];
    let accessor = &out["accessors"][skin["inverseBindMatrices"].as_u64().unwrap() as usize];
    let view = &out["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let offset = view["byteOffset"].as_u64().unwrap() as usize;
    let probe = Vec3::new(0.23, 0.4, -0.12);
    for (i, joint) in skin["joints"].as_array().unwrap().iter().enumerate() {
        let mut values = [0.; 16];
        for (k, bytes) in data[offset + i * 64..offset + (i + 1) * 64]
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
        {
            values[k] = f32::from_le_bytes(*bytes);
        }
        let transformed = (world[joint.as_u64().unwrap() as usize]
            * Mat4::from_cols_array(&values))
        .transform_point3(probe);
        assert!(
            transformed.distance(probe) < 1e-6,
            "rest geometry changed: {transformed}"
        );
    }
    assert_eq!(out["nodes"][0]["children"], json!([2]));
    assert_eq!(
        out["meshes"][1]["primitives"][0]["attributes"]["POSITION"],
        2
    );
    assert_eq!(out["textures"][0]["source"], 0);
}
#[test]
fn malformed_glb_lengths_are_rejected() {
    let s = Scratch::new();
    let path = s.0.join("asset.glb");
    let doc = json!({"asset":{"version":"2.0"},"buffers":[{"byteLength":4}]});
    glb_io::write(&path, &doc, &[1, 2, 3, 4]).unwrap();
    let bytes = fs::read(&path).unwrap();
    for n in 0..bytes.len() {
        fs::write(&path, &bytes[..n]).unwrap();
        assert!(glb_io::read(&path).is_err(), "truncated at {n}");
    }
    let mut corrupted = bytes.clone();
    let n = u32::from_le_bytes(corrupted[12..16].try_into().unwrap()) as usize;
    corrupted[20 + n..24 + n].copy_from_slice(&8u32.to_le_bytes());
    fs::write(&path, corrupted).unwrap();
    assert!(glb_io::read(&path).is_err());
}

#[test]
fn fully_hidden_body_region_has_no_visible_triangle_and_a_valid_accessor() {
    use secretary_avatar_com3d2::{masking, menu::Menu};
    let s = Scratch::new();
    let input = s.0.join("body.glb");
    let output = s.0.join("hidden.glb");
    let mut data = vec![0u8; 24]; // three U16x4 joint records, all joint 0
    for _ in 0..3 {
        for w in [1f32, 0., 0., 0.] {
            data.extend(w.to_le_bytes());
        }
    }
    for i in [0u16, 1, 2] {
        data.extend(i.to_le_bytes());
    }
    let doc = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,1]}],"nodes":[{"name":"Root"},{"name":"Lucina_COM3D2_Body","mesh":0,"skin":0}],"skins":[{"joints":[0]}],"meshes":[{"primitives":[{"attributes":{"JOINTS_0":0,"WEIGHTS_0":1},"indices":2}]}],"buffers":[{"byteLength":data.len()}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":24},{"buffer":0,"byteOffset":24,"byteLength":48},{"buffer":0,"byteOffset":72,"byteLength":6}],"accessors":[{"bufferView":0,"componentType":5123,"type":"VEC4","count":3},{"bufferView":1,"componentType":5126,"type":"VEC4","count":3},{"bufferView":2,"componentType":5123,"type":"SCALAR","count":3}]});
    let mut doc = doc;
    doc["nodes"][1]["name"] = json!("COM3D2_lucina_native_roundtrip");
    doc["materials"] = json!([{"name":"skin_safe3"},{"name":"LucinaFace"}]);
    doc["meshes"][0]["primitives"][0]["material"] = json!(0);
    let mut face = doc["meshes"][0]["primitives"][0].clone();
    face["material"] = json!(1);
    doc["meshes"][0]["primitives"]
        .as_array_mut()
        .unwrap()
        .push(face);
    glb_io::write(&input, &doc, &data).unwrap();
    let menu = Menu {
        version: 2001,
        source: "test".into(),
        name: "test".into(),
        category: "onepiece".into(),
        description: String::new(),
        commands: vec![vec!["node消去".into(), "_ALL_".into()]],
    };
    let result = masking::body(&input, &menu, &output).unwrap();
    assert_eq!(result["removedTriangles"], 1);
    assert_eq!(result["keptTriangles"], 0);
    let (doc, data) = glb_io::read(&output).unwrap();
    assert_eq!(doc["meshes"][0]["primitives"][1]["indices"], 2);
    let index = doc["meshes"][0]["primitives"][0]["indices"]
        .as_u64()
        .unwrap() as usize;
    let a = &doc["accessors"][index];
    assert_eq!(a["count"], 3);
    let view = &doc["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap() as usize;
    assert!(data[start..start + 12].iter().all(|b| *b == 0));
}
