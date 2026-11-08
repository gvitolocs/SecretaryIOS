// "Add secret" helpers for the chat composer: the `/secret` command, the
// token-shape detector (a client copy of Lucina's redactor detector, so a
// pasted key is caught before the optimistic bubble and the local cache),
// target aliases and folder paths.

import type { SecretProject } from "../api/secrets";

export const SECRET_COMMAND = "/secret";
export const SECRET_USAGE = "/secret NAME VALUE [project/env/path]";

/** `/secret` (any case) followed by whitespace or the end. */
export function isSecretCommand(text: string): boolean {
  const t = text.trimStart();
  return t.slice(0, 7).toLowerCase() === SECRET_COMMAND && (t.length === 7 || /\s/.test(t[7]!));
}

export interface SecretCommand {
  name?: string;
  value?: string;
  target?: string;
  /** More words than NAME VALUE TARGET (a value with spaces goes in the sheet). */
  extra: boolean;
}

export function parseSecretCommand(text: string): SecretCommand {
  const words = text.trimStart().slice(7).trim().split(/\s+/).filter(Boolean);
  return { name: words[0], value: words[1], target: words[2], extra: words.length > 3 };
}

/** The word being typed in a `/secret` command: 1 name, 2 value, 3 target. */
export function commandArgIndex(text: string): number {
  if (!isSecretCommand(text)) return 0;
  const rest = text.trimStart().slice(7);
  const words = rest.split(/\s+/);
  // " A B " -> ["", "A", "B", ""]: the last entry is the word in progress.
  return Math.min(words.length - 1, 4);
}

export const NAME_RE = /^[A-Za-z0-9_.@-]{1,256}$/;
const SEGMENT_RE = /^[A-Za-z0-9_.@-]{1,128}$/;

/** `/`, `/a/b`, `a/b/` -> `/a/b`; null when a segment is not allowed. */
export function normalizeFolder(path: string): string | null {
  const t = path.trim().replace(/^\/+|\/+$/g, "");
  if (!t) return "/";
  const segs = t.split("/");
  if (!segs.every((s) => SEGMENT_RE.test(s) && s !== "." && s !== "..")) return null;
  return `/${segs.join("/")}`;
}

export function joinFolder(parent: string, child: string): string {
  return parent === "/" ? `/${child}` : `${parent}/${child}`;
}

export function parentFolder(path: string): string {
  const i = path.lastIndexOf("/");
  return i <= 0 ? "/" : path.slice(0, i);
}

/** Lowercase words a project answers to: its alias, slug, name and the name's first word. */
export function projectKeys(p: SecretProject): string[] {
  const keys = [p.alias, p.slug, p.name, p.name.split(/\s+/)[0]]
    .filter((k): k is string => !!k)
    .map((k) => k.toLowerCase());
  return [...new Set(keys)];
}

/** A project by id, or by alias, slug or name (any case): what a card's `project` names. */
export function findProject(ref: string, projects: SecretProject[]): SecretProject | undefined {
  const key = ref.toLowerCase();
  return projects.find((p) => p.id === ref) ?? projects.find((p) => projectKeys(p).includes(key));
}

export interface Target {
  project: SecretProject;
  environment?: string;
  path?: string;
}

/** `poko`, `poko/prod`, `poko/prod/Pokobot/sub` -> a project (+ env, folder). */
export function resolveTarget(target: string, projects: SecretProject[]): Target | null {
  const [head, env, ...rest] = target.split("/");
  if (!head) return null;
  const key = head.toLowerCase();
  const exact = projects.filter((p) => p.id === head || projectKeys(p).includes(key));
  const project = exact[0] ?? (projects.filter((p) => projectKeys(p).some((k) => k.startsWith(key))).length === 1
    ? projects.find((p) => projectKeys(p).some((k) => k.startsWith(key)))
    : undefined);
  if (!project) return null;
  const out: Target = { project };
  if (env && project.environments.some((e) => e.slug === env)) out.environment = env;
  if (rest.length) {
    const folder = normalizeFolder(rest.join("/"));
    if (folder) out.path = folder;
  }
  return out;
}

