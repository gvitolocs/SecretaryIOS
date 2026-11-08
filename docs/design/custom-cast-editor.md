# Custom Cast's character editor (SceneEdit) and Lucina's "Edit Lucina"

Source: the static decompilation of CUSTOM CAST 1.04.07 on nezopt
(`/home/nez/Decompiled/custom-cast-1.04.07-20261021`). This is a study of behaviour, ranges and
flow only. No code, asset, sprite or text of the app is copied into Secretary. The interaction
inventory (`custom-cast-interactions.md`) listed SceneEdit as "n/a — replaced by the wardrobe";
that decision is reversed. The owner wants Lucina as customisable as a Custom Cast character.

## How the evidence was read

| What | Where in the decompile |
| --- | --- |
| Editor windows, their fields and handlers | `il2cpp/csharp/Assembly-CSharp/SceneEdit.cs`, `SCENE_EDIT/*.cs` (41 classes) |
| The parameter list (MPN) | `il2cpp/csharp/CustomCast.MaidCore/MPN.cs` |
| Slider storage (`MaidProp`: value, default, min, max) | `CustomCast.MaidCore/MaidProp.cs`; values from the 75 shipped presets in `assetripper-content/Assets/Resources/preset/*.bytes` |
| Colour model (`PartsColor`) | `CustomCast.MaidCore/MaidParts.cs`, `SCENE_EDIT/SceneEditColorPaletteWindow.cs` |
| Bone morphs (how sliders move bones) | `CustomCast.MaidCore/BoneMorph.cs`, Ghidra pseudocode `il2cpp/pseudocode-by-type/CustomCast.MaidCore/BoneMorph.c` (`Init`: `SetScale`/`SetPosition` per slider and bone) |
| Bone names | `metadata/il2cpp-string-literals.jsonl` (`Bip01 …`, `… _SCL_`) |
| Scene layout and serialized slider bounds | scene bundle `unity/objects/667329f4…` (`__SceneEdit__`: 1,576 MonoBehaviours, 458 GameObjects) |
| Undo | `SCENE_EDIT/MaidUndo.cs` |

The preset files are `CM3D2_PRESET` binaries: a PNG thumbnail (138×200), then a `CM3D2_MPROP_LIST`
of `MaidProp` records (`CM3D2_MPROP`, version 10108, idx, name, type, default, value, temp, link,
file name, file hash, dirty flag, **max, min**, …). A small Rust reader
(`~/scratch/editor-tools/ccpreset`, not committed) dumped every record of every preset. The
ranges below are those records; "seen" is the span of values the 75 shipped characters use.

## 1. Structure and flow

`SceneEdit` holds one edited `Maid` and a stack of windows:

```
SceneEditMainMenuUI     Exit+Save · Option · View (hide UI) · Background · Pose · Customize
└─ SceneEditEditMenuUI  Body figure · Body parts · Dress parts · Parts custom · Preset · Random
   ├─ SceneEditCategoryWindow  (category rail: CHARA_PRESET, CUSTOM_PARTS, DRESS_PARTS,
   │                            BODY_PARTS, BODY_FIGURE; lists from edit_category_*.csv)
   ├─ SceneEditMenuItemWindow  (part grid: main grid + sub grid of colour variants, a "none" tile,
   │                            sort A–Z / newest / oldest, filter all / female / male, search,
   │                            NEW badges from NewlyCostume.json)
   ├─ SceneEditBodyFigureWindow (slider list built from a prefab per MPN, 0..100)
   ├─ SceneEditColorPaletteWindow (hue bar + SV picker + contrast + shadow rate; main / shadow /
   │                            outline colour; colour presets; eye L / R / both)
   ├─ SceneEditPartsCustomWindow (move / rotate / scale an accessory or hair part with XYZ
   │                            handles; hair length sliders; reset)
   ├─ SceneEditCharacterPresetWindow (save all / outfit / body, load all / outfit / body,
   │                            delete; 20 slots with thumbnails)
   └─ SceneEditRandomWindow    (tick categories, randomise; choices remembered)
SceneEditEyeHighlight   highlight L / R / both     SceneEditMayuAlphaSlider  brow opacity
SceneEditPoseWindow, SceneEditFaceWindow           poses and expressions while editing
SceneEditOptionMenuUI   eyes follow camera · auto camera · filter · lighting · camera · config
```

