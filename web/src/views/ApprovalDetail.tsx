import { A, useParams } from "@solidjs/router";
import { createEffect, createSignal, Match, Show, Switch } from "solid-js";
import { approvalsApi, type Approval } from "../api/approvals";
import { toApiError } from "../api/client";
import { ApprovalCard, ApprovalOutcome, outcomeText } from "../components/ApprovalCard";
import { Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PageHeader, useWide } from "../components/page";
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
  const { wide, track } = useWide();
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

  const body = () => <>
      <Switch>
        <Match when={phase().kind === "loading"}>
          <Show when={wide()} fallback={<p class="muted" role="status">{t("approvals.loading")}</p>}><LoadingState label={t("approvals.loading")} /></Show>
        </Match>
        <Match when={phase().kind === "missing"}>
          <Show when={wide()} fallback={<p class="apv-empty" role="alert">{t("approvals.notFound")}</p>}><Card><EmptyState text={t("approvals.notFound")} /></Card></Show>
        </Match>
        <Match when={phase().kind === "error"}>
          <Show when={wide()} fallback={<>
          <p class="apv-empty" role="alert">
            {t("approvals.loadError")}
          </p>
          <button type="button" class="btn btn--primary" onClick={() => void load(params.id ?? "")}>
            {t("approvals.retry")}
          </button>
          </>}><ErrorState message={t("approvals.loadError")} retryLabel={t("approvals.retry")} onRetry={() => void load(params.id ?? "")} /></Show>
        </Match>
        <Match when={(phase() as { item?: Approval }).item}>
          {(item) => <ApprovalCard desktop={wide()} item={item()} onDone={(outcome) => setPhase({ kind: "closed", outcome })} />}
        </Match>
        <Match when={phase().kind === "closed"}>
          <Show
            when={answered()}
            fallback={<Show when={wide()} fallback={<ApprovalOutcome outcome={(phase() as { outcome: Outcome }).outcome} />}><Card><p role="status">{outcomeText((phase() as { outcome: Outcome }).outcome)}</p></Card></Show>}
          >
            {(a) => (
              <div class={wide() ? "pg-card" : "apv-card apv-card--done"} role="status">
                <p class="apv-done-text">{statusLine(a())}</p>
                <Show when={a().decidedBy}>
                  <p class="muted small">{t("approvals.statusBy", { device: a().decidedBy! })}</p>
                </Show>
                <p class="apv-asking">{a().summary}</p>
              </div>
            )}
          </Show>
        </Match>
      </Switch>
  </>;

  return (
    <div ref={track}><Show when={wide()} fallback={<section class="apv-page" aria-labelledby="apv-page-title">
      <h1 id="apv-page-title" class="apv-page-title">
        {t("approvals.title")}
      </h1>
      {body()}
      <A class="btn" href="/approvals">
        {t("approvals.allApprovals")}
      </A>
    </section>}><Page>
      <PageHeader title={t("approvals.title")} context={t("approvals.listSub")} />
      <A class="pg-link pg-back" href="/approvals">{t("approvals.allApprovals")}</A>
      <PageGrid layout="single">{body()}</PageGrid>
    </Page></Show></div>
  );
}
