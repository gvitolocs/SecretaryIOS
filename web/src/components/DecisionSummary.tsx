import { Show } from "solid-js";
import type { Decision } from "../api/decisions";
import { t } from "../i18n";
import { relativeTime } from "./NotificationBell";
import "../styles/approvals.css";
import "../styles/decisions.css";

export function DecisionSummary(props: { item: Decision; detail?: boolean }) {
  const asker = () => props.item.asking_client + (props.item.asking_session ? ` (${props.item.asking_session})` : "");
  return <>
    <h2 class="apv-summary">{props.item.question}</h2>
    <p class="apv-asking">{t("decisions.asking", { name: asker() })}</p>
    <div class="apv-badges">
      <span class={`apv-badge apv-badge--${props.item.impact}`}>{t(({ low: "decisions.impactLow", medium: "decisions.impactMedium", high: "decisions.impactHigh" } as const)[props.item.impact])}</span>
      <span class="apv-badge">{t(({ low: "decisions.urgencyLow", normal: "decisions.urgencyNormal", high: "decisions.urgencyHigh" } as const)[props.item.urgency])}</span>
      <Show when={props.detail}><span class="apv-badge">{t(props.item.reversible ? "decisions.reversible" : "decisions.irreversible")}</span></Show>
      <time class="apv-expiry" dateTime={props.item.created_at}>{relativeTime(Date.parse(props.item.created_at))}</time>
    </div>
  </>;
}
