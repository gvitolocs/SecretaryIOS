import { createSignal, onCleanup, Show, type JSX } from "solid-js";
import { duration, type Criticality, type HostState, type ServiceState, type TimelineKind } from "../../api/systemOverview";
import { locale, t } from "../../i18n";
import { relativeTime } from "../NotificationBell";

// Small shared pieces of the Server page: number formats, status dots with their words, chips,
// the copy-a-command box and the timeline kind icons.

export const num = (n: number, digits = 0) => new Intl.NumberFormat(locale(), { maximumFractionDigits: digits }).format(n);

/** "422 GB", "1.8 TB". */
export function gb(n: number): string {
  return n >= 1000 ? `${num(n / 1000, 1)} TB` : `${num(n)} GB`;
}

export type AnyState = HostState | ServiceState;

export const stateLabel = (s: AnyState) => t(`serverHealth.${s}`);
export const levelLabel = (c: Criticality) => t(`serverHealth.${c}`);

/** "2 min ago". */
export const ago = (iso: string) => relativeTime(Date.parse(iso));
/** "for 2 hr". */
export const forTime = (iso: string) => t("serverHealth.forTime", { time: duration(iso, Date.now(), locale()) });

/** "10:30" today, "27 Oct, 10:30" otherwise. */
export function clock(iso: string): string {
  const d = new Date(iso);
  const today = new Date().toDateString() === d.toDateString();
  const opts: Intl.DateTimeFormatOptions = today ? { hour: "2-digit", minute: "2-digit" } : { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" };
  return new Intl.DateTimeFormat(locale(), opts).format(d);
}

/** A status dot (colour only); pair it with StateWord so the state is also said in words. */
export function Dot(props: { state: AnyState }) {
  return <span class="sh-dot" data-state={props.state} aria-hidden="true" />;
}

/** The state in words: visible when not ok, for screen readers only when ok. */
export function StateWord(props: { state: AnyState; class?: string }) {
  return <span class={props.state === "ok" ? "sr-only" : (props.class ?? "sh-state-word")}>{stateLabel(props.state)}</span>;
}

export function Chip(props: { children: JSX.Element; title?: string }) {
  return (
    <span class="sh-chip" title={props.title}>
      {props.children}
    </span>
  );
}

/** A command in a code box with a Copy button that says "Copied" for a moment. */
export function CopyCommand(props: { label: string; command: string; copyLabel?: string }) {
  const [copied, setCopied] = createSignal(false);
  let timer: number | undefined;
  onCleanup(() => window.clearTimeout(timer));
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.command);
      setCopied(true);
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setCopied(false), 2000);
    } catch {
      // No clipboard (insecure context): the command stays selectable in the box.
    }
  };
  return (
    <div class="sh-cmd">
      <div class="sh-cmd-head">
        <span class="sh-cmd-label">{props.label}</span>
        <button type="button" class="pg-btn" onClick={() => void copy()} aria-label={copied() ? t("serverHealth.copied") : (props.copyLabel ?? t("serverHealth.copy"))}>
          {copied() ? t("serverHealth.copied") : t("serverHealth.copy")}
        </button>
      </div>
      <pre class="sh-cmd-code">
        <code>{props.command}</code>
      </pre>
      <span class="sr-only" role="status">
        {copied() ? t("serverHealth.copied") : ""}
      </span>
    </div>
  );
}

const KIND_LABEL = { incident: "serverHealth.kindIncident", deploy: "serverHealth.kindDeploy", decision: "serverHealth.kindDecision", change: "serverHealth.kindChange" } as const;
export const kindLabel = (k: TimelineKind) => t(KIND_LABEL[k]);

/** The timeline kind as an icon, named for screen readers. */
export function KindIcon(props: { kind: TimelineKind }) {
  return (
    <span class="sh-kind" data-kind={props.kind} role="img" aria-label={kindLabel(props.kind)} title={kindLabel(props.kind)}>
      <svg class="pg-icon" viewBox="0 0 24 24" aria-hidden="true">
        <Show when={props.kind === "incident"}>
          <path d="M12 4 21 19H3Z" />
          <path d="M12 10v4M12 17h.01" />
        </Show>
        <Show when={props.kind === "deploy"}>
          <path d="M12 19V6M6 11l6-6 6 6" />
          <path d="M5 20h14" />
        </Show>
        <Show when={props.kind === "decision"}>
          <path d="M5 21V4h11l-2 4 2 4H5" />
        </Show>
        <Show when={props.kind === "change"}>
          <path d="M4 8h13l-3-3M20 16H7l3 3" />
        </Show>
      </svg>
    </span>
  );
}

export function ChevronIcon() {
  return (
    <svg class="pg-icon sh-chevron" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 6l6 6-6 6" />
    </svg>
  );
}
