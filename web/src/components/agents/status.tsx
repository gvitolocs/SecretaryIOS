import type { AgentListItem, Bucket } from "../../api/agents";
import { t } from "../../i18n";

/** Labels lucina-agentd sets when a run that hit a provider's usage limit continues on another agent. */
export const CONTINUED_ON = "lucina.continued-on";
export const CONTINUED_PROVIDER = "lucina.continued-provider";
export const CONTINUED_FROM = "lucina.continued-from";
export const HANDOFF = "lucina.handoff";

export function statusLabel(a: Pick<AgentListItem, "bucket" | "status"> & { labels?: Record<string, string> }): string {
  if (a.labels?.[CONTINUED_ON]) return t("agents.stContinued", { provider: a.labels[CONTINUED_PROVIDER] ?? "?" });
  if (a.status === "error" && a.labels?.[HANDOFF] === "pending") return t("agents.stHandingOff");
  switch (a.bucket) {
    case "needs_input":
      return t("agents.stNeedsInput");
    case "failed":
      return t("agents.stError");
    case "running":
      return t("agents.stRunning");
    case "attention":
      return t("agents.stFinished");
    default:
      return a.status === "closed" ? t("agents.stClosed") : a.status === "initializing" ? t("agents.stStarting") : t("agents.stIdle");
  }
}

/** The ring in front of an agent: its colour and motion say the bucket, the badge says it in words. */
export function StatusRing(props: { bucket: Bucket; status?: string }) {
  return <span class="ag-ring" data-bucket={props.bucket} data-status={props.status} aria-hidden="true" />;
}

export function StatusBadge(props: { agent: Pick<AgentListItem, "bucket" | "status"> & { labels?: Record<string, string> } }) {
  return (
    <span class="ag-badge" data-bucket={props.agent.bucket}>
      <StatusRing bucket={props.agent.bucket} status={props.agent.status} />
      {statusLabel(props.agent)}
    </span>
  );
}

/** "3 min ago"-style time, in the device language. */
export function ago(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "";
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return "";
  const sec = Math.round((ms - now) / 1000);
  const abs = Math.abs(sec);
  const rtf = new Intl.RelativeTimeFormat(document.documentElement.lang || undefined, { numeric: "auto", style: "short" });
  if (abs < 60) return rtf.format(Math.trunc(sec / 1) , "second");
  if (abs < 3600) return rtf.format(Math.round(sec / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(sec / 3600), "hour");
  return rtf.format(Math.round(sec / 86400), "day");
}
