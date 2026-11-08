// Chat attachments use their own share path: multipart POST /v1/share through
// XMLHttpRequest (for upload progress) and GET /v1/share/{id} polling for the
// "Lucina is reading…" stage.

import { locale } from "../i18n";
import { chatSessionId, uuid } from "./ids";
import type { DraftAttachment } from "./attachments";

export const SHARE_POLL_MS = 2_000;
export const SHARE_POLL_MAX_MS = 10 * 60_000;

export const SHARE_STAGES = ["received", "reading", "thinking", "done", "error"] as const;
export type ShareStage = (typeof SHARE_STAGES)[number];
export const SHARE_ATTACHMENT_STATES = ["pending", "reading", "read", "failed"] as const;
export type ShareAttachmentState = (typeof SHARE_ATTACHMENT_STATES)[number];

export interface ShareStatusAttachment {
  index: number;
  kind: string;
  mediaId?: string;
  state: ShareAttachmentState;
  summary?: string;
}

export interface ShareStatus {
  stage: ShareStage;
  attachments: ShareStatusAttachment[];
  userItemId?: string;
  lucinaItemId?: string;
  error?: string;
}

export interface ShareAccepted {
  stage?: ShareStage;
  userItemId?: string;
  lucinaItemId?: string;
  raw: unknown;
}

export interface ChatShareInput {
  shareId: string;
  files: readonly DraftAttachment[];
  caption: string;
  meta?: string;
  sessionId?: string;
  /** Defaults to the app locale. */
  lang?: string;
}

export interface ShareUploadHandlers {
  onProgress?: (loaded: number, total: number) => void;
}

export class ShareUploadError extends Error {
  readonly status: number;
  constructor(message: string, status = 0) {
    super(message);
    this.name = "ShareUploadError";
    this.status = status;
  }
}

function str(value: unknown, max = 500): string | undefined {
  return typeof value === "string" && value ? value.slice(0, max) : undefined;
}

function idOf(value: unknown): string | undefined {
  if (typeof value === "string" && value) return value;
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  return undefined;
}

function stageOf(value: unknown): ShareStage | undefined {
  return (SHARE_STAGES as readonly string[]).includes(value as string) ? (value as ShareStage) : undefined;
}

function stateOf(value: unknown): ShareAttachmentState {
  return (SHARE_ATTACHMENT_STATES as readonly string[]).includes(value as string)
    ? (value as ShareAttachmentState)
    : "pending";
}

/** The exact multipart body the composer sends (also used by tests). */
export function buildChatShareForm(input: ChatShareInput): FormData {
  const data = new FormData();
  data.append("shareId", input.shareId);
  data.append("intent", "ask");
  data.append("lang", input.lang ?? locale());
  data.append("source", "web-chat");
  data.append("channel", "web");
  data.append("sessionId", input.sessionId ?? chatSessionId());
  data.append("note", input.caption);
  if (input.meta) data.append("meta", input.meta);
  for (const draft of input.files) data.append("file", draft.file, draft.name);
  return data;
}

function parseBody(text: string): unknown {
  if (!text) return undefined;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}

export function parseShareAccepted(raw: unknown): ShareAccepted {
  if (!raw || typeof raw !== "object") return { raw };
  const body = raw as Record<string, unknown>;
  const share = body.share && typeof body.share === "object" ? (body.share as Record<string, unknown>) : body;
  const accepted: ShareAccepted = { raw };
  const stage = stageOf(share.stage);
  if (stage) accepted.stage = stage;
  const userItemId = idOf(share.userItemId);
  const lucinaItemId = idOf(share.lucinaItemId);
  if (userItemId) accepted.userItemId = userItemId;
  if (lucinaItemId) accepted.lucinaItemId = lucinaItemId;
  return accepted;
}

export function parseShareStatus(raw: unknown): ShareStatus {
  const body = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const share = body.share && typeof body.share === "object" ? (body.share as Record<string, unknown>) : body;
  const stage = stageOf(share.stage) ?? stageOf(share.status) ?? "received";
  const attachments: ShareStatusAttachment[] = [];
  if (Array.isArray(share.attachments)) {
    for (const item of share.attachments) {
      if (!item || typeof item !== "object") continue;
      const rawItem = item as Record<string, unknown>;
      const index = typeof rawItem.index === "number" && Number.isInteger(rawItem.index) && rawItem.index >= 0 ? rawItem.index : null;
      if (index === null) continue;
      const parsed: ShareStatusAttachment = {
        index,
        kind: str(rawItem.kind, 32) ?? "file",
        state: stateOf(rawItem.state),
      };
      const mediaId = idOf(rawItem.mediaId);
      const summary = str(rawItem.summary, 500);
      if (mediaId) parsed.mediaId = mediaId;
      if (summary) parsed.summary = summary;
      attachments.push(parsed);
    }
  }
  const status: ShareStatus = { stage, attachments };
  const userItemId = idOf(share.userItemId);
  const lucinaItemId = idOf(share.lucinaItemId);
  const error = str(share.error, 500);
  if (userItemId) status.userItemId = userItemId;
  if (lucinaItemId) status.lucinaItemId = lucinaItemId;
  if (error) status.error = error;
  return status;
}

