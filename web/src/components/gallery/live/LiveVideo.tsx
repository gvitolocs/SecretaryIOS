import { createEffect, on, onCleanup, type Accessor, type JSX } from "solid-js";
import { mediaApi, type PlayInfo } from "../../../api/player";
import { fadeMs, motionVisible, prefersReducedMotion } from "../../../lib/galleryLive";
import { videoSource } from "../../../lib/galleryVideo";
import type { LiveController } from "./liveController";

type Hls = import("hls.js").default;

export interface LiveVideoProps {
  /** The motion half's item id: its play plan is asked for on the first press, not before. */
  pairId: string;
  live: LiveController;
  /** The still's box (PhotoViewer's `renderOverlay` gives it), so the motion lies exactly over the still. */
  style: Accessor<JSX.CSSProperties | undefined>;
  placed: Accessor<boolean>;
}

/**
 * The motion half of a Live Photo, over its still: muted, played once from the start when the controller says so, then
 * faded out (at once with reduced motion). It takes no pointer, so the still keeps panning and zooming. The play
 * plan is the one a video gets (direct file, or HLS through hls.js); a clip that can't be played drops back to the still.
 */
export function LiveVideo(props: LiveVideoProps) {
  let video: HTMLVideoElement | undefined;
  let hls: Hls | null = null;
  let attached = false;
  let token = 0;
  let fadeTimer: ReturnType<typeof setTimeout> | undefined;

  async function attach(mine: number): Promise<void> {
    const v = video;
    if (!v) return;
    const play: PlayInfo = await mediaApi.play(props.pairId);
    if (mine !== token) return;
    const Hls = play.mode !== "direct" && play.hls_url ? (await import("hls.js")).default : null;
    if (mine !== token) return;
    const source = videoSource(play, !!Hls?.isSupported(), v.canPlayType("application/vnd.apple.mpegurl") !== "");
    if (source.type === "unsupported") throw new Error("unsupported");
    if (source.type === "hls" && Hls) {
      hls = new Hls({ autoStartLoad: true });
      hls.loadSource(source.url);
      hls.attachMedia(v);
      hls.on(Hls.Events.ERROR, (_e, data) => {
        if (data.fatal && mine === token) props.live.dispatch({ type: "failed" });
      });
    } else {
      v.src = source.url;
    }
    attached = true;
  }

  async function start(): Promise<void> {
    const mine = ++token;
    try {
      if (!attached) await attach(mine);
      const v = video;
      if (mine !== token || !v) return;
      v.currentTime = 0;
      await v.play();
      if (mine === token) props.live.dispatch({ type: "ready" });
    } catch {
      if (mine === token) props.live.dispatch({ type: "failed" });
    }
  }

  function stop(): void {
    token++;
    clearTimeout(fadeTimer);
    const v = video;
    if (!v || !attached) return;
    v.pause();
    try {
      v.currentTime = 0;
    } catch {
      // Not seekable yet: it starts from the beginning next time anyway.
    }
  }

  createEffect(
    on(
      () => props.live.state().phase,
      (phase) => {
        if (phase === "loading") void start();
        else if (phase === "fading") {
          clearTimeout(fadeTimer);
          fadeTimer = setTimeout(() => props.live.dispatch({ type: "faded" }), fadeMs(prefersReducedMotion()));
        } else if (phase === "idle") stop();
      },
    ),
  );

  onCleanup(() => {
    stop();
    hls?.destroy();
    hls = null;
    if (video) {
      video.removeAttribute("src");
      video.load();
    }
  });

  return (
    <video
      ref={(el) => {
        video = el;
        // The `muted` attribute only counts when the parser makes the element: a script-made one must be muted by property.
        el.muted = true;
      }}
      class="pv-img gl-video"
      classList={{ "pv-img--placed": props.placed(), "gl-video--on": motionVisible(props.live.state()) }}
      style={{ ...props.style(), transition: `opacity ${fadeMs(prefersReducedMotion())}ms ease-out` }}
      playsinline
      preload="none"
      aria-hidden="true"
      tabindex="-1"
      onEnded={() => props.live.dispatch({ type: "ended" })}
      onError={() => {
        if (attached) props.live.dispatch({ type: "failed" });
      }}
    />
  );
}
