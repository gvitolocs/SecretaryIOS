import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createResource, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { toApiError } from "../api/client";
import { clock, memosApi, PROCESSING, type Memo, type MemoItem, type MemoStatus, type MemoTranscript } from "../api/memos";
import { BackIcon } from "../components/Icons";
import { Button, Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageGrid, PageHeader, RowList, useWide } from "../components/page";
import { t, type MessageKey } from "../i18n";
import { formatDateTime } from "../lib/units";
import { createMemoRecorder, flushQueue, queue, setOnUploaded } from "../lib/memoRecorder";
import "../styles/memos.css";

// Voice memos, like Voice Memos: list, record (live waveform, timer, pause/resume),
// playback, and each memo's summary, action items, transcript and PDF.
// Routes: /memos, /memos/:id.

const STATUS_KEYS: Record<MemoStatus, MessageKey> = {
  uploading: "memos.waiting",
  queued: "memos.statusQueued",
  transcribing: "memos.statusTranscribing",
  summarizing: "memos.statusSummarizing",
  rendering: "memos.statusRendering",
  publishing: "memos.statusPublishing",
  done: "memos.statusDone",
  failed: "memos.statusFailed",
};

function Wave(props: { levels: number[]; live: boolean }) {
  let canvas!: HTMLCanvasElement;
  createEffect(() => {
    const levels = props.levels;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth * dpr;
    const h = canvas.clientHeight * dpr;
    canvas.width = w;
    canvas.height = h;
    ctx.clearRect(0, 0, w, h);
    const css = getComputedStyle(canvas);
    ctx.fillStyle = props.live ? css.getPropertyValue("--lu-accent").trim() || "#7f8fe0" : css.getPropertyValue("--lu-text-secondary").trim() || "#8e8e93";
    const bar = 3 * dpr;
    const gap = 2 * dpr;
    const n = Math.floor(w / (bar + gap));
    const shown = levels.slice(-n);
    const x0 = (n - shown.length) * (bar + gap);
    shown.forEach((l, i) => {
      const bh = Math.max(2 * dpr, l * h);
      ctx.fillRect(x0 + i * (bar + gap), (h - bh) / 2, bar, bh);
    });
  });
  return <canvas ref={canvas} class="memo-wave" aria-hidden="true" />;
}

function Recorder(props: { onSaved: () => void }) {
  const rec = createMemoRecorder();
  const active = () => rec.state() !== "idle";
  return (
    <div class="memo-dock">
      <Show when={rec.error() === "mic"}>
        <p class="memo-muted" role="alert">{t("memos.micDenied")}</p>
      </Show>
      <Show when={active()}>
        <Wave levels={rec.levels()} live={rec.state() === "recording"} />
        <div class="memo-dock__timer" aria-live="off">{clock(rec.elapsed())}</div>
        <Show when={rec.state() === "paused"}>
          <span class="memo-muted">{t("memos.paused")}</span>
        </Show>
      </Show>
      <div class="memo-dock__buttons">
        <Show when={active()}>
          <button
            class="memo-btn"
            onClick={() => {
              if (confirm(t("memos.discardConfirm"))) rec.stop(true);
            }}
          >
            {t("memos.discard")}
          </button>
        </Show>
        <button
          class="memo-rec"
          data-active={active() ? "" : undefined}
          aria-label={active() ? t("memos.stop") : t("memos.record")}
          onClick={() => {
            if (active()) {
              rec.stop();
              setTimeout(props.onSaved, 400);
            } else void rec.start();
          }}
        />
        <Show when={active()}>
          <button class="memo-btn" onClick={() => (rec.state() === "paused" ? rec.resume() : rec.pause())}>
            {rec.state() === "paused" ? t("memos.resume") : t("memos.pause")}
          </button>
        </Show>
      </div>
    </div>
  );
}

function StatusChip(props: { memo: Memo }) {
  return (
    <span class="memo-chip" data-state={props.memo.status}>
      {t(STATUS_KEYS[props.memo.status])}
    </span>
  );
}

