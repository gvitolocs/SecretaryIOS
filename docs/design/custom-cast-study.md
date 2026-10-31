# Custom Cast: design study for the Secretary web redesign

Source: the static decompilation of CUSTOM CAST 1.04.07 (`jp.customcast.cc2`, Unity 6000.0.60f1) on
nezopt at `/home/nez/Decompiled/custom-cast-1.04.07-20261021`. The app is proprietary. This study
records what it does with layout, light and motion, so that Secretary can borrow the ideas. None of
its sprites, fonts, textures, shaders or models are in this repository or in the build: every visual
in Secretary is our own CSS, SVG or GLSL.

Working files (outside the repo): contact sheets of its UI sprites and screens, plus the
RectTransform trees of SceneMain, SceneStudio and SceneTitle, in `~/scratch/redesign-shots/custom-cast/`.

## What was looked at

| Area | Where in the decompilation |
| --- | --- |
| Screen layouts | `unity-project/ExportedProject/Assets/Scene/*.unity`: GameObject and RectTransform anchors, sizes and positions (reference canvas 1080 × 1920, portrait) |
| UI sprites | 1,538 sprites and 1,940 textures, indexed in `unity/objects.tsv`, PNGs under `unity/objects/*/Texture2D/` |
| Motion | `Resources/systemui/LoadIconAnime.anim`, `Resources/sceneredownload/InfoSlide.anim`, `Resources/scenelive/StampExpand.anim` and `StampShrink.anim`, `Resources/scenestudio/animation/Idle.anim` |
| Light and camera | Camera and Light components in SceneMain and SceneStudio |
| Effects | 26 particle systems, 172 shaders (CM3D2 toon and outline, VRM MToon, post-processing bloom and vignette, `Animator/AMIrisShape`) |

## Findings

### 1. The character is the screen

The 3D character fills the whole frame in every main scene. The UI never has a column of its own.
Instead it floats around her:

- **Left rail.** `LeftMenuLayout` is anchored to the left edge, about 52 px in on a 1080 px canvas.
  It holds a vertical stack of round buttons, 130 to 160 px across (12 to 15 % of the width):
  live schedule, events, gacha.
- **Bottom bar.** `MenuButtons` and every `Buttons` group are anchored bottom-stretch, 160 px tall
  with 20 px side margins. They hold round buttons of 128 to 130 px.
- **Top right.** A single reward or help button sits there.
- **Panels.** List panels (Pose List, Face List, SaveLoad) slide up from the bottom and take about
  29 % of the height (562 of 1920). Her face stays visible above them. Dialogs are full-width bands
  800 px tall, with the round Yes/No buttons at the bottom corners.

What Secretary takes from this:

- Lucina gets a full-height portal on desktop and a 38 % strip on phone chat. The strip fades into
  the page instead of ending at a border.
- Her controls are a vertical rail of round glass buttons floating at her side, with the mic first
  and largest.
- Navigation is a floating rail on desktop and a floating bottom dock on phones.
- Sheets slide up from the bottom.

### 2. Round buttons with an icon over a short label

The icon system (`icon_system_*`, about 800 sprites, and `icon_category_*`, 180) uses colour-coded
circles and rounded squares:

| Colour | Meaning |
| --- | --- |
| Yellow `#F9C300` | Primary and confirm: OK, Yes, Close, Delete |
| Sky blue `#5BC0EB` | State and toggles: ON, presets |
| Pink or red `#F0607A` | Save and purchase |
| Green | New and preview |

Each button shows a white glyph with a one- or two-word label underneath. White pills carry an
icon and text (My Page, License, Language).

What Secretary takes from this: rail and dock items are an icon over a short label. Colour carries
meaning: one neon tone per chat channel, red for to-buy tiles and teal for recently used ones, lime
in the Gym, and the holographic gradient for primary actions and anything active. Custom Cast's
pastel candy palette is not used. The owner asked for something futuristic, so the same idea is
translated into neon on glass.

