import { For, Show } from "solid-js";
import { KEEP_COLORS, type KeepColor, type LabelInfo } from "../../api/keep";
import { t, type MessageKey } from "../../i18n";
import { NOTE_TYPES, type NoteType, type SearchState } from "../../lib/keepFilter";
import { swatchFill, type KeepTheme } from "../../lib/keepColors";
import { AudioIcon, BellIcon, CheckBoxIcon, ImageIcon, LabelIcon, LinkIcon } from "./icons";
import { colorName } from "./Pickers";

const TYPE_ICONS: Record<NoteType, () => import("solid-js").JSX.Element> = {
  reminders: BellIcon,
  lists: CheckBoxIcon,
  images: ImageIcon,
  audio: AudioIcon,
  urls: LinkIcon,
};
const TYPE_KEYS: Record<NoteType, MessageKey> = {
  reminders: "keep.typeReminders",
  lists: "keep.typeLists",
  images: "keep.typeImages",
  audio: "keep.typeAudio",
  urls: "keep.typeUrls",
};

export const typeLabel = (type: NoteType): string => t(TYPE_KEYS[type]);

/** What Keep shows when the search box gets focus: filter by type, by label and by colour. */
export function SearchPage(props: { labels: LabelInfo[]; theme: KeepTheme; onPick: (p: Partial<SearchState>) => void }) {
  return (
    <div class="kp-searchpage" role="region" aria-label={t("keep.search")}>
      <h2 class="kp-sp-head">{t("keep.types")}</h2>
      <div class="kp-sp-tiles">
        <For each={NOTE_TYPES}>
          {(type) => {
            const Icon = TYPE_ICONS[type];
            return (
              <button type="button" class="kp-sp-tile" data-type={type} onClick={() => props.onPick({ type })}>
                <Icon />
                <span>{typeLabel(type)}</span>
              </button>
            );
          }}
        </For>
      </div>
      <Show when={props.labels.length > 0}>
        <h2 class="kp-sp-head">{t("keep.labels")}</h2>
        <div class="kp-sp-tiles">
          <For each={props.labels}>
            {(l) => (
              <button type="button" class="kp-sp-tile" data-label={l.tag} onClick={() => props.onPick({ label: l.tag })}>
                <LabelIcon />
                <span>{l.name}</span>
              </button>
            )}
          </For>
        </div>
      </Show>
      <h2 class="kp-sp-head">{t("keep.colors")}</h2>
      <div class="kp-sp-colors">
        <For each={KEEP_COLORS.filter((c) => c !== "default")}>
          {(c: KeepColor) => (
            <button type="button" class="kp-swatch kp-swatch--lg" data-color={c} aria-label={colorName(c)} title={colorName(c)} style={{ "background-color": swatchFill(c, props.theme) }} onClick={() => props.onPick({ color: c })} />
          )}
        </For>
      </div>
    </div>
  );
}
