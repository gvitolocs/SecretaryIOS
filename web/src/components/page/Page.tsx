import { For, Show, splitProps, type JSX } from "solid-js";
import "../../styles/page.css";

// The page system (docs/design/page-system.md): one frame, one header row, a
// content grid, cards, list rows and empty states. Built from the desktop
// Training page; styles in styles/page.css (classes `pg-*`).

/**
 * The page frame: the max width, the page padding and the tokens. The layout
 * switches at a content width of 880 px (a container query), so the shell's
 * rail and Lucina's dock are taken into account.
 */
export function Page(props: { children: JSX.Element; class?: string; ref?: (el: HTMLDivElement) => void; label?: string }) {
  return (
    <div class={`pg ${props.class ?? ""}`} ref={(el) => props.ref?.(el)} aria-label={props.label}>
      <div class="pg-inner">{props.children}</div>
    </div>
  );
}

/**
 * The header row: the title and a context line on the left; the section nav
 * and at most one accent action on the right. It stays put while the section
 * nav switches the content below it.
 */
export function PageHeader(props: {
  title: JSX.Element;
  /** One muted line under the title: a date, a count, a status. */
  context?: JSX.Element;
  /** A SectionNav. */
  nav?: JSX.Element;
  /** The one accent action (PrimaryAction), or quiet buttons. */
  action?: JSX.Element;
}) {
  return (
    <header class="pg-head">
      <div class="pg-titles">
        <h1 class="pg-title">{props.title}</h1>
        <Show when={props.context}>
          <p class="pg-context">{props.context}</p>
        </Show>
      </div>
      <Show when={props.nav || props.action}>
        <div class="pg-head-actions">
          {props.nav}
          {props.action}
        </div>
      </Show>
    </header>
  );
}

export interface SectionNavItem {
  label: string;
  /** A route; without it the item is a button that calls onSelect. */
  href?: string;
  onSelect?: () => void;
  current: boolean;
  /** Shown instead of the label; the label becomes the accessible name. */
  icon?: JSX.Element;
}

/**
 * The sections of a page as one compact segmented control. Items are plain
 * links (the router still handles the click), so a query like ?tab=settings
 * can be its own section.
 */
export function SectionNav(props: { label: string; items: SectionNavItem[] }) {
  return (
    <nav class="pg-seg" aria-label={props.label}>
      <For each={props.items}>
        {(item) => {
          const content = () => (item.icon ? item.icon : item.label);
          const common = () => ({
            class: `pg-seg-item${item.icon ? " pg-seg-icon" : ""}`,
            "aria-current": item.current ? ("page" as const) : undefined,
            "aria-label": item.icon ? item.label : undefined,
            title: item.icon ? item.label : undefined,
          });
          return (
            <Show
              when={item.href}
              fallback={
                <button type="button" {...common()} onClick={() => item.onSelect?.()}>
                  {content()}
                </button>
              }
            >
              {(href) => (
                <a href={href()} {...common()}>
                  {content()}
                </a>
              )}
            </Show>
          );
        }}
      </For>
    </nav>
  );
}

/** The page's one filled accent button: a link with `href`, else a button. */
export function PrimaryAction(props: {
  children: JSX.Element;
  href?: string;
  onClick?: () => void;
  disabled?: boolean;
  icon?: JSX.Element;
}) {
  return (
    <Show
      when={props.href}
      fallback={
        <button type="button" class="pg-cta" disabled={props.disabled} onClick={() => props.onClick?.()}>
          {props.icon}
          {props.children}
        </button>
      }
    >
      {(href) => (
        <a href={href()} class="pg-cta">
          {props.icon}
          {props.children}
        </a>
      )}
    </Show>
  );
}

/**
 * The content grid. `rail`: a main column and a sticky rail (62 / 38) that
 * stack below 880 px; `single`: one column up to 880 px; `full`: the whole
 * width, for media grids and maps.
 */
export function PageGrid(props: { layout?: "rail" | "single" | "full"; children: JSX.Element; rail?: JSX.Element; railLabel?: string }) {
  const layout = () => props.layout ?? (props.rail ? "rail" : "single");
  return (
    <div class={`pg-grid pg-grid--${layout()}`}>
      <div class="pg-main">{props.children}</div>
      <Show when={layout() === "rail" && props.rail}>
        <aside class="pg-rail" aria-label={props.railLabel}>
          {props.rail}
        </aside>
      </Show>
    </div>
  );
}

