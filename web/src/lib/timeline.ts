// Presentation helpers for the unified timeline: channel labels, the client-side
// source filter, the group/guild caption and day separators.

import type { Channel, ItemMeta } from "../api/timeline";

export const CHANNEL_LABELS: Record<Channel, string> = {
  web: "Web",
  ios: "iPhone",
  telegram: "Telegram",
  discord: "Discord",
  alexa: "Alexa",
  other: "Other",
};

export type SourceFilter = "all" | "app" | "telegram" | "discord" | "alexa";

export const SOURCE_FILTERS: { id: SourceFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "app", label: "App" },
  { id: "telegram", label: "Telegram" },
  { id: "discord", label: "Discord" },
  { id: "alexa", label: "Alexa" },
];

/** "App" is this web app plus the iPhone app. */
export function matchesFilter(channel: Channel, filter: SourceFilter): boolean {
  if (filter === "all") return true;
  if (filter === "app") return channel === "web" || channel === "ios";
  return channel === filter;
}

/** Where in the channel a message happened, e.g. "#general · Lucina HQ" or "DM". */
export function contextCaption(channel: Channel, meta: ItemMeta | undefined): string {
  if (!meta) return "";
  if (meta.isDm) return channel === "discord" || channel === "telegram" ? "DM" : "";
  if (channel === "discord") {
    const room = meta.channelName ? (meta.channelName.startsWith("#") ? meta.channelName : `#${meta.channelName}`) : "";
    return [room, meta.guildName ?? ""].filter(Boolean).join(" · ");
  }
  return meta.chatTitle ?? meta.channelName ?? meta.guildName ?? "";
}

/** Full source line, e.g. "Discord · #general · Lucina HQ". */
export function sourceLine(channel: Channel, meta: ItemMeta | undefined): string {
  const ctx = contextCaption(channel, meta);
  return ctx ? `${CHANNEL_LABELS[channel]} · ${ctx}` : CHANNEL_LABELS[channel];
}

export function dayKey(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

export function dayLabel(ms: number, now = Date.now()): string {
  const key = dayKey(ms);
  if (key === dayKey(now)) return "Today";
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  if (key === dayKey(y.getTime())) return "Yesterday";
  const d = new Date(ms);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  return d.toLocaleDateString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

export function timeLabel(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export function fullTimeLabel(ms: number): string {
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}
