// 3D models (docs/server-api.md, "Models"): GET /v1/models, GET/DELETE /v1/models/{id};
// files are fetched from /v1/models/{id}/files/{name} with the session cookie.

import { request } from "./client";

const CLIENT = { "X-Secretary-Client": "web" };

export interface ModelFileInfo {
  name: string;
  bytes: number;
  contentType?: string;
}

export interface GalleryModel {
  id: string;
  title: string;
  prompt?: string | null;
  source: string;
  credits?: number | null;
  animations: string[];
  formats: string[];
  files: ModelFileInfo[];
  createdAt: string;
  updatedAt: string;
  meshy?: { rigTaskId?: string; taskId?: string; taskKind?: string } | null;
}

export async function listModels(q?: string): Promise<GalleryModel[]> {
  const qs = q ? `?q=${encodeURIComponent(q)}` : "";
  const r = await request<{ models: GalleryModel[] }>("GET", `/v1/models${qs}`, { headers: CLIENT });
  return Array.isArray(r.models) ? r.models : [];
}

export async function getModel(id: string): Promise<GalleryModel> {
  const r = await request<{ model: GalleryModel }>("GET", `/v1/models/${encodeURIComponent(id)}`, { headers: CLIENT });
  return r.model;
}

export async function deleteModel(id: string): Promise<void> {
  await request("DELETE", `/v1/models/${encodeURIComponent(id)}`, { headers: CLIENT });
}

/** The thumbnail's file name, if the model has one. */
export function thumbName(m: Pick<GalleryModel, "files">): "thumb.png" | "thumb.jpg" | "thumb.webp" | null {
  const f = m.files.find((x) => x.name === "thumb.png" || x.name === "thumb.jpg" || x.name === "thumb.webp");
  return f ? (f.name as "thumb.png" | "thumb.jpg" | "thumb.webp") : null;
}
