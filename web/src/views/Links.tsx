import { useSearchParams } from "@solidjs/router";
import { createMemo, createSignal, For, type JSX, Match, onMount, Show, Switch } from "solid-js";
import { toApiError } from "../api/client";
import {
  acceptProposal,
  addCustom,
  addIcs,
  type CalendarEvent,
  connectGoogle,
  connectTodoist,
  disconnectGoogle,
  disconnectTodoist,
  dismissProposal,
  getEvents,
  getLinks,
  type LinkAction,
  type LinkCard,
  type LinkGroup,
  type LinksOverview,
  type Proposal,
  removeCustom,
  removeIcs,
  saveTodoistToken,
  syncCalendars,
  syncTodoist,
} from "../api/links";
import {
  BubbleIcon,
  CalendarIcon,
  CheckCircleIcon,
  DriveIcon,
  GamepadIcon,
  LinkIcon,
  MailIcon,
  PhoneIcon,
  PlugIcon,
  PlusIcon,
  RefreshIcon,
  SendIcon,
  SparkIcon,
  SpeakerIcon,
} from "../components/Icons";
import { locale, type MessageKey, t } from "../i18n";
import { nav } from "../lib/navigate";
import "../styles/links.css";

// The Links tab: the server owns every status (docs/server-api.md, "Links");
// this view renders the cards and sends the connect/disconnect requests.

const GROUP_TITLE: Record<string, MessageKey> = {
  calendars: "links.groupCalendars",
  tasks: "links.groupTasks",
  channels: "links.groupChannels",
  ai: "links.groupAi",
  google: "links.groupGoogle",
  custom: "links.groupCustom",
};

const GROUP_HINT: Record<string, MessageKey> = {
  calendars: "links.hintCalendars",
  tasks: "links.hintTasks",
  channels: "links.hintChannels",
  ai: "links.hintAi",
  google: "links.hintGoogle",
  custom: "links.hintCustom",
};

const STATUS: Record<string, MessageKey> = {
  connected: "links.statusConnected",
  disconnected: "links.statusDisconnected",
  reconnect: "links.statusReconnect",
  error: "links.statusError",
  planned: "links.statusPlanned",
  available: "links.statusAvailable",
  unavailable: "links.statusUnavailable",
  unknown: "links.statusUnknown",
  saved: "links.statusSaved",
};

const NAMES: Record<string, MessageKey> = {
  "google-calendar": "links.nameGoogleCalendar",
  "iphone-calendar": "links.nameIphone",
  "google-gmail": "links.nameGmail",
  "google-drive": "links.nameDrive",
};

function cardName(c: LinkCard): string {
  const key = NAMES[c.id];
  return key ? t(key) : c.name;
}

function cardDetail(c: LinkCard): string | null {
  switch (c.id) {
    case "iphone-calendar":
      return c.status === "connected" ? t("links.detailIphoneOn", { count: c.count ?? 0 }) : t("links.detailIphoneOff");
    case "google-calendar":
    case "google-gmail":
    case "google-drive":
      if (c.status === "unavailable") return t("links.detailGoogleUnavailable");
      if (c.id === "google-calendar" && c.count !== undefined && c.status !== "disconnected")
        return t("links.detailGoogleItems", { count: c.count });
      return null;
    case "channel:telegram":
      return t("links.detailTelegram");
    case "channel:discord":
      return t("links.detailDiscord");
    case "channel:alexa":
      return t("links.detailAlexa");
  }
  if (c.kind === "todoist") {
    return c.status === "disconnected"
      ? c.oauth
        ? t("links.detailTodoistOffOauth")
        : t("links.detailTodoistOff")
      : t("links.detailTodoistOn", { count: c.count ?? 0 });
  }
  if (c.kind === "ai") return c.status === "unknown" ? t("links.detailAiUnknown") : t("links.detailAi");
  if (c.kind === "mcp") return t("links.detailMcp");
  if (c.kind === "ics") {
    const n = t("links.icsEvents", { count: c.count ?? 0 });
    return c.detail ? `${c.detail} · ${n}` : n;
  }
  return c.detail ?? null;
}

