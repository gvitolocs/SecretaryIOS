# Claude handoff — Lucina native wardrobe and physics

Updated 2026-10-28. Giuseppe requested this handoff for integration into the production Secretary website and Android app. Production's existing Secretary layout and Lucina identity are the visual reference. This is a Rust avatar/asset compatibility implementation and private fitting preview, **not a complete port of the COM3D2 game/Unity engine, nor approval to publish every imported fit**.

## Checkout and integration boundary

Work directly on nezopt using SSH. The active worktree is `/home/nez/Projects/.worktrees/SecretaryIOS/wip-com3d2-20261026`, branch `codex/lucina-gravity`. Base is main commit `6c86dbbb4453bfb9430394a97c7672c808eda140`. The final checkpoint hash will be recorded in `COM3D2_CHECKPOINT.txt` beside this file.

Main `/home/nez/Projects/SecretaryIOS` was left outside this work. At verification its only modifications were the user's existing `crates/secretary-mcp/src/config.rs` and `deploy/secretary-mcp.env.example`; do not reset them. The worktree has its own Cargo target directory. Migration backup: `/home/nez/.local/state/lucina-nte-preview/worktree-migration-20261027/`.

Owned source changes are the new `crates/secretary-avatar-com3d2` crate, its workspace member/dependencies in root Cargo files, and this handoff. No website/app production renderer, deployment, phone service, Telegram/PDF logic or authentication integration was changed in this iteration. Integrate this work deliberately into the existing renderer; replacing the production UI with the fitting-room HTML is not intended.

## Working implementation

- Rust readers/writers for native archives, models, materials, textures, inert menus, animation and physics records; pure-Rust meshopt decoding. Native `.model/.mate/.tex/.menu` packs round-trip the old garments and real Lucina head, retaining UVs, morphs, source material channels and rig corrections. Pack version is **7**. It is not shader/game-solver parity.
- Canonical native body plus Lucina's actual original head and hair. Shared linear-light skin calibration uses one color for face/neck/body, preserving source shading. Eye whites/teeth/features remain separate from skin tint. Do not replace these with AI artwork or recolor `FaceColor` as skin.
- Body spring solver in Rust/WASM, and fixed-tick hair motion with body capsules, intermediate shaft contacts and an additional distal hair bend. Full matrix outputs preserve scaled/sheared parent frames. There is no synthetic wind driving idle motion.
- Native coat cloth uses the authored **24 × 5** skirt lattice (120 nodes), with coupled XPBD stretch/bending constraints, pinned upper row, gravity, capsule contact and a floor. Rust parses `.psk` values/curves and retains the original profile in metadata. The native source's per-step gravity numbers are not mislabeled as SI accelerations; the new solver uses 9.81 m/s². `nativeSolverParity` remains false.
- Original Uniform now imports its real `Dress` material. Previously it was wrongly excluded. Source arm/leg/spine helper controls now use the corresponding humanoid segment transforms. Original cape controls survive native conversion and form an **open 3 × 4** sheet. Closed Uniform body coverage is recorded as native menu visibility rules, with neck restored; the real head is preserved.
- Original complete garments no longer make boots/gloves/sleeves into waist pendulums because of their low/crouched bind pose. Eligible pelvis-weighted free hems can receive a **12 × 4** coupled sheet. Sparse/open pieces retain their existing rig and require explicit authoring. Shoes, limb-weighted geometry, authored capes/belts and rigid accessory materials are excluded. This inference is not a guarantee of every fit.
- Upper-back garment contact proxies help the original hair stay outside high collars. The preview's `Test motion` applies bounded impulses; normal idle has no injected impulse.

## Scope and catalog

The current private catalog exposes **1416 pieces and 14 looks**. Focused warm verification covers **15 individual pieces and all 14 presets** (one native coat/boots look and 13 original Secretary looks). `ready` means conversion/composition succeeded, not visual certification of every animation or size. Every imported piece retains `fitReviewRequired`.

