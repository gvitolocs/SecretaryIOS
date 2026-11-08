import { A } from "@solidjs/router";
import { Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PrimaryAction, RowList, useWide } from "../components/page";
import { GalleryHeader } from "../components/gallery/GalleryHeader";
import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { listAlbums, listShares, revokeShare, type GalleryShare } from "../api/gallery";
import { AlbumIcon } from "../components/gallery/ViewerIcons";
import { SharedLinkRow } from "../components/gallery/share/SharedLinkRow";
import { ShareSheet } from "../components/gallery/share/ShareSheet";
import { Modal } from "../components/gallery/Modal";
import { BackIcon } from "../components/Icons";
import { t } from "../i18n";
import { orderShares, shareUrl } from "../lib/galleryShare";
import "../styles/gallery-share.css";

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** /gallery/shared: every link made from the Gallery (albums and chosen photos), with its views and expiry, and Turn off. */
export default function GallerySharedLinksView() {
  const { wide, track } = useWide();
  const [shares, { refetch, mutate }] = createResource(listShares);
  const [albums] = createResource(() => listAlbums().catch(() => []));
  const [picking, setPicking] = createSignal(false);
  const [album, setAlbum] = createSignal<{ id: string; title: string } | null>(null);

  // Reading a failed resource throws: the list is empty then, and the error shows below.
  const ordered = createMemo(() => orderShares(shares.error ? [] : (shares() ?? [])));
  const live = () => ordered().filter((s) => s.live);
  const over = () => ordered().filter((s) => !s.live);

  const copy = (share: GalleryShare) => copyText(shareUrl(share, window.location));
  const revoke = async (share: GalleryShare) => {
    await revokeShare(share.token);
    // Show it as turned off at once; the next load has the server's own row.
    mutate((list) => list?.map((s) => (s.token === share.token ? { ...s, live: false, revokedAt: new Date().toISOString() } : s)));
  };

  const rows = (list: GalleryShare[]) => <For each={list}>{(s) => <SharedLinkRow wide={wide()} share={s} onCopy={copy} onRevoke={revoke} />}</For>;
  const group = (title: string, list: GalleryShare[]) => (
    <Show when={wide()} fallback={<><h2 class="sl-section">{title}</h2><ul class="sl-list">{rows(list)}</ul></>}>
      <Card list title={title}>
        <RowList>{rows(list)}</RowList>
      </Card>
    </Show>
  );
  const sections = () => (
    <>
      <Show when={live().length}>{group(t("galleryShare.active", { count: live().length }), live())}</Show>
      <Show when={over().length}>{group(t("galleryShare.ended", { count: over().length }), over())}</Show>
    </>
  );
  const body = () => (
    <Show
      when={!shares.loading}
      fallback={
        <Show when={wide()} fallback={<p class="sl-empty" role="status">{t("galleryShare.loading")}</p>}>
          <LoadingState label={t("galleryShare.loading")} />
        </Show>
      }
    >
      <Show
        when={!shares.error}
        fallback={
          <Show
            when={wide()}
            fallback={
              <p class="sl-empty" role="alert">
                {t("galleryShare.loadFailed")}{" "}
                <button type="button" class="btn" onClick={() => void refetch()}>
                  {t("galleryShare.retry")}
                </button>
              </p>
            }
          >
            <ErrorState message={t("galleryShare.loadFailed")} retryLabel={t("galleryShare.retry")} onRetry={() => void refetch()} />
          </Show>
        }
      >
        <Show
          when={ordered().length > 0}
          fallback={
            <Show when={wide()} fallback={<p class="sl-empty">{t("galleryShare.none")}</p>}>
              <Card label={t("galleryShare.pageTitle")}>
                <EmptyState text={t("galleryShare.none")} />
              </Card>
            </Show>
          }
        >
          {sections()}
        </Show>
      </Show>
    </Show>
  );

  return (
    <div class={wide() ? "g-page-root" : "sl-page"} ref={track}>
      <Show
        when={wide()}
        fallback={
          <>
            <header class="sl-head">
              <A class="sl-back" href="/gallery" aria-label={t("gallery.backToGallery")}>
                <BackIcon />
              </A>
              <h1 class="sl-h1">{t("galleryShare.pageTitle")}</h1>
              <button type="button" class="btn" onClick={() => setPicking(true)}>
                <AlbumIcon /> {t("galleryShare.shareAlbum")}
              </button>
            </header>
            {body()}
          </>
        }
      >
        <Page class="g-lib">
          <GalleryHeader
            count={shares.loading || shares.error ? undefined : ordered().length}
            action={
              <PrimaryAction icon={<AlbumIcon />} onClick={() => setPicking(true)}>
                {t("galleryShare.shareAlbum")}
              </PrimaryAction>
            }
          />
          <PageGrid layout="single">{body()}</PageGrid>
        </Page>
      </Show>

      <Show when={picking()}>
        <Modal labelledBy="sl-pick-title" onClose={() => setPicking(false)}>
          <h2 id="sl-pick-title" class="gv-dialog-title">
            {t("galleryShare.pickAlbum")}
          </h2>
          <div class="gv-dialog-body">
            <Show when={(albums() ?? []).length > 0} fallback={<p class="gv-muted">{t("galleryShare.noAlbums")}</p>}>
              <ul class="gv-albums">
                <For each={albums()}>
                  {(a) => (
                    <li>
                      <button
                        type="button"
                        class="gv-album"
                        onClick={() => {
                          setPicking(false);
                          setAlbum({ id: a.id, title: a.title });
                        }}
                      >
                        <span class="gv-album-title">{a.title}</span>
                        <span class="gv-album-count">{a.count}</span>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </div>
          <div class="gv-dialog-actions">
            <button type="button" class="btn" onClick={() => setPicking(false)}>
              {t("common.cancel")}
            </button>
          </div>
        </Modal>
      </Show>
      <Show when={album()}>
        {(a) => (
          <ShareSheet
            target={{ kind: "album", id: a().id, title: a().title }}
            onClose={() => {
              setAlbum(null);
              void refetch();
            }}
          />
        )}
      </Show>
    </div>
  );
}