function List(props: { memos: Memo[] | undefined; loading: boolean; desk?: boolean; selectedId?: string }) {
  if (props.desk) {
    return (
      <Card list label={t("memos.title")}>
        <RowList>
          <For each={queue()}>
            {(q) => (
              <ListRow
                title={formatDateTime(Date.parse(q.recordedAt))}
                badge={q.error ? t("memos.waiting") : t("memos.uploading", { pct: Math.round((q.uploaded / q.sizeBytes) * 100) })}
                sub={q.error}
              />
            )}
          </For>
          <For each={props.memos ?? []}>
            {(m) => (
              <ListRow
                href={`/memos/${m.id}`}
                class={props.selectedId === m.id ? "pg-row--current" : undefined}
                title={m.title}
                badge={<StatusChip memo={m} />}
                sub={
                  <>
                    {formatDateTime(Date.parse(m.recordedAt))} · {clock(m.durationSec)}
                    <Show when={m.preview}>
                      <span class="memo-row__preview">{m.preview}</span>
                    </Show>
                  </>
                }
              />
            )}
          </For>
        </RowList>
        <Show when={!props.loading && (props.memos?.length ?? 0) === 0 && queue().length === 0}>
          <EmptyState title={t("memos.empty")} text={t("memos.emptySub")} />
        </Show>
      </Card>
    );
  }
  return (
    <>
      <For each={queue()}>
        {(q) => (
          <li class="memo-row">
            <div class="memo-row__top">
              <span class="memo-row__title">{formatDateTime(Date.parse(q.recordedAt))}</span>
              <span class="memo-chip">{q.error ? t("memos.waiting") : t("memos.uploading", { pct: Math.round((q.uploaded / q.sizeBytes) * 100) })}</span>
            </div>
            <Show when={q.error}>
              <span class="memo-row__meta">{q.error}</span>
            </Show>
          </li>
        )}
      </For>
      <Show when={!props.loading && (props.memos?.length ?? 0) === 0 && queue().length === 0}>
        <li class="memo-card">
          <strong>{t("memos.empty")}</strong>
          <p class="memo-muted">{t("memos.emptySub")}</p>
        </li>
      </Show>
      <For each={props.memos ?? []}>
        {(m) => (
          <li>
            <A class="memo-row" href={`/memos/${m.id}`}>
              <div class="memo-row__top">
                <span class="memo-row__title">{m.title}</span>
                <StatusChip memo={m} />
              </div>
              <span class="memo-row__meta">
                {formatDateTime(Date.parse(m.recordedAt))} · {clock(m.durationSec)}
              </span>
              <Show when={m.preview}>
                <span class="memo-row__preview">{m.preview}</span>
              </Show>
            </A>
          </li>
        )}
      </For>
    </>
  );
}

