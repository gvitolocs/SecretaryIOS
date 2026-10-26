// The model3d block: a card with the thumbnail and "View in 3D"; on click an inline
// three.js viewer (orbit, zoom, auto-turn, animation clips with play/pause). The GLB
// comes from /v1/models/{id}/files/model.glb with the session cookie and is parsed
// by lucina-stage's loader (meshopt, no Draco). three.js loads only on click, the
// render loop stops off screen, and the WebGL context is released on unmount, so a
// long chat of models stays light.

import { createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { getModel, thumbName } from "../api/models";
import { t } from "../i18n";
import type { Model3dBlock, ModelFile } from "../lib/model3d";
import { modelFileUrl } from "../lib/model3d";
import { loadStage } from "../lib/stageLoader";

type Mode = "poster" | "loading" | "3d" | "error" | "nowebgl";

interface Viewer {
  clips: string[];
  play(name: string): void;
  setPaused(p: boolean): void;
  setAutoRotate(on: boolean): void;
  dispose(): void;
}

async function mountViewer(host: HTMLElement, url: string, onInteract: () => void): Promise<Viewer> {
  const [{ loadModel }, THREE, { OrbitControls }, { RoomEnvironment }] = await Promise.all([
    loadStage(),
    import("three"),
    import("three/addons/controls/OrbitControls.js"),
    import("three/addons/environments/RoomEnvironment.js"),
  ]);
  const canvas = document.createElement("canvas");
  canvas.className = "model3d-canvas";
  let renderer: InstanceType<typeof THREE.WebGLRenderer>;
  try {
    renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true });
  } catch {
    throw new Error("nowebgl");
  }
  host.appendChild(canvas);
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 0.9;
  const scene = new THREE.Scene();
  const pmrem = new THREE.PMREMGenerator(renderer);
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environmentIntensity = 0.6;
  scene.add(new THREE.HemisphereLight(0xdfe8ff, 0x1a1d29, 0.6));
  const key = new THREE.DirectionalLight(0xffffff, 1.5);
  key.position.set(3, 5, 4);
  scene.add(key);
  const { gltf } = await loadModel(url, { fetchInit: { credentials: "same-origin" } });
  const model = gltf.scene;
  scene.add(model);
  const box = new THREE.Box3().setFromObject(model, true);
  const size = box.getSize(new THREE.Vector3());
  const center = box.getCenter(new THREE.Vector3());
  model.position.sub(new THREE.Vector3(center.x, box.min.y, center.z));
  const radius = Math.max(size.length() * 0.5, 1e-3);
  const camera = new THREE.PerspectiveCamera(35, 1, radius / 100, radius * 100);
  const controls = new OrbitControls(camera, canvas);
  controls.target.set(0, size.y * 0.5, 0);
  controls.enableDamping = true;
  controls.autoRotate = true;
  controls.autoRotateSpeed = 1.6;
  controls.addEventListener("start", () => {
    controls.autoRotate = false;
    onInteract();
  });
  const dist = (radius / Math.sin(THREE.MathUtils.degToRad(17.5))) * 1.05;
  camera.position.set(0, size.y * 0.5 + dist * 0.2, dist * 0.98);
  controls.update();
  const mixer = new THREE.AnimationMixer(model);
  let action: ReturnType<typeof mixer.clipAction> | null = null;
  const play = (name: string) => {
    const clip = gltf.animations.find((c) => c.name === name);
    if (!clip) return;
    action?.stop();
    action = mixer.clipAction(clip);
    action.reset().play();
  };
  const first = gltf.animations[0];
  if (first) play(first.name);
  const resize = () => {
    const w = host.clientWidth || 320;
    const h = host.clientHeight || 240;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
  };
  resize();
  const ro = typeof ResizeObserver === "function" ? new ResizeObserver(resize) : null;
  ro?.observe(host);
  let visible = true;
  const io = typeof IntersectionObserver === "function" ? new IntersectionObserver((e) => (visible = e.some((x) => x.isIntersecting))) : null;
  io?.observe(host);
  const clock = new THREE.Timer();
  let raf = 0;
  let frames = 0;
  const loop = () => {
    raf = requestAnimationFrame(loop);
    clock.update();
    if (!visible) return;
    try {
      mixer.update(clock.getDelta());
      controls.update();
      renderer.render(scene, camera);
      frames += 1;
      // Frame count and size on the host: cheap to read in tests and when debugging.
      if (frames === 1 || frames % 30 === 0) host.dataset.frames = String(frames);
    } catch (e) {
      host.dataset.error = String(e instanceof Error ? e.message : e).slice(0, 200);
    }
  };
  host.dataset.size = `${size.x.toFixed(2)}x${size.y.toFixed(2)}x${size.z.toFixed(2)}`;
  loop();
  return {
    clips: gltf.animations.map((c) => c.name),
    play,
    setPaused: (p) => {
      if (action) action.paused = p;
    },
    setAutoRotate: (on) => {
      controls.autoRotate = on;
    },
    dispose: () => {
      cancelAnimationFrame(raf);
      ro?.disconnect();
      io?.disconnect();
      controls.dispose();
      mixer.stopAllAction();
      model.traverse((o) => {
        const m = o as { geometry?: { dispose(): void }; material?: { dispose(): void } | { dispose(): void }[] };
        m.geometry?.dispose();
        for (const mat of Array.isArray(m.material) ? m.material : m.material ? [m.material] : []) mat.dispose();
      });
      pmrem.dispose();
      renderer.dispose();
      renderer.forceContextLoss();
      canvas.remove();
    },
  };
}

