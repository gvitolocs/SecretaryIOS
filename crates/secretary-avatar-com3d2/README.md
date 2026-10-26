# Secretary native COM3D2 avatar compatibility

This crate implements the first native asset and avatar layer in Rust. It is **not a complete COM3D2 engine port**. The private Lucina review combines the installed game's original body and clothing with her approved real head, facial expressions, hair and tiara. No owned assets, game assemblies, extracted source, or credentials belong in this crate or Git.

## Implemented

- Lazy, bounded, unencrypted `warc` indexing and extraction. Encrypted `warp` archives are refused.
- Native `.model` versions 1000–2001, bone hierarchy, scale helpers, skinning, material descriptions and sparse morphs. GLB export bakes the model root into geometry and corrects inverse binds; it does not substitute a generic torso.
- Native PNG/JPEG `.tex` containers; unsupported texture encodings return an error.
- Inert `.menu` parsing and a private inventory by category. Parsing does not execute scripts or establish outfit compatibility.
- Body visibility rules: substring selection with descendants, ordered hide/show overrides, and triangle removal when a skin influence references a hidden joint. Item masks and rules affecting other garments remain in inventory metadata; they are not silently claimed as implemented.
- Wearable skeleton merging, preservation of item-only attachment joints, rewritten inverse binds for different bone frames, and resource-index remapping.
- Native `.anm` rotation/translation records, including repeated records for a bone; static pose sampling and a mesh-free 30 Hz animation GLB. Source Hermite curves are sampled; this is not lossless curve interchange.
- Native `.phy` parameters and curves. This parser is separate from the body-spring solver.
- A Rust body-spring implementation based on the observed installed game behavior, with size/softness/support controls, fixed reference ticks, axis corrections, teleport resets and a small WASM interface. This is not a claim of full Unity/DynamicBone parity.
- A loopback-only private review server and a rendering adapter to the existing Three.js bundle. New parsing, fitting transforms, native animation export and body dynamics run in Rust; no Node process is required.

## Measured private checks, 2026-10-26

| Check | Result |
|---|---|
| Installed native models in parts + parts2 | 523/523 parsed; 1,579,514 vertices |
| Native menu records in parts | 2,002/2,002 parsed; categorized with source paths |
| Native body | 6,638 source vertices, 145 source bones, 12 scale helpers, 7 body morphs |
| Real Lucina head attachment | 20 facial expressions retained, 22-edge approved head neck port bridged to 32-edge native body cut; 54 bridge faces |
| Source bind frame residual | approximately 2.02e-6 matrix units; approximately 6.31e-7 m maximum rest skin displacement |
| Native standing idle | 2.667 seconds, 81 sampled frames, 145 mapped bones |
| Long coat body masking | 3,222 body triangles removed according to its native menu |
| Noir boots foot masking | 1,810 further body triangles removed; original feet do not remain inside the boots |
| Physics browser test | Rust WASM active; impulse response measured in the live preview; typically under 1 ms on the review Mac, not a cross-device benchmark |

The long coat and boots are the first visually reviewed native outfit. Other inventory entries are explicitly `reviewed:false`, `runtimeReady:false`. The old NTE fitting study is retained separately; NTE garments have not all been refitted to this new body.

## Build and review

From the workspace root:

```sh
cargo test -p secretary-avatar-com3d2
cargo build -p secretary-avatar-com3d2 --release
cargo build -p secretary-avatar-com3d2 --lib --target wasm32-unknown-unknown --release
```

`target/release/com3d2-native` commands:

```text
index ARCHIVE
extract ARCHIVE ENTRY OUTPUT
model-inspect MODEL
model-json MODEL OUTPUT_JSON
bind-audit MODEL
audit-models ARCHIVE
export-model ARCHIVE ENTRY OUTPUT_GLB
menu-inspect MENU
physics-inspect PHY
animation-inspect ANM
animation-json ANM OUTPUT_JSON
catalog ARCHIVE OUTPUT_JSON
mask-body CANONICAL_BIND_GLB MENU OUTPUT_GLB
attach BASE_BIND_GLB WEARABLE_BIND_GLB OUTPUT_GLB
pose-asset NATIVE_BODY_MODEL ANM BIND_GLB OUTPUT_GLB
animate-asset NATIVE_BODY_MODEL ANM BIND_GLB OUTPUT_CLIP_GLB
body-bindings NATIVE_BODY_MODEL CANONICAL_BIND_GLB OUTPUT_JSON
skin-preview INPUT_GLB OUTPUT_GLB
serve PRIVATE_REVIEW_DIRECTORY EXISTING_STAGE_BUNDLE
```

`attach`, `mask-body`, `body-bindings`, and `animate-asset` take **bind-pose** assets. Apply the standing pose after all composition. Never merge a posed wearable against a bind-pose body. The preview reads exactly `native-dressed.glb`, `native-body.glb`, `native-idle.glb`, `camera.json`, `bindings.json`, `body.wasm` from the private directory; the existing stage bundle is a separate explicit argument. It listens only on `127.0.0.1:28983`, checks Host, rejects traversal/unknown paths and sends no-store headers.

The small JS adapter only exchanges bone matrices with WASM and renders them. The native idle supplies fresh animation poses before the solver each frame. Body and coat use the same native `munel` size target and shared physics joints; no separate fake body-fill mesh is added.

## Remaining port work

- Full native toon shader/normal/tangent/material semantics, `.mate` override application and advanced texture formats.
- Complete DynamicBone hair/cloth simulation and its colliders; body springs alone do not provide cloth collision.
- Full bone morph/slider definitions and all apparel, hair and accessory compatibility, including layered garment rules and calibrated foot/heel poses.
- Automated clearance checks over the full wardrobe and all native body settings, plus independent reference-engine motion comparisons.
- Secretary production API/settings and Android/iOS UI integration. This is a private review, not a deployment or app reinstall.
- Game systems beyond avatars, including scene/event/script/UI/audio/save compatibility. None is represented as complete.

## Format provenance and attribution

The implementation was checked against the user's locally installed game data and targeted technical inspection of its own assemblies. Owned files and diagnostic decompilations stay outside the repository. The latest installed 2.47.3 body-spring implementation was compared with the base assembly; core coefficient/force behavior matched, while reset/scale handling differed. The Rust implementation uses fresh frame input and does not accumulate scale.

Format research also used [MeidoSerialization](https://github.com/MeidoPromotionAssociation/MeidoSerialization), commit `b9b3e0e9cb0d23f1eb92da14ae31198e9c3bd057`. Its BSD-3-Clause copyright and license are preserved in `THIRD_PARTY_NOTICES`. No Go runtime is used.

The skin-preview step removes unlit shading from the joined skin and neck adapter, while retaining all original texture pixels and the unlit eye/lash materials. This reduces the lighting discontinuity at the neck.

Thirteen Rust regression tests pass, covering malformed GLBs, archive bounds/path safety, repeated animation records, rest-frame retargeting, wearable inverse binds, body masks, curve interpolation and body-spring stability at 30/60/120 Hz. Clippy passes with warnings denied.

Tests use synthetic fixtures. Real owned-file checks are opt-in manual CLI audits with private paths; game assets must never become public test fixtures.