/** POST /v1/share with upload progress; resolves on 202. */
export function postChatShare(input: ChatShareInput, handlers: ShareUploadHandlers = {}): Promise<ShareAccepted> {
  return new Promise<ShareAccepted>((resolve, reject) => {
    let form: FormData;
    try {
      form = buildChatShareForm(input);
    } catch (e) {
      reject(e instanceof Error ? e : new ShareUploadError("Couldn't prepare the upload"));
      return;
    }
    const xhr = new XMLHttpRequest();
    let settled = false;
    const fail = (message: string, status = 0) => {
      if (settled) return;
      settled = true;
      reject(new ShareUploadError(message, status));
    };
    xhr.open("POST", "/v1/share");
    xhr.withCredentials = true;
    xhr.timeout = 130_000;
    try {
      xhr.setRequestHeader("X-Secretary-CSRF", "1");
    } catch {
      // Some test doubles expose no header API.
    }
    if (xhr.upload) {
      xhr.upload.onprogress = (event) => {
        if (event.total > 0) handlers.onProgress?.(Math.min(event.loaded, event.total), event.total);
        else handlers.onProgress?.(event.loaded, 0);
      };
    }
    xhr.onload = () => {
      if (settled) return;
      const body = parseBody(xhr.responseText ?? "");
      if (xhr.status === 202 || (xhr.status >= 200 && xhr.status < 300)) {
        settled = true;
        resolve(parseShareAccepted(body));
        return;
      }
      const message =
        body && typeof body === "object" && typeof (body as Record<string, unknown>).error === "string"
          ? String((body as Record<string, unknown>).error)
          : `Upload failed (${xhr.status})`;
      fail(message, xhr.status);
    };
    xhr.onerror = () => fail("Network error");
    xhr.ontimeout = () => fail("Upload timed out");
    xhr.onabort = () => fail("Upload cancelled");
    try {
      xhr.send(form);
    } catch (e) {
      fail(e instanceof Error ? e.message : "Couldn't start the upload");
    }
  });
}

/** GET /v1/share/{id}: the reading stage and per-file progress. */
export async function getShareStatus(id: string): Promise<ShareStatus> {
  const res = await fetch(`/v1/share/${encodeURIComponent(id)}`, {
    method: "GET",
    credentials: "same-origin",
    headers: { Accept: "application/json" },
    cache: "no-store",
  });
  if (!res.ok) {
    if (res.status === 401) throw new ShareUploadError("Signed out", 401);
    throw new ShareUploadError(`Status failed (${res.status})`, res.status);
  }
  const body: unknown = await res.json().catch(() => null);
  return parseShareStatus(body);
}

export interface SharePollOptions {
  onUpdate: (status: ShareStatus) => void;
  intervalMs?: number;
  maxMs?: number;
  /** Test seam; defaults to document.visibilityState. */
  visibility?: () => DocumentVisibilityState;
  /** Test seam; defaults to Date.now. */
  now?: () => number;
}

/**
 * Poll the share status every 2 s for at most 10 min. Stops immediately when
 * the tab is hidden, when the share is done/error, or when the returned
 * function is called.
 */
export function pollShareStatus(id: string, options: SharePollOptions): () => void {
  const interval = options.intervalMs ?? SHARE_POLL_MS;
  const maxMs = options.maxMs ?? SHARE_POLL_MAX_MS;
  const visibility = options.visibility ?? (() => (typeof document === "undefined" ? "visible" : document.visibilityState));
  const now = options.now ?? (() => Date.now());
  const started = now();
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const stop = () => {
    if (stopped) return;
    stopped = true;
    if (timer !== null) clearTimeout(timer);
    timer = null;
    if (typeof document !== "undefined") document.removeEventListener("visibilitychange", onVisibility);
  };
  const onVisibility = () => {
    if (visibility() === "hidden") stop();
  };
  const schedule = () => {
    if (stopped) return;
    timer = setTimeout(tick, interval);
  };
  const tick = () => {
    timer = null;
    if (stopped) return;
    if (visibility() === "hidden") {
      stop();
      return;
    }
    if (now() - started >= maxMs) {
      stop();
      return;
    }
    void getShareStatus(id)
      .then((status) => {
        if (stopped) return;
        options.onUpdate(status);
        if (status.stage === "done" || status.stage === "error") stop();
        else schedule();
      })
      .catch((e: unknown) => {
        if (stopped) return;
        if (e instanceof ShareUploadError && e.status === 401) {
          stop();
          return;
        }
        if (now() - started >= maxMs) stop();
        else schedule();
      });
  };

  if (visibility() === "hidden") {
    stop();
    return stop;
  }
  if (typeof document !== "undefined") document.addEventListener("visibilitychange", onVisibility);
  schedule();
  return stop;
}

/** A stable id for the optimistic share (also the server's idempotency key). */
export function newShareId(): string {
  return uuid();
}
