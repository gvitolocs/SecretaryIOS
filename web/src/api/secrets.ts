// "Add secret" (/v1/secrets*): the owner stores secrets in Infisical and
// creates projects. Names come back, never values; the value is sent once,
// in the body of the save, after the owner confirmed it.

import { request } from "./client";

export interface SecretEnvironment {
  slug: string;
  name: string;
}

export interface SecretProject {
  id: string;
  name: string;
  slug?: string | null;
  alias?: string | null;
  environments: SecretEnvironment[];
}

export interface SecretFolder {
  project: string;
  environment: string;
  path: string;
  folders: string[];
  secrets: { key: string; version?: number | null; updated_at?: string | null }[];
}

export interface SaveSecret {
  name: string;
  project: string;
  environment: string;
  path: string;
  overwrite: boolean;
  createFolder: boolean;
  /** Exactly one of value / held. */
  value?: string;
  held?: string;
}

export interface SecretReceipt {
  key: string;
  project: { id: string; name: string };
  environment: string;
  path: string;
  action: "created" | "updated";
  version?: number | null;
  fingerprint: string;
  receipt: string;
}

export interface CreatedProject {
  project: SecretProject;
  receipt: string;
}

function asProjects(v: unknown): SecretProject[] {
  const list = (v as { projects?: unknown })?.projects;
  if (!Array.isArray(list)) return [];
  return list
    .filter((p): p is SecretProject => !!p && typeof p === "object" && typeof (p as SecretProject).id === "string")
    .map((p) => ({
      id: p.id,
      name: typeof p.name === "string" ? p.name : p.id,
      slug: p.slug ?? null,
      alias: p.alias ?? null,
      environments: Array.isArray(p.environments)
        ? p.environments.filter((e) => e && typeof e.slug === "string").map((e) => ({ slug: e.slug, name: e.name || e.slug }))
        : [],
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

export const secretsApi = {
  catalog: async () => asProjects(await request<unknown>("GET", "/v1/secrets/catalog")),
  folder: (project: string, environment: string, path: string) =>
    request<SecretFolder>(
      "GET",
      `/v1/secrets/names?project=${encodeURIComponent(project)}&environment=${encodeURIComponent(environment)}&path=${encodeURIComponent(path)}`,
    ),
  save: (body: SaveSecret) => request<SecretReceipt>("POST", "/v1/secrets", { body: { ...body, confirmed: true }, timeoutMs: 60_000 }),
  createProject: (name: string) =>
    request<CreatedProject>("POST", "/v1/secrets/projects", { body: { name, confirmed: true }, timeoutMs: 60_000 }),
};
