# Custom Cast interactions: inventory and plan for Lucina

Source: the static decompilation of CUSTOM CAST 1.04.07 on nezopt
(`/home/nez/Decompiled/custom-cast-1.04.07-20261021`). This study maps every user-facing
interaction to the scenes and classes that implement it and says how Lucina's stage
(`packages/lucina-stage`) does the same. It is a reference for behaviour only: no code, asset,
sprite or sound of the app is copied. The earlier visual study is `custom-cast-study.md`.

## How the evidence was read

- **Classes and fields:** `il2cpp/csharp/Assembly-CSharp/*` (structure only; method bodies were
  not recovered).
- **Serialized values:** the MonoBehaviour blocks in
  `unity-project/ExportedProject/Assets/Scene/*.unity`, matched to scripts through the `.cs.meta`
  GUIDs.
- **Behaviour of key methods:** the Ghidra pseudocode in `il2cpp/pseudocode-by-type/`. Float
  constants were read from `libil2cpp.so` at the address minus the 0x100000 image base.
- **UI labels:** the I2 localisation terms (`I2Languages`, object `9ae3f464…`). They name every
  settings switch, for example `SceneMain/コンフィグ/カメラ/ジャイロカメラ`, the gyro camera.

Status marks:

- **now:** shipped in phases 2–3 of this work.
- **later:** documented, not built yet.
- **n/a:** does not fit a private assistant: store, gacha, accounts, live streaming services.

## 1. Camera

Every 3D scene puts the same `UltimateOrbitCamera` on `CameraMain`, with identical values in
SceneMain, SceneStudio, SceneLive, SceneEdit and the rest:

- `distance 2`, `minDistance 0.1`, `maxDistance 25`;
- `xSpeed`/`ySpeed 1`, `zoomSpeed 1`, `moveSpeed 0.03`;
- `dampeningX 0.833`, `dampeningY 0.839`, `smoothingZoom 0.3`, `smoothingMove 0.3`;
- `limitY 0` (pitch unlimited by default; the scenes clamp it themselves), `TapCamResetOn 1`;
- `initialAngleX -180`, `initialAngleY 10` (in front of her, 10° above).

