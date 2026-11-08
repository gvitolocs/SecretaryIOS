import { A, useLocation } from "@solidjs/router";
import { Show } from "solid-js";
import { t } from "../i18n";
import { pendingDecisionCount } from "../state/decisions";
import { CheckCircleIcon } from "./Icons";

/** The top-bar button next to the bell: opens /decisions, with the bell's count badge (hidden at 0). */
export function DecisionsButton() {
  const location = useLocation();
  const active = () => location.pathname === "/decisions" || location.pathname.startsWith("/decisions/");
  const label = () => (pendingDecisionCount() > 0 ? t("decisions.waiting", { count: pendingDecisionCount() }) : t("decisions.title"));
  return (
    <A href="/decisions" class="icon-btn decisions-btn" aria-label={label()} title={label()} aria-current={active() ? "page" : undefined}>
      <CheckCircleIcon />
      <Show when={pendingDecisionCount() > 0}>
        <span class="notif-badge" aria-hidden="true">
          {pendingDecisionCount() > 99 ? "99+" : pendingDecisionCount()}
        </span>
      </Show>
    </A>
  );
}
