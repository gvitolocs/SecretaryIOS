import { t } from "../i18n";
import { checkHealth, health, healthLabel } from "../state/health";

/** Lucina status dot. Tapping it re-checks immediately. */
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
      <span class="status-text" aria-hidden="true">
        {t("shell.lucina")}
      </span>
    </button>
  );
}
