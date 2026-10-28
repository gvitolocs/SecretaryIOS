import { A, useParams } from "@solidjs/router";
import { createEffect, createSignal, Match, Show, Switch } from "solid-js";
import { approvalsApi, type Approval } from "../api/approvals";
import { toApiError } from "../api/client";
import { ApprovalCard, ApprovalOutcome } from "../components/ApprovalCard";
import { t } from "../i18n";
import type { Outcome } from "../state/approvals";

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; item: Approval }
  | { kind: "closed"; outcome: Outcome }
  | { kind: "missing" }
  | { kind: "error" };

// /approvals/:id: the page a Web Push opens. An unknown, answered or expired id says so.
export default function ApprovalDetail() {
  const params = useParams();
  const [phase, setPhase] = createSignal<Phase>({ kind: "loading" });
  const [answered, setAnswered] = createSignal<Approval | null>(null);

  async function load(id: string) {
    setPhase({ kind: "loading" });
    setAnswered(null);
    try {
      const item = await approvalsApi.get(id);
      if (params.id !== id) return;
      if (!item) setPhase({ kind: "missing" });
      else if (item.status !== "pending") {
        setAnswered(item);
        setPhase({ kind: "closed", outcome: { kind: item.status === "approved" ? "approved" : item.status === "denied" ? "denied" : "expired" } });
      } else setPhase({ kind: "ready", item });
    } catch (e) {
      if (params.id !== id) return;
      const status = toApiError(e).status;
      setPhase(status === 404 || status === 410 ? { kind: "missing" } : { kind: "error" });
    }
  }

  createEffect(() => {
    const id = params.id;
    if (id) void load(id);
    else setPhase({ kind: "missing" });
  });

  /** "This request was approved. Answered on iPhone." for one answered earlier. */
  const statusLine = (a: Approval) =>
    a.status === "approved" ? t("approvals.statusApproved") : a.status === "denied" ? t("approvals.statusDenied") : t("approvals.statusExpired");

  return (
    <section class="apv-page" aria-labelledby="apv-page-title">
      <h1 id="apv-page-title" class="apv-page-title">
        {t("approvals.title")}
      </h1>
      <Switch>
        <Match when={phase().kind === "loading"}>
          <p class="muted" role="status">
            {t("approvals.loading")}
          </p>
        </Match>
        <Match when={phase().kind === "missing"}>
          <p class="apv-empty" role="alert">
            {t("approvals.notFound")}
          </p>
        </Match>
        <Match when={phase().kind === "error"}>
          <p class="apv-empty" role="alert">
            {t("approvals.loadError")}
          </p>
          <button type="button" class="btn btn--primary" onClick={() => void load(params.id ?? "")}>
            {t("approvals.retry")}
          </button>
        </Match>
        <Match when={(phase() as { item?: Approval }).item}>
          {(item) => <ApprovalCard item={item()} onDone={(outcome) => setPhase({ kind: "closed", outcome })} />}
        </Match>
        <Match when={phase().kind === "closed"}>
          <Show
            when={answered()}
            fallback={<ApprovalOutcome outcome={(phase() as { outcome: Outcome }).outcome} />}
          >
            {(a) => (
              <div class="apv-card apv-card--done" role="status">
                <p class="apv-done-text">{statusLine(a())}</p>
                <Show when={a().decidedVia ?? a().decidedBy}>
                  <p class="muted small">
                    {a().decidedVia ? t("approvals.statusVia", { via: a().decidedVia! }) : t("approvals.statusBy", { device: a().decidedBy! })}
                  </p>
                </Show>
                <p class="apv-asking">{a().summary}</p>
              </div>
            )}
          </Show>
        </Match>
      </Switch>
      <A class="btn" href="/approvals">
        {t("approvals.allApprovals")}
      </A>
    </section>
  );
}
