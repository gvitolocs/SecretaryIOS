import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { t } from "../i18n";
import { deleteShare, linkHost, listShares, sendShare, type ShareEntry, type ShareIntent } from "../lib/share";
import { safeHref } from "../lib/linkify";
import { loadLatest } from "../state/chat";
import { fileKind } from "../lib/attachments";
import { AttachmentTray } from "./AttachmentTray";
import "../styles/share.css";

export function ShareSheet(props: { entry: ShareEntry; onSent: () => void; onClose: () => void }) {
  const [note, setNote] = createSignal("");
  const [intent, setIntent] = createSignal<ShareIntent>("ask");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  let panel!: HTMLDivElement;
  onMount(() => {
    const previous = document.activeElement as HTMLElement | null;
    panel.focus();
    onCleanup(() => previous?.focus());
  });

  async function send() {
    if (busy()) return;
    setBusy(true);
    setError("");
    try {
      await sendShare(props.entry, intent(), note());
      await deleteShare(props.entry.id);
      const url = new URL(window.location.href);
      url.searchParams.delete("share");
      window.history.replaceState(window.history.state, "", url.pathname + url.search + url.hash);
      props.onSent();
      void loadLatest();
    } catch (e) {
      const message = e instanceof Error ? e.message : "";
      setError(
        message === "limits" ? t("share.limits") : message === "empty" ? t("share.empty")
          : `${t("share.failed")}${message ? ` (${message})` : ""}`,
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <Portal>
      <div class="share-backdrop" onClick={(e) => {
        if (e.target === e.currentTarget && !busy()) props.onClose();
      }}>
        <div
          ref={panel}
          class="share-sheet"
          role="dialog"
          aria-modal="true"
          aria-label={t("share.title")}
          tabindex="-1"
          onKeyDown={(e) => {
            if (e.key === "Escape" && !busy()) {
              e.preventDefault();
              props.onClose();
            }
            if (e.key === "Tab") {
              const controls = [...panel.querySelectorAll<HTMLElement>('button:not(:disabled), textarea:not(:disabled), a[href]')];
              const first = controls[0];
              const last = controls[controls.length - 1];
              if (e.shiftKey && (document.activeElement === first || document.activeElement === panel)) {
                e.preventDefault();
                last?.focus();
              } else if (!e.shiftKey && (document.activeElement === last || document.activeElement === panel)) {
                e.preventDefault();
                first?.focus();
              }
            }
          }}
        >
          <header>
            <h2>{t("share.title")}</h2>
            <button type="button" disabled={busy()} onClick={props.onClose} aria-label={t("common.close")}>×</button>
          </header>
          <Show when={props.entry.title}><p>{props.entry.title}</p></Show>
          <div class="share-previews">
            <AttachmentTray items={props.entry.files.map((file) => ({ name: file.name, size: file.size, kind: fileKind(file), file }))} />
            <For each={props.entry.url}>{(url) => (
              <div class="share-preview">
                <Show when={safeHref(url)} fallback={<span>{linkHost(url)}</span>}>
                  {(href) => <a href={href()} target="_blank" rel="noopener noreferrer">🔗 {linkHost(url)}</a>}
                </Show>
              </div>
            )}</For>
            <For each={props.entry.text}>{(text) => <p class="share-preview">{text.slice(0, 400)}</p>}</For>
          </div>
          <label for="share-note">{t("share.note")}</label>
          <textarea id="share-note" maxlength={4000} value={note()} disabled={busy()} onInput={(e) => setNote(e.currentTarget.value)} />
          <div class="share-intents">
            <For each={["library", "shopping", "note", "remind"] as const}>{(value) => (
              <button
                type="button"
                disabled={busy()}
                aria-pressed={intent() === value}
                onClick={() => setIntent(intent() === value ? "ask" : value)}
              >
                {t(value === "note" ? "share.saveNote" : `share.${value}`)}
              </button>
            )}</For>
          </div>
          <Show when={error()}><p role="alert">{error()}</p></Show>
          <button class="share-send" type="button" disabled={busy()} onClick={() => void send()}>
            {busy() ? t("share.sending") : error() ? t("common.tryAgain") : t("share.ask")}
          </button>
        </div>
      </div>
    </Portal>
  );
}

/**
 * Shares that arrived through the PWA share target wait in the IndexedDB inbox
 * and open in the share sheet. New attachments are picked from the chat
 * composer's attach menu, so this only shows a button while shares are pending.
 */
export function ShareComposer() {
  const [entries, setEntries] = createSignal<ShareEntry[]>([]);
  const [open, setOpen] = createSignal(false);
  const [error, setError] = createSignal(false);

  async function refresh(preferredId?: string) {
    if (typeof indexedDB === "undefined") return;
    try {
      const all = (await listShares()).sort((a, b) => a.createdAt - b.createdAt);
      const id = preferredId ?? new URLSearchParams(window.location.search).get("share");
      setEntries([...all.filter((e) => e.id === id), ...all.filter((e) => e.id !== id)]);
      setOpen(all.length > 0);
      setError(false);
    } catch {
      setError(true);
    }
  }

  onMount(() => {
    void refresh();
    const onFocus = () => { if (!open()) void refresh(); };
    window.addEventListener("focus", onFocus);
    onCleanup(() => window.removeEventListener("focus", onFocus));
  });

  return (
    <>
      <Show when={entries().length}>
        <button
          type="button"
          class="share-attach share-pending"
          aria-label={t("share.pending", { count: entries().length })}
          onClick={() => setOpen(true)}
        >
          📥<small>{entries().length}</small>
        </button>
      </Show>
      <Show when={error()}>
        <button type="button" role="alert" onClick={() => void refresh()}>{t("share.storageFailed")}</button>
      </Show>
      <Show when={open() && entries()[0]} keyed>{(entry) => (
        <ShareSheet
          entry={entry}
          onClose={() => setOpen(false)}
          onSent={() => {
            setOpen(false);
            setEntries((all) => all.filter((e) => e.id !== entry.id));
          }}
        />
      )}</Show>
    </>
  );
}
