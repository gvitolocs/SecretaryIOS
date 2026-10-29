import { createSignal, onCleanup, Show } from "solid-js";
import { ApiError } from "../api/client";
import { nclientApi, saveFinished, type SaveStatus } from "../api/nclient";
import { t } from "../i18n";

/** Delay between two looks at a running save. A module value so tests can shorten it. */
export const savePoll = { ms: 1200 };

export interface SaveView {
  starting: boolean;
  status?: SaveStatus;
  /** The request itself failed (before or while the job ran). */
  error?: "not_found" | "busy" | "failed";
}

const sleep = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));

/**
 * "Save to Lucina": one background job per gallery on the service (every page into the Gallery as
 * source nclient, album = the title). The job runs on the server; this only starts it, looks at its
 * progress while the view is open, and can cancel it. Nothing is kept in storage.
 */
export function createSaves() {
  const [views, setViews] = createSignal<Record<number, SaveView>>({});
  let alive = true;
  onCleanup(() => { alive = false; });
  const put = (id: number, v: SaveView) => setViews(all => ({ ...all, [id]: v }));
  const running = (id: number) => { const v = views()[id]; return !!v && (v.starting || (!!v.status && !saveFinished(v.status.state))); };

  async function watch(id: number, job: string) {
    let failures = 0;
    while (alive) {
      await sleep(savePoll.ms);
      if (!alive) return;
      try {
        const status = await nclientApi.saveStatus(job);
        failures = 0;
        put(id, { starting: false, status });
        if (saveFinished(status.state)) return;
      } catch (e) {
        // A lost job (service restarted) or three failed looks in a row end the watching.
        if ((e instanceof ApiError && e.status === 404) || ++failures >= 3) {
          put(id, { starting: false, status: views()[id]?.status, error: "failed" });
          return;
        }
      }
    }
  }

  async function start(id: number) {
    if (running(id)) return;
    put(id, { starting: true });
    try {
      const status = await nclientApi.save(id);
      if (!alive) return;
      put(id, { starting: false, status });
      if (!saveFinished(status.state)) void watch(id, status.job);
    } catch (e) {
      if (!alive) return;
      const code = e instanceof ApiError ? e.status : 0;
      put(id, { starting: false, error: code === 404 ? "not_found" : code === 429 ? "busy" : "failed" });
    }
  }

  async function cancel(id: number) {
    const job = views()[id]?.status?.job;
    if (!job) return;
    try { await nclientApi.cancelSave(job); } catch { /* the next look shows the state */ }
  }

  return { views, start, cancel, running };
}

export type Saves = ReturnType<typeof createSaves>;

/** The button and its progress for one gallery. `compact` is the reader's top bar. */
export function SaveControl(props: { saves: Saves; id: number; compact?: boolean }) {
  const view = () => props.saves.views()[props.id];
  const status = () => view()?.status;
  const active = () => props.saves.running(props.id);
  const pct = () => { const s = status(); return s && s.total ? Math.round((s.done / s.total) * 100) : 0; };
  const [open, setOpen] = createSignal(false);

  const summary = () => {
    const s = status();
    if (view()?.error === "not_found") return t("nclient.saveNotFound");
    if (view()?.error === "busy") return t("nclient.saveBusy");
    if (!s) return view()?.error ? t("nclient.saveFailed") : "";
    if (s.state === "queued") return t("nclient.saveQueued");
    if (s.state === "running") return t("nclient.saving", { done: s.done, total: s.total });
    if (s.state === "cancelled") return t("nclient.saveCancelled", { count: s.saved.length });
    if (s.state === "failed") return s.error === "media_unavailable" ? t("nclient.saveMediaDown") : t("nclient.saveFailed");
    const dup = s.saved.filter(x => x.duplicate).length;
    const base = t("nclient.saveDone", { count: s.saved.length });
    const extra = [dup ? t("nclient.saveDuplicates", { count: dup }) : "", s.failed.length ? t("nclient.saveSomeFailed", { count: s.failed.length }) : ""].filter(Boolean).join(", ");
    return extra ? `${base} (${extra})` : base;
  };

  return <div class="nclient-save" classList={{ "nclient-save--compact": props.compact }}>
    <Show when={!(active() && status())} fallback={
      <button type="button" onClick={() => void props.saves.cancel(props.id)}>{t("nclient.saveCancel")}</button>
    }>
      <button type="button" disabled={view()?.starting} onClick={() => { setOpen(true); void props.saves.start(props.id); }}>
        {status() && saveFinished(status()!.state) || view()?.error ? t("nclient.saveAgain") : t("nclient.save")}
      </button>
    </Show>
    <Show when={(open() || status()) && (summary() || active())}>
      <div class="nclient-save-state" role="status">
        <Show when={active() && status()}>
          <progress max={status()!.total || 1} value={status()!.done} aria-label={t("nclient.saveProgress")}>{pct()}%</progress>
        </Show>
        <span>{summary()}</span>
      </div>
    </Show>
  </div>;
}