Flow: the main menu's **Customize** opens the edit menu. A tap on a top button opens its
category rail. A tap on a category opens the part grid or the slider window. A colourable part
puts a palette button on its tile. Every change goes through `MaidUndo.RegisterMaidUndo`.
**Exit and save** writes the maid. **View** hides every window, and a tap brings them back.

### Camera

`SceneEdit.LookToMpn(mpn, time = 0.5)` flies the orbit camera to the part being edited, and
`LookToBody(boneName = "Bip01 Pelvis")` frames the body. Each MPN carries its own `distance` and
look target, so face parts zoom to the face and shoes to the feet. `CameraSetDistance` and
`CameraMoveTo(target, distance, angle, time)` animate over 0.5 s. `SceneEditCameraMoveMgr` keeps
the free orbit, clamps the pitch (`_yMinLimit`/`_yMaxLimit`) and locks while a slider is dragged.
Options: **auto camera** (`m_isUseAutoCam`) and **eyes to camera** (`m_isUseEyeToCam`).

### Undo and redo

`MaidUndo` keeps two `FixedStack<UndoInfo>` (undo and redo, oldest dropped when full). The body
figure, part grid and parts custom windows subscribe to `OnExecuteUndo`/`OnExecuteRedo` and
refresh their sliders and tiles.

### Randomise

`SceneEditRandomWindow` lists MPN categories with a switch each (all on, all off, execute).
`RandomPresetData` saves the switches. `EditMenuItemData.GetRandomItem(mpn)` picks a part per
enabled category. `SceneEditRandomPartsManager` does the same for colours.

### Presets

`SceneEditCharacterPresetWindow` modes: `SAVE_MODE_ALL`, `SAVE_MODE_WEAR`, `SAVE_MODE_BODY`,
`LOAD_MODE_ALL`, `LOAD_MODE_ALL_ONLY`, `LOAD_MODE_WEAR`, `LOAD_MODE_BODY`, `DELETE_MODE`,
`PRE_RESTORE`. `PRESET_MAX = 20`. A preset is the whole `MaidProp` list plus a thumbnail.
Body-only or outfit-only loads apply half of it. `ColorPresetMgr.COLOR_PRESET_MAX = 100` colour
swatches are kept separately.

## 2. Body figure (`SceneEditBodyFigureWindow`)

All body sliders are integer 0..100 in the UI (the slider prefab `0..100, whole numbers`).
Most defaults are 50 and mean "no change". Type 1 is a blend-shape weight and type 2 a bone morph.

| MPN | Meaning | Type | Range | Default | Seen in presets |
| --- | --- | --- | --- | --- | --- |
| `sintyou` | height | bone | 20..80 | 50 | 20..72 |
| `DouPer` | torso / leg ratio (leg length) | bone | 20..80 | 50 | 20..80 |
| `KubiScl` | neck length | bone | 20..80 | 50 | 31..68 |
| `HeadX` | head width | bone | 0..100 | 50 | 37..69 |
| `HeadY` | head size | bone | 0..100 | 50 | 35..70 |
| `kata` | shoulder width | bone | 0..100 | 50 | 0..61 |
| `UdeScl` | arm length | bone | 0..100 | 50 | 20..100 |
| `west` | waist | bone | 0..100 | 50 | 23..100 |
| `koshi` | hips | bone | 0..100 | 50 | 22..100 |
| `MuneL` | bust size | shape | 0..130 | 10 | 0..130 (mean 50) |
| `MuneS` | bust shape | shape | 0..100 | 0 | 0..100 |
| `MuneTare` | bust droop | shape | 0..*MuneL* | 10 | 0..50 |
| `MuneUpDown` | bust height | bone | 0..100 | 10 | 0..68 |
| `MuneYori` | bust spacing | bone | 0..100 | 40 | 0..80 |
| `MuneYawaraka` | bust softness (physics) | bone | 0..100 | 50 | 0..100 |
| `RegFat` | leg thickness | shape | 0..100 | 40 | 0..100 |
| `RegMeet` | leg muscle | shape | 0..100 | 40 | 0..80 |
| `ArmL` | arm thickness | shape | 0..100 | 20 | 0..75 |
| `Hara` | belly | shape | 0..100 | 20 | 0..30 |