export default function Model3dViewer(props: { block: Model3dBlock; open?: boolean }) {
  const [mode, setMode] = createSignal<Mode>("poster");
  const [clips, setClips] = createSignal<string[]>([]);
  const [clip, setClip] = createSignal("");
  const [paused, setPaused] = createSignal(false);
  const [turning, setTurning] = createSignal(true);
  const [meta] = createResource(() => props.block.id, (id) => getModel(id).catch(() => null));
  const thumb = () => {
    const m = meta();
    const n = m ? thumbName(m) : null;
    return n ? modelFileUrl(props.block.id, n) : null;
  };
  const files = () => new Set((meta()?.files ?? []).map((f) => f.name));
  // The canvas lives in its own element: Solid never touches its children (a <Show>
  // next to it may clear its parent when it unmounts).
  let host!: HTMLDivElement;
  let viewer: Viewer | null = null;
  onCleanup(() => viewer?.dispose());

  async function open() {
    if (mode() === "loading" || mode() === "3d") return;
    setMode("loading");
    try {
      viewer = await mountViewer(host, modelFileUrl(props.block.id, "model.glb"), () => setTurning(false));
      setClips(viewer.clips);
      setClip(viewer.clips[0] ?? "");
      setMode("3d");
    } catch (e) {
      setMode(e instanceof Error && e.message === "nowebgl" ? "nowebgl" : "error");
    }
  }
  if (props.open) queueMicrotask(() => void open());

  const subtitle = () => {
    const parts: string[] = [];
    if (props.block.animations?.length) parts.push(props.block.animations.join(", "));
    if (props.block.credits !== undefined) parts.push(t("models.credits", { count: props.block.credits }));
    return parts.join(" · ");
  };

  return (
    <figure class="model3d" data-testid="model3d" data-model-id={props.block.id}>
      <div class="model3d-stage" classList={{ "model3d-stage--live": mode() === "3d" }}>
        <div class="model3d-gl" ref={host} />
        <Show when={mode() !== "3d"}>
          <button type="button" class="model3d-poster" onClick={() => void open()} aria-label={`${t("models.view3d")}: ${props.block.title}`}>
            <Show when={thumb()} fallback={<span class="model3d-cube" aria-hidden="true">🧊</span>}>
              {(src) => <img src={src()} alt="" loading="lazy" />}
            </Show>
            <span class="model3d-cta">
              {mode() === "loading" ? t("models.loading3d") : mode() === "error" ? t("models.failed") : mode() === "nowebgl" ? t("models.noWebgl") : `▶ ${t("models.view3d")}`}
            </span>
          </button>
        </Show>
      </div>
      <figcaption class="model3d-caption">
        <span class="model3d-title">{props.block.title}</span>
        <Show when={subtitle()}>
          <span class="model3d-sub">{subtitle()}</span>
        </Show>
        <Show when={mode() === "3d"}>
          <div class="model3d-controls">
            <Show when={clips().length > 0}>
              <label class="model3d-clip">
                <span class="sr-only">{t("models.clip")}</span>
                <select
                  value={clip()}
                  onChange={(e) => {
                    setClip(e.currentTarget.value);
                    viewer?.play(e.currentTarget.value);
                    setPaused(false);
                  }}
                >
                  <For each={clips()}>{(c) => <option value={c}>{c.replace(/_inplace$/i, " (in place)").replace(/_/g, " ")}</option>}</For>
                </select>
              </label>
              <button
                type="button"
                class="btn btn--small"
                onClick={() => {
                  const p = !paused();
                  setPaused(p);
                  viewer?.setPaused(p);
                }}
              >
                {paused() ? t("models.play") : t("models.pause")}
              </button>
            </Show>
            <label class="model3d-turn">
              <input
                type="checkbox"
                checked={turning()}
                onChange={(e) => {
                  setTurning(e.currentTarget.checked);
                  viewer?.setAutoRotate(e.currentTarget.checked);
                }}
              />
              {t("models.autoRotate")}
            </label>
          </div>
        </Show>
        <div class="model3d-links">
          <a href={modelFileUrl(props.block.id, "model.glb")} download={`${props.block.title}.glb`}>
            {t("models.download", { format: "GLB" })}
          </a>
          <For each={(props.block.formats ?? []).filter((f) => f !== "glb" && files().has(`model.${f}`))}>
            {(f) =>
              f === "usdz" ? (
                <a rel="ar" href={modelFileUrl(props.block.id, "model.usdz")}>
                  <Show when={thumb()}>{(src) => <img class="sr-only" src={src()} alt="" />}</Show>
                  {t("models.arQuickLook")}
                </a>
              ) : (
                <a href={modelFileUrl(props.block.id, `model.${f}` as ModelFile)} download={`${props.block.title}.${f}`}>
                  {t("models.download", { format: f.toUpperCase() })}
                </a>
              )
            }
          </For>
          <a href="/models">{t("models.gallery")}</a>
        </div>
      </figcaption>
    </figure>
  );
}
