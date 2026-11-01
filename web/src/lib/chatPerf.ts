declare global {
  interface Window { __secretaryPerf?: { firstMessageMs?: number; firstMessageSource?: "cache" | "server"; syncedMs?: number } }
}
export function markFirstMessage(source: "cache" | "server"): void {
  if (typeof window === "undefined") return;
  const perf = window.__secretaryPerf ??= {};
  if (perf.firstMessageMs !== undefined) return;
  performance.mark?.("secretary:first-message");
  perf.firstMessageMs = performance.now(); perf.firstMessageSource = source;
  console.info(`[perf] secretary:first-message ${perf.firstMessageMs.toFixed(1)}ms (${source})`);
}
export function markSynced(): void {
  if (typeof window === "undefined") return;
  const perf = window.__secretaryPerf ??= {};
  if (perf.syncedMs !== undefined) return;
  performance.mark?.("secretary:chat-synced"); perf.syncedMs = performance.now();
  console.info(`[perf] secretary:chat-synced ${perf.syncedMs.toFixed(1)}ms`);
}