NTE is deliberately deferred for gravity-oriented re-authoring. Keep its files, but do not re-enable those garments or the rejected bob hairstyle merely to fill the catalog. Current config has `deferred_origins=["NTE"]`, `deferred_items=["legacy-hair-bob"]` and no loose NTE component opt-ins. Other imported replacement hair is not certified by this pass. Original Lucina hair is the reviewed baseline.

## Native settings

`bindings.json` is **version 2**, containing `customization` version 1. The catalog has 33 native control/capability entries: **17 working native controls**, with 16 native face controls explicitly unavailable on Lucina's original topology/rig. Existing chest size, softness, skin and hair colors remain available separately.

Working controls: neck/arm length, head width/height, torso/leg proportion, height, hip/shoulder/waist proportions; native `arml`, `hara`, `munes`, `munetare`, `regfat`, `regmeet` shape blends; inward/elevation body-spring controls. Garment support is also exposed. Scale-helper bindings are generated explicitly even though `_SCL_` nodes are generated by the exporter rather than explicit source `.model` bones; there are **27** current body shape bindings.

`customization.rs` and `native_shape_rules.json` evaluate native numeric curves in the original bone frames. Curves are normalized to the fitted bind baseline so neutral controls preserve existing geometry. Upper/lower torso-proportion inversion is retained. Do not implement these as arbitrary world-axis JS scales. Existing animation is the input to each shape frame, avoiding accumulated deformation.

Native eye/nose/ear/brow/face-topology settings need new authored Lucina mappings/morphs. They are listed with reasons, not fake active sliders. Native body/garment morphs apply where matching targets exist; old imported garments do not automatically acquire every native body morph. Extreme settings still need fit checks. Facial expression/blink assets are retained independently of unavailable native facial customization.

Settings schema (serde defaults keep older saved settings readable):

```json
{
  "skinColor": "#ffd9c2",
  "size": 0.41,
  "softness": 0.5,
  "physics": true,
  "hairColor": null,
  "items": ["original-uniform"],
  "customization": {"koshi": 0.5, "MuneYori": 0.5},
  "supported": true,
  "secondary": true,
  "gravity": true
}
```

Values are bounded in Rust; unknown customization keys and unavailable face values are rejected. Writes are atomic and mode 0600 in the private preview. Catalog/import reports use pipeline revision **27**, legacy fit cache **12**, composed cache **native27**. Stale failure reports from the older Uniform importer are ignored.

## Private assets and running preview

Root: `/home/nez/.local/state/lucina-nte-preview/wardrobe-native-v3`.

- `library-config.json` contains private source/archive paths and current exclusions.
- Canonical bind asset: `native-canonical-tone-v1/render/part-0000.glb`.
- `bindings.json`, `native-idle.glb`, `camera.json`, `body.wasm` belong to that canonical generation. Move/version these **together**.
- `native/` contains generated native-format packs and render GLBs, `parts/` the converted source pieces, `cache/view-*.glb` the final composed models.
- `avatar-settings.json` contains current private fitting preferences. Preserve Giuseppe's choices rather than overwriting with sample JSON.
- `focused-warm-report-20261028.json`, `asset-import-report.json`, `preset-import-report.json` contain actual conversion results.
- `cloth-benchmark-20261028.json` and `cloth-benchmark-memory-20261028.txt` contain solver measurement evidence.
- Inspection/source profile: `/home/nez/.local/state/lucina-nte-preview/com3d2-inspection-20261025`; real original GLBs: `/home/nez/.local/state/lucina-avatar-repair/original/`.

The user service is `lucina-native-avatar-preview.service`; its worktree override is `/home/nez/.config/systemd/user/lucina-native-avatar-preview.service.d/20-worktree.conf`. It binds **127.0.0.1:28983**. The Mac only displays the SSH-forwarded preview. Keep asset processing/builds on nezopt; do not crawl SMB. Preview process is a private fitting server, not a public production API.

