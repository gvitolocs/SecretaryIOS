import { useNavigate } from "@solidjs/router";
import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { createAlbumShare, createSelectionShare, revokeShare, type GalleryItem, type GalleryShare } from "../../../api/gallery";
import { t, type MessageKey } from "../../../i18n";
import {
  canShareFiles,
  canShareLinks,
  DEFAULT_EXPIRY,
  downloadEntries,
  downloadSequentially,
  EXPIRY_CHOICES,
  expiryDays,
  expiryState,
  fileNameOf,
  fitsAsFiles,
  shareable,
  shareUrl,
  startDownload,
  type ExpiryChoice,
} from "../../../lib/galleryShare";
import { Modal } from "../Modal";
import "../../../styles/gallery-share.css";

/** What a sheet shares: chosen photos and videos, or one whole album. */
export type ShareTarget = { kind: "items"; items: GalleryItem[] } | { kind: "album"; id: string; title: string };

export interface ShareSheetProps {
  target: ShareTarget;
  onClose: () => void;
}

const wait = (ms: number) => new Promise<void>((resolve) => window.setTimeout(resolve, ms));

/** Copies text; true when it worked (the Clipboard API, else a selected field and the old command). */
async function copyText(text: string, field: HTMLInputElement | undefined): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Blocked (not secure, no permission): fall through to the field.
  }
  if (!field) return false;
  field.focus();
  field.select();
  try {
    return document.execCommand("copy");
  } catch {
    return false;
  }
}

/**
 * The Share sheet (Google Photos' Share): make a link with an expiry and an optional right to download originals,
 * copy it, pass it to the system share sheet, send the pictures themselves (files when the browser can, else the link),
 * or just download them. The links made here are managed in /gallery/shared.
 */
