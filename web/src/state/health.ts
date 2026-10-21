import { createSignal } from "solid-js";
import { api } from "../api/secretary";
import { markLucinaOnline } from "./lucina";

export type HealthLevel = "ok" | "degraded" | "down";

export interface HealthView {
  level: HealthLevel;
  label: string;
}

export const HEALTH_POLL_MS = 30_000;
const SLOW_MS = 3_000;

const [health, setHealth] = createSignal<HealthView>({ level: "degraded", label: "Checking Lucina…" });
export { health };

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
      const label =
        why === "lucina_timeout"
          ? "Lucina is not responding"
          : why === "not_configured" || why === "lucina_not_configured"
            ? "Lucina is not configured on the server"
            : "Lucina is offline";
      setHealth({ level: "down", label });
    } else if (h.ok === false || (latency !== null && latency > SLOW_MS)) {
      setHealth({ level: "degraded", label: latency !== null ? `Lucina is slow (${latency} ms)` : "Lucina is degraded" });
      markLucinaOnline();
    } else {
      setHealth({ level: "ok", label: latency !== null ? `Lucina is online (${latency} ms)` : "Lucina is online" });
      markLucinaOnline();
    }
  } catch {
    if (mine !== seq) return;
    setHealth({ level: "down", label: "Server unreachable" });
  }
}