| # | Interaction | Custom Cast (where, how) | Lucina | Status |
| --- | --- | --- | --- | --- |
| C1 | One-finger drag orbits the camera | `UltimateOrbitCamera.Update`: with one touch, `xVelocity += Δx·xSpeed·0.2` and `yVelocity −= Δy·ySpeed·0.2` (degrees per pixel). The angles add the velocities, then the velocities are multiplied by the dampening each frame. The camera orbits a target transform at her chest (`m_TargetPos (0, 1.25, 0)` in SceneLive); the model never turns. | `interact/orbit.ts`: the same velocity model, 0.2°/px, damped by 0.833/0.839 per 60 Hz frame (time-corrected), around the framing target. | now |
| C2 | Yaw unlimited, pitch clamped | Yaw is free. SceneLive clamps the pitch with `m_AngleYMin −30` and `m_AngleYMax 45` (`ApplyAngleLimit`). | Yaw free (wraps); pitch from −30° (below) to +45° (above). | now |
| C3 | Inertia and damping | The velocity keeps going after release and decays by ×0.833 per frame, so the glide is short (about 0.3 s). | Same decay, scaled by `dt`. Reduced motion: no glide. | now |
| C4 | Two-finger pinch zoom | `targetDistance += (previous pinch distance − current)·0.01·zoomSpeed`, clamped to [min, max], and `distance` eases toward it by 0.3 per frame. | Pinch moves a zoom value along a full-body → bust → face path: target, distance and FOV are blended, so zooming in walks up to her face. Soft limits rubber-band 6 % past each end, then spring back. | now |
| C5 | Two-finger drag pans | Pan applies when the two fingers move the same way: the dot product of their normalised deltas is ≥ 0.8. The target offset moves by `moveSpeed · mean delta` in camera right/up and eases by 0.3 per frame. | Same parallel-motion test, 0.8. Pan is proportional to the distance and clamped to her bounding box, so she cannot be pushed off screen. | now |
| C6 | Double-tap resets the camera | `CheckCamReset` / `WaitTripleTapCo`: a tap that is not on a UI element starts a 0.2 s (`DOUBLE_TAP_TIME`) window, and a second tap in it resets to the saved position (`SaveResetPosition`/`ResetCamera`). `CameraMain.ResetCamera` animates it. | A double-tap off her body (0.3 s window, 24 px) animates back to the default view in 0.45 s. A double-tap on her body counts as touches, so head pats still work. Double-click with the mouse does the same. | now |
| C7 | Camera presets | `DecorationCameraPoseMenu.CameraMoveTo(target, distance, angle, 0.5 s)` and `LookToBody(maid, 0.5 s, "Bip01 Pelvis")`. `WfCameraMoveSupport` saves and restores positions. | `stage.cameraPreset('face' | 'bust' | 'full')` animates in 0.45 s. The web and iOS hosts get face / bust / full buttons. | now |
| C8 | Mouse | `mouseControl 1`, `rightClickToRotate 1`, `Mouse ScrollWheel` zoom (desktop builds and the editor). | Left drag orbits, right or middle drag pans, the wheel zooms (only over the stage), double-click resets. | now |
| C9 | Camera roll | `CameraRollController` (Studio): a slider sets `m_fRotZ`, the bank angle. | — | later |
| C10 | Camera lock | `CameraLockButton` (Studio) freezes the orbit while handles are dragged. | `stage.setInteractive(false)`; the studio uses it while sliders are dragged. | now |
| C11 | Model distance adjustment | Settings: `3Dモデルの距離調整`, model distance. | Covered by the zoom (C4). | now |
| C12 | Camera filters | `CameraConfigUI.FilterType`: sepia, monotone, vignette, each with a strength. Bloom strength in the general settings, default 2.85. | The stage already has bloom on the high tier. Sepia and monotone filters are not needed. | later |
| C13 | Wall and floor limits | `CameraOverLimiter` (sphere or cuboid, `LimitUnderY`) keeps the camera out of the floor. | The pitch clamp keeps the camera above the floor, and the distance is limited. | now |

## 2. Touching the character

What the decompilation shows:

- **No touch reactions on her body.** No scene uses `ColliderEvent` or `MaidColliderCollect`; the
  `Grab`/`Touch` capsule colliders are a leftover of the CM3D2 VR code (`OvrTouchHit_`,
  `OvrUseTouch`).
- **No reaction voice lines.** None appear in the string tables or the localisation.
- **Touch input on the stage does three things:** orbit the camera (above), fire flick shortcuts
  (`FlickShortcut`), and take a random pose on the main screen (setting
  `メイン画面でランダムポーズをとる`).

The owner asked for touch reactions, so these are our own design, built on her clips and
expressions:

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| T1 | Tap on her body hits a region | — (VR leftovers only) | `interact/regions.ts`: a raycast on the skinned mesh, then the skin weights of the hit triangle. The heaviest bone is mapped by name (English, MMD Japanese and Mixamo patterns) to head, face, chest, hips, skirt, arm, hand or leg, with left/right. Without bone names, the hit height relative to her head and bounding box decides. | now |
| T2 | Region reactions | — | `interact/reactions.ts`. Each region has a few reactions (a clip from her library when there is one, else a procedural gesture, plus an expression), picked without repeating. **Head:** tilt and smile. **Face:** surprised, then smile. **Chest:** startled, a small "ehi!" and an angry pout. **Hips or skirt:** shy and flustered. **Arm:** looks at it, smiles. **Hand:** waves back. **Leg:** steps, surprised. | now |
| T3 | Head pat | — | A second head tap within 1.6 s is a pat. Pats escalate: happy, then eyes closed with a big smile, then a happy bounce. | now |
| T4 | Too many pokes | — | Four taps on the same non-head region within 4 s: a sigh and a sulky look. | now |
| T5 | Voice lines | — | Each reaction carries a line key (for example `pat`, `poke_chest`). The host turns it into speech only when she is unmuted. A tap is a user gesture, so audio can start. Lines are short, translated (it/en/da), and cached after the first TTS request. | now |
| T6 | Sparkle at the touch point | Particle sparkle prefabs (warm gold) on stage | `env.pulse(point)` at the hit point (the existing burst), when the stage set is on. | now |