function ItemList(props: { title: string; items: MemoItem[]; seek: (t: number) => void }) {
  return (
    <Show when={props.items.length > 0}>
      <section class="memo-card">
        <h2>{props.title}</h2>
        <ul>
          <For each={props.items}>
            {(i) => (
              <li>
                {i.text}
                <Show when={i.owner || i.due}>
                  <span class="memo-muted">
                    {" "}
                    · {[i.owner === "me" ? t("memos.you") : i.owner, i.due ? `${t("memos.due")} ${i.due}` : null].filter(Boolean).join(" · ")}
                  </span>
                </Show>
                <Show when={i.at !== undefined}>
                  <button class="memo-seek" onClick={() => props.seek(i.at ?? 0)}>
                    ▶ {clock(i.at ?? 0)}
                  </button>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </section>
    </Show>
  );
}

function Detail(props: { id: string; onChanged: () => void; desk?: boolean }) {
  const navigate = useNavigate();
  const [detail, { refetch }] = createResource(() => props.id, (id) => memosApi.detail(id));
  const [transcript, setTranscript] = createSignal<MemoTranscript | null>(null);
  const [now, setNow] = createSignal(0);
  const [error, setError] = createSignal<string | null>(null);
  let audio!: HTMLAudioElement;

  createEffect(
    on(detail, (d) => {
      if (!d) return;
      if (d.summary || ["done", "rendering", "publishing", "summarizing"].includes(d.memo.status)) {
        memosApi.transcript(props.id).then(setTranscript, () => undefined);
      }
    }),
  );
  // Follow the pipeline until the memo is done.
  const timer = setInterval(() => {
    const s = detail()?.memo.status;
    if (s && s !== "done" && s !== "failed") void refetch();
  }, 4000);
  onCleanup(() => clearInterval(timer));

  const seek = (t: number) => {
    audio.currentTime = t;
    void audio.play();
  };
  const act = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
      await refetch();
      props.onChanged();
    } catch (e) {
      setError(toApiError(e).message);
    }
  };

  return (
    <article class="memo-detail">
      <Show when={!props.desk}>
        <A href="/memos" class="memo-muted">
          <BackIcon /> {t("memos.back")}
        </A>
      </Show>
      <Show when={error()}>
        <p role="alert" class="memo-chip" data-state="failed">{error()}</p>
      </Show>
      <Show when={detail()} fallback={
        <Show when={props.desk} fallback={<p class="memo-muted">{t("memos.loading")}</p>}>
          <LoadingState label={t("memos.loading")} />
        </Show>
      }>
        {(d) => (
          <>
            <Show when={props.desk} fallback={<h1>{d().memo.title}</h1>}>
              <h2 class="trd-detail-title">{d().memo.title}</h2>
            </Show>
            <span class="memo-muted">
              {formatDateTime(Date.parse(d().memo.recordedAt))} · {clock(d().memo.durationSec)}
              {d().memo.language ? ` · ${d().memo.language?.toUpperCase()}` : ""} <StatusChip memo={d().memo} />
            </span>
            <audio ref={audio} controls preload="metadata" src={memosApi.audioUrl(props.id)} onTimeUpdate={() => setNow(audio.currentTime)} />
            <div class="memo-actions">
              <Show when={d().pdf}>
                <a href={memosApi.pdfUrl(props.id)} target="_blank" rel="noopener">{t("memos.pdf")}</a>
              </Show>
              <Show when={props.desk} fallback={
                <>
                  <button
                    onClick={() => {
                      const title = prompt(t("memos.renamePrompt"), d().memo.title)?.trim();
                      if (title) void act(() => memosApi.rename(props.id, title));
                    }}
                  >
                    {t("memos.rename")}
                  </button>
                  <button
                    disabled={d().memo.status === "uploading" || PROCESSING.includes(d().memo.status)}
                    onClick={() => {
                      const instr = prompt(t("memos.resummarizePrompt"), "");
                      if (instr !== null) void act(() => memosApi.resummarize(props.id, instr));
                    }}
                  >
                    {t("memos.resummarize")}
                  </button>
                  <button
                    onClick={() => {
                      if (!confirm(t("memos.deleteConfirm", { title: d().memo.title }))) return;
                      void act(async () => {
                        await memosApi.remove(props.id);
                        navigate("/memos");
                      });
                    }}
                  >
                    {t("memos.delete")}
                  </button>
                </>
              }>
                <Button
                  onClick={() => {
                    const title = prompt(t("memos.renamePrompt"), d().memo.title)?.trim();
                    if (title) void act(() => memosApi.rename(props.id, title));
                  }}
                >
                  {t("memos.rename")}
                </Button>
                <Button
                  disabled={d().memo.status === "uploading" || PROCESSING.includes(d().memo.status)}
                  onClick={() => {
                    const instr = prompt(t("memos.resummarizePrompt"), "");
                    if (instr !== null) void act(() => memosApi.resummarize(props.id, instr));
                  }}
                >
                  {t("memos.resummarize")}
                </Button>
                <Button
                  variant="danger"
                  onClick={() => {
                    if (!confirm(t("memos.deleteConfirm", { title: d().memo.title }))) return;
                    void act(async () => {
                      await memosApi.remove(props.id);
                      navigate("/memos");
                    });
                  }}
                >
                  {t("memos.delete")}
                </Button>
              </Show>
            </div>
            <Show when={d().summary}>
              {(s) => (
                <>
                  <section class="memo-card">
                    <h2>{t("memos.summary")}</h2>
                    <p>{s().overview}</p>
                    <Show when={s().tags.length}>
                      <p class="memo-muted">{s().tags.map((x) => `#${x}`).join("  ")}</p>
                    </Show>
                  </section>
                  <ItemList title={t("memos.keyPoints")} items={s().keyPoints} seek={seek} />
                  <ItemList title={t("memos.decisions")} items={s().decisions} seek={seek} />
                  <ItemList title={t("memos.actionItems")} items={s().actionItems} seek={seek} />
                </>
              )}
            </Show>
            <Show when={transcript()}>
              {(tr) => (
                <section class="memo-card">
                  <h2>{t("memos.transcript")}</h2>
                  <p class="memo-muted">{t("memos.transcriptNote", { engine: tr().engine })}</p>
                  <For each={tr().segments}>
                    {(seg) => (
                      <button class="memo-seg" data-current={now() >= seg.start && now() < seg.end ? "" : undefined} onClick={() => seek(seg.start)}>
                        <span class="memo-seg__t">{clock(seg.start)}</span>
                        <span class="memo-seg__x">
                          {seg.speaker ? <b>{seg.speaker}: </b> : null}
                          {seg.text}
                        </span>
                      </button>
                    )}
                  </For>
                </section>
              )}
            </Show>
          </>
        )}
      </Show>
    </article>
  );
}

