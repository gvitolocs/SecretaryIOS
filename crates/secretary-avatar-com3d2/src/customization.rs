//! Native numeric shape curves, evaluated in the game's bone frames in Rust.
//! Whole-head dimensions preserve Lucina's identity. Native face-specific
//! topology controls are advertised as unavailable, never silently substituted.
use crate::{invalid, Result};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::OnceLock};
#[derive(Clone, Deserialize)]
pub struct Rule {
    pub kind: String,
    pub control: String,
    pub bone: String,
    pub min: [f32; 3],
    pub max: [f32; 3],
}
pub fn rules() -> &'static [Rule] {
    static R: OnceLock<Vec<Rule>> = OnceLock::new();
    R.get_or_init(|| {
        serde_json::from_str(include_str!("native_shape_rules.json"))
            .expect("compiled native rules")
    })
}
pub const KEYS: [&str; 21] = [
    "KubiScl",
    "UdeScl",
    "EyeSclX",
    "EyeSclY",
    "EyePosX",
    "EyePosY",
    "HeadX",
    "HeadY",
    "DouPer",
    "sintyou",
    "koshi",
    "kata",
    "west",
    "arml",
    "hara",
    "munes",
    "munetare",
    "regfat",
    "regmeet",
    "MuneYori",
    "MuneUpDown",
];
const LABELS: [&str; 21] = [
    "Neck length",
    "Arm length",
    "Eye width",
    "Eye height",
    "Eye spacing",
    "Eye position",
    "Head width",
    "Head height",
    "Torso / leg proportion",
    "Height",
    "Hip width",
    "Shoulder width",
    "Waist",
    "Arm fullness",
    "Abdomen",
    "Chest narrowness",
    "Chest droop",
    "Thigh fullness",
    "Lower leg fullness",
    "Chest inward",
    "Chest elevation",
];
pub fn default_value(key: &str) -> f32 {
    if matches!(
        key,
        "arml" | "hara" | "munes" | "munetare" | "regfat" | "regmeet"
    ) {
        0.
    } else {
        0.5
    }
}
pub fn values(settings: &BTreeMap<String, f32>) -> Result<[f32; 21]> {
    let mut out = [0.; 21];
    for (i, key) in KEYS.iter().enumerate() {
        out[i] = settings
            .get(*key)
            .copied()
            .unwrap_or_else(|| default_value(key));
    }
    for (k, v) in settings {
        if !KEYS.contains(&k.as_str()) || !v.is_finite() || !(0.0..=1.).contains(v) {
            return Err(invalid("unknown or out-of-range customization"));
        }
        if k.starts_with("Eye") {
            return Err(invalid("native eye controls require a compatible face rig"));
        }
    }
    Ok(out)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    pub key: &'static str,
    pub label: &'static str,
    pub default: f32,
    pub kind: &'static str,
    pub supported: bool,
    pub reason: &'static str,
}
pub fn controls() -> Vec<Control> {
    let mut out=KEYS.iter().enumerate().map(|(i,&key)|{let supported=!key.starts_with("Eye");Control{key,label:LABELS[i],default:default_value(key),kind:if i>=19{"spring"}else if (13..19).contains(&i){"morph"}else{"bone"},supported,reason:if supported{""}else{"Lucina's original face has no native Eyepos rig; an authored face mapping is required."}}}).collect::<Vec<_>>();
    for (key, label) in [
        ("NosePos", "Nose position"),
        ("NoseScl", "Nose size"),
        ("EarScl", "Ear size"),
        ("Earrot", "Ear rotation"),
        ("EarElf", "Elf ears"),
        ("FaceShape", "Face shape"),
        ("FaceShapeSlim", "Face slimness"),
        ("MayuX", "Eyebrow spacing"),
        ("MayuY", "Eyebrow height"),
        ("MayuRot", "Eyebrow rotation"),
        ("EyeBallSclX", "Iris width"),
        ("EyeBallSclY", "Iris height"),
    ] {
        out.push(Control{key,label,default:0.5,kind:"face",supported:false,reason:"Requires native face topology or a new authored Lucina morph; not available on this head."});
    }
    out
}
pub fn bone_names() -> &'static [String] {
    static N: OnceLock<Vec<String>> = OnceLock::new();
    N.get_or_init(|| {
        let mut names = rules().iter().map(|r| r.bone.clone()).collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    })
}
pub fn evaluate(
    bone: &str,
    local: Mat4,
    parent_correction: Mat4,
    correction: Mat4,
    values: &[f32; 21],
) -> Result<Mat4> {
    if !local.is_finite()
        || !parent_correction.is_finite()
        || !correction.is_finite()
        || parent_correction.determinant().abs() < 1e-8
        || correction.determinant().abs() < 1e-8
        || values
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.).contains(v))
    {
        return Err(invalid("shape frame or value"));
    }
    let (mut scale, mut position) = (Vec3::ONE, Vec3::ONE);
    for r in rules()
        .iter()
        .filter(|r| r.bone == bone && !r.control.starts_with("Eye"))
    {
        let i = KEYS
            .iter()
            .position(|&k| k == r.control)
            .ok_or_else(|| invalid("shape key"))?;
        let mut value = values[i];
        if r.control == "DouPer"
            && !bone.contains("Thigh")
            && !bone.contains("Calf")
            && !bone.contains("Foot")
            && !bone.starts_with("momotwist")
        {
            value = 1. - value;
        }
        if r.control == "EyePosX" {
            value *= 0.5 + values[5] * 0.5;
        }
        let baseline = Vec3::from_array(r.min).lerp(Vec3::from_array(r.max), 0.5);
        let factor = Vec3::from_array(r.min).lerp(Vec3::from_array(r.max), value) / baseline;
        if r.kind == "scale" {
            scale *= factor;
        } else {
            position *= factor;
        }
    }
    let mut native = parent_correction.inverse() * local * correction;
    native.x_axis *= scale.x;
    native.y_axis *= scale.y;
    native.z_axis *= scale.z;
    native.w_axis.x *= position.x;
    native.w_axis.y *= position.y;
    native.w_axis.z *= position.z;
    let out = parent_correction * native * correction.inverse();
    if !out.is_finite() {
        return Err(invalid("shape output"));
    }
    Ok(out)
}
/// Additional garment roots (not present in the body model) retain their own
/// native source frames. Calibrate after skeleton merge and before posing.
#[cfg(not(target_arch = "wasm32"))]
pub fn annotate_garment_bindings(path: &std::path::Path) -> Result<usize> {
    let (mut doc, data) = crate::glb_io::read(path)?;
    let (parents, _, world) = crate::compose::scene_frames(&doc)?;
    let mut updates = Vec::new();
    for (i, node) in doc["nodes"].as_array().unwrap().iter().enumerate() {
        let source = &node["extras"]["secretaryShapeSource"];
        if !source.is_object() {
            continue;
        }
        let name = node["name"].as_str().unwrap_or("");
        let Some(index) = bone_names().iter().position(|n| n == name) else {
            continue;
        };
        let matrix = |key: &str| -> Result<Mat4> {
            let a = source[key]
                .as_array()
                .filter(|a| a.len() == 16)
                .ok_or_else(|| invalid("garment shape frame"))?;
            let mut out = [0.; 16];
            for (k, v) in a.iter().enumerate() {
                out[k] = v.as_f64().ok_or_else(|| invalid("garment shape number"))? as f32;
            }
            let m = Mat4::from_cols_array(&out);
            if !m.is_finite() || m.determinant().abs() < 1e-8 {
                return Err(invalid("garment shape matrix"));
            }
            Ok(m)
        };
        let parent = if parents[i] >= 0 {
            world[parents[i] as usize]
        } else {
            Mat4::IDENTITY
        };
        let correction = world[i].inverse() * matrix("world")?;
        let parent_correction = parent.inverse() * matrix("parentWorld")?;
        updates.push((i,serde_json::json!({"index":index,"name":name,"correction":correction.to_cols_array(),"parentCorrection":parent_correction.to_cols_array()})));
    }
    for (i, value) in &updates {
        doc["nodes"][*i]["extras"]["secretaryShapeBinding"] = value.clone();
    }
    if !updates.is_empty() {
        crate::glb_io::write(path, &doc, &data)?;
    }
    Ok(updates.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;
    #[test]
    fn defaults_keep_animated_frames_without_accumulation() {
        let vals = values(&BTreeMap::new()).unwrap();
        let parent = Mat4::from_rotation_y(0.7);
        let c = Mat4::from_rotation_z(0.8);
        let local = Mat4::from_scale_rotation_translation(
            Vec3::new(1., 0.9, 1.1),
            Quat::from_rotation_x(0.2),
            Vec3::new(0.2, 0.3, 0.4),
        );
        for n in bone_names() {
            let m = evaluate(n, local, parent, c, &vals).unwrap();
            assert!(m.abs_diff_eq(local, 1e-5), "{n}");
        }
    }
    #[test]
    fn native_axes_ranges_and_lower_body_inversion_are_preserved() {
        let mut vals = values(&BTreeMap::new()).unwrap();
        vals[1] = 1.;
        let c = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let m = evaluate("Bip01 L UpperArm", Mat4::IDENTITY, Mat4::IDENTITY, c, &vals).unwrap();
        assert!((m.y_axis.length() - 1.15).abs() < 1e-5);
        vals[8] = 1.;
        let upper = evaluate(
            "Bip01 Spine0a",
            Mat4::from_translation(Vec3::X),
            Mat4::IDENTITY,
            Mat4::IDENTITY,
            &vals,
        )
        .unwrap();
        let lower = evaluate(
            "Bip01 L Calf",
            Mat4::from_translation(Vec3::X),
            Mat4::IDENTITY,
            Mat4::IDENTITY,
            &vals,
        )
        .unwrap();
        assert!((upper.w_axis.x - 0.88).abs() < 1e-5);
        assert!((lower.w_axis.x - 1.13).abs() < 1e-5);
    }
    #[test]
    fn stale_keys_unsupported_faces_and_nonfinite_values_are_rejected() {
        for (k, v) in [
            ("invented", 0.5),
            ("EyeSclX", 0.5),
            ("arml", f32::NAN),
            ("regfat", 1.1),
        ] {
            assert!(values(&BTreeMap::from([(k.into(), v)])).is_err());
        }
    }
}
#[cfg(all(test, not(target_arch = "wasm32")))]
mod garment_tests {
    use super::*;
    #[test]
    fn additional_skirt_root_is_calibrated_after_merge_and_changes_with_hip_size() {
        let parent = Mat4::from_rotation_y(0.3);
        let local = Mat4::from_translation(Vec3::new(0., 0.9, 0.));
        let source_parent = Mat4::from_rotation_x(0.2);
        let source = source_parent * local;
        let doc = serde_json::json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"name":"NativePelvis","matrix":parent.to_cols_array(),"children":[1]},{"name":"Skirt","matrix":local.to_cols_array(),"extras":{"secretaryShapeSource":{"world":source.to_cols_array(),"parentWorld":source_parent.to_cols_array()}}}],"buffers":[{"byteLength":0}]});
        let path = std::env::temp_dir().join(format!("garment-shape-{}.glb", std::process::id()));
        crate::glb_io::write(&path, &doc, &[]).unwrap();
        assert_eq!(annotate_garment_bindings(&path).unwrap(), 1);
        let (d, _) = crate::glb_io::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let b = &d["nodes"][1]["extras"]["secretaryShapeBinding"];
        let matrix = |key: &str| {
            Mat4::from_cols_array(&std::array::from_fn(|i| b[key][i].as_f64().unwrap() as f32))
        };
        let mut values = values(&BTreeMap::new()).unwrap();
        let neutral = evaluate(
            "Skirt",
            local,
            matrix("parentCorrection"),
            matrix("correction"),
            &values,
        )
        .unwrap();
        assert!(neutral.abs_diff_eq(local, 1e-5));
        values[10] = 1.;
        let large = evaluate(
            "Skirt",
            local,
            matrix("parentCorrection"),
            matrix("correction"),
            &values,
        )
        .unwrap();
        assert!(!large.abs_diff_eq(neutral, 0.01));
        assert!(large.is_finite());
    }
}
