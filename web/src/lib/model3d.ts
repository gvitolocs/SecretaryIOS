// The `model3d` block of Lucina message format v1 (docs/lucina-message-format.md,
// "model3d"): a 3D model from the private gallery. Blocks are network data, so
// each field is validated and anything invalid is dropped. The block never
// carries a URL: the client builds same-origin `/v1/models/{id}/files/...` URLs
// itself, behind the app's auth.

export interface Model3dBlock {
  type: "model3d";
  id: string;
  title: string;
  prompt?: string;
  animations?: string[];
  credits?: number;
  formats?: string[];
  poster?: boolean;
  video?: boolean;
  rigged?: boolean;
}

export const MODEL_FORMATS = ["glb", "fbx", "usdz", "obj", "stl", "3mf"] as const;
const ID_RE = /^mdl_[a-z0-9]{8,40}$/;

function text(v: unknown, max: number): string | undefined {
  if (typeof v !== "string") return undefined;
  // eslint-disable-next-line no-control-regex
  const t = v.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  if (!t) return undefined;
  return t.length > max ? t.slice(0, max - 1) + "…" : t;
}

export function isModelId(id: unknown): id is string {
  return typeof id === "string" && ID_RE.test(id);
}

export function parseModel3dBlock(raw: unknown): Model3dBlock | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (r.type !== "model3d" || !isModelId(r.id)) return null;
  const b: Model3dBlock = { type: "model3d", id: r.id, title: text(r.title, 120) ?? "3D model" };
  const prompt = text(r.prompt, 400);
  if (prompt) b.prompt = prompt;
  if (Array.isArray(r.animations)) {
    const a = r.animations.map((x) => text(x, 60)).filter((x): x is string => !!x).slice(0, 10);
    if (a.length) b.animations = a;
  }
  if (typeof r.credits === "number" && Number.isInteger(r.credits) && r.credits >= 0 && r.credits <= 10_000) b.credits = r.credits;
  if (Array.isArray(r.formats)) {
    const f = [...new Set(r.formats.filter((x): x is string => (MODEL_FORMATS as readonly unknown[]).includes(x)))];
    if (f.length) b.formats = f;
  }
  for (const k of ["poster", "video", "rigged"] as const) if (r[k] === true) b[k] = true;
  return b;
}

/** The valid model3d blocks of a message's `meta.blocks` (at most 4). */
export function parseModel3dBlocks(raw: unknown): Model3dBlock[] {
  if (!Array.isArray(raw)) return [];
  return raw.map(parseModel3dBlock).filter((b): b is Model3dBlock => !!b).slice(0, 4);
}

export type ModelFile = "model.glb" | "thumb.png" | "thumb.jpg" | "thumb.webp" | "preview.mp4" | `model.${(typeof MODEL_FORMATS)[number]}`;

export function modelFileUrl(id: string, name: ModelFile): string {
  if (!isModelId(id)) throw new Error("bad model id");
  return `/v1/models/${id}/files/${name}`;
}

/** A gallery entry (GET /v1/models) as a block, for the same viewer. */
export function blockFromModel(m: { id?: unknown; title?: unknown; prompt?: unknown; animations?: unknown; credits?: unknown; formats?: unknown; files?: unknown; meshy?: unknown }): Model3dBlock | null {
  const files = Array.isArray(m.files) ? m.files.map((f) => (f && typeof f === "object" ? (f as { name?: unknown }).name : null)) : [];
  const meshy = (m.meshy && typeof m.meshy === "object" ? m.meshy : {}) as { rigTaskId?: unknown };
  return parseModel3dBlock({
    type: "model3d",
    id: m.id,
    title: m.title,
    prompt: m.prompt,
    animations: m.animations,
    credits: m.credits,
    formats: m.formats,
    poster: files.some((n) => typeof n === "string" && n.startsWith("thumb.")),
    video: files.includes("preview.mp4"),
    rigged: typeof meshy.rigTaskId === "string",
  });
}