## 3. Eyes and head

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| E1 | Eyes to camera | `TBody.boEyeToCam` and `Maid.EyeToCamera(EyeMoveType, fade)`. A settings switch, `カメラ目線` (on by default), and Studio's `SystemMenuManager.ChangeEyeToCam`. `OvrMobileIK.m_fEyeToCamThresholdAngle` stops following past an angle. | She already looks at the camera. New: when the camera passes about 110° to her side or behind her, the target eases back to straight ahead, so her head does not snap from one shoulder to the other. | now |
| E2 | Eyes and head follow the finger | — (the Studio drives gaze with sliders, E3) | While a finger or mouse button is down on the stage, her eyes and head follow it. On release they ease back to the camera after 0.6 s. Hover-following with the mouse stays opt-in (`lookAtPointer`). | now |
| E3 | Gaze sliders | `FaceToObjectManager`: X/Y sliders move a `faceToObject` under `Bip01 Neck`, plus eye-to-cam toggles. | — | later |
| E4 | Face angle correction | Settings `フェイス角度補正` (face tracking). | — (needs face tracking) | later |

## 4. Studio: poses, expressions, scene

`SceneStudioManager` holds the managers: TopMenu, Character, Pose, Face, MaidTransform, BgMenu,
BgChange, BgTransform, Object, ObjectAdd, CameraControll, FaceToObjectControl and FingerControl.

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| S1 | Pose list | `PoseManager`, `PoseList` (`DataBase.Pose`), base poses, `MaidIdleMotionAdditiveHook` for an idle under poses | Studio sheet, Poses tab: every clip of the library (`stage.info.clips`: idle_maid, status_loop, talk poses, greetings…), plus the procedural gestures. Loops hold; one-shots return to idle. | now |
| S2 | Hand shapes | `HandList`, `FingerManager`, finger presets per hand | — | later |
| S3 | IK pose editing | `IKManager`, `IKDragPoint`, `TransformHandle`, limb targets (head, arms, legs, pelvis), save pose | — | later |
| S4 | Expression list | `FaceManager`, `FaceList`, base faces (initial id 100) | Studio sheet, Expressions tab: neutral, smile, sad, surprised, angry, with an intensity slider. | now |
| S5 | Morph sliders, save face | `FaceMorphList`, `UserOriginalFacialExpressionUtil` | — | later |
| S6 | Move or rotate the character | `MaidTransformManager` (position and rotation handles, reset, CharaFocus) | Orbit and pan cover the view. Moving her is not needed. | n/a |
| S7 | Props | `ObjectManager` (add, move, rotate, scale, remove, with a limit) | — | later |
| S8 | Scene save and load | `TopMenuManager` (slots: camera position, distance, angle, bank, plus every manager's state) | The studio remembers its choices (pose, expression, background, light) locally. | now |
| S9 | Screen orientation | `ScreenRotationManager` (portrait, landscape, auto) | The OS handles it. | n/a |

## 5. Photo mode

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| P1 | Hide the UI | `PhotoGraphyManager` fades every UI `CanvasGroup` out; a tap on the hit area brings it back. `CameraMain.UIHide` and `ScreenShot(noUI)`. | Photo mode hides all stage chrome. A tap anywhere brings it back, and a shutter button stays. | now |
| P2 | Capture | `CameraMain.ScreenShot(file, superSize, noUI)` (supersampled), `ThumShot` | `stage.snapshot()` renders one frame and returns a PNG at device resolution. | now |
| P3 | Save or share | `SNSShare` (Twitter, Facebook, Instagram, LINE), `PhotoLibraryExporter`, NativeShare | Web: the Web Share API with the file, else a download. iOS: the share sheet. | now |
| P4 | Frames, stamps, text, paint | `SceneDecoration`: Frame (categories, fit vertical/horizontal/stretch, alpha), Stamp (categories, alpha, mirror, undo/redo), Text (fonts, colours, outline), Paint, presets | — | later |

## 6. Background, rooms, light

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| B1 | Background list | `LiveBgData.BgType`: Bg2D, Bg3D, BgCamera, BgExternal (imported image). `BgListViewer`, `BgChangeManager` (Model2D/Model3D) | Studio sheet, Backgrounds tab: our stage set (nebula), solid colours, and three rooms built from simple three.js geometry (a café, a bedroom, a rooftop at night). `stage.setBackground(id)`. | now |
| B2 | 2D background scaling | `BG_2D_SCALE`: fit, vertical, horizontal, loop | — (ours are 3D or solid) | n/a |
| B3 | Imported image background | BgExternal, `ImportBGButton` | — | later |
| B4 | Lighting | `LightMain` (default intensity 0.95, shadow strength 0.098). `LightConfigUI`: intensity, shadow, colour R/G/B, angle X/Y, reset. | Studio sheet, Light tab: presets (studio, warm, cool, sunset, night) that set the key colour, intensity and direction, with a reset. | now |
| B5 | Room decoration | Studio props (S7) and the 3D background prefabs | Rooms are fixed sets; no editing. | later |

## 7. Dance and live

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| D1 | Dance with music | `CommonDance.LoadDanceScene(scene, danceId, rhythm, lyrics, idxs)`, songs in the `SceneMain/ダンス` terms, karaoke mode, lyrics, benchmark | Dance mode: `dance_loop` (or the procedural bounce), stage lights (coloured spots, a floor glow, sweeping beams). Music plays only on an explicit tap, from a local, royalty-free or owner-provided file (`/v1/assets/dance.mp3` when it exists; otherwise a short synthesised beat). Never autoplays. | now |
| D2 | Abort with a four-finger tap | `SceneMain/ダンス/（4本指タップで中断できます）` | Stop button, Esc, or a tap on the stop pill. | now |
| D3 | Rhythm game | `CustomCast.Utage.RhythmGame` | — | n/a |
| D4 | Multi-character dance | 2+ characters (high-end devices only) | — (one Lucina) | n/a |
| D5 | Live streaming | `LiveMgr`, Nicocas, NDI, `VirtualLiveWindow` | — | n/a |
| D6 | Flick shortcuts | `FlickShortcut`: a touch, then a flick in a direction fires a face, motion or stamp from the ring (`CircleCommandUI`, radius 180, clockwise). Split modes: `Flick_BG`, `BG_Flick`, `All_Flick`, `All_BG` (screen halves for flick vs camera). | — (taps on her already cover reactions) | later |
| D7 | Live stamps | `LiveStampData`, `StampExpand`/`StampShrink` (0.42 s) | — | later |
| D8 | Lip sync from the mic or camera | `LipSyncConfigUI`: voice or video recognition, strength | Her lip sync follows her own TTS. Mirroring the owner's mic is not needed. | n/a |

## 8. AR

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| A1 | Place her in the room | `SceneOutSideAR`, ARKit (`UnityARCameraManager`, `CMARHit`): scan the floor, tap inside the blue frame to place. Deploy modes Move, Rot, Scaling, Live. | iOS: export her current pose as USDZ and open AR Quick Look (no new runtime), or a RealityKit view with the GLB converted at build time. Web: WebXR `immersive-ar` where supported (Android Chrome). | later |
| A2 | AR rotate and scale | `FlickObjRotate` (horizontal flick, speed 5), `FlickObjScaling` (vertical flick, 0.01–3×), two-finger tap resets | Covered by Quick Look's own gestures. | later |
| A3 | 3-DoF AR | `AR3DOFCameraManager` | — | later |

## 9. Face tracking

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| F1 | Face tracking drives her head and mouth | `SceneFaceSetting` plus `CMDlibFaceTracking` (dlib 68 landmarks on the front camera, low-pass on position and rotation, `mouthOpenVal`). Neck-bone driven; a calibration screen with OK and Back. | iOS: ARKit `ARFaceTrackingConfiguration` (TrueDepth) blend shapes → JS `setFaceDrive({yaw, pitch, roll, jawOpen, blink…})`; the rig already has logical morphs for the mouth, blink and smile. Web: MediaPipe Face Landmarker. Camera frames stay on the device. | later |

## 10. Device gestures

| # | Interaction | Custom Cast | Lucina | Status |
| --- | --- | --- | --- | --- |
| G1 | Gyro camera | `GyroCamMove`: the device rotation turns the camera. Dead zone 0.0175 rad/s, limited to about 10° (`0.1736` = sin 10°). Separate switches for the 3D model and the background (`ジャイロカメラ(3Dモデル)`, `(背景)`). | Gyro parallax: the camera offsets up to ±8° yaw and ±5° pitch with the device tilt, low-passed, with a 1°/s dead zone. Web: DeviceOrientation, permission asked on a tap (iOS Safari). iOS: the WKWebView asks the app, and the app grants it only after the owner turns it on. Off by default; off in reduced motion. | now |
| G2 | Shake | — (no shake handler found) | — | n/a |
| G3 | Long press | — | — | n/a |

## 11. Everything else

Title, download and data repair, sign-up and account transfer, shop, gacha, missions, gift codes,
banners, subscription, ads (Adfurikun, Tapjoy), My Page, VRM export and import (`VRMNGUIManager`),
character customisation (`SceneEdit`: parts, colours, body, presets), seed online sharing, help and
hints: **n/a**. Customisation is replaced by the wardrobe (outfits, hair, colour).

## Totals

Counting every row in the tables (section 11 counted as one):

| Status | Count |
| --- | --- |
| now | 30 |
| later | 17 |
| n/a | 10 |

## Implementation notes

- **Code:** everything new is in `packages/lucina-stage/src/interact/`:
  - `orbit.ts`: view state, damping, zoom path, clamps and presets (pure math, unit-tested);
  - `gestures.ts`: pointer tracking → rotate, pinch, pan, tap, double-tap and press;
  - `regions.ts`: skin-weight and height region mapping;
  - `reactions.ts`: the reaction planner (pats and repeats);
  - `controller.ts`: DOM wiring;
  - `studio.ts`: backgrounds, rooms, light presets, dance lights, gyro and snapshot.

  `stage.ts` only gets hooks: a camera override, a gaze override, a frame callback, and the new
  API methods.
- **No scroll hijacking.** The stage canvas sets `touch-action: none` and listens to pointer events
  on itself. The wheel is captured (`preventDefault`) only while the pointer is over the canvas.
  The rest of the page scrolls normally.
- **iOS.** The WKWebView turns off its own pinch zoom, bounce and double-tap zoom: the scroll view
  is disabled with a zoom scale of 1, the page has `user-scalable=no`, `touch-action: none` and
  `-webkit-touch-callout: none`. The stage handles every gesture.
- **Reduced motion.**
  - The orbit still works, with no inertia and no fly-to animation.
  - Reactions keep only the expression change (no clip or gesture).
  - Dance mode shows static lights.
  - Gyro is off.