function relative(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return null;
  const diff = (ms - Date.now()) / 1000;
  const abs = Math.abs(diff);
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: "auto" });
  if (abs < 45) return rtf.format(0, "second");
  if (abs < 3600) return rtf.format(Math.round(diff / 60), "minute");
  if (abs < 86_400) return rtf.format(Math.round(diff / 3600), "hour");
  if (abs < 30 * 86_400) return rtf.format(Math.round(diff / 86_400), "day");
  return new Date(ms).toLocaleDateString(locale(), { day: "numeric", month: "short", year: "numeric" });
}

function footLine(c: LinkCard): string | null {
  const when = relative(c.lastSyncAt);
  if (c.kind === "channel" || c.kind === "ai") return when ? t("links.activeAt", { time: when }) : null;
  if (c.kind === "bookmark" || c.kind === "mcp") return when ? t("links.addedAt", { time: when }) : null;
  if (c.status === "connected" || c.status === "reconnect" || c.status === "error") {
    return when ? t("links.syncedAt", { time: when }) : t("links.neverSynced");
  }
  return null;
}

function Glyph(props: { card: LinkCard }): JSX.Element {
  const c = props.card;
  const icon = (): JSX.Element => {
    if (c.icon) return <span class="link-glyph-emoji">{c.icon}</span>;
    switch (c.id) {
      case "google-calendar":
        return <CalendarIcon />;
      case "iphone-calendar":
        return <PhoneIcon />;
      case "google-gmail":
        return <MailIcon />;
      case "google-drive":
        return <DriveIcon />;
      case "channel:telegram":
        return <SendIcon />;
      case "channel:discord":
        return <GamepadIcon />;
      case "channel:alexa":
        return <SpeakerIcon />;
    }
    if (c.kind === "ics") return <CalendarIcon />;
    if (c.kind === "todoist") return <CheckCircleIcon />;
    if (c.kind === "ai") return <SparkIcon />;
    if (c.kind === "mcp") return <PlugIcon />;
    if (c.kind === "channel") return <BubbleIcon />;
    return <LinkIcon />;
  };
  return (
    <span class={`link-glyph link-glyph--${c.kind}`} aria-hidden="true">
      {icon()}
    </span>
  );
}

function StatusPill(props: { status: string }) {
  return (
    <span class={`link-status link-status--${props.status}`}>
      <span class="link-status-dot" aria-hidden="true" />
      {t(STATUS[props.status] ?? "links.statusUnknown")}
    </span>
  );
}

interface CardHandlers {
  busy: () => string | null;
  run: (card: LinkCard, action: LinkAction) => Promise<void>;
}