/** Completions for the target word of `/secret`. */
export function targetCompletions(prefix: string, projects: SecretProject[], limit = 6): string[] {
  const [head = "", env] = prefix.split("/");
  const key = head.toLowerCase();
  if (env === undefined) {
    return projects
      .filter((p) => !key || projectKeys(p).some((k) => k.startsWith(key)))
      .map((p) => (p.alias || p.slug || p.name.split(/\s+/)[0] || p.id).toLowerCase())
      .slice(0, limit);
  }
  const t = resolveTarget(head, projects);
  if (!t) return [];
  return t.project.environments
    .filter((e) => e.slug.startsWith(env))
    .map((e) => `${head}/${e.slug}`)
    .slice(0, limit);
}

// ---------------------------------------------------------------- detector

/** Token shapes, the same list as Lucina's redactor (lucina-secrets redact.rs). */
const SHAPES: RegExp[] = [
  /-----BEGIN [A-Z0-9 ]{1,40}-----[\s\S]*?-----END [A-Z0-9 ]{1,40}-----/g,
  /-----BEGIN [A-Z0-9 ]{0,40}PRIVATE KEY-----(?:[A-Za-z0-9+/=\s]|\\[nr])*/g,
  /\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/g,
  /\bsk-[A-Za-z0-9_-]{16,}/g,
  /\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,}/g,
  /\bgh[pousr]_[A-Za-z0-9]{20,}/g,
  /\bgithub_pat_[A-Za-z0-9_]{20,}/g,
  /\bglpat-[A-Za-z0-9_-]{20,}/g,
  /\b(?:mcp|sec|svc)_[A-Za-z0-9_-]{16,}/g,
  /\bxox[abposr]-[A-Za-z0-9-]{10,}/g,
  /\bxapp-[0-9]-[A-Za-z0-9-]{10,}/g,
  /\bAKIA[0-9A-Z]{16}\b/g,
  /\bAIza[0-9A-Za-z_-]{35}/g,
  /\b\d{8,10}:AA[A-Za-z0-9_-]{30,}/g,
  /\bst\.[A-Za-z0-9-]{8,}\.[a-f0-9]{16,}\.[a-f0-9]{16,}/g,
  /\bhf_[A-Za-z0-9]{30,}/g,
  /\bnpm_[A-Za-z0-9]{36}\b/g,
  /\bpypi-AgE[A-Za-z0-9_-]{50,}/g,
  /\bSG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}/g,
  /\bre_[A-Za-z0-9]{8}_[A-Za-z0-9]{16,}/g,
  /\bdop_v1_[a-f0-9]{64}\b/g,
  /\bAGE-SECRET-KEY-1[0-9A-Z]{58}\b/g,
  /\bya29\.[A-Za-z0-9_-]{20,}/g,
  /\b1\/\/0[A-Za-z0-9_-]{30,}/g,
  /\b(?:glsa|sntrys|lin_api)_[A-Za-z0-9_=-]{20,}/g,
  /\bSK[0-9a-f]{32}\b/g,
  /\b[MNO][A-Za-z0-9_-]{23,27}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,40}/g,
];

