import { A } from "@solidjs/router";
import { createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import {
  createStack,
  dismissDuplicates,
  dismissStackSuggestion,
  listDuplicates,
  listStackSuggestions,
  resolveAllDuplicates,
  resolveDuplicates,
  undoDuplicates,
  type DuplicateGroup,
  type DuplicatesPage,
  type GalleryItem,
  type StackSuggestion,
} from "../api/gallery";
import { ApiError } from "../api/client";
import { Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PrimaryAction, useWide } from "../components/page";
import { GalleryDetail, GalleryHeader } from "../components/gallery/GalleryHeader";
import { DuplicateGroupCard } from "../components/gallery/duplicates/DuplicateGroupCard";
import { GalleryViewer } from "../components/gallery/GalleryViewer";
import { Modal } from "../components/gallery/Modal";
import { StackSuggestionList } from "../components/gallery/stacks/StackSuggestionList";
import { BackIcon } from "../components/Icons";
import { locale, t } from "../i18n";
import { formatBytes } from "../lib/drive";
import { copiesOf, withoutPhotos, withoutSuggestion } from "../lib/galleryDedup";
import { supportsJxl } from "../lib/jxl";
import "../styles/gallery-dedup.css";

// /gallery/duplicates: the groups of the same photo the server found, side by side with the suggested keeper marked,
// "Keep this" / "Not duplicates" per group, "Apply all suggestions" for the sure ones, and the proposals to stack bursts.
// Resolving hides the copies under the photo you keep; nothing is deleted, and every step has an Undo.

const PAGE = 50;

interface Toast {
  text: string;
  error?: boolean;
  undo?: () => Promise<void>;
}

export default function GalleryDuplicatesView() {
  const { wide, track } = useWide();
  const [jxl] = createResource(supportsJxl);
  const [data, setData] = createSignal<DuplicatesPage | null>(null);
  const [groups, setGroups] = createSignal<DuplicateGroup[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [failed, setFailed] = createSignal(false);
  const [limit, setLimit] = createSignal(PAGE);
  const [busy, setBusy] = createSignal(false);
  const [confirming, setConfirming] = createSignal(false);
  const [toast, setToast] = createSignal<Toast | null>(null);
  const [viewer, setViewer] = createSignal<{ items: GalleryItem[]; index: number } | null>(null);

  const [bursts, setBursts] = createSignal<StackSuggestion[] | null>(null);
  const [burstsPending, setBurstsPending] = createSignal(false);
  const [burstsOpen, setBurstsOpen] = createSignal(false);
  const [burstsFailed, setBurstsFailed] = createSignal(false);
  const [burstCount, setBurstCount] = createSignal(0);

  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(toastTimer));
  function say(next: Toast, ms = next.undo ? 15_000 : next.error ? 8000 : 4000) {
    clearTimeout(toastTimer);
    setToast(next);
    toastTimer = setTimeout(() => setToast(null), ms);
  }

  async function load(): Promise<void> {
    try {
      const page = await listDuplicates({ limit: limit() });
      setData(page);
      setGroups(page.groups);
      setBurstCount(page.stackSuggestions);
      setFailed(false);
    } catch {
      setFailed(true);
    } finally {
      setLoading(false);
    }
  }
  onMount(() => void load());

  const applicable = () => groups().filter((g) => g.applicable).length;
  const total = () => data()?.total ?? 0;
  const hasMore = () => groups().length < total();
  const reclaimable = () => formatBytes(data()?.reclaimableBytes ?? 0, locale());

  async function run(job: () => Promise<void>): Promise<void> {
    if (busy()) return;
    setBusy(true);
    try {
      await job();
    } catch {
      say({ text: t("galleryDedup.actionFailed"), error: true });
    } finally {
      setBusy(false);
    }
  }

  /** Puts merged copies back, then loads the groups again (they come back as groups). */
  const undoMerge = (ids: string[]) => async () => {
    await undoDuplicates(ids);
    setToast(null);
    say({ text: t("galleryDedup.undone") });
    await load();
  };

  function afterMerge(merged: { id: string }[], skipped: number) {
    const ids = merged.map((m) => m.id);
    setGroups((list) => withoutPhotos(list, new Set(ids)));
    if (ids.length) say({ text: t("galleryDedup.resolved", { count: ids.length }), undo: undoMerge(ids) });
    else if (skipped) say({ text: t("galleryDedup.skipped", { count: skipped }), error: true });
  }

  const keep = (group: DuplicateGroup, keepId: string) =>
    run(async () => {
      const result = await resolveDuplicates([keepId], copiesOf(group, keepId));
      // The group is settled even when a copy was skipped: refresh to see what is left.
      afterMerge(result.merged, result.skipped.length);
      if (result.skipped.length) await load();
    });

  const dismiss = (group: DuplicateGroup) =>
    run(async () => {
      const ids = group.members.map((m) => m.item.id);
      await dismissDuplicates(ids);
      setGroups((list) => list.filter((g) => g.id !== group.id));
      say({
        text: t("galleryDedup.dismissedDone"),
        undo: async () => {
          await dismissDuplicates(ids, true);
          setToast(null);
          await load();
        },
      });
    });

  const applyAll = () =>
    run(async () => {
      const result = await resolveAllDuplicates();
      setConfirming(false);
      const skipped = Object.values(result.skipped).reduce((a, b) => a + b, 0);
      afterMerge(result.merged, skipped);
      await load();
    });

  // ---- bursts -------------------------------------------------------------------------------------------------------

  async function showBursts() {
    setBurstsOpen(true);
    if (bursts() !== null) return;
    try {
      const result = await listStackSuggestions();
      setBursts(result.suggestions);
      setBurstsPending(result.hashesPending > 0);
      setBurstCount(result.total);
    } catch {
      setBurstsFailed(true);
    }
  }

  const stackBurst = (s: StackSuggestion) =>
    run(async () => {
      try {
        await createStack(s.items.map((i) => i.id), { primary: s.primary, kind: s.kind });
      } catch (e) {
        // Something in the set is stacked or gone since the proposal: the proposal is stale.
        if (e instanceof ApiError && e.status === 409) {
          setBursts((list) => (list ? withoutSuggestion(list, s.id) : list));
          setBurstCount((n) => Math.max(0, n - 1));
        }
        say({ text: t("galleryDedup.stackFailed"), error: true });
        return;
      }
      setBursts((list) => (list ? withoutSuggestion(list, s.id) : list));
      setBurstCount((n) => Math.max(0, n - 1));
      say({ text: t("galleryDedup.stackedDone", { count: s.items.length }) });
    });

  const dismissBurst = (s: StackSuggestion) =>
    run(async () => {
      await dismissStackSuggestion(s.items.map((i) => i.id));
      setBursts((list) => (list ? withoutSuggestion(list, s.id) : list));
      setBurstCount((n) => Math.max(0, n - 1));
      say({ text: t("galleryDedup.suggestionDismissed") });
    });

  // ---- the viewer over one group ---------------------------------------------------------------------------------------

  function open(group: DuplicateGroup, index: number) {
    setViewer({ items: group.members.map((m) => m.item), index });
  }

  function removedInViewer(id: string) {
    setGroups((list) => withoutPhotos(list, new Set([id])));
    setViewer((v) => {
      if (!v) return v;
      const items = v.items.filter((i) => i.id !== id);
      return items.length < 2 ? null : { items, index: Math.min(v.index, items.length - 1) };
    });
  }

  const retryLoad = () => {
    setLoading(true);
    void load();
  };
  const loadingLine = () => (
    <Show when={wide()} fallback={<p class="dd-state" role="status">{t("gallery.loading")}</p>}>
      <LoadingState label={t("gallery.loading")} />
    </Show>
  );

  const content = () => (
    <Show when={!loading()} fallback={loadingLine()}>
      <Show
        when={!failed()}
        fallback={
          <Show
            when={wide()}
            fallback={
              <p class="dd-state" role="alert">
                {t("galleryDedup.loadError")}{" "}
                <button type="button" class="btn" onClick={retryLoad}>
                  {t("galleryDedup.retry")}
                </button>
              </p>
            }
          >
            <ErrorState message={t("galleryDedup.loadError")} retryLabel={t("galleryDedup.retry")} onRetry={retryLoad} />
          </Show>
        }
      >
        <p class="dd-intro">{t("galleryDedup.intro")}</p>

        <Show when={burstCount() > 0}>
          <div class="dd-banner" classList={{ "pg-card": wide() }} role="status">
            <span>{t("galleryDedup.burstsBanner", { count: burstCount() })}</span>
            <button type="button" class={wide() ? "pg-btn" : "btn"} onClick={() => (burstsOpen() ? setBurstsOpen(false) : void showBursts())}>
              {burstsOpen() ? t("galleryDedup.burstsHide") : t("galleryDedup.burstsReview")}
            </button>
          </div>
        </Show>
        <Show when={burstsOpen()}>
          <Show when={!burstsFailed()} fallback={<p class="dd-state" role="alert">{t("galleryDedup.loadError")}</p>}>
            <Show when={bursts()} fallback={loadingLine()}>
              {(list) => (
                <StackSuggestionList
                  wide={wide()}
                  suggestions={list()}
                  jxl={jxl() ?? false}
                  busy={busy()}
                  pending={burstsPending()}
                  onStack={(s) => void stackBurst(s)}
                  onDismiss={(s) => void dismissBurst(s)}
                />
              )}
            </Show>
          </Show>
        </Show>

        <Show when={(data()?.hashes.pending ?? 0) > 0}>
          <p class="dd-note" role="status">
            {t("galleryDedup.pending", { count: data()?.hashes.pending ?? 0 })}
          </p>
        </Show>

        <Show
          when={groups().length > 0}
          fallback={
            <Show when={(data()?.hashes.pending ?? 0) === 0}>
              <Show
                when={wide()}
                fallback={
                  <div class="dd-state">
                    <p>{t("galleryDedup.empty")}</p>
                    <p class="dd-note">{t("galleryDedup.emptyHint")}</p>
                  </div>
                }
              >
                <Card label={t("galleryDedup.title")}>
                  <EmptyState title={t("galleryDedup.empty")} text={t("galleryDedup.emptyHint")} />
                </Card>
              </Show>
            </Show>
          }
        >
          <p class="dd-summary">{t("galleryDedup.summary", { count: total() })}</p>
          <div class="dd-groups">
            <For each={groups()}>
              {(group) => <DuplicateGroupCard wide={wide()} group={group} jxl={jxl() ?? false} busy={busy()} onKeep={(g, id) => void keep(g, id)} onDismiss={(g) => void dismiss(g)} onOpen={open} />}
            </For>
          </div>
          <Show when={hasMore()}>
            <div class="dd-more">
              <button
                type="button"
                class={wide() ? "pg-btn" : "btn"}
                disabled={busy()}
                onClick={() => {
                  setLimit((n) => n + PAGE);
                  void load();
                }}
              >
                {t("galleryDedup.loadMore")}
              </button>
            </div>
          </Show>
        </Show>
      </Show>
    </Show>
  );

  return (
    <div class={wide() ? "g-page-root" : "dd-page"} ref={track}>
      <Show
        when={wide()}
        fallback={
          <>
            <header class="dd-head">
              <A class="g-back" href="/gallery" aria-label={t("gallery.backToGallery")}>
                <BackIcon />
              </A>
              <h1 class="dd-h1">{t("galleryDedup.title")}</h1>
              <Show when={applicable() > 0}>
                <button type="button" class="btn btn--primary" disabled={busy()} onClick={() => setConfirming(true)}>
                  {t("galleryDedup.applyAll")}
                </button>
              </Show>
            </header>
            {content()}
          </>
        }
      >
        <Page class="g-lib">
          <GalleryHeader
            count={loading() || failed() ? undefined : total()}
            action={
              <Show when={applicable() > 0}>
                <PrimaryAction disabled={busy()} onClick={() => setConfirming(true)}>
                  {t("galleryDedup.applyAll")}
                </PrimaryAction>
              </Show>
            }
          />
          <GalleryDetail href="/gallery" back={t("gallery.backToGallery")} title={t("galleryDedup.title")} />
          <PageGrid layout="full">{content()}</PageGrid>
        </Page>
      </Show>

      <Show when={toast()}>
        {(message) => (
          <div class="dd-toast" classList={{ "dd-toast--error": message().error }} role={message().error ? "alert" : "status"}>
            <span>{message().text}</span>
            <Show when={message().undo}>
              {(undo) => (
                <button type="button" onClick={() => void run(undo())}>
                  {t("galleryDedup.undo")}
                </button>
              )}
            </Show>
            <button type="button" aria-label={t("galleryDedup.dismiss")} onClick={() => setToast(null)}>
              ×
            </button>
          </div>
        )}
      </Show>

      <Show when={confirming()}>
        <Modal labelledBy="dd-apply-title" describedBy="dd-apply-body" busy={busy()} onClose={() => setConfirming(false)} initialFocus={() => document.getElementById("dd-apply-cancel")}>
          <h2 id="dd-apply-title" class="gv-dialog-title">
            {t("galleryDedup.applyAllTitle")}
          </h2>
          <div id="dd-apply-body" class="gv-dialog-body">
            <p>{t("galleryDedup.applyAllBody", { count: applicable() })}</p>
            <p>{t("galleryDedup.applyAllSize", { size: reclaimable() })}</p>
          </div>
          <div class="gv-dialog-actions">
            <button type="button" id="dd-apply-cancel" class="btn" disabled={busy()} onClick={() => setConfirming(false)}>
              {t("galleryDedup.cancel")}
            </button>
            <button type="button" class="btn btn--primary" disabled={busy()} onClick={() => void applyAll()}>
              {busy() ? t("galleryDedup.applying") : t("galleryDedup.applyAllConfirm")}
            </button>
          </div>
        </Modal>
      </Show>

      <Show when={viewer()}>
        {(v) => (
          <GalleryViewer
            items={v().items}
            index={v().index}
            onIndex={(index) => setViewer({ ...v(), index })}
            onClose={() => setViewer(null)}
            onChanged={(item) => setViewer({ ...v(), items: v().items.map((i) => (i.id === item.id ? item : i)) })}
            onRemoved={removedInViewer}
          />
        )}
      </Show>
    </div>
  );
}