function Card(props: { card: LinkCard; h: CardHandlers }) {
  const [confirming, setConfirming] = createSignal<LinkAction | null>(null);
  const c = () => props.card;
  const busy = () => props.h.busy() === c().id;
  const danger = (a: LinkAction) => a === "disconnect" || a === "remove";
  const label = (a: LinkAction): string => {
    switch (a) {
      case "connect":
        return t("links.connect");
      case "disconnect":
        return t("links.disconnect");
      case "reconnect":
        return t("links.reconnect");
      case "sync":
        return t("links.sync");
      case "remove":
        return t("links.remove");
      case "token":
        return t("links.todoistPasteToken");
      default:
        return a;
    }
  };
  const actions = () => c().actions.filter((a) => a !== "edit");

  async function act(a: LinkAction) {
    if (danger(a) && confirming() !== a) {
      setConfirming(a);
      return;
    }
    setConfirming(null);
    await props.h.run(c(), a);
  }

  return (
    <li class={`link-card link-card--${c().status}`} classList={{ "link-card--busy": busy() }} aria-busy={busy()}>
      <div class="link-card-top">
        <Glyph card={c()} />
        <div class="link-card-title">
          <h3 class="link-name">{cardName(c())}</h3>
          <Show when={c().account}>{(a) => <span class="link-account">{a()}</span>}</Show>
        </div>
        <StatusPill status={c().status} />
      </div>
      <Show when={cardDetail(c())}>{(d) => <p class="link-detail">{d()}</p>}</Show>
      <Show when={c().error}>{(e) => <p class="link-error">{e()}</p>}</Show>
      <Show when={(c().scopes?.length ?? 0) > 0 || (c().tags?.length ?? 0) > 0}>
        <div class="link-chips" aria-label={t("links.access")}>
          <For each={c().scopes ?? []}>{(s) => <span class="link-chip link-chip--scope">{s}</span>}</For>
          <For each={c().tags ?? []}>{(s) => <span class="link-chip">#{s}</span>}</For>
        </div>
      </Show>
      <Show when={footLine(c()) || actions().length > 0 || c().url}>
        <div class="link-card-foot">
          <span class="link-when">{footLine(c())}</span>
          <div class="link-actions">
            <Show when={c().url}>
              {(u) => (
                <a class="btn btn--small" href={u()} target="_blank" rel="noopener noreferrer">
                  {t("links.open")}
                </a>
              )}
            </Show>
            <For each={actions()}>
              {(a) => (
                <Switch>
                  <Match when={confirming() === a}>
                    <span class="confirm-pair">
                      <button type="button" class="btn btn--small btn--danger" disabled={busy()} onClick={() => void act(a)}>
                        {a === "remove" ? t("links.confirmRemove") : t("links.confirmDisconnect")}
                      </button>
                      <button type="button" class="btn btn--small" onClick={() => setConfirming(null)}>
                        {t("common.cancel")}
                      </button>
                    </span>
                  </Match>
                  <Match when={confirming() !== a}>
                    <button
                      type="button"
                      class="btn btn--small"
                      classList={{ "btn--primary": a === "connect" || a === "reconnect" || (a === "token" && !c().oauth) }}
                      disabled={busy()}
                      onClick={() => void act(a)}
                    >
                      {label(a)}
                    </button>
                  </Match>
                </Switch>
              )}
            </For>
          </div>
        </div>
      </Show>
    </li>
  );
}

function eventTime(e: CalendarEvent): string {
  if (e.allDay) return t("links.allDay");
  return new Date(e.startMs).toLocaleTimeString(locale(), { hour: "2-digit", minute: "2-digit" });
}

function eventDay(e: CalendarEvent): string {
  const d = e.allDay ? new Date(`${e.start}T12:00:00`) : new Date(e.startMs);
  return d.toLocaleDateString(locale(), { weekday: "short", day: "numeric", month: "short" });
}

function Upcoming(props: { events: CalendarEvent[] | null }) {
  const MAX = 6;
  return (
    <Show when={props.events}>
      {(events) => (
        <div class="links-upcoming card">
          <h3 class="links-upcoming-title">{t("links.upcomingTitle")}</h3>
          <Show when={events().length > 0} fallback={<p class="muted small">{t("links.upcomingEmpty")}</p>}>
            <ol class="links-agenda">
              <For each={events().slice(0, MAX)}>
                {(e) => (
                  <li class={`links-agenda-row links-agenda-row--${e.source}`}>
                    <span class="links-agenda-day">{eventDay(e)}</span>
                    <span class="links-agenda-time">{eventTime(e)}</span>
                    <span class="links-agenda-title">{e.title}</span>
                    <span class="links-agenda-cal">{e.calendar ?? e.source}</span>
                  </li>
                )}
              </For>
            </ol>
            <Show when={events().length > MAX}>
              <p class="muted small">{t("links.moreEvents", { count: events().length - MAX })}</p>
            </Show>
          </Show>
        </div>
      )}
    </Show>
  );
}

function IcsForm(props: { onAdded: (msg: string) => void }) {
  const [label, setLabel] = createSignal("");
  const [url, setUrl] = createSignal("");
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (pending() || !url().trim()) return;
    setPending(true);
    setError(null);
    try {
      const r = await addIcs(label().trim(), url().trim());
      setLabel("");
      setUrl("");
      props.onAdded(t("links.icsAdded", { count: r.eventCount }));
    } catch (err) {
      setError(toApiError(err).message);
    } finally {
      setPending(false);
    }
  }

  return (
    <form class="card links-form" onSubmit={(e) => void submit(e)} aria-label={t("links.addIcsTitle")}>
      <h3 class="links-form-title">
        <PlusIcon />
        {t("links.addIcsTitle")}
      </h3>
      <p class="muted small">{t("links.icsHelp")}</p>
      <div class="links-form-row">
        <label class="links-field links-field--narrow">
          <span class="field-label">{t("links.icsLabel")}</span>
          <input class="text-input" value={label()} onInput={(e) => setLabel(e.currentTarget.value)} maxlength="80" disabled={pending()} />
        </label>
        <label class="links-field">
          <span class="field-label">{t("links.icsUrl")}</span>
          <input
            class="text-input"
            type="url"
            inputmode="url"
            placeholder="webcal://… · https://…/basic.ics"
            value={url()}
            onInput={(e) => setUrl(e.currentTarget.value)}
            autocomplete="off"
            spellcheck={false}
            disabled={pending()}
            required
          />
        </label>
      </div>
      <Show when={error()}>{(m) => <p class="form-error">{m()}</p>}</Show>
      <button class="btn btn--primary" type="submit" disabled={pending() || !url().trim()}>
        {pending() ? t("links.icsAdding") : t("links.icsAdd")}
      </button>
    </form>
  );
}

