// Playback of the owner's videos and audio from the server (docs/media-player.md).
//
// The browser plays: one <video> element (it plays audio too) for the whole page, so sound
// continues while the owner moves to other tabs; the media view borrows it while open.
// hls.js is loaded only where the browser has no HLS of its own (Chrome, Firefox).
// The player core (lib/playerCore.ts, the same Rust engine as the iPhone) owns the queue,
// resume positions and their sync, subtitle parsing and the remembered subtitle choice.

import { batch, createSignal } from "solid-js";
import { fetchSubtitles, mediaApi, type MediaItem, type PlayInfo, type ProgressEntry, type SubtitleTrack } from "../api/media";
import { toApiError } from "../api/client";
import { playerCore, type PlayerCore } from "../lib/playerCore";

type Hls = import("hls.js").default;

interface QueueView {
  items: MediaItem[];
  current: number | null;
  has_next: boolean;
  has_prev: boolean;
}

interface Cue {
  start_ms: number;
  end_ms: number;
  text: string;
}

const [queue, setQueue] = createSignal<MediaItem[]>([]);
const [currentIndex, setCurrentIndex] = createSignal<number | null>(null);
const [hasNext, setHasNext] = createSignal(false);
const [info, setInfo] = createSignal<PlayInfo | null>(null);
const [loading, setLoading] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);
const [playing, setPlaying] = createSignal(false);
const [position, setPosition] = createSignal(0);
const [duration, setDuration] = createSignal(0);
const [subtitle, setSubtitle] = createSignal<SubtitleTrack | null>(null);
const [subtitleStatus, setSubtitleStatus] = createSignal<string | null>(null);
const [rate, setRateSignal] = createSignal(1);
const [open, setOpen] = createSignal(false);
/** Bumped when saved positions change, so lists redraw their progress bars. */
const [revision, setRevision] = createSignal(0);

export const media = {
  queue,
  currentIndex,
  current: () => {
    const i = currentIndex();
    return i === null ? null : (queue()[i] ?? null);
  },
  hasNext,
  info,
  loading,
  error,
  playing,
  position,
  duration,
  subtitle,
  subtitleStatus,
  rate,
  open,
  setOpen,
  revision,
};

let video: HTMLVideoElement | null = null;
let holder: HTMLDivElement | null = null;
let hls: Hls | null = null;
let core: PlayerCore | null = null;
let lastReport = 0;
let pushing = false;
let loadToken = 0;
let subtitleToken = 0;
let textTrack: TextTrack | null = null;

async function getCore(): Promise<PlayerCore> {
  core ??= await playerCore();
  return core;
}

/** The page's one media element, created on first use. */
export function mediaElement(): HTMLVideoElement {
  if (video) return video;
  video = document.createElement("video");
  video.className = "media-video";
  video.controls = true;
  video.playsInline = true;
  video.preload = "metadata";
  video.addEventListener("play", () => setPlaying(true));
  video.addEventListener("pause", () => {
    setPlaying(false);
    report(false);
  });
  video.addEventListener("timeupdate", () => {
    const v = video!;
    setPosition(v.currentTime);
    // A playlist the server is still writing reports only what exists so far; the server's
    // probe of the file knows the whole length.
    if (Number.isFinite(v.duration) && v.duration > 0 && !info()?.duration_ms) setDuration(v.duration);
    if (!v.paused && (v.currentTime - lastReport >= 1 || v.currentTime < lastReport)) report(true);
  });
  video.addEventListener("seeked", () => report(!video!.paused));
  video.addEventListener("ratechange", () => setRateSignal(video!.playbackRate));
  video.addEventListener("ended", () => void itemEnded());
  video.addEventListener("error", () => {
    if (video?.error && media.current()) setError(video.error.message || "This file can't be played.");
  });
  park();
  return video;
}

/** Keeps the element in the document (hidden) while no view shows it, so audio continues. */
function park(): void {
  if (!video) return;
  holder ??= Object.assign(document.createElement("div"), { hidden: true, className: "media-parking" });
  if (!holder.isConnected) document.body.appendChild(holder);
  if (video.parentElement !== holder) holder.appendChild(video);
}

