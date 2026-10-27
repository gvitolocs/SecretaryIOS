import { useNavigate } from "@solidjs/router";
import { For, type JSX } from "solid-js";
import { BookIcon, BubbleIcon, CheckCircleIcon, CodeIcon, ComputerIcon, DriveIcon, FilmIcon, GamepadIcon, GridIcon, HangerIcon, LinkIcon, MicIcon, NoteIcon, PhoneIcon, PulseIcon, QrIcon } from "../components/Icons";
import { t } from "../i18n";
import { desktopShell } from "../lib/desktopShell";
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
};

/** The Android Secretary app's WebView adds "SecretaryAndroid/<version>" to its user agent. */
export function inAndroidApp(): boolean {
  return typeof navigator !== "undefined" && /\bSecretaryAndroid\//.test(navigator.userAgent);
}

export const PLUGINS: Plugin[] = [
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
  { id: "desk", icon: CheckCircleIcon, title: () => "My desk", href: "/desk" },
  // Approve a QR sign-in on another screen (views/ScanQr.tsx).
  { id: "scan", icon: QrIcon, title: () => t("login.scanOpen"), detail: () => t("login.scanOpenHint"), href: "/scan" },
  { id: "memos", icon: MicIcon, title: () => t("plugins.memos"), detail: () => t("plugins.memosDetail"), href: "/memos" },
  { id: "films", icon: FilmIcon, title: () => t("plugins.films"), detail: () => t("plugins.filmsDetail"), href: "/films" },
  { id: "drive", icon: DriveIcon, title: () => t("plugins.drive"), detail: () => t("plugins.driveDetail"), href: "/drive" },
  { id: "library", icon: BookIcon, title: () => t("plugins.library"), detail: () => t("plugins.libraryDetail"), href: "/library" },
  { id: "notes", icon: NoteIcon, title: () => t("plugins.notes"), detail: () => t("plugins.notesDetail"), href: "/notes" },
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
  { id: "models", icon: GridIcon, title: () => t("plugins.models"), detail: () => t("plugins.modelsDetail"), href: "/models" },
];

export default function PluginsView() {
  const navigate = useNavigate();
  return (
    <section class="plugins-page" aria-labelledby="plugins-title">
      <h1 id="plugins-title">{t("plugins.title")}</h1>
      <div class="plugins-grid">
        <For each={PLUGINS.filter((plugin) => plugin.visible?.() ?? true)}>
          {(plugin) => (
            <button
              type="button"
              class="plugin-card"
              data-plugin={plugin.id}
              onClick={() => (plugin.href ? navigate(plugin.href) : plugin.run?.(navigate))}
            >
              <span class="plugin-icon">{plugin.icon()}</span>
              <span class="plugin-title">{plugin.title()}</span>
              <span class="plugin-detail">{plugin.detail?.() ?? ""}</span>
            </button>
          )}
        </For>
      </div>
    </section>
  );
}