function TodoistTokenForm(props: { onSaved: (msg: string) => void; onCancel: () => void }) {
  const [token, setToken] = createSignal("");
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let input: HTMLInputElement | undefined;
  onMount(() => input?.focus());

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const v = token().trim();
    if (!v || pending()) return;
    setPending(true);
    setError(null);
    try {
      const r = await saveTodoistToken(v);
      setToken("");
      props.onSaved(t("links.todoistTokenSaved", { account: r.account ?? "Todoist" }));
    } catch (err) {
      const e2 = toApiError(err);
      setError(e2.code === "todoist_token_rejected" ? t("links.todoistTokenRejected") : e2.message);
    } finally {
      setPending(false);
    }
  }

  return (
    <form class="card links-form todoist-token" onSubmit={(e) => void submit(e)} aria-label={t("links.todoistPasteToken")}>
      <label class="field-label" for="todoist-token">
        {t("links.todoistTokenLabel")}
      </label>
      <p class="links-hint">{t("links.todoistTokenHint")}</p>
      <div class="links-form-row">
        <input
          ref={input}
          id="todoist-token"
          class="text-input"
          type="password"
          autocomplete="off"
          spellcheck={false}
          value={token()}
          onInput={(e) => setToken(e.currentTarget.value)}
          disabled={pending()}
        />
        <button type="submit" class="btn btn--primary btn--small" disabled={pending() || !token().trim()}>
          {pending() ? t("links.todoistChecking") : t("links.todoistTokenSave")}
        </button>
        <button type="button" class="btn btn--small" onClick={() => props.onCancel()}>
          {t("common.cancel")}
        </button>
      </div>
      <Show when={error()}>{(m) => <p class="link-error">{m()}</p>}</Show>
    </form>
  );
}

