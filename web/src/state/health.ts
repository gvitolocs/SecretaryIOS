import { createSignal } from "solid-js";
import { api } from "../api/secretary";
import { t, type MessageKey } from "../i18n";
import { markLucinaOnline } from "./lucina";

export type HealthLevel = "ok" | "degraded" | "down";

export interface HealthView {
  level: HealthLevel;
  /** Message key for the label; translated when shown. */
  key: MessageKey;
  latencyMs?: number;
}

export const HEALTH_POLL_MS = 30_000;
const SLOW_MS = 3_000;

const [health, setHealth] = createSignal<HealthView>({ level: "degraded", key: "shell.healthChecking" });
export { health };

/** The status text in the current language, e.g. "Lucina is online (42 ms)". */
export function healthLabel(view: HealthView = health()): string {
  return t(view.key, view.latencyMs === undefined ? undefined : { ms: view.latencyMs });
}

let seq = 0;

export async function checkHealth(): Promise<void> {
  const mine = ++seq;
  try {
    const h = await api.getHealth();
    if (mine !== seq) return;
    const reachable = h?.lucina?.reachable === true;
    const latency = typeof h?.lucina?.latencyMs === "number" ? Math.round(h.lucina.latencyMs) : null;
    if (!reachable) {
      const why = h?.lucina?.error;
      const key: MessageKey =
        why === "lucina_timeout"
          ? "shell.healthNotResponding"
          : why === "not_configured" || why === "lucina_not_configured"
            ? "shell.healthNotConfigured"
            : "shell.healthOffline";
      setHealth({ level: "down", key });
    } else if (h.ok === false || (latency !== null && latency > SLOW_MS)) {
      setHealth(
        latency !== null
          ? { level: "degraded", key: "shell.healthSlow", latencyMs: latency }
          : { level: "degraded", key: "shell.healthDegraded" },
      );
      markLucinaOnline();
    } else {
      setHealth(
        latency !== null
          ? { level: "ok", key: "shell.healthOnline", latencyMs: latency }
          : { level: "ok", key: "shell.healthOnlinePlain" },
      );
      markLucinaOnline();
    }
  } catch {
    if (mine !== seq) return;
    setHealth({ level: "down", key: "shell.healthServerDown" });
  }
}