/** Shows the element inside `container` (the player view); `release` parks it again. */
export function attachElement(container: HTMLElement): () => void {
  const v = mediaElement();
  container.appendChild(v);
  return () => {
    if (v.parentElement === container) park();
  };
}

function apply(view: QueueView): void {
  batch(() => {
    setQueue(view.items);
    setCurrentIndex(view.current);
    setHasNext(view.has_next);
  });
}

export async function playQueue(items: MediaItem[], index: number): Promise<void> {
  report(false);
  const c = await getCore();
  const answer = c.request({ cmd: "queue_set", items, start: index });
  apply(answer.queue as QueueView);
  await load();
}

export async function next(): Promise<void> {
  report(false);
  const answer = (await getCore()).request({ cmd: "queue_next", auto: false });
  if (answer.moved) {
    apply(answer.queue as QueueView);
    await load();
  }
}

export async function previous(): Promise<void> {
  const answer = (await getCore()).request({ cmd: "queue_prev", position_ms: Math.round(position() * 1000) });
  if (answer.prev === "moved") {
    report(false);
    apply(answer.queue as QueueView);
    await load();
  } else {
    seek(0);
  }
}

async function itemEnded(): Promise<void> {
  setPosition(Math.max(position(), duration()));
  report(false);
  const answer = (await getCore()).request({ cmd: "queue_next", auto: true });
  if (answer.moved) {
    apply(answer.queue as QueueView);
    await load();
  }
}

export function toggle(): void {
  const v = mediaElement();
  if (v.paused) void v.play().catch(() => undefined);
  else v.pause();
}

export function seek(seconds: number): void {
  const v = mediaElement();
  v.currentTime = Math.max(0, duration() > 0 ? Math.min(seconds, duration()) : seconds);
}

export function setRate(r: number): void {
  mediaElement().playbackRate = r;
  setRateSignal(r);
}

export function stop(): void {
  report(false);
  loadToken++;
  const v = mediaElement();
  v.pause();
  hls?.destroy();
  hls = null;
  v.removeAttribute("src");
  v.load();
  batch(() => {
    setInfo(null);
    setCurrentIndex(null);
    setSubtitle(null);
    setOpen(false);
  });
  if ("mediaSession" in navigator) navigator.mediaSession.metadata = null;
}

async function load(): Promise<void> {
  const item = media.current();
  if (!item) return;
  const token = ++loadToken;
  const v = mediaElement();
  v.pause();
  hls?.destroy();
  hls = null;
  clearSubtitleTrack();
  batch(() => {
    setInfo(null);
    setError(null);
    setSubtitle(null);
    setSubtitleStatus(null);
    setPosition(0);
    setDuration((item.duration_ms ?? 0) / 1000);
    setLoading(true);
  });
  try {
    const play = await mediaApi.play(item.id);
    if (token !== loadToken) return;
    const c = await getCore();
    if (play.progress) c.request({ cmd: "merge", entries: [play.progress] });
    const resumeMs = Number(c.request({ cmd: "resume", id: item.id, duration_ms: play.duration_ms ?? undefined }).position_ms ?? play.resume_ms);
    batch(() => {
      setInfo(play);
      if (play.duration_ms) setDuration(play.duration_ms / 1000);
    });
    v.poster = play.poster_url ?? "";
    v.playbackRate = rate();
    await attachSource(v, play, resumeMs / 1000, token);
    if (token !== loadToken) return;
    await v.play().catch(() => undefined);
    updateMediaSession(item, play);
    const pick = c.request({ cmd: "subtitle_pick", id: item.id, tracks: play.subtitles }).track as string | null;
    const track = play.subtitles.find((t) => t.id === pick);
    if (track) void selectSubtitle(track, false);
  } catch (e) {
    if (token === loadToken) setError(toApiError(e).message);
  } finally {
    if (token === loadToken) setLoading(false);
  }
}

/** Direct files use `src`. HLS goes through hls.js wherever MediaSource exists: it starts at the
 *  resume point and seeks inside a playlist the server is still writing, which native HLS
 *  (Safari, and Chrome since it plays HLS itself) reports as unseekable until it ends. */
