# Secretary native COM3D2 avatar compatibility

This crate implements the first native asset and avatar layer in Rust. It is **not a complete COM3D2 engine port**. The private Lucina review combines the installed game's original body and clothing with her approved real head, facial expressions, hair and tiara. No owned assets, game assemblies, extracted source, or credentials belong in this crate or Git.

## Implemented

- Lazy, bounded, unencrypted `warc` indexing and extraction. Encrypted `warp` archives are refused.
- Native `.model` versions 1000–2001, bone hierarchy, scale helpers, skinning, material descriptions and sparse morphs. GLB export bakes the model root into geometry and corrects inverse binds; it does not substitute a generic torso.
- Native PNG/JPEG `.tex` containers and pure Rust PNG/JPEG/WebP skin processing; unsupported encodings return an error.
- Inert `.menu` parsing and a private inventory by category. Parsing does not execute scripts or establish outfit compatibility.
- Body visibility rules: substring selection with descendants, ordered hide/show overrides, and triangle removal when a skin influence references a hidden joint. Item masks and rules affecting other garments remain in inventory metadata; they are not silently claimed as implemented.
- Wearable skeleton merging, preservation of item-only attachment joints, rewritten inverse binds for different bone frames, and resource-index remapping.
- Native `.anm` rotation/translation records, including repeated records for a bone; static pose sampling and a mesh-free 30 Hz animation GLB. Source Hermite curves are sampled; this is not lossless curve interchange.
- Native `.phy` parameters and curves. This parser is separate from the body-spring solver.
- A Rust body-spring implementation based on the observed installed game behavior, with size/softness/support controls, fixed reference ticks, axis corrections, teleport resets and a small WASM interface. This is not a claim of full Unity/DynamicBone parity.
- A loopback-only private review server and a rendering adapter to the existing Three.js bundle. New parsing, fitting transforms, native animation export and body dynamics run in Rust; no Node process is required.
- Native `.mate` overrides and archive overlay resolution. Legacy Secretary garments and NTE pieces are transferred onto the native rig, retaining garment-only meshes and mapped size targets; donor heads and bodies are excluded.
- Native `.model`/`.mate`/`.tex`/`.menu` writing and round-trip validation. The fitter now loads its generated native models back through the Rust reader before composition. Standard body bone frames are authoritative; additional authored face/accessory joints keep their world rest frames. Native exports are private files, not bundled game assets.
- Standalone Lucina head export, keeping her real face, eyes, mouth, neck connector and all 20 expressions. Hair/tiara remain separate slots. Four native expression aliases are provided; `mouths` and `mouthc` are phoneme approximations, not exact equivalents.
- Normal maps retained as original RGB `.tex` resources for the Rust viewer, with an additional desktop AG-packed native `_BumpMap`. This follows Unity's [normal unpacking conventions](https://github.com/Unity-Technologies/Graphics/blob/master/Packages/com.unity.render-pipelines.core/ShaderLibrary/Packing.hlsl); the installed game's shader variant has not been verified. Other authored material textures, including metallic/roughness, are retained in native texture resources and remapped for the Rust viewer. Unmapped native shader channels are explicitly reported instead of silently dropped.
- One shared `skinColor` setting across face, neck, body and tagged garment skin. Source albedo is calibrated to a neutral detail map; original maps remain embedded. The separate safe-body patch uses a neutral texel because its UVs differ from the main skin atlas. Native source shadow detail is retained at a restrained blend. This does not add anatomical geometry absent from the safe source model.
- A searchable modular picker with separate clothing, shoes, stockings, gloves, accessories and hair categories, hairstyle and hair-color controls, atomic settings persistence, explicit failed-import states, and lazy cached composition in Rust.

