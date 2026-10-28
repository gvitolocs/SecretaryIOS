import { A } from "@solidjs/router";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { acceptLabJob, cancelLabJob, discardLabJob, listLab, retryLabJob, type LabJob } from "../api/gallery";
import { ApiError } from "../api/client";
import { Card, ErrorState, LoadingState, EmptyState, Page, PageGrid, useWide } from "../components/page";
import { GalleryDetail, GalleryHeader } from "../components/gallery/GalleryHeader";
import { LabCard } from "../components/gallery/lab/LabCard";
import { BackIcon } from "../components/Icons";
import { t } from "../i18n";
import { pollDelay, sortJobs } from "../lib/lab";
import "../styles/gallery.css";
import "../styles/gallery-lab.css";

// The AI Lab (/gallery/lab): the edits Lucina's model makes on the owner's own computer, one at a time, in the background.
// A finished edit waits here: Accept puts it in the library as a new picture linked to the original (the original is never
// changed), Discard deletes it, Retry queues a failed one again. The page asks the server often while anything is moving.

type Busy = "accept" | "discard" | "retry" | "cancel";

export default function GalleryLab() {
  const { wide, track } = useWide();
  // A store, reconciled by id: a card keeps its DOM (the slider position, the loaded pictures) while the page asks again.
  const [jobs, setJobs] = createStore<LabJob[]>([]);
  // Copies (plain JSON, so store proxies copy too): the store merges into the objects it is given.
  const show = (next: readonly LabJob[]) => setJobs(reconcile(sortJobs(JSON.parse(JSON.stringify(next)) as LabJob[]), { key: "id" }));
  const [enabled, setEnabled] = createSignal(true);
  const [state, setState] = createSignal<"loading" | "ready" | "error">("loading");
  const [busy, setBusy] = createSignal<Record<string, Busy>>({});
  const [errors, setErrors] = createSignal<Record<string, string>>({});
  let timer: ReturnType<typeof setTimeout> | undefined;
  let alive = true;
  let inFlight = false;

  const refresh = async () => {
    if (inFlight) return;
    inFlight = true;
    try {
      const lab = await listLab();
      if (!alive) return;
      show(lab.jobs);
      setEnabled(lab.enabled);
      setState("ready");
    } catch {
      if (alive && jobs.length === 0) setState("error");
    } finally {
      inFlight = false;
      if (alive) {
        clearTimeout(timer);
        timer = setTimeout(() => void refresh(), pollDelay(jobs));
      }
    }
  };

  onMount(() => void refresh());
  onCleanup(() => {
    alive = false;
    clearTimeout(timer);
  });

  const mark = (id: string, action: Busy | null) =>
    setBusy((all) => {
      const next = { ...all };
      if (action) next[id] = action;
      else delete next[id];
      return next;
    });
  const say = (id: string, message: string | null) =>
    setErrors((all) => {
      const next = { ...all };
      if (message) next[id] = message;
      else delete next[id];
      return next;
    });

  const replace = (job: LabJob | null) => {
    if (!job) return;
    show(job.status === "discarded" ? jobs.filter((j) => j.id !== job.id) : jobs.map((j) => (j.id === job.id ? job : j)));
  };

  /** Run one action on a job; a 409 means the job moved on without us, so the list is reloaded. */
  const run = async (job: LabJob, action: Busy, work: () => Promise<void>) => {
    if (busy()[job.id]) return;
    mark(job.id, action);
    say(job.id, null);
    try {
      await work();
    } catch (err) {
      const conflict = err instanceof ApiError && (err.status === 409 || err.status === 404);
      say(job.id, t(conflict ? "lab.actionConflict" : "lab.actionFailed"));
      if (conflict) void refresh();
    } finally {
      mark(job.id, null);
    }
  };

  const accept = (job: LabJob) =>
    run(job, "accept", async () => {
      const done = await acceptLabJob(job.id);
      replace(done.job);
    });

  const discard = (job: LabJob) =>
    run(job, "discard", async () => {
      await discardLabJob(job.id);
      show(jobs.filter((j) => j.id !== job.id));
    });

  const retry = (job: LabJob) =>
    run(job, "retry", async () => {
      replace(await retryLabJob(job.id));
      void refresh();
    });

  const remove = (job: LabJob) =>
    run(job, "cancel", async () => {
      await cancelLabJob(job.id);
      show(jobs.filter((j) => j.id !== job.id));
    });

  const content = () => (
    <>
      <p class="g-note">{t("lab.intro")}</p>
      <Show when={state() === "ready" && !enabled()}>
        <p classList={{ "lab-warn": !wide(), "pg-notice": wide() }} role="status">
          {t("lab.notSetUp")}
        </p>
      </Show>
      <Show when={state() !== "loading"} fallback={<Show when={wide()} fallback={<p class="g-note" role="status">{t("lab.loading")}</p>}><LoadingState label={t("lab.loading")} /></Show>}>
        <Show
          when={state() !== "error"}
          fallback={
            <Show
              when={wide()}
              fallback={
                <div class="lab-error">
                  <p class="gv-error" role="alert">
                    {t("lab.loadFailed")}
                  </p>
                  <button type="button" class="btn btn--small" onClick={() => void refresh()}>
                    {t("lab.reload")}
                  </button>
                </div>
              }
            >
              <ErrorState message={t("lab.loadFailed")} retryLabel={t("lab.reload")} onRetry={() => void refresh()} />
            </Show>
          }
        >
          <Show
            when={jobs.length > 0}
            fallback={
              <Show when={wide()} fallback={<p class="g-note lab-empty">{t("lab.empty")}</p>}>
                <Card label={t("lab.title")}>
                  <EmptyState text={t("lab.empty")} />
                </Card>
              </Show>
            }
          >
            <ul class="lab-list" aria-label={t("lab.jobsLabel")}>
              <For each={jobs}>
                {(job) => (
                  <LabCard
                    wide={wide()}
                    job={job}
                    busy={busy()[job.id] ?? null}
                    error={errors()[job.id] ?? null}
                    onAccept={(j) => void accept(j)}
                    onDiscard={(j) => void discard(j)}
                    onRetry={(j) => void retry(j)}
                    onRemove={(j) => void remove(j)}
                  />
                )}
              </For>
            </ul>
          </Show>
        </Show>
      </Show>
    </>
  );

  return (
    <section class="gallery lab" classList={{ "g-page-root": wide(), "gallery--wide": wide() }} ref={track} aria-labelledby={wide() ? undefined : "lab-title"} aria-label={wide() ? t("lab.title") : undefined}>
      <Show
        when={wide()}
        fallback={
          <>
            <div class="g-top">
              <header class="g-bar">
                <A class="g-back" href="/gallery" aria-label={t("lab.back")}>
                  <BackIcon />
                </A>
                <h1 class="g-title" id="lab-title">
                  {t("lab.title")}
                </h1>
              </header>
            </div>
            <div class="lab-scroll">{content()}</div>
          </>
        }
      >
        <Page class="g-lib">
          <GalleryHeader count={state() === "ready" ? jobs.length : undefined} />
          <GalleryDetail href="/gallery" back={t("gallery.backToGallery")} title={t("lab.title")} />
          <PageGrid layout="full">{content()}</PageGrid>
        </Page>
      </Show>
    </section>
  );
}
