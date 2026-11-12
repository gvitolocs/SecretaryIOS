import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { toApiError } from "../api/client";
import { addParcel, deleteParcel, getParcel, listParcels, refreshParcel, setParcelLabel, type Parcel } from "../api/parcels";
import { PlusIcon } from "../components/Icons";
import { Button, Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PageHeader, PrimaryAction, RowList, Sheet, useWide } from "../components/page";
import { t } from "../i18n";
import { carrierName, dateOnly, parcelStage, parcelTitle, splitParcels, stageName, stageTone } from "../lib/parcels";
import { relativeTime } from "../lib/relativeTime";
import { formatDate, formatDateTime } from "../lib/units";
import { parseTs } from "../api/timeline";
import "../styles/parcels.css";

// Parcels: the packages Lucina tracks. /parcels lists them (active first, then a collapsed "Delivered and
// stopped"), /parcels/:id shows one with its history. Lucina checks them hourly when one is near and daily
// otherwise; the owner can check now, rename or stop tracking here.

/** Why a request failed, in words: the parcel service being down has its own sentence. */
function failure(e: unknown): string {
  const err = toApiError(e);
  return err.code === "parcels_unavailable" ? t("parcels.unavailable") : err.message;
}

function StageChip(props: { stage: string }) {
  return (
    <span class={`parcel-chip parcel-chip--${stageTone(props.stage)}`} data-stage={props.stage}>
      {stageName(props.stage)}
    </span>
  );
}

function ScheduleBadge(props: { parcel: Parcel; cap?: boolean }) {
  const label = () => {
    const p = props.parcel;
    if (p.schedule === "hourly") return p.near ? t("parcels.hourlyNear") : t("parcels.hourly");
    if (p.schedule === "daily") return t("parcels.daily");
    return t("parcels.notChecked");
  };
  const next = () => {
    const p = props.parcel;
    if (!p.nextCheck || p.schedule === "stopped") return "";
    const time = relativeTime(p.nextCheck);
    return props.cap ? t("parcels.nextCheckCap", { time }) : t("parcels.nextCheck", { time });
  };
  return (
    <span class="parcel-schedule">
      <span class="pg-badge" data-schedule={props.parcel.schedule}>
        {label()}
      </span>
      <Show when={next()}>
        <span class="parcel-next">{next()}</span>
      </Show>
    </span>
  );
}

function ParcelRow(props: { parcel: Parcel; current: boolean }) {
  const p = () => props.parcel;
  const latest = () => p().events[0];
  const where = () => latest()?.location ?? p().status?.location ?? null;
  const when = () => latest()?.time ?? p().status?.lastUpdate ?? null;
  return (
    <li class="pg-row parcel-row" classList={{ "parcel-row--current": props.current }} data-parcel={p().id}>
      <A href={`/parcels/${encodeURIComponent(p().id)}`} class="parcel-link" aria-current={props.current ? "page" : undefined}>
        <span class="parcel-line">
          <span class="parcel-title">{parcelTitle(p())}</span>
          <StageChip stage={parcelStage(p())} />
        </span>
        <span class="parcel-sub">
          {p().label ? `${carrierName(p().carrier) || t("parcels.noCarrier")} · ${p().number}` : carrierName(p().carrier) || t("parcels.noCarrier")}
        </span>
        <Show when={p().status?.text}>
          <span class="parcel-text">{p().status?.text}</span>
        </Show>
        <Show when={where() || when()}>
          <span class="parcel-sub">
            {[where(), when() ? relativeTime(when()!) : null].filter(Boolean).join(" · ")}
          </span>
        </Show>
        <ScheduleBadge parcel={p()} />
        <Show when={p().lastError}>
          <span class="parcel-error">{p().lastError}</span>
        </Show>
      </A>
    </li>
  );
}

function ParcelList(props: { parcels: Parcel[]; currentId: string | undefined }) {
  const groups = createMemo(() => splitParcels(props.parcels));
  return (
    <>
      <Show when={groups().active.length}>
        <Card list label={t("parcels.listLabel")}>
          <RowList label={t("parcels.listLabel")}>
            <For each={groups().active}>{(p) => <ParcelRow parcel={p} current={p.id === props.currentId} />}</For>
          </RowList>
        </Card>
      </Show>
      <Show when={groups().done.length}>
        <details class="parcel-done" open={groups().done.some((p) => p.id === props.currentId)}>
          <summary>{t("parcels.doneHeading", { count: groups().done.length })}</summary>
          <Card list class="parcel-done-card">
            <RowList>
              <For each={groups().done}>{(p) => <ParcelRow parcel={p} current={p.id === props.currentId} />}</For>
            </RowList>
          </Card>
        </details>
      </Show>
    </>
  );
}