Notes:

- `MuneTare`'s maximum follows `MuneL` (`SceneEditBodyFigureWindow.MuneTareUpdate`,
  `m_muneLvalue`): the presets show droop maxima from 10 to 130.
- Bone morphs scale and move bones between a minimum and a maximum vector per slider
  (`BoneProp.vMinP/vMaxP/vMinM/vMaxM`, 50 = identity). They scale **`… _SCL_` twin bones**
  (`Bip01 Spine_SCL_`, `Bip01 Pelvis_SCL_`, `Bip01 ? Thigh_SCL_`, `Bip01 Head_SCL_` …): the
  skin is weighted to the twin, the children hang off the real bone. So a thicker waist never
  stretches the chest, and an accessory on a scaled bone never shears. `SetPosition` moves a child
  to change a length (neck, legs).

## 3. Face (`BODY_PARTS` and body figure face sliders)

| MPN | Meaning | Range | Default | Seen |
| --- | --- | --- | --- | --- |
| `EyeScl` | eye size | 0..100 | 50 | 12..83 |
| `EyeSclX` / `EyeSclY` | eye width / height | 0..100 | 50 | 10..100 / 0..100 |
| `EyePosX` / `EyePosY` | eye spacing / height | 0..100 | 50 | 32..73 / 0..100 |
| `EyeClose` | resting eye openness | 0..100 | 0 | 0..44 |
| `EyeBallPosX/Y` | iris position | 0..100 | 50 | 50 / 26..67 |
| `EyeBallSclX/Y` | iris size | 0..100 | 50 | 29..100 / 20..100 |
| `FaceShape` / `FaceShapeSlim` | face shape / slimness | 0..100 | 0 | 0..100 |
| `MayuX` / `MayuY` | brow position | 0..100 | 50 | 50 / 0..100 |
| `MayuShapeIn/Out`, `MayuRot`, `MayuThick`, `MayuLong` | brow inner/outer shape, angle, thickness, length | 0..100 | 50 | (newer than the presets) |
| `Yorime` | eyes toward the nose | 0..100 | 50 | — |
| `MabutaUpIn/In2/Middle/Out/Out2`, `MabutaLowIn/UpMiddle/UpOut` | eyelid control points | 0..100 | 50 | — |
| `EarNone` / `EarElf` / `EarRot` / `EarScl` | no ears, elf ears, ear angle, ear size | 0..100 | 0/0/50/50 | the three ear sliders lock while `EarNone` is on (`EarUpdate`) |
| `NosePos` / `NoseScl` | nose height / size | 0..100 | 50 | — |

Part grids in the same rail: `head` (face type), `eye` (iris texture), `eye_hi`, `eye_hi_r`
(highlights, separate per eye), `eyewhite`, `mayu` (brow, with an opacity slider), `matsuge_up`,
`matsuge_low` (lashes), `futae` (double eyelid), `nose`, `lip`, `hokuro` (moles), `facegloss`,
`skin`, `acctatoo` (tattoo / face paint decals), `accnail` (nails), `underhair`.

## 4. Colour (`SceneEditColorPaletteWindow`, `MaidParts.PartsColor`)

Colourable slots (`PARTS_COLOR`): `EYE_L`, `EYE_R`, `HAIR`, `EYE_BROW`, `UNDER_HAIR`, `SKIN`,
`NIPPLE`, `HAIR_OUTLINE`, `SKIN_OUTLINE`, `EYE_WHITE`, `MATSUGE_UP`, `MATSUGE_LOW`, `FUTAE`.

Each colour has a main and a shadow half: `Hue 0..255`, `Chroma 0..255`, `Brightness 0..510`,
`Contrast 0..200`, plus `ShadowRate 0..255`. The window: a hue bar (`Hue 0..255`), a
saturation/value square (`EditColorPicker`: drag the cursor; the square is redrawn for the hue),
`Contrast` and `ShadowRate` sliders, and three mode buttons: **main colour**, **shadow colour**,
**outline colour**. A **Preset** tab holds up to 100 saved swatches. For the eyes, **L / R /
LR** chooses which eye the colour goes to. The tinted texture is rebuilt through a 256-entry
table per slot (`m_aryPartsColorTable`).

## 5. Hair