The renderer bundle used for review is `packages/lucina-stage/dist/lucina-stage.standalone.js` in the worktree. It is an existing build artifact; no Node runtime was added. Game archives, proprietary binaries and licensed model files are not committed as source/test fixtures. Keep private assets out of public bundles until the actual distribution scope is resolved.

## Renderer integration contract

Read `ui/wardrobe.html` as the thin Three.js integration example and `src/wasm.rs` as the authoritative ABI. New parsing, calibration, conversion, persistence, shape evaluation and simulation are Rust.

1. Preserve Secretary's production layout and wardrobe categories. Use actual outfit/hair assets and regenerate thumbnails after validating fits; existing grayscale thumbnail faces are not a desired appearance.
2. Load the canonical-generation bindings, matching idle clip and WASM before wiring a composed model. Disable the stage's old physics (`physics:false`) so two solvers do not fight.
3. On each model load, rebuild bone maps, body corrections, hair springs, cloth groups and colliders. Collect `node.extras.secretarySecondary.grid` groups in **row × columns + column** order. Read `closed`; Uniform cape is open. Exclude grid bones from the independent spring solver.
4. Each frame, start with the native idle animation. Apply Rust customization to body/garment shared bones and matching morphs, then body springs, coupled cloth, independent hair springs, update world matrices and skeletons, render. Rebuild source local matrices from animation each frame; do not compound the previous shape result.
5. Apply full local matrices to cloth/hair bones with `matrixAutoUpdate=false`. TRS decomposition discards shear in scaled parent frames and caused earlier distortion. Body child corrections are separately provided.
6. Colliders use current animated/scaled world frames. Include the garment's `secretaryOutfitCapsules` proxies. Reset solver state on model switch, large time discontinuity/teleport and physics/shape changes. Pause/reset when hidden; don't catch up an unbounded time backlog.
7. Wire model changes to Rust composition and settings persistence; don't execute `.menu` script records. Production authentication, error handling and API ownership must use the existing Secretary backend. Do not expose private archive/source paths through an unauthenticated public fitting server.

WASM ABI (float offsets, not byte offsets; pointers are byte addresses):

- Body: `body_io_ptr()` is 192 f32; inputs base/parent/corrections at 0/16/32/48, size/softness/support/blend/inward/elevation at 64..70. See child fields 128..151 and output matrices 80..96 / 160..176. `body_step`, `body_reset`, `body_impulse`; `skin_color` shares this buffer's color output at 110..113.
- Customization: `customization_io_ptr()` is 128 f32. 21 ordered control values at 0..21 (schema order), animated local matrix at 32, parent correction 48, bone correction 64, output local matrix 80. Call `customization_apply(binding.index)`; nonzero means rejected input. Do not reorder compiled control indices.
- Independent springs: `secondary_io_ptr()` is 192 f32; world/parent matrices 0/16, local lever 32..35, parameters 35..40, collider count 40, up to 16 capsules at 48 (seven floats each). Local output at 160, angle/contact/drop diagnostics 176..179. `secondary_step`, `secondary_reset`, `secondary_impulse`.
- Cloth: use exported `cloth_io_len` and offset functions, not guessed constants. Profile params at 0..8, capsule count 8, gravity enabled 9, diagnostics 10..14, closed flag 14; capsules begin 16. World matrix and external-parent matrix per node use stride 32 starting `cloth_world_offset`. Parent-index/gravity/output arrays have their own exported offsets. `cloth_clear`, `cloth_init(slot,columns,rows)`, `cloth_step`, `cloth_reset`, `cloth_impulse`. Limits: four grids, each ≤24 columns ×48 rows /1152 nodes, ≤16 capsules. Reject incompatible scenes instead of silently disabling simulation.

## Validation and measurements

Run from the worktree:

```sh
cargo test -p secretary-avatar-com3d2
cargo clippy -p secretary-avatar-com3d2 --all-targets -- -D warnings
cargo build -p secretary-avatar-com3d2 --release
cargo build -p secretary-avatar-com3d2 --release --target wasm32-unknown-unknown --lib
cargo run --release -p secretary-avatar-com3d2 --example cloth_bench
```