/** `password=…`, `"token": "…"`: group 1 is the value. */
const PAIR =
  /\b(?:password|passwd|pwd|passphrase|secret|client[_-]?secret|api[_-]?key|apikey|access[_-]?token|auth[_-]?token|token)["']?\s*[:=]\s*["']?([^\s"'&,;[\]{}/][^\s"'&,;[\]{}]{7,})/gi;
const BEARER = /\b(?:bearer|basic)\s+([A-Za-z0-9._~+/-]{16,}=*)/gi;
const ENTROPY = /[A-Za-z0-9+/_-]{32,}={0,2}/g;

function shannon(s: string): number {
  const counts = new Map<string, number>();
  for (const c of s) counts.set(c, (counts.get(c) ?? 0) + 1);
  let h = 0;
  for (const n of counts.values()) {
    const p = n / s.length;
    h -= p * Math.log2(p);
  }
  return h;
}

/** 32+ characters with lower, upper case and 3+ digits, high entropy (UUIDs, ULIDs, hex pass). */
export function looksRandom(s: string): boolean {
  const core = s.replace(/=+$/, "");
  if (core.length < 32) return false;
  const digits = (core.match(/[0-9]/g) ?? []).length;
  return /[a-z]/.test(core) && /[A-Z]/.test(core) && digits >= 3 && shannon(core) >= 4.2;
}

export interface Detected {
  start: number;
  end: number;
}

/** Spans of `text` that look like secrets (merged where they overlap). */
export function detectSecrets(text: string): Detected[] {
  const spans: Detected[] = [];
  const add = (start: number, end: number) => {
    if (start > 0 && text[start - 1] === "[") return;
    if (text.slice(start, end).startsWith("secret://")) return;
    spans.push({ start, end });
  };
  for (const re of SHAPES) for (const m of text.matchAll(re)) add(m.index!, m.index! + m[0].length);
  for (const re of [PAIR, BEARER]) {
    for (const m of text.matchAll(re)) {
      const v = m[1]!;
      const start = m.index! + m[0].lastIndexOf(v);
      if (!v.startsWith("secret://")) add(start, start + v.length);
    }
  }
  for (const m of text.matchAll(ENTROPY)) {
    const s = m[0];
    const at = m.index!;
    if (text.slice(0, at).endsWith("gdrive:") && /^[A-Za-z0-9_-]{25,60}$/.test(s)) continue;
    const base64ish = s.includes("+") || s.endsWith("=");
    if (!base64ish && (s.match(/\//g) ?? []).length >= 2) {
      let off = at;
      for (const seg of s.split("/")) {
        if (looksRandom(seg)) add(off, off + seg.length);
        off += seg.length + 1;
      }
    } else if (looksRandom(s)) add(at, at + s.length);
  }
  spans.sort((a, b) => a.start - b.start || b.end - a.end);
  const out: Detected[] = [];
  for (const d of spans) {
    const last = out[out.length - 1];
    if (last && d.start < last.end) last.end = Math.max(last.end, d.end);
    else out.push({ ...d });
  }
  return out;
}

/** The text with every detected span replaced by `[secret:token]`. */
export function maskSecrets(text: string, spans = detectSecrets(text)): string {
  let out = "";
  let last = 0;
  for (const d of spans) {
    out += text.slice(last, d.start) + "[secret:token]";
    last = d.end;
  }
  return out + text.slice(last);
}

/** A hint that names a value without showing it: `sk-…, 51 characters`. */
export function valueHint(v: string): string {
  return `${[...v].slice(0, 3).join("")}…, ${[...v].length}`;
}

/** A name suggestion for a pasted token from its shape. */
export function suggestName(v: string): string {
  if (/^sk-ant-/.test(v)) return "ANTHROPIC_API_KEY";
  if (/^sk-(?:proj-)?/.test(v)) return "OPENAI_API_KEY";
  if (/^gh[pousr]_|^github_pat_/.test(v)) return "GITHUB_TOKEN";
  if (/^xox|^xapp-/.test(v)) return "SLACK_TOKEN";
  if (/^AIza/.test(v)) return "GOOGLE_API_KEY";
  if (/^\d{8,10}:AA/.test(v)) return "TELEGRAM_BOT_TOKEN";
  if (/^hf_/.test(v)) return "HF_TOKEN";
  if (/^-----BEGIN/.test(v)) return "PRIVATE_KEY";
  return "";
}