async function attachSource(v: HTMLVideoElement, play: PlayInfo, resume: number, token: number): Promise<void> {
  const isHls = play.mode !== "direct" && play.hls_url;
  const Hls = isHls ? (await import("hls.js")).default : null;
  if (token !== loadToken) return;
  if (Hls && !Hls.isSupported() && !v.canPlayType("application/vnd.apple.mpegurl")) throw new Error("This browser can't play this file.");
  if (Hls?.isSupported()) {
    hls = new Hls({ startPosition: resume > 0 ? resume : -1 });
    hls.loadSource(play.hls_url!);
    hls.attachMedia(v);
    await new Promise<void>((resolve, reject) => {
      hls!.once(Hls.Events.MANIFEST_PARSED, () => resolve());
      hls!.once(Hls.Events.ERROR, (_e, data) => data.fatal && reject(new Error(data.details)));
    });
    // hls.js starts at the resume point when it is encoded already; otherwise wait for it.
    if (resume > 0) {
      if (v.readyState < HTMLMediaElement.HAVE_METADATA) await new Promise((r) => v.addEventListener("loadedmetadata", r, { once: true }));
      if (Math.abs(v.currentTime - resume) > 3) await seekWhenAvailable(v, resume, token);
    }
    return;
  }
  v.src = play.url;
  await new Promise<void>((resolve, reject) => {
    v.addEventListener("loadedmetadata", () => resolve(), { once: true });
    v.addEventListener("error", () => reject(new Error(v.error?.message || "This file can't be played.")), { once: true });
  });
  if (resume > 0) await seekWhenAvailable(v, resume, token);
}

/** Seeks to `target`, waiting (up to 30 s) while an HLS remux is still being written. */
async function seekWhenAvailable(v: HTMLVideoElement, target: number, token: number): Promise<void> {
  for (let i = 0; i < 60 && token === loadToken; i++) {
    const end = v.seekable.length ? v.seekable.end(v.seekable.length - 1) : 0;
    if (end >= target - 1) {
      v.currentTime = target;
      return;
    }
    await new Promise((r) => setTimeout(r, 500));
  }
}

/** Hides the shown track and stops its polling (text tracks cannot be removed from an element). */
function clearSubtitleTrack(): void {
  subtitleToken++;
  if (textTrack) textTrack.mode = "disabled";
  textTrack = null;
}

/** Shows `track` (null = off) as native cues, so they also show in full screen. A choice
 *  from the menu is remembered for this file, and its language becomes the default. */
export async function selectSubtitle(track: SubtitleTrack | null, remember = true): Promise<void> {
  const item = media.current();
  const c = await getCore();
  if (remember && item) c.request({ cmd: "subtitle_choose", id: item.id, track: track?.id ?? null, lang: track?.lang ?? null });
  clearSubtitleTrack();
  const token = subtitleToken;
  batch(() => {
    setSubtitle(track);
    setSubtitleStatus(track ? (track.kind === "image" ? "reading" : "loading") : null);
  });
  if (!track) return;
  const v = mediaElement();
  const tt = v.addTextTrack("subtitles", track.label, track.lang ?? "");
  tt.mode = "showing";
  textTrack = tt;
  let shown = 0;
  let failures = 0;
  while (token === subtitleToken) {
    try {
      const got = await fetchSubtitles(track.url);
      if (token !== subtitleToken) return;
      if (got.state === "preparing") {
        await new Promise((r) => setTimeout(r, 5000));
        continue;
      }
      const cues = (c.request({ cmd: "subtitles", text: got.text }).cues ?? []) as Cue[];
      // Partial OCR results grow at the end: add only the new cues.
      for (const cue of cues.slice(shown)) tt.addCue(new VTTCue(cue.start_ms / 1000, cue.end_ms / 1000, cue.text));
      shown = Math.max(shown, cues.length);
      if (!got.partial) {
        setSubtitleStatus(null);
        return;
      }
      setSubtitleStatus(shown ? null : "reading");
      failures = 0;
      await new Promise((r) => setTimeout(r, 15000));
    } catch (e) {
      if (++failures >= 3) {
        if (token === subtitleToken) {
          setSubtitleStatus(null);
          setError(toApiError(e).message);
        }
        return;
      }
      await new Promise((r) => setTimeout(r, 5000));
    }
  }
}

