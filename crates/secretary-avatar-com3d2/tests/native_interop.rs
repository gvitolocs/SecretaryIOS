use glam::{Mat4, Quat, Vec3};
use secretary_avatar_com3d2::{
    accessor, glb_io,
    model::{Bone, Material, Model},
    native_pack,
};
use serde_json::json;
#[test]
fn native_head_roundtrip_standardizes_frames_without_moving_face_uvs_or_blink() {
    let dir = std::env::temp_dir().join(format!(
        "native-head-interop-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let native_rotation = Quat::from_rotation_y(0.7);
    let template = Model {
        version: 2001,
        name: "body".into(),
        root: "body".into(),
        bones: vec![
            Bone {
                name: "body".into(),
                parent: -1,
                scale_helper: false,
                translation: [0.; 3],
                rotation: Quat::IDENTITY.to_array(),
                scale: [1.; 3],
            },
            Bone {
                name: "Bip01 Head".into(),
                parent: 0,
                scale_helper: false,
                translation: [0., 1., 0.],
                rotation: native_rotation.to_array(),
                scale: [1.; 3],
            },
        ],
        skin_bones: vec![],
        inverse_bind: vec![],
        vertices: vec![],
        tangents: vec![],
        skin: vec![],
        submeshes: vec![],
        materials: vec![Material {
            name: "native".into(),
            shader: "CM3D2/Toony_Lighted".into(),
            shader_file: String::new(),
            properties: vec![],
        }],
        morphs: vec![],
    };
    let source_head = Mat4::from_translation(Vec3::Y) * Mat4::from_rotation_z(-0.4);
    let source_eye = source_head * Mat4::from_translation(Vec3::new(0.03, 0.04, -0.08));
    let mut doc = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,3]}],"nodes":[{"name":"body","children":[1]},{"name":"Bip01 Head","translation":[0,1,0],"rotation":Quat::from_rotation_z(-0.4).to_array(),"children":[2]},{"name":"LucinaEye","translation":[0.03,0.04,-0.08]},{"name":"face","mesh":0,"skin":0}],"skins":[],"meshes":[],"accessors":[],"bufferViews":[],"buffers":[{"byteLength":0}],"materials":[{"name":"FaceSkin","pbrMetallicRoughness":{"baseColorTexture":{"index":0},"baseColorFactor":[1,1,1,1]}}]});
    let mut data = Vec::new();
    let points = vec![vec![0., 1., -0.1], vec![0.1, 1., -0.1], vec![0., 1.1, -0.1]];
    let uvs = vec![vec![0.2, 0.3], vec![0.4, 0.5], vec![0.6, 0.7]];
    let pos = accessor::floats(&mut doc, &mut data, &points, 3).unwrap();
    let normal = accessor::floats(&mut doc, &mut data, &vec![vec![0., 0., -1.]; 3], 3).unwrap();
    let uv = accessor::floats(&mut doc, &mut data, &uvs, 2).unwrap();
    let joints = accessor::shorts(&mut doc, &mut data, &[[2, 0, 0, 0]; 3]);
    let weights = accessor::floats(&mut doc, &mut data, &vec![vec![1., 0., 0., 0.]; 3], 4).unwrap();
    let bind = accessor::floats(
        &mut doc,
        &mut data,
        &[
            Mat4::IDENTITY.to_cols_array().to_vec(),
            source_head.inverse().to_cols_array().to_vec(),
            source_eye.inverse().to_cols_array().to_vec(),
        ],
        16,
    )
    .unwrap();
    let index = accessor::shorts(&mut doc, &mut data, &[[0, 1, 2, 0]]);
    doc["accessors"][index]["type"] = json!("SCALAR");
    doc["accessors"][index]["count"] = json!(3);
    let blink = accessor::floats(
        &mut doc,
        &mut data,
        &[vec![0.01, 0., 0.], vec![0.; 3], vec![0.; 3]],
        3,
    )
    .unwrap();
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[250, 80, 20, 255, 30, 120, 240, 128])
            .unwrap();
    }
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let view = doc["bufferViews"].as_array().unwrap().len();
    doc["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":data.len(),"byteLength":png.len()}));
    data.extend(&png);
    doc["textures"] = json!([{"source":0}]);
    doc["images"] = json!([{"bufferView":view,"mimeType":"image/png"}]);
    doc["skins"] = json!([{"joints":[0,1,2],"inverseBindMatrices":bind}]);
    doc["meshes"] = json!([{"primitives":[{"attributes":{"POSITION":pos,"NORMAL":normal,"TEXCOORD_0":uv,"JOINTS_0":joints,"WEIGHTS_0":weights},"indices":index,"material":0,"targets":[{"POSITION":blink}]}],"extras":{"targetNames":["blink"]}}]);
    doc["buffers"][0]["byteLength"] = json!(data.len());
    doc["materials"][0]["normalTexture"] = json!({"index":0,"scale":0.7});
    doc["materials"][0]["pbrMetallicRoughness"]["metallicRoughnessTexture"] = json!({"index":0});
    doc["materials"][0]["occlusionTexture"] = json!({"index":0,"strength":0.3});
    let input = dir.join("source.glb");
    glb_io::write(&input, &doc, &data).unwrap();
    let output = dir.join("native");
    let result = native_pack::export(&input, &template, &output, "head").unwrap();
    assert_eq!(result["standardBodyBones"], 2);
    assert_eq!(result["additionalBones"], 1);
    assert!(result["maxRestErrorMeters"].as_f64().unwrap() < 1e-6);
    let model =
        Model::read(&std::fs::read(output.join(result["models"][0].as_str().unwrap())).unwrap())
            .unwrap();
    assert_eq!(model.bones[1].rotation, native_rotation.to_array());
    assert_eq!(model.bones[2].parent, 1);
    assert_eq!(
        model
            .morphs
            .iter()
            .find(|m| m.name == "blink")
            .unwrap()
            .positions,
        model
            .morphs
            .iter()
            .find(|m| m.name == "eyeclose")
            .unwrap()
            .positions
    );
    assert_eq!(result["convertedNormalMaps"], 1);
    let bump = model.materials[0]
        .properties
        .iter()
        .find(|p| p["name"] == "_BumpMap")
        .unwrap();
    let encoded = secretary_avatar_com3d2::texture::Texture::read(
        &std::fs::read(output.join(format!(
            "{}.tex",
            bump["value"]["texture"].as_str().unwrap()
        )))
        .unwrap(),
    )
    .unwrap()
    .encoded;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(encoded))
        .read_info()
        .unwrap();
    let mut decoded = vec![0; decoder.output_buffer_size().unwrap()];
    decoder.next_frame(&mut decoded).unwrap();
    assert_eq!(decoded, [255, 175, 0, 250, 255, 135, 0, 30]);
    let (roundtrip, bytes) =
        glb_io::read(&output.join(result["render"][0].as_str().unwrap())).unwrap();
    let primitive = &roundtrip["meshes"][0]["primitives"][0];
    let read = |name: &str| {
        accessor::read(
            &roundtrip,
            &bytes,
            primitive["attributes"][name].as_u64().unwrap() as usize,
        )
        .unwrap()
    };
    let out_points = read("POSITION");
    let out_uv = read("TEXCOORD_0");
    for (i, p) in out_points.iter().enumerate() {
        let old = points
            .iter()
            .position(|v| Vec3::from_slice(v).distance(Vec3::from_slice(p)) < 1e-6)
            .unwrap();
        assert!(out_uv[i]
            .iter()
            .zip(&uvs[old])
            .all(|(a, b)| (a - b).abs() < 1e-6));
    }
    let image =
        &roundtrip["bufferViews"][roundtrip["images"][0]["bufferView"].as_u64().unwrap() as usize];
    let start = image["byteOffset"].as_u64().unwrap() as usize;
    assert_eq!(&bytes[start..start + png.len()], png);
    let normal = roundtrip["materials"][0]["normalTexture"]["index"]
        .as_u64()
        .unwrap() as usize;
    let image = roundtrip["textures"][normal]["source"].as_u64().unwrap() as usize;
    let view = &roundtrip["bufferViews"]
        [roundtrip["images"][image]["bufferView"].as_u64().unwrap() as usize];
    let begin = view["byteOffset"].as_u64().unwrap() as usize;
    assert_eq!(&bytes[begin..begin + png.len()], png);
    assert!(
        (roundtrip["materials"][0]["normalTexture"]["scale"]
            .as_f64()
            .unwrap()
            - 0.7)
            .abs()
            < 1e-6
    );
    for pointer in [
        "/pbrMetallicRoughness/metallicRoughnessTexture",
        "/occlusionTexture",
    ] {
        let info = roundtrip["materials"][0].pointer(pointer).unwrap();
        let texture = info["index"].as_u64().unwrap() as usize;
        let image = roundtrip["textures"][texture]["source"].as_u64().unwrap() as usize;
        let view = &roundtrip["bufferViews"]
            [roundtrip["images"][image]["bufferView"].as_u64().unwrap() as usize];
        let begin = view["byteOffset"].as_u64().unwrap() as usize;
        assert_eq!(&bytes[begin..begin + png.len()], png);
    }
    assert_eq!(
        roundtrip["materials"][0]["occlusionTexture"]["strength"],
        0.3
    );
    assert_eq!(
        result["unsupportedMaterialChannels"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    std::fs::remove_dir_all(dir).unwrap();
}