`hairf`, `hairr`, `hairt`, `hairs`, `hairaho` (front, back, top/tail, side, ahoge) are separate
part grids. `haircolor` is a part grid of colour presets, and the palette tints it (main,
shadow, outline). **Parts custom** (`SceneEditPartsCustomWindow`) gives each hair part
**length** sliders (`TBodySkin.HairLengthCtrl`, one per hair group) and a position/scale handle.

## 6. Outfit slots (`DRESS_PARTS`)

`wear` (top), `skirt`, `onepiece` (replaces top and skirt), `mizugi` (swimsuit), `bra`, `panz`,
`stkg` (legwear), `shoes`, `headset` (headdress), `glove`, plus sets (`set_maidwear`,
`set_mywear`, `set_underwear`, `set_body`). A `_del` item empties a slot
(`_I_stkg_del.menu`). An outfit part ships its own culling of the body underneath (the CM3D2
`.menu` "mask item" mechanism hides body or other parts covered by it).

## 7. Accessory slots

`acchead` (head), `acchat` (hat), `acckami` / `acckamisub` (hair ornaments, two), `accmimi` (ears),
`megane` (glasses), `acckubi` (neck) and `acckubiwa` (collar), `accheso` (navel), `accude` (arm),
`accashi` (leg), `accsenaka` (back), `accshippo` (tail), `accha` / `acchana` (teeth, nose),
`handitem`, `accxxx`. **Parts custom** moves, rotates and scales an accessory on its attach point
(`TBody.SlotID` + attach point name) with XYZ handles, and resets it. Accessories ride on named
bones (`AttachPrefab`, `TAttachPoint`).

## 8. Body type presets

`edit_category_chara_preset_list.csv` drives the **Preset** rail: shipped characters (the 75
`pre_*` files: `pre_mazime`, `pre_muku`, `pre_rindere`, … each a whole look) and the user's 20
slots. Loading **body only** applies just the body figure and face. That is how a "body type" is
picked.

## 9. Lucina's editor: the mapping

Lucina is one fixed character (the G model: Fire Emblem Engage rig names on B's armature, a
morph-driven face), so the editor keeps Custom Cast's flow and slider semantics. The mechanics fit
our rig.

### Categories (left rail)

| Rail | Panel | Camera | Custom Cast equivalent |
| --- | --- | --- | --- |
| Looks | saved looks grid, save / save as / load / rename / delete, randomise, reset | full | Preset + Random |
| Body | 13 sliders | full | Body figure |
| Face | 14 sliders | face | Body figure face sliders, `Mabuta*`, `Mayu*` |
| Eyes | iris colour wheel, highlight on/off | face | `eye`, `eye_hi`, palette `EYE_L/R` |
| Skin & makeup | skin tone, lipstick, eyeliner, eyeshadow, blush (colour + strength) | face | `skin`, `lip`, `acctatoo`, palette `SKIN` |
| Hair | hairstyle grid, colour wheel (HSV), highlight colour | bust | `hairf/r/t/s`, `haircolor`, palette `HAIR` |
| Accessories | slot rail (head, hair, ears, eyes, neck, back, wrists) + part grid, tiara on/off | per slot | `acc*`, `megane`, `acchat` |
| Outfit | whole outfits, or parts per slot (top, bottom, legwear, shoes, gloves) | full | `wear`, `skirt`, `stkg`, `shoes`, `glove`, `onepiece` |

### Body sliders (UI 0..100, default 50)

The UI keeps Custom Cast's 0..100 with 50 as "as authored". Each slider maps linearly to a factor
between a minimum (0) and a maximum (100), with 50 at 1.0. Two-sided ranges are not symmetric
where taste says so (a bust can shrink more than it grows).

