import { useNavigate } from "@solidjs/router";
import { For, type JSX } from "solid-js";
import { CheckCircleIcon, GamepadIcon, GridIcon, HangerIcon, LinkIcon, NoteIcon } from "../components/Icons";
import { t } from "../i18n";
import { requestPrefill } from "../state/chat";
import { openWardrobe } from "../state/wardrobe";

// The Plugins page (owner, 2026-10-24): every feature that is not Chat, Shopping or
// Training + Weight is a card here instead of a tab. A new feature adds one entry to PLUGINS.

export type Plugin = {
  id: string;
  icon: () => JSX.Element;
  title: () => string;
  detail?: () => string;
  /** A route to open, or an action. */
  href?: string;
  run?: (navigate: (to: string) => void) => void;
};

export const PLUGINS: Plugin[] = [
  { id: "notes", icon: NoteIcon, title: () => t("plugins.notes"), detail: () => t("plugins.notesDetail"), href: "/notes" },
  { id: "tasks", icon: CheckCircleIcon, title: () => t("plugins.tasks"), detail: () => t("plugins.tasksDetail"), href: "/tasks" },
  { id: "links", icon: LinkIcon, title: () => t("plugins.links"), detail: () => t("plugins.linksDetail"), href: "/links" },
  {
    id: "pokemon",
    icon: GamepadIcon,
    title: () => t("plugins.pokemon"),
    detail: () => t("plugins.pokemonDetail"),
    // Lucina plays on nezopt; the chat is where she reports and takes orders.
    run: (navigate) => {
      requestPrefill(t("plugins.pokemonAsk"));
      navigate("/");
    },
  },
  { id: "wardrobe", icon: HangerIcon, title: () => t("plugins.wardrobe"), detail: () => t("plugins.wardrobeDetail"), run: () => openWardrobe() },
  { id: "models", icon: GridIcon, title: () => t("plugins.models"), detail: () => t("plugins.modelsDetail"), href: "/models" },
];

export default function PluginsView() {
  const navigate = useNavigate();
  return (
    <section class="plugins-page" aria-labelledby="plugins-title">
      <h1 id="plugins-title">{t("plugins.title")}</h1>
      <div class="plugins-grid">
        <For each={PLUGINS}>
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
