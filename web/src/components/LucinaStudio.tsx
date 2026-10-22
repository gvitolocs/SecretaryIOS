import type { WardrobeStage } from "@secretary/lucina-stage";
import { createSignal, For, onCleanup, Show } from "solid-js";
import { t } from "../i18n";
import { readString, writeString } from "../lib/storage";

/** Clips that look wrong on the live model (broken hands): never offered as poses. */
const BLOCKED_POSES = new Set(["idle_maid", "bow", "shy", "sit_idle", "think_chin"]);
const EXPRESSIONS = ["smile", "sad", "surprised", "angry"] as const;
const BACKGROUNDS = ["default", "color:#101426", "color:#f4f1ec", "color:#ffd6e5", "color:#cfe8ff", "room:cafe", "room:bedroom", "room:rooftop"] as const;
const LIGHTS = ["default", "studio", "warm", "cool", "sunset", "night"] as const;
const BG_KEY = "secretary.studio.background";
const LIGHT_KEY = "secretary.studio.lighting";
/** Optional owner-provided dance track (served behind the session); otherwise the built-in beat. */
const DANCE_TRACK = "/v1/assets/dance.mp3";

type Tab = "view" | "poses" | "face" | "background" | "light";

type Key = Parameters<typeof t>[0];
const BG_LABEL: Record<string, Key> = {
  default: "studio.bgStage",
  "color:#101426": "studio.bgNight",
  "color:#f4f1ec": "studio.bgPaper",
  "color:#ffd6e5": "studio.bgPink",
  "color:#cfe8ff": "studio.bgSky",
  "room:cafe": "studio.roomCafe",
  "room:bedroom": "studio.roomBedroom",
  "room:rooftop": "studio.roomRooftop",
};
const LIGHT_LABEL: Record<string, Key> = {
  default: "studio.lightDefault",
  studio: "studio.lightStudio",
  warm: "studio.lightWarm",
  cool: "studio.lightCool",
  sunset: "studio.lightSunset",
  night: "studio.lightNight",
};
const EXPR_LABEL: Record<string, Key> = {
  smile: "studio.exprSmile",
  sad: "studio.exprSad",
  surprised: "studio.exprSurprised",
  angry: "studio.exprAngry",
};

/** Applies the remembered background and light to a new stage. */
export function restoreStudio(stage: WardrobeStage): void {
  try {
    const bg = readString("local", BG_KEY);
    if (bg && bg !== "default") stage.setBackground(bg);
    const light = readString("local", LIGHT_KEY);
    if (light && light !== "default") stage.setLighting(light as never);
  } catch {
    // A stale value: start plain.
  }
}

function poseLabel(name: string): string {
  return name.replace(/_/g, " ");
}

/**
 * Lucina's studio (Custom Cast's Studio, our own UI): camera views, poses, expressions,
 * backgrounds, light, photo mode, dance mode and gyro parallax. Music and motion sensors only
 * ever start from a tap here.
 */
