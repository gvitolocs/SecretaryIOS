// How a Gallery video is played inside the viewer: which URL to give the <video> element, and when to tell the
// player core where the owner is. The shared player (state/media.ts) plays one queue through a single element;
// the viewer needs one video per page, so it uses these small rules with the same server calls and the same core.

import type { PlayInfo } from "../api/player";

export type VideoSource =
  /** A file the element plays itself (direct play, or a remux served as one file). */
  | { type: "file"; url: string }
  /** An HLS playlist played through hls.js (Chrome, Firefox). */
  | { type: "hls"; url: string }
  /** An HLS playlist the browser plays natively (Safari). */
  | { type: "native-hls"; url: string }
  | { type: "unsupported" };

/**
 * The server plans the playback (`mode`): "direct" is the file itself; anything else comes as an HLS playlist when
 * `hls_url` is set. hls.js wins wherever it runs (it can start at the resume point and seek in a playlist still being
 * written); Safari's own HLS is the fallback.
 */
export function videoSource(play: Pick<PlayInfo, "mode" | "url" | "hls_url">, hlsJs: boolean, nativeHls: boolean): VideoSource {
  if (play.mode !== "direct" && play.hls_url) {
    if (hlsJs) return { type: "hls", url: play.hls_url };
    if (nativeHls) return { type: "native-hls", url: play.hls_url };
    return { type: "unsupported" };
  }
  return play.url ? { type: "file", url: play.url } : { type: "unsupported" };
}

/** Seconds between two reports of the position while a video plays. */
export const REPORT_EVERY_S = 5;

/** Whether a `timeupdate` at `position` should be reported: every few seconds forward, at once after a jump back. */
export function dueReport(position: number, lastReported: number): boolean {
  return position - lastReported >= REPORT_EVERY_S || position < lastReported;
}

/** The seconds to start at: the core's saved position, else the server's, never negative or not a number. */
export function resumeSeconds(coreMs: unknown, serverMs: number): number {
  const ms = typeof coreMs === "number" && Number.isFinite(coreMs) ? coreMs : serverMs;
  return Number.isFinite(ms) && ms > 0 ? ms / 1000 : 0;
}