/** Tells the core where we are; pushes to the server when it says so. */
function report(isPlaying: boolean): void {
  const item = media.current();
  if (!core || !item || !info() || item.kind === "photo") return;
  lastReport = position();
  const tick = core.request({
    cmd: "position",
    id: item.id,
    position_ms: Math.round(position() * 1000),
    duration_ms: Math.round(duration() * 1000),
    playing: isPlaying,
    device: "web",
  });
  if (tick.sync) {
    setRevision((r) => r + 1);
    void pushProgress();
  }
}

export async function pushProgress(): Promise<void> {
  if (pushing) return;
  const c = await getCore();
  const pending = (c.request({ cmd: "pending" }).entries ?? []) as ProgressEntry[];
  if (!pending.length) return;
  pushing = true;
  try {
    const answer = await mediaApi.push(pending);
    c.request({ cmd: "acknowledge", entries: answer.acks });
    if (answer.entries.length) c.request({ cmd: "merge", entries: answer.entries });
    setRevision((r) => r + 1);
  } catch {
    // Offline: the core keeps them pending for the next push.
  } finally {
    pushing = false;
  }
}

/** Adopts positions saved by other devices, then pushes ours. */
export async function syncProgress(): Promise<void> {
  try {
    const c = await getCore();
    const entries = await mediaApi.progress(0);
    const changed = (c.request({ cmd: "merge", entries }).changed ?? []) as string[];
    if (changed.length) setRevision((r) => r + 1);
  } catch {
    // The list still works without them.
  }
  await pushProgress();
}

/** Fraction played for a list row, or null when unseen or finished. */
export function progressOf(item: MediaItem): number | null {
  revision();
  if (!core) return null;
  const entry = core.request({ cmd: "resume", id: item.id }).entry as ProgressEntry | null;
  if (!entry || entry.finished || entry.position_ms <= 0) return null;
  const total = entry.duration_ms > 0 ? entry.duration_ms : (item.duration_ms ?? 0);
  return total > 0 ? Math.min(1, entry.position_ms / total) : null;
}

export function isFinished(item: MediaItem): boolean {
  revision();
  return !!core && !!(core.request({ cmd: "resume", id: item.id }).entry as ProgressEntry | null)?.finished;
}

/** Started, unfinished items (newest first) among `items`. */
export function continueItems(items: MediaItem[]): MediaItem[] {
  revision();
  if (!core) return [];
  const recent = (core.request({ cmd: "recent", limit: 20 }).entries ?? []) as ProgressEntry[];
  const byId = new Map(items.map((i) => [i.id, i]));
  return recent.map((e) => byId.get(e.id)).filter((i): i is MediaItem => !!i);
}

/** Loads the core (positions for progress bars) before the first list render. */
export async function ready(): Promise<void> {
  await getCore();
  setRevision((r) => r + 1);
}

function updateMediaSession(item: MediaItem, play: PlayInfo): void {
  if (!("mediaSession" in navigator)) return;
  navigator.mediaSession.metadata = new MediaMetadata({
    title: item.title,
    album: item.album ?? "",
    artwork: play.poster_url ? [{ src: play.poster_url }] : [],
  });
  const handlers: [MediaSessionAction, MediaSessionActionHandler][] = [
    ["play", () => toggle()],
    ["pause", () => toggle()],
    ["seekbackward", () => seek(position() - 15)],
    ["seekforward", () => seek(position() + 15)],
    ["previoustrack", () => void previous()],
    ["nexttrack", () => void next()],
    ["seekto", (d) => d.seekTime !== undefined && seek(d.seekTime)],
  ];
  for (const [action, handler] of handlers) {
    try {
      navigator.mediaSession.setActionHandler(action, handler);
    } catch {
      // Not every browser knows every action.
    }
  }
}
