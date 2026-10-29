// "Lucina needs your decision": a small dismissible banner for a pending decision, over any page (browser, phone and
// Secretary Code, like the approval pop-up). One banner at a time, each decision once; "Review" opens it.

import { useLocation, useNavigate } from "@solidjs/router";
import { Show } from "solid-js";
import { t } from "../i18n";
import { decisionHref } from "../lib/decisionsRoute";
import { dismissDecision, unannouncedDecisions } from "../state/decisions";
import "../styles/decisions.css";

export function DecisionBanner() {
  const location = useLocation();
  const navigate = useNavigate();
  // The /decisions pages show the questions themselves.
  const onDecisionsPage = () => location.pathname === "/decisions" || location.pathname.startsWith("/decisions/");
  const queue = () => unannouncedDecisions();
  const current = () => (onDecisionsPage() ? undefined : queue()[0]);
  return (
    <Show when={current()} keyed>
      {(item) => (
        <div class="dc-banner" role="status" aria-label={t("decisions.bannerLabel")}>
          <span class="dc-banner-text">
            {t("decisions.banner", { question: item.question })}
            <Show when={queue().length > 1}>
              <small>{t("decisions.bannerMore", { count: queue().length - 1 })}</small>
            </Show>
          </span>
          <button
            type="button"
            class="btn btn--primary btn--small"
            onClick={() => {
              dismissDecision(item.decision_id);
              navigate(decisionHref(item.decision_id));
            }}
          >
            {t("decisions.review")}
          </button>
          <button type="button" class="icon-btn" aria-label={t("common.dismiss")} onClick={() => dismissDecision(item.decision_id)}>
            ×
          </button>
        </div>
      )}
    </Show>
  );
}
