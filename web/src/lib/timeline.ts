// Presentation helpers for the unified timeline: channel labels, the client-side
// source filter, the group/guild caption and day separators.

import type { Channel, ItemMeta, TimelineItem } from "../api/timeline";
import { locale, t, type MessageKey } from "../i18n";

const CHANNEL_KEYS: Record<Channel, MessageKey> = {
  web: "chat.channelWeb",
  ios: "chat.channelIos",
  telegram: "chat.channelTelegram",
  discord: "chat.channelDiscord",
  alexa: "chat.channelAlexa",
  other: "chat.channelOther",
};

export function channelLabel(channel: Channel): string {
  return t(CHANNEL_KEYS[channel]);
}

/** MCP identity and bubble text, including turns written before meta.via existed. */
export function mcpSource(
  role: string,
  text: string,
  meta?: ItemMeta,
): { caller: string | null; label: string; text: string } | null {
  let caller = meta?.caller ?? null;
  if (meta?.via !== "mcp") {
    if (meta?.via !== undefined || role !== "user") return null;
    const prefix = /^\[via Lucina MCP, from ([^\]]*)\] /.exec(text);
    if (!prefix) return null;
    caller = prefix[1]!.trim();
    text = text.slice(prefix[0].length);
  }
  const name = caller?.trim().toLowerCase() ?? "";
  let label = "MCP";
  if (name.startsWith("claude-code") || name === "claude code") label = "Claude Code";
  else if (name === "claude-desktop") label = "Claude Desktop";
  else if (name.startsWith("claude")) label = "Claude";
  else if (name.startsWith("chatgpt") || name.startsWith("openai")) label = "ChatGPT";
  else if (name.startsWith("grok") || name.startsWith("xai")) label = "Grok";
  else if (name.startsWith("codex")) label = "Codex";
  else if (name.startsWith("cursor") || name === "www") label = "Cursor";
  else if (name.startsWith("gemini")) label = "Gemini";
  else if (name) label = `MCP · ${caller}`;
  return { caller, label, text };
}

export type SourceFilter = "all" | "app" | "mcp" | "telegram" | "discord" | "alexa";

export const SOURCE_FILTERS: { id: SourceFilter; label: MessageKey }[] = [
  { id: "all", label: "chat.filterAll" },
  { id: "app", label: "chat.filterApp" },
  { id: "mcp", label: "chat.filterMcp" },
  { id: "telegram", label: "chat.channelTelegram" },
  { id: "discord", label: "chat.channelDiscord" },
  { id: "alexa", label: "chat.channelAlexa" },
];

/** "App" includes web, iPhone and proactive messages, excluding MCP sources. */
export function matchesFilter(item: Pick<TimelineItem, "channel" | "role" | "text" | "meta">, filter: SourceFilter): boolean {
  if (filter === "all") return true;
  const isMcp = mcpSource(item.role, item.text, item.meta) !== null;
  if (filter === "mcp") return isMcp;
  if (filter === "app") {
    return !isMcp && (item.channel === "web" || item.channel === "ios" || item.meta?.kind === "proactive");
  }
  return item.channel === filter;
}

/** Where in the channel a message happened, e.g. "#general · Lucina HQ" or "DM". */
export function contextCaption(channel: Channel, meta: ItemMeta | undefined): string {
  if (!meta) return "";
  if (meta.isDm) return channel === "discord" || channel === "telegram" ? t("chat.dm") : "";
  if (channel === "discord") {
    const room = meta.channelName ? (meta.channelName.startsWith("#") ? meta.channelName : `#${meta.channelName}`) : "";
    return [room, meta.guildName ?? ""].filter(Boolean).join(" · ");
  }
  return meta.chatTitle ?? meta.channelName ?? meta.guildName ?? "";
}

/** Full source line, e.g. "Discord · #general · Lucina HQ". */
export function sourceLine(channel: Channel, meta: ItemMeta | undefined, role = "lucina", text = ""): string {
  const source = mcpSource(role, text, meta);
  if (source) return source.label;
  const ctx = contextCaption(channel, meta);
  return ctx ? `${channelLabel(channel)} · ${ctx}` : channelLabel(channel);
}

export function dayKey(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

export function dayLabel(ms: number, now = Date.now()): string {
  const key = dayKey(ms);
  if (key === dayKey(now)) return t("chat.today");
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  if (key === dayKey(y.getTime())) return t("chat.yesterday");
  const d = new Date(ms);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  return d.toLocaleDateString(locale(), {
    weekday: "long",
    day: "numeric",
    month: "long",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

export function timeLabel(ms: number): string {
  return new Date(ms).toLocaleTimeString(locale(), { hour: "2-digit", minute: "2-digit" });
}

export function fullTimeLabel(ms: number): string {
  return new Date(ms).toLocaleString(locale(), { dateStyle: "medium", timeStyle: "short" });
}
