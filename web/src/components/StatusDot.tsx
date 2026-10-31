import { t } from "../i18n";
import { checkHealth, health, healthLabel } from "../state/health";

/** Lucina status: one faint dot, louder only when something is wrong. Tapping it re-checks. */
export function StatusDot() {
  return (
    <button
      type="button"
      class="status"
      onClick={() => void checkHealth()}
      aria-label={t("shell.healthCheckAgain", { label: healthLabel() })}
      title={healthLabel()}
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
    </button>
  );
}