export function ShareSheet(props: ShareSheetProps) {
  const items = createMemo(() => (props.target.kind === "items" ? shareable(props.target.items) : []));
  const isAlbum = () => props.target.kind === "album";
  const [expiry, setExpiry] = createSignal<ExpiryChoice>(DEFAULT_EXPIRY);
  const [original, setOriginal] = createSignal(false);
  const [title, setTitle] = createSignal("");
  const [link, setLink] = createSignal<GalleryShare | null>(null);
  const [busy, setBusy] = createSignal<"link" | "off" | "files" | "download" | null>(null);
  const [message, setMessage] = createSignal<{ text: string; error: boolean } | null>(null);
  let field: HTMLInputElement | undefined;
  let closed = false;
  // The desktop shell's viewer lives outside the router: there the link is an ordinary page load.
  let go: ((to: string) => void) | undefined;
  try {
    go = useNavigate();
  } catch {
    go = undefined;
  }
  onCleanup(() => {
    closed = true;
  });

  const say = (key: MessageKey, error = false, values?: Record<string, string | number>) =>
    setMessage({ text: t(key, values), error });
  const address = () => {
    const l = link();
    return l ? shareUrl(l, window.location) : "";
  };
  const heading = () => {
    const target = props.target;
    if (target.kind === "album") return t("galleryShare.titleAlbum", { title: target.title });
    return t("galleryShare.titleItems", { count: items().length });
  };

  // ---- the link ----------------------------------------------------------------------------------------------------

  const create = async (): Promise<GalleryShare | null> => {
    const existing = link();
    if (existing?.live) return existing;
    const options = { expiresInDays: expiryDays(expiry()), allowOriginal: original(), title: title() };
    setBusy("link");
    setMessage(null);
    try {
      const target = props.target;
      const made =
        target.kind === "album"
          ? await createAlbumShare(target.id, options)
          : await createSelectionShare(
              items().map((i) => i.id),
              options,
            );
      if (closed) return made;
      setLink(made);
      return made;
    } catch {
      if (!closed) say("galleryShare.createFailed", true);
      return null;
    } finally {
      if (!closed) setBusy(null);
    }
  };

  const copy = async () => {
    const made = await create();
    if (!made) return;
    const ok = await copyText(shareUrl(made, window.location), field);
    say(ok ? "galleryShare.copied" : "galleryShare.copyFailed", !ok);
  };

  const shareLink = async () => {
    const made = await create();
    if (!made) return;
    try {
      await navigator.share({ title: made.title || heading(), url: shareUrl(made, window.location) });
    } catch {
      // Cancelled in the system sheet.
    }
  };

  const turnOff = async () => {
    const l = link();
    if (!l || busy()) return;
    setBusy("off");
    try {
      await revokeShare(l.token);
      if (closed) return;
      setLink(null);
      say("galleryShare.turnedOff");
    } catch {
      if (!closed) say("galleryShare.turnOffFailed", true);
    } finally {
      if (!closed) setBusy(null);
    }
  };

  // ---- the files ---------------------------------------------------------------------------------------------------

  /** The originals as files, fetched one by one (HEIC stays HEIC: the system share sheet takes it). */
  const fetchFiles = async (): Promise<File[]> => {
    const files: File[] = [];
    for (const item of items()) {
      const href = item.urls.download ?? item.urls.original;
      if (!href) continue;
      const response = await fetch(href, { credentials: "same-origin" });
      if (!response.ok) throw new Error(String(response.status));
      const blob = await response.blob();
      files.push(new File([blob], fileNameOf(item), { type: item.mime || blob.type }));
    }
    return files;
  };

  const sendFiles = async () => {
    if (busy()) return;
    setBusy("files");
    setMessage(null);
    try {
      if (fitsAsFiles(items())) {
        const files = await fetchFiles();
        if (canShareFiles(files)) {
          try {
            await navigator.share({ files, title: heading() });
          } catch {
            // Cancelled in the system sheet.
          }
          return;
        }
      }
      // The browser can't hand these over as files (or there are too many): share the link instead. (The button is
      // only there where `navigator.share` exists.)
      setBusy(null);
      await shareLink();
      say("galleryShare.sharedLinkInstead");
    } catch {
      if (!closed) say("galleryShare.filesFailed", true);
    } finally {
      if (!closed) setBusy(null);
    }
  };

  const download = async () => {
    const entries = downloadEntries(items());
    if (busy() || !entries.length) return;
    setBusy("download");
    setMessage(null);
    const started = await downloadSequentially(entries, (e) => startDownload(e), wait, 600, () => closed);
    if (closed) return;
    setBusy(null);
    say("galleryShare.downloading", false, { count: started });
  };

  const life = () => {
    const l = link();
    if (!l) return "";
    const state = expiryState(l, Date.now());
    if (state.kind === "never") return t("galleryShare.neverExpires");
    if (state.kind === "days") return t("galleryShare.expiresIn", { count: state.days });
    return "";
  };

  return (
    <Modal labelledBy="gs-title" describedBy="gs-body" busy={busy() === "link" || busy() === "off"} onClose={props.onClose}>
      <h2 id="gs-title" class="gv-dialog-title">
        {heading()}
      </h2>
      <div id="gs-body" class="gv-dialog-body gs-body">
        <section class="gs-section" aria-labelledby="gs-link-title">
          <h3 id="gs-link-title" class="gs-h">
            {t("galleryShare.linkTitle")}
          </h3>
          <Show
            when={link()}
            fallback={
              <>
                <p class="gv-muted">{t("galleryShare.linkHelp")}</p>
                <Show when={!isAlbum() && items().length > 1}>
                  <label class="gs-field">
                    <span>{t("galleryShare.nameLabel")}</span>
                    <input
                      type="text"
                      class="text-input"
                      maxLength={200}
                      value={title()}
                      placeholder={t("galleryShare.namePlaceholder")}
                      onInput={(e) => setTitle(e.currentTarget.value)}
                    />
                  </label>
                </Show>
                <label class="gs-field">
                  <span>{t("galleryShare.expiryLabel")}</span>
                  <select class="text-input" value={expiry()} onChange={(e) => setExpiry(e.currentTarget.value as ExpiryChoice)}>
                    <For each={EXPIRY_CHOICES}>
                      {(choice) => <option value={choice}>{t(`galleryShare.expiry_${choice}` as MessageKey)}</option>}
                    </For>
                  </select>
                </label>
                <label class="gv-check">
                  <input type="checkbox" checked={original()} onChange={(e) => setOriginal(e.currentTarget.checked)} />
                  <span>
                    <span class="gv-check-label">{t("galleryShare.allowOriginal")}</span>
                    <span class="gv-check-hint">{t("galleryShare.allowOriginalHint")}</span>
                  </span>
                </label>
                <button type="button" class="btn btn--primary" disabled={busy() !== null || (!isAlbum() && items().length === 0)} onClick={() => void create()}>
                  {busy() === "link" ? t("galleryShare.creating") : t("galleryShare.create")}
                </button>
              </>
            }
          >
            {(made) => (
              <>
                <label class="gs-field">
                  <span class="gs-sr">{t("galleryShare.linkLabel")}</span>
                  <input ref={field} type="text" class="text-input gs-link" readOnly value={address()} onFocus={(e) => e.currentTarget.select()} />
                </label>
                <p class="gv-muted gs-life">
                  {life()} · {made().allowOriginal ? t("galleryShare.originalsAllowed") : t("galleryShare.originalsNot")}
                </p>
                <div class="gs-row">
                  <button type="button" class="btn btn--primary" onClick={() => void copy()}>
                    {t("galleryShare.copy")}
                  </button>
                  <Show when={canShareLinks()}>
                    <button type="button" class="btn" onClick={() => void shareLink()}>
                      {t("galleryShare.shareLink")}
                    </button>
                  </Show>
                  <button type="button" class="btn" disabled={busy() === "off"} onClick={() => void turnOff()}>
                    {busy() === "off" ? t("galleryShare.turningOff") : t("galleryShare.turnOff")}
                  </button>
                </div>
                <p class="gv-muted gs-note">{t("galleryShare.stripped")}</p>
              </>
            )}
          </Show>
        </section>

        <Show when={!isAlbum()}>
          <section class="gs-section" aria-labelledby="gs-files-title">
            <h3 id="gs-files-title" class="gs-h">
              {t("galleryShare.filesTitle", { count: items().length })}
            </h3>
            <div class="gs-row">
              <Show when={canShareLinks()}>
                <button type="button" class="btn" disabled={busy() !== null || items().length === 0} onClick={() => void sendFiles()}>
                  {busy() === "files" ? t("galleryShare.preparing") : t("galleryShare.sendFiles")}
                </button>
              </Show>
              <button type="button" class="btn" disabled={busy() !== null || downloadEntries(items()).length === 0} onClick={() => void download()}>
                {busy() === "download" ? t("galleryShare.downloadingNow") : t("galleryShare.download", { count: items().length })}
              </button>
            </div>
          </section>
        </Show>

        <Show when={message()}>
          {(m) => (
            <p class={m().error ? "gv-error" : "gs-status"} role={m().error ? "alert" : "status"}>
              {m().text}
            </p>
          )}
        </Show>
      </div>
      <div class="gv-dialog-actions gs-actions">
        <a
          class="gs-manage"
          href="/gallery/shared"
          onClick={(e) => {
            if (!go || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
            e.preventDefault();
            props.onClose();
            go("/gallery/shared");
          }}
        >
          {t("galleryShare.manage")}
        </a>
        <button type="button" class="btn" onClick={() => props.onClose()}>
          {t("galleryShare.done")}
        </button>
      </div>
    </Modal>
  );
}