Tests include native head/model round trips, mesh/UV/morph preservation, body visibility without deleting the real face, frame-rate consistency, pinned roots, gravity versus zero gravity, capsule shaft contacts, teleports, scaled/sheared parent matrices, coupled cloth seams/floor/collision, bounded `.psk` parsing, native curve axes/defaults, generated scale-helper bindings, and a boot sharing a clothing material remaining rigid while the hem is rigged.

Synthetic cloth measurement on nezopt: 120 nodes, 18 iterations, eight capsules, 3600 frames after warm-up; median **0.188 ms**, p95 **0.214 ms**, p99 **2.495 ms**; process peak RSS **2688 KiB**. This is native CPU simulation only, excludes browser/WASM bridge, animation, asset loading and GPU rendering. It is not an Android performance result. Actual live preview timings vary; use device measurements before assigning a mobile frame budget.

## Release gates and remaining limitations

- This is not the complete COM3D2 engine. Native Windows physics DLLs/Unity are not loaded. Toon-shader parity, fabric self-collision, full native pannier/stress behavior and the rest of the game are not ported.
- Conversion success is not fit approval. Individually review old garments and native outfits from front/side/back at neutral and extreme supported shapes, idle and impulse motion; check collars, arm bends, underwear masks, boots, waist seams and hem contacts. The new inferred hem deliberately refuses sparse/open geometry; author those seams separately.
- Hair uses coarse bone constraints, not strand-level simulation. Verify collar contact and dynamic motion in the production camera/animations; do not judge only from the 'physics on' badge.
- Native facial controls without an authored mapping must remain unavailable. Do not alter Lucina's face or add artificial 'makeup' to hide a material problem.
- Body proportions share the rig, but arbitrary imported garments may lack native morph targets, may have their own unsupported root-scale controls, or clip at extremes. Add per-fit capability/range evidence before treating every control as universally compatible.
- NTE, the rejected bob, unreviewed alternative hairstyles, app installation and public deployment are outside this focused release. Keep them gated. Preserve Telegram/PDF fixes and all unrelated Secretary features.

A practical first production integration should carry the native canonical generation, Rust/WASM adapter and reviewed native coat/Uniform path behind a feature gate. Expand the wardrobe only after per-look review. The user asked for a written handoff, not automatic messages to another Claude chat.


Final sizing correction: native garment `Skirt` roots are not body-model bones.
Their source frames are retained as `secretaryShapeSource`, calibrated after
skeleton merge into `secretaryShapeBinding`, and collected in `shapeRig` alongside
the 27 body bindings. This makes the native coat receive hip/waist proportions;
visual comparison at the saved 100% hip setting removed the large side clipping.
Do not omit this extra-root collection when integrating just `bindings.json`.
A regression covers neutral-pose preservation and hip deformation of an extra
root in a different source frame. Native cloth metadata revision is 2.


Visual release note: the native coat was compared with gravity off/on and with
a bounded test impulse; its original hair and coupled cloth responded rather
than merely changing an enabled badge. The restored Uniform was checked front
and rear with its cape hanging down and boots following the legs. A spot-check
of the old **Maid** import still showed body intersections, and its free hem did
not satisfy the conservative rig-selection criteria (zero generated hem nodes).
Do not promote that fit, or infer that the other old outfits are approved from
the successful conversion report. Further pattern/weight authoring is required.
The provisional production whitelist for this handoff is the native coat and
original Uniform, subject to production-camera/animation checks; all other fits
remain review-only. Final artifact checksums are in the private root's
`final-build-sha256-20261028.txt`.


Final verification: **56 tests passed**, Clippy passed with warnings denied, and
native/WASM release builds passed. Code checkpoint:
`308dea21cdc40210f70bd6c42e99d24c4ba3c2c1`.
The private root also contains `review-uniform-20261028.png` as a current view of
the restored Uniform. Production remains unchanged; the wider garment-fitting
and full-engine work is explicitly unfinished.