function LinkForm(props: { onAdded: (msg: string) => void }) {
  const [kind, setKind] = createSignal<"bookmark" | "mcp">("bookmark");
  const [label, setLabel] = createSignal("");
  const [url, setUrl] = createSignal("");
  const [icon, setIcon] = createSignal("");
  const [tags, setTags] = createSignal("");
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (pending()) return;
    setPending(true);
    setError(null);
    try {
      await addCustom({
        kind: kind(),
        label: label().trim(),
        url: url().trim(),
        icon: icon().trim() || undefined,
        tags: tags()
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
      });
      setLabel("");
      setUrl("");
      setIcon("");
      setTags("");
      props.onAdded(t("links.linkAdded"));
    } catch (err) {
      setError(toApiError(err).message);
    } finally {
      setPending(false);
    }
  }

  return (
    <form class="card links-form" onSubmit={(e) => void submit(e)} aria-label={t("links.addLinkTitle")}>
      <div class="links-form-head">
        <h3 class="links-form-title">
          <PlusIcon />
          {t("links.addLinkTitle")}
        </h3>
        <div class="segmented" role="group">
          <button
            type="button"
            class="segmented-btn"
            classList={{ "segmented-btn--active": kind() === "bookmark" }}
            aria-pressed={kind() === "bookmark" ? "true" : "false"}
            onClick={() => setKind("bookmark")}
          >
            {t("links.kindBookmark")}
          </button>
          <button
            type="button"
            class="segmented-btn"
            classList={{ "segmented-btn--active": kind() === "mcp" }}
            aria-pressed={kind() === "mcp" ? "true" : "false"}
            onClick={() => setKind("mcp")}
          >
            {t("links.kindMcp")}
          </button>
        </div>
      </div>
      <Show when={kind() === "mcp"}>
        <p class="muted small">{t("links.mcpHelp")}</p>
      </Show>
      <div class="links-form-row">
        <label class="links-field links-field--icon">
          <span class="field-label">{t("links.linkIcon")}</span>
          <input class="text-input" value={icon()} onInput={(e) => setIcon(e.currentTarget.value)} maxlength="16" placeholder="✦" disabled={pending()} />
        </label>
        <label class="links-field links-field--narrow">
          <span class="field-label">{t("links.linkLabel")}</span>
          <input class="text-input" value={label()} onInput={(e) => setLabel(e.currentTarget.value)} maxlength="80" required disabled={pending()} />
        </label>
        <label class="links-field">
          <span class="field-label">{t("links.linkUrl")}</span>
          <input
            class="text-input"
            type="url"
            inputmode="url"
            placeholder={kind() === "mcp" ? "https://…/mcp" : "https://"}
            value={url()}
            onInput={(e) => setUrl(e.currentTarget.value)}
            autocomplete="off"
            spellcheck={false}
            required
            disabled={pending()}
          />
        </label>
      </div>
      <label class="links-field">
        <span class="field-label">
          {t("links.linkTags")} <span class="muted">· {t("links.linkTagsHint")}</span>
        </span>
        <input class="text-input" value={tags()} onInput={(e) => setTags(e.currentTarget.value)} disabled={pending()} />
      </label>
      <Show when={error()}>{(m) => <p class="form-error">{m()}</p>}</Show>
      <button class="btn btn--primary" type="submit" disabled={pending() || !label().trim() || !url().trim()}>
        {t("links.linkAdd")}
      </button>
    </form>
  );
}