export function LucinaStudio(props: {
  stage: () => WardrobeStage | null;
  muted: () => boolean;
  fetchInit: RequestInit;
  onClose: () => void;
  onPhoto: () => void;
}) {
  const [tab, setTab] = createSignal<Tab>("view");
  const [expr, setExpr] = createSignal<string | null>(null);
  const [bg, setBg] = createSignal<string>(props.stage()?.background ?? "default");
  const [light, setLight] = createSignal<string>(props.stage()?.lighting ?? "default");
  const [dancing, setDancing] = createSignal(props.stage()?.dancing ?? false);
  const [gyro, setGyro] = createSignal(props.stage()?.gyro ?? false);
  const [pose, setPose] = createSignal<string | null>(null);
  const gyroAvailable = typeof window !== "undefined" && "DeviceOrientationEvent" in window && "ontouchstart" in window;

  const poses = () => {
    const s = props.stage();
    if (!s) return [] as { name: string; clip: boolean; loop: boolean }[];
    const clips = (s.info.clips ?? []).filter((c) => !BLOCKED_POSES.has(c.name) && c.name !== "dance_loop").map((c) => ({ name: c.name, clip: true, loop: c.loop }));
    const gestures = (s.gestures ?? []).filter((g) => g !== "dance").map((g) => ({ name: g, clip: false, loop: false }));
    return [...clips, ...gestures];
  };

  function playPose(p: { name: string; clip: boolean; loop: boolean }) {
    const s = props.stage();
    if (!s) return;
    setPose(p.name);
    if (p.clip) void s.play(p.name, { loop: p.loop }).catch(() => {});
    else s.gesture(p.name);
  }

  function chooseExpr(e: string | null) {
    setExpr(e);
    props.stage()?.setExpression(e as never, 1);
  }

  function chooseBg(id: string) {
    setBg(id);
    props.stage()?.setBackground(id);
    writeString("local", BG_KEY, id);
  }

  function chooseLight(id: string) {
    setLight(id);
    props.stage()?.setLighting(id as never);
    writeString("local", LIGHT_KEY, id);
  }

  async function toggleDance() {
    const s = props.stage();
    if (!s) return;
    if (s.dancing) {
      setDancing(false);
      await s.setDance(false);
      return;
    }
    setDancing(true);
    // Inside the tap: audio may start. Muted: lights and moves only.
    void s.unlockAudio().catch(() => {});
    let music: boolean | Blob = !props.muted();
    if (music) {
      try {
        const r = await fetch(DANCE_TRACK, props.fetchInit);
        if (r.ok && (r.headers.get("content-type") ?? "").startsWith("audio/")) music = await r.blob();
      } catch {
        // No track: the built-in beat.
      }
    }
    await s.setDance(true, music ? { music } : {});
  }

  async function toggleGyro() {
    const s = props.stage();
    if (!s) return;
    if (gyro()) {
      s.setGyro(false);
      setGyro(false);
      return;
    }
    const D = (window as unknown as { DeviceOrientationEvent?: { requestPermission?: () => Promise<string> } }).DeviceOrientationEvent;
    let ok = true;
    if (D && typeof D.requestPermission === "function") ok = (await D.requestPermission().catch(() => "denied")) === "granted";
    if (!ok) return;
    s.setGyro(true);
    setGyro(s.gyro);
  }

  onCleanup(() => props.stage()?.setInteractive(true));

  const tabs: { id: Tab; label: Key }[] = [
    { id: "view", label: "studio.tabView" },
    { id: "poses", label: "studio.tabPoses" },
    { id: "face", label: "studio.tabFace" },
    { id: "background", label: "studio.tabBackground" },
    { id: "light", label: "studio.tabLight" },
  ];

  return (
    <div class="stage-studio" role="dialog" aria-label={t("studio.title")}>
      <div class="stage-studio-tabs" role="tablist">
        <For each={tabs}>
          {(x) => (
            <button type="button" role="tab" class="studio-tab" aria-selected={tab() === x.id ? "true" : "false"} onClick={() => setTab(x.id)}>
              {t(x.label)}
            </button>
          )}
        </For>
        <button type="button" class="studio-close" onClick={() => props.onClose()} aria-label={t("studio.close")}>
          ×
        </button>
      </div>
      <div class="stage-studio-chips">
        <Show when={tab() === "view"}>
          <button type="button" class="studio-chip" onClick={() => props.stage()?.cameraPreset("face")}>{t("shell.viewFace")}</button>
          <button type="button" class="studio-chip" onClick={() => props.stage()?.cameraPreset("bust")}>{t("shell.viewBust")}</button>
          <button type="button" class="studio-chip" onClick={() => props.stage()?.cameraPreset("full")}>{t("shell.viewFull")}</button>
          <button type="button" class="studio-chip" onClick={() => props.stage()?.resetView()}>{t("shell.viewReset")}</button>
        </Show>
        <Show when={tab() === "poses"}>
          <For each={poses()} fallback={<span class="studio-empty">{t("studio.noPoses")}</span>}>
            {(p) => (
              <button type="button" class="studio-chip" aria-pressed={pose() === p.name ? "true" : "false"} onClick={() => playPose(p)}>
                {poseLabel(p.name)}
              </button>
            )}
          </For>
        </Show>
        <Show when={tab() === "face"}>
          <button type="button" class="studio-chip" aria-pressed={expr() === null ? "true" : "false"} onClick={() => chooseExpr(null)}>
            {t("studio.exprNeutral")}
          </button>
          <For each={EXPRESSIONS}>
            {(e) => (
              <button type="button" class="studio-chip" aria-pressed={expr() === e ? "true" : "false"} onClick={() => chooseExpr(e)}>
                {t(EXPR_LABEL[e]!)}
              </button>
            )}
          </For>
        </Show>
        <Show when={tab() === "background"}>
          <For each={BACKGROUNDS}>
            {(id) => (
              <button type="button" class="studio-chip" aria-pressed={bg() === id ? "true" : "false"} onClick={() => chooseBg(id)}>
                <Show when={id.startsWith("color:")}>
                  <span class="studio-swatch" data-color={id.slice(6)} />
                </Show>
                {t(BG_LABEL[id]!)}
              </button>
            )}
          </For>
        </Show>
        <Show when={tab() === "light"}>
          <For each={LIGHTS}>
            {(id) => (
              <button type="button" class="studio-chip" aria-pressed={light() === id ? "true" : "false"} onClick={() => chooseLight(id)}>
                {t(LIGHT_LABEL[id]!)}
              </button>
            )}
          </For>
        </Show>
      </div>
      <div class="stage-studio-actions">
        <button type="button" class="studio-action" onClick={() => props.onPhoto()}>
          {t("studio.photo")}
        </button>
        <button type="button" class="studio-action" aria-pressed={dancing() ? "true" : "false"} onClick={() => void toggleDance()}>
          {dancing() ? t("studio.danceStop") : t("studio.dance")}
        </button>
        <Show when={gyroAvailable}>
          <button type="button" class="studio-action" aria-pressed={gyro() ? "true" : "false"} onClick={() => void toggleGyro()}>
            {t("studio.gyro")}
          </button>
        </Show>
      </div>
    </div>
  );
}

/** Saves or shares a PNG of her (Web Share with the file when possible, else a download). */
export async function savePhoto(blob: Blob): Promise<"shared" | "downloaded" | "cancelled"> {
  const name = `lucina-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-")}.png`;
  const file = typeof File === "function" ? new File([blob], name, { type: "image/png" }) : null;
  const nav = navigator as Navigator & { canShare?: (d: ShareData) => boolean };
  if (file && nav.share && nav.canShare?.({ files: [file] })) {
    try {
      await nav.share({ files: [file], title: "Lucina" });
      return "shared";
    } catch (e) {
      if ((e as { name?: string }).name === "AbortError") return "cancelled";
    }
  }
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
  return "downloaded";
}