## Measured private checks, 2026-10-27

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
| Legacy native packs | 99: 82 NTE pieces, five hairstyles and 12 original Secretary outfits; one `.model` per piece, with `.mate`, `.tex` and `.menu` files |
| Native pack rest-position round trip | Maximum 1.686e-7 m across the 99 converted pieces; this measures serialization, not garment clearance |
| Standalone native head | 2,989 vertices; 145 standard bones plus 21 authored additional bones; 20 original expressions plus four aliases; maximum rest error 1.49e-8 m |
| Legacy material detail | Normal maps retained on Casual, Chef, Crimson, Gym and Maid; authored metallic/roughness maps are also preserved; their native shader mapping remains explicitly unsupported |

Akane's rear fit was compared with the original static reference in Blender and then checked in the native round-trip preview. The correction raises pelvis-anchored skirt/belt pieces by 0.10 m, with a smooth vertical falloff, without lifting leg or hand attachments. A rear surface envelope clears penetrating fabric. The private preview uses the native canonical body/head and refreshed native idle/bone bindings. Representative outfit and hairstyle changes were checked in-browser. This does not establish universal wardrobe compatibility or native-game load compatibility.

The combined library indexes 1,499 entries: 1,399 native COM3D2 entries, 82 existing NTE pieces, five existing hairstyles and 13 original Secretary outfits. The import audit prepared 1,473 entries and assembled 26 of 27 complete looks. The 26 failures comprise 15 native material-slot mismatches, 10 unavailable native model references and one original outfit without separately identified clothing surfaces. Failed selections are disabled and their errors remain in private reports. The legacy fitter preserves tangent coordinates and uses the native front-surface envelope to clear penetrating torso fabric; body and garment receive corresponding native size deltas and spring-joint weights. Representative old school and NTE tops were rendered again after this correction. Slider endpoints are fitted against their corresponding native body surfaces, and front torso fabric uses the corresponding native skin weights even where its size delta is zero. Akane’s top has a bounded per-item vertical fitting profile; the browser was checked at 0% and 100%, including an impulse test with physics active. This profile is garment data, and does not alter the canonical body. Fitting-profile changes invalidate both prepared-part and composed-look caches. Import success is not proof of final fit: NTE categories inherited from the old fitting study are approximate, and every imported fit still requires clearance review through motion and body-size ranges.

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
unify-skin INPUT_GLB OUTPUT_GLB [SOURCE_SHADOW_TEX]
retarget-legacy INPUT_GLB CANONICAL_BIND_GLB OUTPUT_GLB [hair]
export-native-pack INPUT_GLB NATIVE_BODY_MODEL OUTPUT_DIRECTORY CATEGORY
export-native-head INPUT_GLB NATIVE_BODY_MODEL OUTPUT_DIRECTORY
glb-summary INPUT_GLB
library-init CANONICAL_BIND_GLB BODY_MODEL ANM OLD_LIBRARY PARTS_ARC PARTS2_ARC OUTPUT_DIRECTORY [ORIGINAL_LIBRARY]
library-compose PRIVATE_LIBRARY_DIRECTORY COMMA_SEPARATED_ITEM_IDS
library-warm PRIVATE_LIBRARY_DIRECTORY [all]
serve PRIVATE_REVIEW_DIRECTORY EXISTING_STAGE_BUNDLE
```

`attach`, `mask-body`, `body-bindings`, and `animate-asset` take **bind-pose** assets. Apply the standing pose after all composition. Never merge a posed wearable against a bind-pose body. The library preview exposes redacted catalog metadata, validated settings and composition APIs, strict derived `cache/view-<hex>.glb` paths and the explicit viewer resources. Archives, source paths, part caches and private configuration are not served. It listens only on `127.0.0.1:28983`, checks Host and mutation origin, rejects traversal/unknown paths and sends no-store headers.

The small JS adapter only exchanges bone matrices with WASM and renders them. The native idle supplies fresh animation poses before the solver each frame. Body and coat use the same native `munel` size target and shared physics joints; no separate fake body-fill mesh is added.

## Remaining port work

- Full native toon shader/normal/tangent/material semantics and advanced texture formats.
- Load testing generated packs inside the installed COM3D2 game. Manifests explicitly report `installedGameLoadVerified: false` and `shaderParity: false`. Successful Rust parsing and visual review are not evidence of engine parity.
- Complete native DynamicBone parity, calibrated per-item collider profiles and mesh cloth/self-collision; the new secondary solver provides bounded bone-chain capsule contacts.
- Full bone morph/slider definitions and all apparel, hair and accessory compatibility, including layered garment rules and calibrated foot/heel poses.
- Automated clearance checks over the full wardrobe and all native body settings, plus independent reference-engine motion comparisons.
- Secretary production API/settings and Android/iOS UI integration. This is a private review, not a deployment or app reinstall.
- Game systems beyond avatars, including scene/event/script/UI/audio/save compatibility. None is represented as complete.

## Format provenance and attribution

The implementation was checked against the user's locally installed game data and targeted technical inspection of its own assemblies. Owned files and diagnostic decompilations stay outside the repository. The latest installed 2.47.3 body-spring implementation was compared with the base assembly; core coefficient/force behavior matched, while reset/scale handling differed. The Rust implementation uses fresh frame input and does not accumulate scale.

Format research also used [MeidoSerialization](https://github.com/MeidoPromotionAssociation/MeidoSerialization), commit `b9b3e0e9cb0d23f1eb92da14ae31198e9c3bd057`. Its BSD-3-Clause copyright and license are preserved in `THIRD_PARTY_NOTICES`. No Go runtime is used.

The skin-preview step removes unlit shading from the joined skin and neck adapter, while retaining all original texture pixels and the unlit eye/lash materials. This reduces the lighting discontinuity at the neck.

Forty-one Rust regression tests pass, covering malformed GLBs, archive bounds/path safety, repeated animation records, rest-frame retargeting, wearable inverse binds, body masks, curve interpolation, body-spring stability at 30/60/120 Hz, child-joint radians/frame corrections, shared skin calibration, safe patch color, unchanged hair, pure Rust meshopt decoding, pelvis-only waist correction, bounded native serialization, U16 partitioning and an end-to-end native head/material round trip. That round trip checks native bone frames, UVs, blink aliases, exact source RGB normal pixels, desktop AG encoding and remapped metallic/roughness/occlusion resources. Clippy passes with warnings denied; host and WASM release builds pass.

Original Secretary GLBs require meshopt decoding, including virtual fallback buffers. `meshoptimizer-rs` 0.2.4 is built with only its scalar `std` feature; no C/C++ FFI or native meshoptimizer dependency is introduced. See its [upstream implementation](https://github.com/Tavrin/meshoptimizer-rs).

Tests use synthetic fixtures. Real owned-file checks are opt-in manual CLI audits with private paths; game assets must never become public test fixtures.

## Lightweight secondary-motion research and implementation, 2026-10-27

[Godot SpringBoneSimulator3D](https://docs.godotengine.org/en/stable/classes/class_springbonesimulator3d.html) specifically targets hair, cloth and tails, using per-joint stiffness, drag, gravity and separate simple colliders. [Godot's interpolation guide](https://docs.godotengine.org/en/stable/tutorials/physics/interpolation/physics_interpolation_introduction.html) separates fixed simulation ticks from rendering; [its CPU optimization guide](https://docs.godotengine.org/en/stable/tutorials/performance/cpu_optimization.html) recommends profiling actual bottlenecks and repeated measurements. These are useful design references; Godot's engine is not an added runtime dependency.

| Candidate | Fit for this avatar | Decision for this iteration |
|---|---|---|
| [vrm-runtime-rs](https://github.com/nanowater/vrm-runtime-rs) | Renderer-independent Rust VRM runtime with authored spring chains and sphere/capsule collision; published 0.1.0 | Closest Rust reference, but adopting the whole loader/runtime would require a COM3D2-to-VRM asset adapter and independent compatibility checks. No dependency added. |
| [Rapier](https://github.com/dimforge/rapier) | Rust rigid-body simulation and general collision worlds | Useful for future interactive physical props. A rigid-body world alone does not supply garment rigging or cloth behavior. No dependency added. |
| [Avian](https://github.com/avianphysics/avian) | ECS physics integrated with Bevy | Useful if the renderer migrates to Bevy; not selected for the current standalone Rust/WASM bridge. No Bevy runtime added. |
| Current bounded Rust secondary solver | Existing authored hair joints and generated loose garment panels, capsule contacts, pinned roots, gravity, damping and angle/length limits | Integrated and measured. This is not a claim of being faster than the alternatives; no equivalent end-to-end comparative engine benchmark was run. |

The implementation uses a fixed 60 Hz simulation with interpolated output, four constraint projection passes, no wind oscillator, teleport reset, bounded joint/collider counts, and fixed-stack capsule storage in the WASM bridge. It skips secondary work when the document is hidden. Generated skirt/dress panels keep the waist pinned and retain the exact bind shape. Eligible original skirt looks and five interchangeable hairstyles receive generated secondary joints; authored default Lucina hair joints remain separate. Shoes, hands and the humanoid skeleton are not simulated as soft cloth. These are coarse bone panels, not a mesh cloth/self-collision system. Original Gym and Crimson are not automatically classified as skirt garments.

Reproduce the CPU-only benchmark with `cargo run -p secretary-avatar-com3d2 --release --example secondary_bench`. On nezopt's AMD Ryzen 5 7600, 1,000 measured frames after 300 warm-up frames with eight capsule checks per joint gave:

| Joints | Mean/frame | P95/frame | Maximum/frame |
|---|---:|---:|---:|
| 22 | 0.006408 ms | 0.006392 ms | 0.037802 ms |
| 128 | 0.047322 ms | 0.050416 ms | 0.057249 ms |

This synthetic benchmark excludes rendering, browser/WASM boundary costs, animation evaluation and asset loading. The live browser timing includes adapter overhead and varies with device and scene. It is not an Android benchmark. Regression tests cover impulse decay without wind, collision contact, teleport reset, tick consistency at 30/60/120 render rates, malformed inputs, and unchanged skirt bind geometry/waist pinning.

The native round-trip can rename and merge the canonical body mesh. Clothing visibility therefore also identifies `skin_safe` body primitives, preserving the real head/hair primitives in the same mesh. Generated skirt/dress menus carry hip hiding rules, and cached menu records are refreshed independently of geometry. The masking regression covers a renamed combined model and verifies that the face primitive remains untouched. This visibility rule is not proof of geometric clearance across all animations.

Eye-white correction: Lucina's `FaceColor` primitive contains sclera, teeth and mouth features, while her actual face is `FaceSkin`. Only the latter joins the shared skin uniform. The original feature texture is restored from the pre-native source, rendered without a skin multiplier, and retained through native export. A migration/idempotence regression checks white pixels, material separation and unchanged skin binding.

Gravity follow-up: the former world-down leaf-axis fallback could produce no gravitational torque on floating hair geometry. The Rust composer now derives each participating joint's lever arm from the vertices it actually skins, records it in bone-local coordinates and passes gravity/stiffness/damping profiles to WASM. The placeholder axis is removed. Gravity uses 9.81 m/s² and can be toggled independently for an in-place comparison; the preview reports measured vertical tip displacement rather than only an enabled flag. New regressions verify a lateral mesh-derived axis and a horizontal strand dropping under gravity without an impulse, with no drop at zero gravity. This does not introduce mesh cloth or full native-game solver parity.
Gravity and tone review — 2026-10-27

The previous visible tip-drop reading was not proof of natural whole-mesh motion.
Blender inspection found two main back-hair joints, with no distal bend. Rust now
adds a lower bend with smooth weights while retaining the original bind surface,
face/crown weights, UVs and real Lucina assets. A regression checks unchanged bind
geometry, pinned upper hair, normalized weights and repeated processing.

The Rust secondary solver now checks intermediate shaft samples against body
capsules, in addition to tips. Root position and strand length remain constrained.
Loose garment limits were relaxed from 0.22 to 0.85 radians (stiffness 90 to 35).
This is still a coarse bone solver, not mesh fabric self-collision.

Long hanging components can be selected without softening nearby disconnected
bags. Selection welds coincident UV/normal seam points for classification only;
render topology is retained. Visual QA rejected a broad geometric guess that softened a stocking. The preview instead explicitly selects the two 236-vertex ribbon components in Akane's mixed stocking item; the 2678-vertex stocking component is excluded. Unreviewed items remain unchanged. Each reviewed ribbon uses three connected joints, with its upper edge pinned. These chains are authored after baking the actual standing pose into that wearable; inverse binds, normals and morph directions are updated together. A posed-surface regression verifies unchanged world geometry, UVs and morph vectors. The configured opt-ins are
included in the composed-cache key.

Skin calibration now normalizes the different source face/body albedo baselines
in linear light. The selected skin color remains shared and unchanged. Neutral
fitting-room lights replace warm/cool lighting. Eye whites and face features are
excluded from the skin recoloring. Controls can be hidden to inspect the model.

Verification: 41 Rust tests passed, including native asset round trips, bind-shape
preservation, seam-aware ribbon selection, pinned skirt waist, shaft contacts,
frame-rate stability, skin matching and unchanged eye pixels. Clippy passed with
warnings denied. Host/WASM builds passed. New CPU-only solver benchmark on nezopt:
22 joints / 8 capsules: mean 0.012235 ms, p95 0.012754 ms;
128 joints / 8 capsules: mean 0.072468 ms, p95 0.076435 ms.
This excludes browser bridge, rendering and all other avatar work.

Scope: private preview, not a production release or complete COM3D2 engine port.
Full fabric self-collision, native shader parity and individual outfit fitting
remain unfinished. The Akane upper-back fit still has visible artifacts.

## Current focused handoff — 2026-10-28

The active branch is `codex/lucina-gravity` in an isolated worktree directly on
nezopt. NTE garments and the rejected bob are deferred. The native coat now uses
its authored 24×5 lattice and parsed `.psk` profile in a coupled Rust XPBD sheet;
Lucina's original Uniform retains its authored open 3×4 cape. Original hems can
receive a 12×4 sheet, excluding boots, gloves, sleeves and source cape controls.
Legacy segment mappings include the original arm/leg/spine helper joints.

The native customization schema exposes 17 working native shape/spring controls
in addition to existing size, softness and colors. Generated `_SCL_` bindings
are included. Native face-topology controls without Lucina mappings are listed
as unavailable. Neutral shape values preserve the fitted bind baseline.
Bindings schema is version 2; pack version stays 7. Current cache revisions are
fit12/native27 and import reports carry pipeline revision 27.

Validation: 56 Rust tests passed; Clippy with warnings denied and host/WASM
release builds passed. Focused preparation/composition succeeded for 15 pieces
and 14 presets. This is conversion evidence, not certification of every fit.
Native CPU cloth benchmark (120 nodes, 18 iterations, 8 capsules, 3600 frames):
median 0.188 ms, p95 0.214 ms, p99 2.495 ms; process peak RSS 2688 KiB. These
figures exclude rendering, the browser bridge and asset loading.

Production remains unchanged. See
[the Claude integration handoff](../../docs/handoffs/LUCINA_COM3D2_CLAUDE.md)
for private asset locations, the WASM contract, integration order and release
gates. Full COM3D2 game/Unity conversion, native shader/cloth parity, fabric
self-collision and universal garment fitting are not claimed.