let cardSeq = 0;

/**
 * One surface for every block of content. `list`: the rows inside run edge to
 * edge (ListRow); otherwise the card has 24 px padding. A titled card is a
 * region named by its title.
 */
export function Card(props: {
  title?: JSX.Element;
  /** Quiet actions at the right of the title (ghost buttons, text links). */
  actions?: JSX.Element;
  list?: boolean;
  class?: string;
  /** The region's name when the card has no visible title. */
  label?: string;
  children?: JSX.Element;
}) {
  const id = `pg-card-${++cardSeq}`;
  return (
    <section
      class={`pg-card${props.list ? " pg-card--list" : ""} ${props.class ?? ""}`}
      aria-labelledby={props.title ? id : undefined}
      aria-label={props.title ? undefined : props.label}
    >
      <Show when={props.title || props.actions}>
        <div class="pg-card-head">
          <Show when={props.title}>
            <h2 class="pg-card-title" id={id}>
              {props.title}
            </h2>
          </Show>
          <Show when={props.actions}>
            <div class="pg-card-actions">{props.actions}</div>
          </Show>
        </div>
      </Show>
      {props.children}
    </section>
  );
}

/** The list inside a list card. */
export function RowList(props: { children: JSX.Element; label?: string }) {
  return (
    <ul class="pg-rows" aria-label={props.label}>
      {props.children}
    </ul>
  );
}

/**
 * One row of a list card: an optional leading control (play button, icon,
 * thumbnail), a title with an optional badge, one muted line, a muted meta
 * column and trailing quiet actions. With `href` the title is the link.
 */
export function ListRow(props: {
  title: JSX.Element;
  href?: string;
  badge?: JSX.Element;
  sub?: JSX.Element;
  meta?: JSX.Element;
  leading?: JSX.Element;
  trailing?: JSX.Element;
  class?: string;
}) {
  return (
    <li class={`pg-row ${props.class ?? ""}`}>
      {props.leading}
      <div class="pg-row-main">
        <p class="pg-row-title">
          <Show when={props.href} fallback={<span class="pg-row-name">{props.title}</span>}>
            {(href) => (
              <a href={href()} class="pg-row-name pg-row-link">
                {props.title}
              </a>
            )}
          </Show>
          <Show when={props.badge}>
            <span class="pg-badge">{props.badge}</span>
          </Show>
        </p>
        <Show when={props.sub}>
          <p class="pg-row-sub">{props.sub}</p>
        </Show>
      </div>
      <Show when={props.meta}>
        <p class="pg-row-meta">{props.meta}</p>
      </Show>
      {props.trailing}
    </li>
  );
}

/**
 * A friendly empty state: one muted sentence (an optional title above it) and
 * at most one action, usually a quiet button or a text link.
 */
export function EmptyState(props: { title?: string; text: string; action?: JSX.Element; class?: string }) {
  return (
    <div class={`pg-empty ${props.class ?? ""}`}>
      <Show when={props.title}>
        <p class="pg-empty-title">{props.title}</p>
      </Show>
      <p class="pg-empty-text">{props.text}</p>
      {props.action}
    </div>
  );
}

/** Loading: one quiet line where the content goes. */
export function LoadingState(props: { label: string }) {
  return (
    <p class="pg-loading" role="status">
      {props.label}
    </p>
  );
}

/** A load error: the message and a retry, on a card. */
export function ErrorState(props: { message: string; retryLabel: string; onRetry: () => void }) {
  return (
    <div class="pg-card pg-error" role="alert">
      <p>{props.message}</p>
      <button type="button" class="pg-btn" onClick={() => props.onRetry()}>
        {props.retryLabel}
      </button>
    </div>
  );
}

/** A quiet button; `variant` ghost (no fill), danger, or the default fill. */
export function Button(props: JSX.ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "ghost" | "danger" }) {
  const [own, rest] = splitProps(props, ["variant", "class"]);
  return (
    <button
      type="button"
      {...rest}
      class={`pg-btn${own.variant ? ` pg-btn--${own.variant}` : ""} ${own.class ?? ""}`}
    />
  );
}
