import { For, Show, type JSX } from "solid-js";
import type { GallerySource, SourceKind } from "../../api/gallery";
import { t } from "../../i18n";

/** A small stroke icon for where a picture came from (phone, Mac, old backup, chat, Lucina, everything). */
export function SourceIcon(props: { kind: SourceKind | "all" }): JSX.Element {
  const body = (): JSX.Element => {
    switch (props.kind) {
      case "iphone":
      case "android":
        return (
          <>
            <rect x="7" y="3" width="10" height="18" rx="2.4" />
            <path d="M11 18.2h2" />
          </>
        );
      case "ipad":
        return (
          <>
            <rect x="5" y="3" width="14" height="18" rx="2.2" />
            <path d="M11 18.2h2" />
          </>
        );
      case "mac":
      case "pc":
        return (
          <>
            <rect x="4" y="5" width="16" height="11" rx="1.8" />
            <path d="M2.5 19.5h19" />
          </>
        );
      case "backup":
        return (
          <>
            <rect x="3.5" y="4.5" width="17" height="5" rx="1.6" />
            <path d="M5 9.5v8.2A1.8 1.8 0 0 0 6.8 19.5h10.4a1.8 1.8 0 0 0 1.8-1.8V9.5M10 13.5h4" />
          </>
        );
      case "camera":
        return (
          <>
            <path d="M4 8.5A1.5 1.5 0 0 1 5.5 7H8l1.2-2h5.6L16 7h2.5A1.5 1.5 0 0 1 20 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 17.5z" />
            <circle cx="12" cy="13" r="3.2" />
          </>
        );
      case "chat":
        return <path d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H10l-4.2 3.5c-.5.4-1.3.1-1.3-.6V16A2.5 2.5 0 0 1 4 13.5z" />;
      case "lucina":
        return <path d="M12 3.5l1.9 5.1 5.1 1.9-5.1 1.9L12 17.5l-1.9-5.1L5 10.5l5.1-1.9zM18.5 16l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8z" />;
      case "all":
        return (
          <>
            <rect x="4" y="4" width="6.5" height="6.5" rx="1.6" />
            <rect x="13.5" y="4" width="6.5" height="6.5" rx="1.6" />
            <rect x="4" y="13.5" width="6.5" height="6.5" rx="1.6" />
            <rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.6" />
          </>
        );
      default:
        return (
          <>
            <rect x="4" y="5" width="16" height="14" rx="2.4" />
            <circle cx="9" cy="10" r="1.6" />
            <path d="m5 17 4.5-4.5 3.5 3.5 2.5-2.5L19 17" />
          </>
        );
    }
  };
  return (
    <svg class="icon g-source__icon" viewBox="0 0 24 24" aria-hidden="true">
      {body()}
    </svg>
  );
}

/** The name shown for a source: devices by their name, the two groups in the app's language. */
export function sourceLabel(s: GallerySource): string {
  if (s.id === "chat") return t("gallery.sourceChat");
  if (s.id === "lucina") return t("gallery.sourceLucina");
  return s.label;
}

export interface SourceBarProps {
  sources: GallerySource[];
  all: { photos: number; videos: number } | null;
  /** The selected source id, or `all`. */
  value: string;
  onChange: (id: string) => void;
}

/** Where the pictures come from: All, each phone/Mac/backup, chat shares and Lucina's own, each with its count. */
export function SourceBar(props: SourceBarProps) {
  const total = (s: { photos: number; videos: number }) => s.photos + s.videos;
  const chip = (id: string, kind: SourceKind | "all", label: string, count: number | null): JSX.Element => (
    <button
      type="button"
      class="g-source"
      classList={{ "g-source--on": props.value === id }}
      aria-current={props.value === id ? "true" : undefined}
      data-source={id}
      onClick={() => props.onChange(id)}
    >
      <SourceIcon kind={kind} />
      <span class="g-source__label">{label}</span>
      <Show when={count !== null}>
        <span class="g-source__count">{new Intl.NumberFormat().format(count!)}</span>
      </Show>
    </button>
  );
  return (
    <nav class="g-sources" aria-label={t("gallery.sources")}>
      {chip("all", "all", t("gallery.sourceAll"), props.all ? total(props.all) : null)}
      <For each={props.sources}>{(s) => chip(s.id, s.kind, sourceLabel(s), total(s))}</For>
    </nav>
  );
}