| Slider | Effect on G's rig | Factor at 0 / 100 | CC |
| --- | --- | --- | --- |
| height | whole-model scale, feet stay on the floor | 0.92 / 1.08 | `sintyou` |
| head | `head` node scale (face, hair, eyes and head accessories scale together) | 0.88 / 1.12 | `HeadY` |
| neck length | `neck` skin stretched along the bone, `head` moved by the same factor | 0.80 / 1.25 | `KubiScl` |
| neck width | `neck` skin scaled across | 0.85 / 1.20 | — |
| shoulders | `leftShoulder`/`rightShoulder` skin along the bone, upper arms moved | 0.85 / 1.15 | `kata` |
| bust | `breast_left`/`breast_right` skin scale | 0.75 / 1.35 | `MuneL` |
| waist | `spine` skin scaled across (X, Z) | 0.85 / 1.12 | `west` |
| hips | `hips`/`lowerBody` skin across, legs set apart | 0.88 / 1.15 | `koshi` |
| arm length | upper arm + forearm skin along the bone, children moved | 0.90 / 1.10 | `UdeScl` |
| arm thickness | upper arm + forearm skin across | 0.85 / 1.20 | `ArmL` |
| leg length | thigh + calf skin along the bone, children moved, model lifted | 0.90 / 1.10 | `DouPer` |
| leg thickness | thigh + calf skin across | 0.85 / 1.20 | `RegFat` |
| hand size | `leftHand`/`rightHand` node scale | 0.85 / 1.15 | — |

"Skin scale" is our `_SCL_` twin: the stage multiplies a per-bone local scale into that bone's
**skinning matrix only** (`Skeleton.update` is wrapped). The bone's children, physics bones and
accessories keep their transforms. A length change also moves the child bones, so joints stay
connected. Only `head` and `hands` use real node scale, which is uniform, so the eyes, hair and
props under them stay undistorted. The G clips animate only rotations and the hips translation
(checked on `lucina-anims.glb`), so the edits survive every clip.

### Face sliders (UI 0..100, default 50)

G's face has no face bones in the GLB (it is driven by 20 baked morphs). The stage therefore
**generates morph targets at load** from G's own geometry, so no game asset is re-exported. The
landmarks come from the model: eye centres from the `EyeL`/`EyeR` primitives, the mouth from the
vertices `mouth_a` moves, the brows from the `Lash` primitive above each eye, the chin and cheeks
from the face skin. Each slider is a smooth, local displacement field (radial falloff) applied to
every face primitive alike, so the eyes, lashes, brows and skin move together without cracks:

eye size, eye width, eye height (shape), eye spacing, eye vertical position, eye tilt
(`Yorime`/`Mabuta*` outer corner), brow height, brow angle, mouth width, mouth vertical position,
face width (`FaceShapeSlim`), jaw/chin length (`FaceShape`), cheeks, nose height (`NosePos`).

### Colours

Custom Cast's palette is reduced to what reads on a toon shader: one colour wheel (hue ring +
saturation/value square), plus a strength slider for makeup. Main/shadow/outline split and
contrast are not exposed. The toon ramps already make the shadow colour.

- **Iris:** a hue rotation of the eye texture (shader uniform), so the iris pattern stays.
- **Highlight:** on/off (`EyeHL` material visibility).
- **Skin tone:** a multiply on the toon `tint` of the skin materials (`FaceSkin`, `BodySkin`,
  `FaceColor`, `Mouth`).
- **Makeup:** four procedural masks drawn into one RGBA texture **in the face's UV space**
  (lipstick, eyeliner, eyeshadow, blush), rasterised at load from the same landmarks. Each layer
  has a colour and a strength uniform, blended over the face albedo in the shader.
- **Hair:** HSV colour (through the wardrobe's hair tint, so outfit swaps keep it) and the angel
  ring highlight colour (toon `ringColor`).

### Undo, randomise, reset, looks

- Undo/redo: 50 steps. A slider drag is one step (committed on release).
- Randomise "within taste": each slider is drawn from a triangular distribution centred on 50 and
  bounded by the span the 75 Custom Cast presets actually use (§2, §3), scaled down. Colours come
  from curated palettes (natural hair, iris and skin swatches). Accessories get a 30% chance per
  slot, never a duplicated slot. Optional per-category switches, as `SceneEditRandomWindow` has.
- Reset: per category, or everything (to G as authored).
- Looks: up to 20 named looks (`PRESET_MAX = 20`), stored on the server in `GET/PUT /v1/avatar`
  (`looks`, `current`). Loading a look applies all of it. Body-only/outfit-only loads are not built.
  Lucina can switch looks by name ("mettiti il look serata").

### Not taken over

Gender filters, NEW badges, sorting, shop and unlock state, nipples and other adult slots
(`chikubi`, `underhair`, `seieki_*`, `kousoku_*`), VRM export and Seed online sharing.