### 3. Light: a telephoto camera, one key light, toon shading, bloom

- **Camera.** `CameraMain` uses a vertical field of view of 35° (30° on the thumbnail camera),
  which is flattering and shows little perspective. The near plane is 0.01.
- **Lights.** There are two directional lights. `CharaDirectionalLight` is white at intensity 1.9
  with soft shadows.
- **Shading.** The character uses toon shaders with outlines (`CM3D2/Toony_Lighted_Outline`,
  `VRM/MToon`). Rim and silhouette come from the toon ramp, not from a physical light rig.
- **Background.** A 2D `Canvas2DBG`/`BG-Texture` layer or a stage set sits behind her, with
  `Skybox/Procedural` available.
- **Post-processing.** Bloom, vignette, sepia and grayscale (`Hidden/PostProcessing/Bloom`,
  `Hidden/Custom/Vignette`).

What Secretary takes from this:

- The manifest framing at 30° is kept.
- She gets a warm key light plus cyan and magenta rim lights, and the rims swell with her voice.
- Behind her is a separate background layer: a full-screen nebula shader, not a texture.
- On the high quality tier she gets bloom and a vignette.

### 4. The stage under her feet

`LiveStage_*` and `Theater_*` give the dance stages emissive floor patterns, spot and pillar lights,
and a `ScreenGrid`.

What Secretary takes from this: a holographic floor disc with rings, rotating dashed arcs, tick
marks and a faint grid. It ripples while she listens, swirls violet while she thinks, and glows
with her voice. It is our own shader.

### 5. Motion language

| Clip | What it does | Secretary's version |
| --- | --- | --- |
| `LoadIconAnime` | 12-frame spinner at 12 fps (`cm3d2_loadingmark01–12`), a stepped rotation | `.holo-loader`: a ring of 12 ticks turning in `steps(12)`, with a counter-rotating inner arc |
| `InfoSlide` | Notice slides in from x = −652 over 0.83 s with ease-out, holds, then fades out at 1.67 s | Banners and HUD chips slide in with `--ease-out` over `--dur-4` |
| `StampExpand` / `StampShrink` | Scale 0 → 1 (and back) in 0.42 s | `pop-in` with `--ease-spring` for the FAB, bubbles, menus and snackbars; `tile-in` for shopping tiles |
| `AMIrisShape`, `am_iris_round/star`, `am_wipe_*` | Iris and wipe scene transitions | Route changes fade and un-blur the page in 380 ms; no full-screen wipe, to keep it calm for an everyday app |
| Studio `Idle.anim` | A 2.67 s looping idle | The clip layer (`lucina-anims.glb`) when present; otherwise the procedural idle with arm sway in time with her breath, so she never stands in the rest pose |

The particle systems include warm sparkles (start colour `(1, 0.91, 0.44)`, lifetime 1.5 s,
speed 12). Secretary has ambient motes in cyan, violet and magenta, and a gold-tinted sparkle
burst when you tap her.

### 6. Typography

Custom Cast uses Noto Sans CJK JP Medium through TextMeshPro SDF, plus LiberationSans. Secretary uses
Sora (OFL, through `@fontsource-variable/sora`) for text and JetBrains Mono (OFL) for the HUD
voice: small uppercase tracked labels for state, channel tags, dates and eyebrows.

## What Secretary does differently

- **Palette.** Custom Cast is bright and candy-coloured: yellow circles, rainbow gacha frames,
  white pills. Secretary is dark-first glass with holographic accents. Pearl, the light theme, keeps
  the same structure on white glass.
- **Interface density.** Custom Cast is a toy with dozens of tools. Secretary is a daily assistant:
  four sections, and Lucina's controls reduced to mic, mute, minimise and hide.
- **Shopping.** It keeps the Bring! layout exactly, because the owner has muscle memory for it. Only
  the colours, glow and motion change: red still means to buy, teal still means recently used.
