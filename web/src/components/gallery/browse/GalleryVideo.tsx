import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { mediaApi, type PlayInfo } from "../../../api/player";
import { toApiError } from "../../../api/client";
import { t } from "../../../i18n";
import { dueReport, resumeSeconds, videoSource } from "../../../lib/galleryVideo";
import { playerCore } from "../../../lib/playerCore";
import { pushProgress } from "../../../state/media";
import "../../../styles/gallery-browse.css";

type Hls = import("hls.js").default;

export interface GalleryVideoProps {
  id: string;
  poster?: string | null;
  durationMs?: number | null;
  /** Only the video on screen asks the server for a play URL; the neighbours show their poster. */
  active: boolean;
}

/**
 * A Gallery video inside the viewer (PhotoViewer's `renderVideo`): the server's tokenised play URL, HLS through hls.js
 * where the browser has no HLS of its own, native controls, the poster as the still frame, and the player core's
 * resume position (the same engine as the iPhone, so "where I stopped" agrees across devices). It does not start by
 * itself: a swipe to a video must not make sound.
 */
export function GalleryVideo(props: GalleryVideoProps) {
  let video: HTMLVideoElement | undefined;
  let hls: Hls | null = null;
  let token = 0;
  let loadedId: string | null = null;
  let lastReport = 0;
  /** The length the server gave, for a stream whose element does not know it yet. */
  let durationHintMs = 0;
  const [failed, setFailed] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [poster, setPoster] = createSignal<string | null>(null);

  const posterUrl = () => poster() ?? props.poster ?? undefined;

  async function report(playing: boolean): Promise<void> {
    const v = video;
    const id = loadedId;
    if (!v || !id) return;
    // Read the element now: a release unloads it right after (and `load()` rewinds it to zero).
    const positionMs = Math.round(v.currentTime * 1000);
    const durationMs = Math.round((Number.isFinite(v.duration) && v.duration > 0 ? v.duration : durationHintMs / 1000) * 1000);
    if (!durationMs && !positionMs) return;
    lastReport = v.currentTime;
    const core = await playerCore();
    const tick = core.request({ cmd: "position", id, position_ms: positionMs, duration_ms: durationMs, playing, device: "web" });
    if (tick.sync) void pushProgress();
  }

  function release(): void {
    token++;
    void report(false).catch(() => undefined);
    hls?.destroy();
    hls = null;
    if (video) {
      video.pause();
      video.removeAttribute("src");
      video.load();
    }
    loadedId = null;
  }

  async function load(id: string): Promise<void> {
    const mine = ++token;
    setFailed(null);
    setLoading(true);
    try {
      const play: PlayInfo = await mediaApi.play(id);
      if (mine !== token || !video) return;
      const core = await playerCore();
      if (play.progress) core.request({ cmd: "merge", entries: [play.progress] });
      const resume = resumeSeconds(core.request({ cmd: "resume", id, duration_ms: play.duration_ms ?? undefined }).position_ms, play.resume_ms);
      setPoster(play.poster_url);
      durationHintMs = play.duration_ms ?? props.durationMs ?? 0;
      const v = video;
      const Hls = play.mode !== "direct" && play.hls_url ? (await import("hls.js")).default : null;
      if (mine !== token) return;
      const source = videoSource(play, !!Hls?.isSupported(), v.canPlayType("application/vnd.apple.mpegurl") !== "");
      if (source.type === "unsupported") throw new Error(t("browse.videoUnsupported"));
      loadedId = id;
      if (source.type === "hls" && Hls) {
        hls = new Hls({ startPosition: resume > 0 ? resume : -1, autoStartLoad: true });
        hls.loadSource(source.url);
        hls.attachMedia(v);
        hls.on(Hls.Events.ERROR, (_e, data) => {
          if (data.fatal && mine === token) setFailed(t("browse.videoError"));
        });
      } else {
        v.src = source.url;
        if (resume > 0) v.addEventListener("loadedmetadata", () => { if (mine === token) v.currentTime = resume; }, { once: true });
      }
    } catch (e) {
      if (mine === token) setFailed(toApiError(e).message || t("browse.videoError"));
    } finally {
      if (mine === token) setLoading(false);
    }
  }

  createEffect(() => {
    const id = props.id;
    if (props.active) void load(id);
    else if (loadedId) release();
    onCleanup(() => {
      if (loadedId) release();
    });
  });

  return (
    <div class="gv-video">
      <video
        ref={video}
        class="pv-video"
        controls
        playsinline
        preload="metadata"
        poster={posterUrl()}
        onPause={() => void report(false)}
        onSeeked={() => void report(!!video && !video.paused)}
        onEnded={() => void report(false)}
        onTimeUpdate={() => {
          if (video && !video.paused && dueReport(video.currentTime, lastReport)) void report(true);
        }}
        onError={() => {
          if (loadedId && video?.error) setFailed(t("browse.videoError"));
        }}
      />
      <Show when={loading() && !failed()}>
        <span class="gv-video__spinner" role="status" aria-label={t("browse.videoLoading")} />
      </Show>
      <Show when={failed()}>
        {(message) => (
          <div class="gv-video__error" role="alert">
            <p>{message()}</p>
            <button type="button" class="pv-btn pv-text" onClick={() => void load(props.id)}>
              {t("browse.retry")}
            </button>
          </div>
        )}
      </Show>
    </div>
  );
}