export default function MemosView() {
  const params = useParams<{ id?: string }>();
  const { wide, track } = useWide();
  const [memos, { refetch }] = createResource(() => memosApi.list());
  const reload = () => void refetch();
  onMount(() => {
    setOnUploaded(reload);
    void flushQueue();
  });
  const poll = setInterval(() => {
    const busy = (memos() ?? []).some((m) => m.status !== "done" && m.status !== "failed") || queue().length > 0;
    if (busy) reload();
  }, 5000);
  onCleanup(() => {
    clearInterval(poll);
    setOnUploaded(null);
  });

  const phone = () => (
    <>
      <Show
        when={params.id}
        fallback={
          <>
            <h1>{t("memos.title")}</h1>
            <Show when={memos.error}>
              <p role="alert" class="memo-chip" data-state="failed">{toApiError(memos.error).message}</p>
            </Show>
            <ul class="memos-list">
              <List memos={memos()} loading={memos.loading} />
            </ul>
          </>
        }
      >
        {(id) => <Detail id={id()} onChanged={reload} />}
      </Show>
      <Recorder onSaved={reload} />
    </>
  );

  const desk = () => (
    <Page class="memos-desk">
      <PageHeader title={t("memos.title")} />
      <Show when={memos.error}>
        <ErrorState message={toApiError(memos.error!).message} retryLabel={t("common.tryAgain")} onRetry={reload} />
      </Show>
      <Show when={memos.loading && !memos.error}>
        <LoadingState label={t("memos.loading")} />
      </Show>
      <Show when={!memos.loading && !memos.error}>
        <PageGrid
          layout="split"
          railLabel={t("memos.title")}
          rail={<List memos={memos()} loading={memos.loading} desk selectedId={params.id} />}
        >
          <Show
            when={params.id}
            fallback={<EmptyState text={t("memos.pickOne")} />}
          >
            {(id) => (
              <Card class="memos-desk-detail">
                <Detail id={id()} onChanged={reload} desk />
              </Card>
            )}
          </Show>
        </PageGrid>
      </Show>
      <Recorder onSaved={reload} />
    </Page>
  );

  return (
    <main id="main" class="memos" classList={{ "memos--wide": wide() }} ref={track}>
      <Show when={wide()} fallback={phone()}>
        {desk()}
      </Show>
    </main>
  );
}
