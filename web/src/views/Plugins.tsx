import { useNavigate } from "@solidjs/router";
import { For, Show, type JSX } from "solid-js";
import { AgentsIcon, BookIcon, BulbIcon, BubbleIcon, CheckCircleIcon, CodeIcon, ComputerIcon, DriveIcon, FilmIcon, GamepadIcon, GridIcon, HangerIcon, InstagramIcon, LinkIcon, ListIcon, MicIcon, NoteIcon, NotebookIcon, PagesIcon, PhoneIcon, PixivIcon, PulseIcon, QrIcon, SpeakerIcon } from "../components/Icons";
import { EmptyState, Page, PageGrid, PageHeader, useWide } from "../components/page";
import { t } from "../i18n";
import { desktopShell } from "../lib/desktopShell";
import { setShowAdult, showAdult } from "../lib/adult";
import { instagramBadges } from "../state/instagramBadge";
import { openWardrobe } from "../state/wardrobe";

// The Plugins page (owner, 2026-10-24): every feature that is not Chat, Shopping or
// Training + Weight is a card here instead of a tab. A new feature adds one entry to PLUGINS.
// /media (audio and photos) has no card while the web Drive does not exist; Films replaced it.

export type Plugin = {
  id: string;
  icon: () => JSX.Element;
  title: () => string;
  detail?: () => string;
  /** A route to open, or an action. */
  href?: string;
  run?: (navigate: (to: string) => void) => void;
  /** Hidden where the plugin can't run (the native phone screens exist only in the Android app). */
  visible?: () => boolean;
  /** Adult content: owner-only on the server, hidden here when "Show adult plugins" is off. */
  adult?: boolean;
  /** A count shown as a red dot on the card (0 or undefined: none). */
  badge?: () => number;
};

/** The cards this device shows. */
export function visiblePlugins(): Plugin[] {
  return PLUGINS.filter((plugin) => (plugin.visible?.() ?? true) && (!plugin.adult || showAdult()));
}

/** The Android Secretary app's WebView adds "SecretaryAndroid/<version>" to its user agent. */
export function inAndroidApp(): boolean {
  return typeof navigator !== "undefined" && /\bSecretaryAndroid\//.test(navigator.userAgent);
}

