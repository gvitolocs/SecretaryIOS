import type { JSX } from "solid-js";
import type { InsightStatus, PodcastStatus, SourceKind, SourceStatus } from "../../api/notebooks";
import { BulbIcon, ComputerIcon, FileIcon, FilmIcon, GlobeIcon, MicIcon, NoteIcon } from "../../components/Icons";
import type { MessageKey } from "../../i18n";

// Shared bits of the Notebooks views.

export const KIND_ICON: Record<SourceKind, () => JSX.Element> = {
  pdf: FileIcon,
  youtube: FilmIcon,
  web: GlobeIcon,
  text: NoteIcon,
  audio: MicIcon,
  file: ComputerIcon,
  note: BulbIcon,
};

export const KIND_KEY: Record<SourceKind, MessageKey> = {
  pdf: "notebooks.kindPdf",
  youtube: "notebooks.kindYoutube",
  web: "notebooks.kindWeb",
  text: "notebooks.kindText",
  audio: "notebooks.kindAudio",
  file: "notebooks.kindFile",
  note: "notebooks.kindNote",
};

export const SOURCE_STATUS_KEY: Record<SourceStatus, MessageKey> = {
  queued: "notebooks.statusQueued",
  extracting: "notebooks.statusExtracting",
  embedding: "notebooks.statusEmbedding",
  ready: "notebooks.statusReady",
  failed: "notebooks.statusFailed",
};

export const INSIGHT_STATUS_KEY: Record<InsightStatus, MessageKey> = {
  queued: "notebooks.statusQueued",
  running: "notebooks.statusRunning",
  ready: "notebooks.statusReady",
  failed: "notebooks.statusFailed",
};

export const PODCAST_STATUS_KEY: Record<PodcastStatus, MessageKey> = {
  queued: "notebooks.statusQueued",
  outline: "notebooks.podOutline",
  script: "notebooks.podScript",
  voicing: "notebooks.podVoicing",
  mixing: "notebooks.podMixing",
  ready: "notebooks.statusReady",
  failed: "notebooks.statusFailed",
};

/** The host of a URL origin ("example.com"), or the origin as it is (sandbox:/path, upload:name). */
export function originLabel(origin: string): string {
  if (!/^https?:\/\//i.test(origin)) return origin.replace(/^(upload|note):/, "");
  try {
    return new URL(origin).hostname.replace(/^www\./, "");
  } catch {
    return origin;
  }
}

/** A busy status gets the badge colour of work in progress, a failed one the danger colour. */
export function statusTone(status: string): "work" | "ok" | "bad" {
  if (status === "ready") return "ok";
  if (status === "failed") return "bad";
  return "work";
}

/** Hours:minutes:seconds or minutes:seconds of an audio position or a duration. */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor(s / 60) % 60;
  const sec = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}