function ProposalRow(props: { p: Proposal; onDone: (msg: string | null) => void; onError: (m: string) => void }) {
  const [pending, setPending] = createSignal(false);
  const when = () => {
    const e = props.p.event;
    if (/^\d{4}-\d{2}-\d{2}$/.test(e.start)) {
      return new Date(`${e.start}T12:00:00`).toLocaleDateString(locale(), { weekday: "short", day: "numeric", month: "short" });
    }
    return new Date(e.start).toLocaleString(locale(), { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  };
  async function run(accept: boolean) {
    setPending(true);
    try {
      if (accept) await acceptProposal(props.p.id);
      else await dismissProposal(props.p.id);
      props.onDone(accept ? t("links.proposalAccepted") : null);
    } catch (err) {
      props.onError(toApiError(err).message);
    } finally {
      setPending(false);
    }
  }
  return (
    <li class="links-proposal">
      <div class="links-proposal-text">
        <strong>{props.p.event.title}</strong>
        <span class="muted small">
          {when()}
          <Show when={props.p.event.location}>{(l) => ` · ${l()}`}</Show>
        </span>
        <span class="muted small">{t("links.proposalFrom", { from: props.p.from })}</span>
      </div>
      <div class="link-actions">
        <button type="button" class="btn btn--small btn--primary" disabled={pending()} onClick={() => void run(true)}>
          {t("links.accept")}
        </button>
        <button type="button" class="btn btn--small" disabled={pending()} onClick={() => void run(false)}>
          {t("links.dismiss")}
        </button>
      </div>
    </li>
  );
}

export default function LinksView() {
  const [params, setParams] = useSearchParams();
  const [data, setData] = createSignal<LinksOverview | null>(null);
  const [events, setEvents] = createSignal<CalendarEvent[] | null>(null);
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [syncing, setSyncing] = createSignal(false);
  const [tokenOpen, setTokenOpen] = createSignal(false);

  async function refresh() {
    try {
      setData(await getLinks());
      setLoadError(null);
    } catch (err) {
      setLoadError(toApiError(err).message);
    }
    try {
      setEvents((await getEvents(7)).events);
    } catch {
      setEvents(null);
    }
  }

  onMount(() => {
    const g = params.google;
    if (g === "connected") setNotice(t("links.googleConnected"));
    else if (g === "error") setError(t("links.googleError", { reason: String(params.reason ?? "unknown") }));
    const td = params.todoist;
    if (td === "connected") setNotice(t("links.todoistConnected"));
    else if (td === "error") setError(t("links.todoistError", { reason: String(params.reason ?? "unknown") }));
    if (g || td) setParams({ google: undefined, todoist: undefined, reason: undefined }, { replace: true });
    void refresh();
  });

  async function syncAll() {
    if (syncing()) return;
    setSyncing(true);
    setError(null);
    try {
      await syncCalendars();
      setNotice(t("links.synced"));
      await refresh();
    } catch (err) {
      setError(toApiError(err).message);
    } finally {
      setSyncing(false);
    }
  }

  const handlers: CardHandlers = {
    busy,
    async run(card, action) {
      setBusy(card.id);
      setError(null);
      setNotice(null);
      try {
        if (card.kind === "todoist") {
          if (action === "connect" || action === "reconnect") {
            const { authorizeUrl } = await connectTodoist();
            nav.assign(authorizeUrl);
            return;
          }
          if (action === "token") {
            // no refresh: it would rebuild the cards and the form with them
            setTokenOpen(true);
            return;
          }
          if (action === "sync") {
            await syncTodoist();
            setNotice(t("links.synced"));
          } else if (action === "disconnect") {
            await disconnectTodoist();
            setNotice(t("links.disconnected"));
          }
          await refresh();
          return;
        }
        if (action === "connect" || action === "reconnect") {
          const services = card.kind === "google" && card.service ? [card.service] : ["calendar"];
          const { authorizeUrl } = await connectGoogle(services);
          nav.assign(authorizeUrl);
          return;
        }
        if (action === "sync") {
          await syncCalendars();
          setNotice(t("links.synced"));
        } else if (action === "disconnect" && card.kind === "google") {
          await disconnectGoogle();
          setNotice(t("links.disconnected"));
        } else if (action === "remove" && card.kind === "ics") {
          await removeIcs(card.id.slice("ics:".length));
        } else if (action === "remove" && (card.kind === "bookmark" || card.kind === "mcp")) {
          await removeCustom(card.id.slice("custom:".length));
        }
        await refresh();
      } catch (err) {
        setError(toApiError(err).message);
      } finally {
        setBusy(null);
      }
    },
  };

  const groups = createMemo<LinkGroup[]>(() => data()?.groups ?? []);
  const proposals = createMemo(() => data()?.proposals ?? []);
  const added = (msg: string) => {
    setNotice(msg);
    void refresh();
  };

  return (
    <div class="links">
      <div class="links-inner">
        <header class="links-head">
          <div>
            <h1 class="view-title">{t("links.title")}</h1>
            <p class="links-sub">{t("links.subtitle")}</p>
          </div>
          <button type="button" class="btn btn--small links-sync" classList={{ "is-spinning": syncing() }} onClick={() => void syncAll()} disabled={syncing()}>
            <RefreshIcon />
            {syncing() ? t("links.syncing") : t("links.syncNow")}
          </button>
        </header>

        <Show when={notice()}>
          {(m) => (
            <div class="links-toast" role="status">
              <span>{m()}</span>
              <button type="button" class="banner-close" onClick={() => setNotice(null)} aria-label={t("common.dismiss")}>
                ×
              </button>
            </div>
          )}
        </Show>
        <Show when={error()}>
          {(m) => (
            <div class="notice" role="alert">
              <span>{m()}</span>
              <button type="button" class="banner-close" onClick={() => setError(null)} aria-label={t("common.dismiss")}>
                ×
              </button>
            </div>
          )}
        </Show>

        <Show
          when={data()}
          fallback={
            <Show when={loadError()} fallback={<p class="muted center" role="status">{t("links.loading")}</p>}>
              {(m) => (
                <div class="empty" role="alert">
                  <p class="empty-title">{m()}</p>
                  <button class="btn" type="button" onClick={() => void refresh()}>
                    {t("common.tryAgain")}
                  </button>
                </div>
              )}
            </Show>
          }
        >
          <Show when={proposals().length > 0}>
            <section class="card links-proposals" aria-label={t("links.proposalsTitle")}>
              <h2 class="section-title">{t("links.proposalsTitle")}</h2>
              <ul class="links-proposal-list">
                <For each={proposals()}>
                  {(p) => (
                    <ProposalRow
                      p={p}
                      onDone={(m) => {
                        if (m) setNotice(m);
                        void refresh();
                      }}
                      onError={setError}
                    />
                  )}
                </For>
              </ul>
            </section>
          </Show>

          <For each={groups()}>
            {(g) => (
              <section class={`links-group links-group--${g.id}`} aria-labelledby={`links-group-${g.id}`}>
                <div class="links-group-head">
                  <h2 class="section-title" id={`links-group-${g.id}`}>
                    {GROUP_TITLE[g.id] ? t(GROUP_TITLE[g.id] as MessageKey) : g.id}
                  </h2>
                  <Show when={GROUP_HINT[g.id]}>{(k) => <p class="links-hint">{t(k())}</p>}</Show>
                </div>
                <Show when={g.id === "calendars"}>
                  <Upcoming events={events()} />
                </Show>
                <Show when={g.cards.length > 0} fallback={<p class="muted small links-empty">{t("links.noLinks")}</p>}>
                  <ul class="links-grid">
                    <For each={g.cards}>{(c) => <Card card={c} h={handlers} />}</For>
                  </ul>
                </Show>
                <Show when={g.id === "calendars"}>
                  <IcsForm onAdded={added} />
                </Show>
                <Show when={g.id === "tasks" && tokenOpen()}>
                  <TodoistTokenForm
                    onSaved={(m) => {
                      setTokenOpen(false);
                      added(m);
                    }}
                    onCancel={() => setTokenOpen(false)}
                  />
                </Show>
                <Show when={g.id === "custom"}>
                  <LinkForm onAdded={added} />
                </Show>
              </section>
            )}
          </For>
        </Show>
      </div>
    </div>
  );
}
