import { checkHealth, health } from "../state/health";

/** Lucina status dot. Tapping it re-checks immediately. */
export function StatusDot() {
  return (
    <button
      type="button"
      class="status"
      onClick={() => void checkHealth()}
      aria-label={`${health().label}. Check again`}
      title={health().label}
    >
      <span
        class="dot"
        classList={{
          "dot--ok": health().level === "ok",
          "dot--degraded": health().level === "degraded",
          "dot--down": health().level === "down",
        }}
        aria-hidden="true"
      />
      <span class="status-text" aria-hidden="true">
        Lucina
      </span>
    </button>
  );
}
