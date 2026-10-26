import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { Attachment } from "../api/timeline";
import { t } from "../i18n";
import { formatBytes, formatDuration } from "../lib/attachments";

export interface TrayItem {
  id?: string;
  name: string;
  size: number;
  kind?: Attachment["kind"];
  /** Already-created preview (composer drafts). */
  previewUrl?: string;
  /** Share-target files: the tray creates and revokes its own URL. */
  file?: File;
  durationMs?: number;
}

const ICONS: Record<Attachment["kind"], string> = {
  image: "📷",
  video: "🎬",
  voice: "🎙",
  audio: "🎵",
  pdf: "📕",
  document: "📄",
  file: "📎",
  link: "🔗",
  text: "📝",
  contact: "👤",
  location: "📍",
};

function TrayCard(props: { item: TrayItem; onRemove?: (item: TrayItem) => void }) {
  const [owned, setOwned] = createSignal("");
  onMount(() => {
    if (props.item.previewUrl || !props.item.file) return;
    if (!/^(image|video)\//.test(props.item.file.type)) return;
    try {
      setOwned(URL.createObjectURL(props.item.file));
    } catch {
      // No preview available.
    }
  });
  onCleanup(() => {
    if (!owned()) return;
    try {
      URL.revokeObjectURL(owned());
    } catch {
      // Ignore.
    }
  });
  const src = () => props.item.previewUrl || owned();
  const kind = () => props.item.kind ?? "file";
  return (
    <li class="tray-item" data-kind={kind()}>
      <div class="tray-thumb" aria-hidden={src() ? undefined : "true"}>
        <Show when={src()} fallback={<span>{ICONS[kind()]}</span>}>
          {(url) => (
            <Show when={kind() === "video"} fallback={<img src={url()} alt={props.item.name} />}>
              {/* A video's first frame: an <img> cannot show a video blob. */}
              <video src={url()} muted playsinline preload="metadata" aria-label={props.item.name} />
            </Show>
          )}
        </Show>
        <Show when={(props.item.durationMs ?? 0) > 0}>
          <span class="tray-duration">{formatDuration(props.item.durationMs ?? 0)}</span>
        </Show>
      </div>
      <span class="tray-body">
        <span class="tray-name">
          {props.item.name} <small>{formatBytes(props.item.size)}</small>
        </span>
      </span>
      <Show when={props.onRemove}>
        <button
          type="button"
          class="tray-remove"
          aria-label={t("share.remove", { name: props.item.name })}
          onClick={() => props.onRemove?.(props.item)}
        >
          ×
        </button>
      </Show>
    </li>
  );
}

/**
 * One preview list shared by the PWA share sheet and the chat composer:
 * thumbnail/icon, name, size, video duration and an optional remove button.
 */
export function AttachmentTray(props: {
  items: readonly TrayItem[];
  onRemove?: (item: TrayItem) => void;
  label?: string;
}) {
  return (
    <Show when={props.items.length}>
      <ul class="attachment-tray" aria-label={props.label ?? t("share.tray")}>
        <For each={[...props.items]}>{(item) => <TrayCard item={item} onRemove={props.onRemove} />}</For>
      </ul>
    </Show>
  );
}

export function trayIcon(kind: Attachment["kind"]): string {
  return ICONS[kind];
}