export const PLUGINS: Plugin[] = [
  { id: "approvals", icon: CheckCircleIcon, title: () => t("approvals.title"), href: "/approvals" },
  { id: "decisions", icon: CheckCircleIcon, title: () => t("decisions.title"), detail: () => t("decisions.listSub"), href: "/decisions" },
  { id: "chats", icon: BubbleIcon, title: () => t("chats.title"), detail: () => t("chats.detail"), href: "/chats" },
  {
    id: "code",
    icon: CodeIcon,
    title: () => t("plugins.code"),
    detail: () => t("plugins.codeDetail"),
    // Secretary Code on the Mac shows its coding sessions over the web app (lib/desktopShell).
    visible: () => !!desktopShell(),
    run: () => desktopShell()?.showCode(),
  },
  // Lucina Agents: coding agents on nezopt (views/Agents.tsx).
  { id: "agents", icon: AgentsIcon, title: () => t("agents.title"), detail: () => t("agents.detail"), href: "/agents" },
  { id: "desk", icon: CheckCircleIcon, title: () => "My desk", href: "/desk" },
  // Approve a QR sign-in on another screen (views/ScanQr.tsx).
  { id: "scan", icon: QrIcon, title: () => t("login.scanOpen"), detail: () => t("login.scanOpenHint"), href: "/scan" },
  { id: "memos", icon: MicIcon, title: () => t("plugins.memos"), detail: () => t("plugins.memosDetail"), href: "/memos" },
  // Lucina Prompter: talking points crossed out live (views/Prompter.tsx).
  { id: "prompter", icon: ListIcon, title: () => t("prompter.title"), detail: () => t("prompter.detail"), href: "/prompter" },
  { id: "films", icon: FilmIcon, title: () => t("plugins.films"), detail: () => t("plugins.filmsDetail"), href: "/films" },
  { id: "drive", icon: DriveIcon, title: () => t("plugins.drive"), detail: () => t("plugins.driveDetail"), href: "/drive" },
  { id: "library", icon: BookIcon, title: () => t("plugins.library"), detail: () => t("plugins.libraryDetail"), href: "/library" },
  // Notebooks: sources, linked Keep notes and a cited chat, from Open Notebook (views/Notebooks.tsx).
  { id: "notebooks", icon: NotebookIcon, title: () => t("notebooks.title"), detail: () => t("notebooks.detail"), href: "/notebooks" },
  { id: "notes", icon: NoteIcon, title: () => t("plugins.notes"), detail: () => t("plugins.notesDetail"), href: "/notes" },
  // Adult, owner-only (secretary-nclient's child-safety filter is on the server): hidden when "Show adult plugins" is off.
  { id: "nclient", icon: PagesIcon, title: () => t("nclient.title"), detail: () => t("nclient.detail"), href: "/nclient", adult: true },
  { id: "keep", icon: BulbIcon, title: () => t("keep.title"), detail: () => t("keep.detail"), href: "/keep" },
  { id: "tasks", icon: CheckCircleIcon, title: () => t("plugins.tasks"), detail: () => t("plugins.tasksDetail"), href: "/tasks" },
  { id: "links", icon: LinkIcon, title: () => t("plugins.links"), detail: () => t("plugins.linksDetail"), href: "/links" },
  {
    id: "pokemon",
    icon: GamepadIcon,
    title: () => t("plugins.pokemon"),
    detail: () => t("plugins.pokemonDetail"),
    href: "/pokemon",
  },
  { id: "computer", icon: ComputerIcon, title: () => t("computer.title"), detail: () => t("computer.detail"), href: "/computer" },
  { id: "wardrobe", icon: HangerIcon, title: () => t("plugins.wardrobe"), detail: () => t("plugins.wardrobeDetail"), run: () => openWardrobe() },
  {
    id: "phone",
    icon: PhoneIcon,
    title: () => t("plugins.phone"),
    detail: () => t("plugins.phoneDetail"),
    visible: inAndroidApp,
    // The app handles secretary://calls and opens its native Calls screen.
    run: () => {
      window.location.href = "secretary://calls";
    },
  },
  { id: "server", icon: PulseIcon, title: () => t("plugins.server"), detail: () => t("plugins.serverDetail"), href: "/server-health" },
  // The LucinaVoice ESP32 speaker: status, latency, power (views/Voice.tsx).
  { id: "voice", icon: SpeakerIcon, title: () => t("voice.title"), detail: () => t("voice.detail"), href: "/voice" },
  { id: "instagram", icon: InstagramIcon, title: () => t("instagram.title"), detail: () => t("instagram.detail"), href: "/instagram", badge: () => instagramBadges().total },
  { id: "models", icon: GridIcon, title: () => t("plugins.models"), detail: () => t("plugins.modelsDetail"), href: "/models" },
  { id: "pixiv", icon: PixivIcon, title: () => t("pixiv.title"), detail: () => t("pixiv.detail"), href: "/pixiv" },
];

export default function PluginsView() {
  const navigate = useNavigate();
  const { wide, track } = useWide();
  const tiles = () => <>
        <For each={visiblePlugins()}>
          {(plugin) => (
            <button
              type="button"
              class={wide() ? "pg-card plugins-desk-tile" : "plugin-card"}
              data-plugin={plugin.id}
              onClick={() => (plugin.href ? navigate(plugin.href) : plugin.run?.(navigate))}
            >
              <span class="plugin-icon">{plugin.icon()}</span>
              <Show when={(plugin.badge?.() ?? 0) > 0}>
                <span class="plugin-badge" role="img" aria-label={t("instagram.newCount", { count: plugin.badge?.() ?? 0 })}>
                  {plugin.badge?.()}
                </span>
              </Show>
              <span class="plugin-title">{plugin.title()}</span>
              <span class="plugin-detail">{plugin.detail?.() ?? ""}</span>
            </button>
          )}
        </For>
  </>;
  const adultToggle = () => <div class="plugins-adult-settings"><label class={wide() ? "plugins-desk-adult" : "plugins-adult"}>
    <input type="checkbox" checked={showAdult()} onChange={(e) => setShowAdult(e.currentTarget.checked)} />
    <span>{t("plugins.showAdult")}</span>
  </label><a class="pg-link" href="/account?section=plugins">{t("plugins.settings")}</a></div>;
  return (
    <div ref={track}><Show when={wide()} fallback={<section class="plugins-page" aria-labelledby="plugins-title">
      <h1 id="plugins-title">{t("plugins.title")}</h1>
      <div class="plugins-grid">
        {tiles()}
      </div>
      {adultToggle()}
    </section>}><Page>
      <PageHeader title={t("plugins.title")} context={t("plugins.count", { count: visiblePlugins().length })} />
      <PageGrid layout="full">
        <Show when={visiblePlugins().length} fallback={<EmptyState text={t("plugins.empty")} />}>
          <div class="plugins-desk-grid" role="region" aria-label={t("plugins.title")}>{tiles()}</div>
        </Show>
        {adultToggle()}
      </PageGrid>
    </Page></Show></div>
  );
}