function Fact(props: { label: string; value: string | null | undefined }) {
  return (
    <Show when={props.value}>
      <div class="parcel-fact">
        <dt>{props.label}</dt>
        <dd>{props.value}</dd>
      </div>
    </Show>
  );
}

function ParcelDetail(props: { parcel: Parcel; onChange: (p: Parcel) => void; onStopped: (id: string) => void }) {
  const p = () => props.parcel;
  const [busy, setBusy] = createSignal(false);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [copied, setCopied] = createSignal(false);
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  // Another parcel opened in the same pane: start clean.
  createEffect(
    on(
      () => p().id,
      () => {
        setNotice(null);
        setEditing(false);
        setCopied(false);
      },
      { defer: true },
    ),
  );

  async function checkNow() {
    if (busy()) return;
    setBusy(true);
    setNotice(null);
    try {
      props.onChange(await refreshParcel(p().id));
    } catch (e) {
      setNotice(failure(e));
    } finally {
      setBusy(false);
    }
  }

  async function stop() {
    if (!confirm(t("parcels.stopConfirm", { name: parcelTitle(p()) }))) return;
    setNotice(null);
    try {
      await deleteParcel(p().id);
      props.onStopped(p().id);
    } catch (e) {
      setNotice(failure(e));
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(p().number);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      setCopied(false);
    }
  }

  function startEdit() {
    setDraft(p().label ?? "");
    setEditing(true);
  }

  async function saveLabel(e: Event) {
    e.preventDefault();
    if (saving()) return;
    setSaving(true);
    setNotice(null);
    try {
      const label = draft().trim();
      props.onChange(await setParcelLabel(p().id, label || null));
      setEditing(false);
    } catch (err) {
      setNotice(failure(err));
    } finally {
      setSaving(false);
    }
  }

  const expected = () => {
    const d = dateOnly(p().status?.expectedDelivery);
    return d ? formatDate(d.getTime()) : null;
  };
  const checked = () => {
    const ms = parseTs(p().lastChecked);
    return ms === null ? t("parcels.neverChecked") : `${formatDateTime(ms)} · ${relativeTime(ms)}`;
  };
  const eventTime = (time: string | null) => {
    const ms = parseTs(time);
    return ms === null ? "" : formatDateTime(ms);
  };

  return (
    <section class="parcel-detail" aria-label={parcelTitle(p())}>
      <header class="parcel-head">
        <Show
          when={editing()}
          fallback={
            <div class="parcel-head-row">
              <h2 class="parcel-name">{parcelTitle(p())}</h2>
              <Button variant="ghost" onClick={startEdit} aria-label={t("parcels.editLabel")}>
                {t("common.edit")}
              </Button>
            </div>
          }
        >
          <form class="parcel-head-row" onSubmit={saveLabel}>
            <input
              class="pg-input parcel-label-input"
              aria-label={t("parcels.labelField")}
              placeholder={t("parcels.labelPlaceholder")}
              maxLength={80}
              autocomplete="off"
              value={draft()}
              onInput={(e) => setDraft(e.currentTarget.value)}
            />
            <Button type="submit" disabled={saving()}>
              {saving() ? t("common.saving") : t("common.save")}
            </Button>
            <Button variant="ghost" onClick={() => setEditing(false)}>
              {t("common.cancel")}
            </Button>
          </form>
        </Show>
        <div class="parcel-line">
          <StageChip stage={parcelStage(p())} />
          <Show when={p().status?.text}>
            <span class="parcel-text">{p().status?.text}</span>
          </Show>
        </div>
      </header>

      <Show when={notice()}>
        <p class="pg-notice" role="alert">
          {notice()}
        </p>
      </Show>
      <Show when={p().lastError}>
        <p class="parcel-error">{p().lastError}</p>
      </Show>

      <dl class="parcel-facts">
        <Fact label={t("parcels.carrierFact")} value={carrierName(p().carrier) || t("parcels.noCarrier")} />
        <div class="parcel-fact">
          <dt>{t("parcels.numberLabel")}</dt>
          <dd class="parcel-number">
            <code>{p().number}</code>
            <Button variant="ghost" onClick={() => void copy()}>
              {copied() ? t("parcels.copied") : t("parcels.copy")}
            </Button>
          </dd>
        </div>
        <Fact label={t("parcels.expected")} value={expected()} />
        <Fact label={t("parcels.destination")} value={p().status?.destinationCountry} />
        <div class="parcel-fact">
          <dt>{t("parcels.schedule")}</dt>
          <dd>
            <ScheduleBadge parcel={p()} cap />
          </dd>
        </div>
        <Fact label={t("parcels.lastChecked")} value={checked()} />
      </dl>

      <div class="parcel-actions">
        <Button onClick={() => void checkNow()} disabled={busy()}>
          {busy() ? t("parcels.checking") : t("parcels.checkNow")}
        </Button>
        <Button variant="danger" onClick={() => void stop()}>
          {t("parcels.stop")}
        </Button>
      </div>

      <h3 class="parcel-section">{t("parcels.timeline")}</h3>
      <Show when={p().events.length} fallback={<p class="parcel-empty">{t("parcels.noEvents")}</p>}>
        <ol class="parcel-timeline">
          <For each={p().events}>
            {(ev) => (
              <li class="parcel-event" data-stage={ev.stage}>
                <span class={`parcel-dot parcel-dot--${stageTone(ev.stage)}`} aria-hidden="true" />
                <div>
                  <p class="parcel-event-time">{eventTime(ev.time)}</p>
                  <p class="parcel-event-text">{ev.description}</p>
                  <Show when={ev.location}>
                    <p class="parcel-event-where">{ev.location}</p>
                  </Show>
                </div>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </section>
  );
}

export default function ParcelsView() {
  const params = useParams();
  const navigate = useNavigate();
  const { wide, track } = useWide();
  const [parcels, setParcels] = createSignal<Parcel[] | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  // A parcel opened by link before the list answers (or one the list does not hold).
  const [fetched, setFetched] = createSignal<Parcel | null>(null);
  const [missing, setMissing] = createSignal(false);
  let listSeq = 0;
  let alive = true;
  onCleanup(() => {
    alive = false;
  });

  const current = (): Parcel | null => {
    const id = params.id;
    if (!id) return null;
    return parcels()?.find((p) => p.id === id) ?? (fetched()?.id === id ? fetched() : null);
  };

  async function loadList() {
    const mine = ++listSeq;
    try {
      const answer = await listParcels();
      if (!alive || mine !== listSeq) return;
      setParcels(answer.parcels);
      setError(null);
    } catch (e) {
      if (!alive || mine !== listSeq) return;
      // A list already on screen stays; only an empty page reports the failure.
      if (parcels() === null) setError(failure(e));
    }
  }

  async function loadOne(id: string) {
    try {
      const parcel = await getParcel(id);
      if (!alive || params.id !== id) return;
      upsert(parcel);
      setMissing(false);
    } catch (e) {
      if (!alive || params.id !== id) return;
      if (toApiError(e).status === 404) setMissing(true);
      else if (!current()) setError(failure(e));
    }
  }

  function upsert(parcel: Parcel) {
    setFetched(parcel);
    setParcels((list) => (list ? (list.some((p) => p.id === parcel.id) ? list.map((p) => (p.id === parcel.id ? parcel : p)) : [parcel, ...list]) : list));
  }

  function reload() {
    setError(null);
    void loadList();
    if (params.id) void loadOne(params.id);
  }

  createEffect(
    on(
      () => params.id,
      (id) => {
        setMissing(false);
        if (id) void loadOne(id);
      },
    ),
  );

  onMount(() => {
    void loadList();
    // Back on the page (another tab, the app resumed): ask again.
    const onVisible = () => {
      if (document.hidden) return;
      void loadList();
      if (params.id) void loadOne(params.id);
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);
    onCleanup(() => {
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
    });
  });

  // The add form.
  const [adding, setAdding] = createSignal(false);
  const [number, setNumber] = createSignal("");
  const [carrier, setCarrier] = createSignal("");
  const [label, setLabel] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [formError, setFormError] = createSignal<string | null>(null);

  function openAdd() {
    setNumber("");
    setCarrier("");
    setLabel("");
    setFormError(null);
    setAdding(true);
  }

  async function submit(e: Event) {
    e.preventDefault();
    if (!number().trim() || saving()) return;
    setSaving(true);
    setFormError(null);
    try {
      const parcel = await addParcel({
        number: number().trim(),
        ...(carrier().trim() ? { carrier: carrier().trim() } : {}),
        ...(label().trim() ? { label: label().trim() } : {}),
      });
      upsert(parcel);
      setAdding(false);
      navigate(`/parcels/${encodeURIComponent(parcel.id)}`);
    } catch (err) {
      setFormError(failure(err));
    } finally {
      setSaving(false);
    }
  }

  function stopped(id: string) {
    setParcels((list) => list?.filter((p) => p.id !== id) ?? list);
    setFetched(null);
    navigate("/parcels");
  }

  const activeCount = () => {
    const list = parcels();
    return list ? t("parcels.activeCount", { count: list.filter((p) => p.state === "active").length }) : undefined;
  };

  const list = () => (
    <Show
      when={parcels()}
      fallback={
        <Show when={error()} fallback={<LoadingState label={t("parcels.loading")} />}>
          <ErrorState message={error()!} retryLabel={t("common.tryAgain")} onRetry={reload} />
        </Show>
      }
    >
      {(all) => (
        <Show
          when={all().length}
          fallback={
            <Card label={t("parcels.title")}>
              <EmptyState title={t("parcels.emptyTitle")} text={t("parcels.emptyText")} action={<Button onClick={openAdd}>{t("parcels.add")}</Button>} />
            </Card>
          }
        >
          <ParcelList parcels={all()} currentId={params.id} />
        </Show>
      )}
    </Show>
  );

  const detail = () => (
    <Show
      when={current()}
      fallback={
        <Show
          when={!missing()}
          fallback={
            <Card label={t("parcels.title")}>
              <EmptyState text={t("parcels.notFound")} action={<A href="/parcels" class="pg-link">{t("parcels.back")}</A>} />
            </Card>
          }
        >
          <Show when={error()} fallback={<LoadingState label={t("parcels.loading")} />}>
            <ErrorState message={error()!} retryLabel={t("common.tryAgain")} onRetry={reload} />
          </Show>
        </Show>
      }
    >
      {(parcel) => (
        <Card>
          <Show when={!wide()}>
            <A href="/parcels" class="pg-link pg-back">
              {t("parcels.back")}
            </A>
          </Show>
          <ParcelDetail parcel={parcel()} onChange={upsert} onStopped={stopped} />
        </Card>
      )}
    </Show>
  );

  return (
    <div ref={track}>
      <Page class="parcels-page">
        <PageHeader
          title={t("parcels.title")}
          context={activeCount()}
          action={
            <PrimaryAction icon={<PlusIcon />} onClick={openAdd}>
              {t("parcels.add")}
            </PrimaryAction>
          }
        />
        <Show
          when={wide()}
          fallback={
            <PageGrid layout="single">
              <Show when={params.id} fallback={list()}>
                {detail()}
              </Show>
            </PageGrid>
          }
        >
          <PageGrid layout="split" rail={list()} railLabel={t("parcels.listLabel")}>
            <Show
              when={params.id}
              fallback={
                <Card label={t("parcels.title")}>
                  <EmptyState text={t("parcels.pick")} />
                </Card>
              }
            >
              {detail()}
            </Show>
          </PageGrid>
        </Show>

        <Sheet
          open={adding()}
          onClose={() => setAdding(false)}
          title={t("parcels.addTitle")}
          footer={
            <>
              <Button variant="ghost" onClick={() => setAdding(false)}>
                {t("common.cancel")}
              </Button>
              <Button type="submit" form="parcel-add-form" disabled={!number().trim() || saving()}>
                {saving() ? t("parcels.tracking") : t("parcels.track")}
              </Button>
            </>
          }
        >
          <form id="parcel-add-form" class="parcel-form" onSubmit={submit}>
            <Show when={formError()}>
              <p class="pg-notice" role="alert">
                {formError()}
              </p>
            </Show>
            <label class="pg-field">
              <span class="pg-field-label">{t("parcels.number")}</span>
              <input class="pg-input" value={number()} maxLength={80} autocomplete="off" autocapitalize="characters" onInput={(e) => setNumber(e.currentTarget.value)} />
            </label>
            <div class="pg-field">
              <label class="pg-field-label" for="parcel-carrier">{t("parcels.carrier")}</label>
              <input id="parcel-carrier" class="pg-input" value={carrier()} aria-describedby="parcel-carrier-hint" autocomplete="off" autocapitalize="none" onInput={(e) => setCarrier(e.currentTarget.value)} />
              <span class="pg-field-label" id="parcel-carrier-hint">{t("parcels.carrierHint")}</span>
            </div>
            <label class="pg-field">
              <span class="pg-field-label">{t("parcels.label")}</span>
              <input class="pg-input" value={label()} maxLength={80} autocomplete="off" onInput={(e) => setLabel(e.currentTarget.value)} />
            </label>
          </form>
        </Sheet>
      </Page>
    </div>
  );
}
