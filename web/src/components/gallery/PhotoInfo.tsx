import { createEffect, createMemo, createResource, createSignal, For, Show, untrack, type JSX } from "solid-js";
import { getDetails, listAlbums, listSources, type GalleryItem, type ItemDetails } from "../../api/gallery";
import { locale, t, type MessageKey } from "../../i18n";
import { formatBytes } from "../../lib/drive";
import { formatDuration } from "../../lib/attachments";
import {
  daysLeft,
  formatExposure,
  formatNumber,
  formatTaken,
  megapixels,
  normalizeOffset,
  placeLine,
} from "../../lib/galleryInfo";
import { chipLabel, personPath } from "../../lib/galleryPeople";
import { placeRoute } from "../../lib/mapKit";
import { LucinaMap } from "../LucinaMap";
import { CopyInfo } from "./compare/CopyInfo";
import { CategoryValue } from "./categories/CategoryValue";
import "../../styles/gallery-viewer.css";
import "../../styles/lucinaMap.css";

export interface PhotoInfoProps {
  item: GalleryItem;
  /** Shown in the Trash: the days left. */
  trash?: boolean;
  onClose: () => void;
  /** Saves the caption; rejects when the server refuses (the draft stays). */
  onCaption: (caption: string) => Promise<void>;
  /** A person chip was chosen (their page opens); without it the chips are plain links. */
  onPerson?: (id: string) => void;
  /** Changes when the photo's people did (a face was assigned): the details are asked for again. */
  peopleKey?: number;
}

// ---- what the panel asks the server once per page, not per picture -------------------------------------------------

let albumsCache: { at: number; titles: Promise<Map<string, string>> } | null = null;

/** Album id -> title, cached for a minute; `fresh` asks again (an id the cache doesn't know). */
function albumTitles(fresh = false): Promise<Map<string, string>> {
  if (!fresh && albumsCache && Date.now() - albumsCache.at < 60_000) return albumsCache.titles;
  const titles = listAlbums().then((list) => new Map(list.map((a) => [a.id, a.title])));
  albumsCache = { at: Date.now(), titles };
  titles.catch(() => {
    if (albumsCache?.titles === titles) albumsCache = null;
  });
  return titles;
}

let kindsCache: { at: number; kinds: Promise<Map<string, string>> } | null = null;

/** Device id -> kind ("iphone", "mac"…) from the source list. */
function deviceKinds(): Promise<Map<string, string>> {
  if (kindsCache && Date.now() - kindsCache.at < 60_000) return kindsCache.kinds;
  const kinds = listSources().then((r) => new Map(r.sources.filter((s) => s.id.startsWith("device:")).map((s) => [s.id.slice("device:".length), s.kind as string])));
  kindsCache = { at: Date.now(), kinds };
  kinds.catch(() => {
    if (kindsCache?.kinds === kinds) kindsCache = null;
  });
  return kinds;
}

/** For tests: forget what was cached. */
export function resetPhotoInfoCaches(): void {
  albumsCache = null;
  kindsCache = null;
}

const KNOWN_KINDS = new Set(["iphone", "ipad", "android", "mac", "pc", "backup", "camera", "chat", "lucina", "other"]);

function Row(props: { label: string; children: JSX.Element }) {
  return (
    <>
      <dt>{props.label}</dt>
      <dd>{props.children}</dd>
    </>
  );
}

/** The info panel: when, where, what with, how big, from where, which albums, and the caption. */
export function PhotoInfo(props: PhotoInfoProps) {
  // The key is `<id>|<people revision>`: a string, so an edit of the item (a new object, the same id) asks for nothing.
  const [details] = createResource(
    () => `${props.item.id}|${props.peopleKey ?? 0}`,
    async (key): Promise<ItemDetails | null> => {
      try {
        return await getDetails(key.slice(0, key.lastIndexOf("|")));
      } catch {
        return null;
      }
    },
  );
  const [kinds] = createResource(
    () => props.item.device,
    async (device) => {
      if (!device) return null;
      try {
        return (await deviceKinds()).get(device) ?? null;
      } catch {
        return null;
      }
    },
  );
  const [titles] = createResource(
    () => props.item.albums.join("\n"),
    async (key) => {
      if (!key) return new Map<string, string>();
      try {
        const known = await albumTitles();
        return props.item.albums.every((id) => known.has(id)) ? known : await albumTitles(true);
      } catch {
        return new Map<string, string>();
      }
    },
  );

  // The details of this photo: kept while they are asked for again after a face changed hands (so nothing blinks), never
  // those of the photo before.
  const loaded = (): ItemDetails | null => {
    if (details.state !== "ready" && details.state !== "refreshing") return null;
    const d = details();
    return d && d.item.id === props.item.id ? d : null;
  };
  const exif = () => loaded()?.exif ?? null;
  const people = () => loaded()?.people ?? [];
  const item = () => props.item;

  const takenAt = () => item().takenAt ?? exif()?.takenAt ?? null;
  const offset = () => normalizeOffset(exif()?.offset);
  const taken = createMemo(() => formatTaken(takenAt(), offset(), locale()));
  const kindLabel = () => {
    const kind = kinds();
    return kind && KNOWN_KINDS.has(kind) ? t(`galleryInfo.kind${kind}` as MessageKey) : "";
  };
  const camera = () => {
    const e = exif();
    const name = [e?.make, e?.model].filter((v): v is string => !!v);
    // "Apple" + "Apple iPhone 15": the model already names the make.
    const text = name.length === 2 && name[1]!.toLowerCase().startsWith(name[0]!.toLowerCase()) ? name[1]! : name.join(" ");
    return text || item().camera || "";
  };
  const settings = () => {
    const e = exif();
    if (!e) return "";
    const parts: string[] = [];
    if (e.iso !== null) parts.push(t("galleryInfo.iso", { value: String(Math.round(e.iso)) }));
    const exposure = formatExposure(e.exposureS);
    if (exposure) parts.push(t("galleryInfo.exposure", { value: exposure }));
    const aperture = formatNumber(e.fNumber);
    if (aperture) parts.push(t("galleryInfo.aperture", { value: aperture }));
    const focal = formatNumber(e.focalMm, 0);
    if (focal) parts.push(t("galleryInfo.focal", { value: focal }));
    return parts.join(" · ");
  };
  const dimensions = () => {
    const { width, height } = item();
    if (!width || !height) return "";
    const mp = megapixels(width, height);
    return `${width} × ${height}${mp ? ` · ${t("galleryInfo.megapixels", { value: mp })}` : ""}`;
  };
  const duration = () => (item().durationMs ? formatDuration(item().durationMs ?? undefined) : "");
  const albumNames = () => item().albums.map((id) => titles()?.get(id) ?? "").filter(Boolean);
  const origin = () => item().origin;
  const left = () => (props.trash || item().trashedAt ? daysLeft(item().trashExpiresAt) : null);

  // The caption: a draft that is saved on request.
  const [draft, setDraft] = createSignal(item().caption ?? "");
  const [saving, setSaving] = createSignal(false);
  let seen = { id: item().id, caption: item().caption ?? "" };
  createEffect(() => {
    const now = { id: item().id, caption: item().caption ?? "" };
    const before = seen;
    seen = now;
    // Another picture: its caption. The same picture's caption changed elsewhere (the rename dialog): follow it, unless
    // something else is typed or our own save is running (it sets, and on a failure unsets, the caption under the draft).
    if (now.id !== before.id) setDraft(now.caption);
    else if (now.caption !== before.caption && !untrack(saving) && untrack(draft).trim() === before.caption) setDraft(now.caption);
  });
  const dirty = () => draft().trim() !== (item().caption ?? "");
  const saveCaption = async () => {
    if (!dirty() || saving()) return;
    setSaving(true);
    try {
      await props.onCaption(draft().trim());
    } catch {
      // The viewer shows the error; the draft stays so nothing typed is lost.
    } finally {
      setSaving(false);
    }
  };

  return (
    <div class="gv-info" role="region" aria-label={t("galleryInfo.infoTitle")}>
      <div class="gv-info-head">
        <h2 class="gv-info-title">{t("galleryInfo.infoTitle")}</h2>
        <button type="button" class="gv-info-close" aria-label={t("galleryInfo.infoClose")} onClick={() => props.onClose()}>
          ×
        </button>
      </div>

      <Show when={item().trashedAt}>
        <p class="gv-note gv-note--warn">
          {left() === null || left() === 0 ? t("galleryInfo.trashExpired") : t("galleryInfo.trashExpires", { count: left() ?? 0 })}
        </p>
      </Show>

      <Show when={details.loading && !loaded()}>
        <p class="gv-note" role="status">{t("galleryInfo.infoLoading")}</p>
      </Show>
      <Show when={details.state === "ready" && details() === null}>
        <p class="gv-note">{t("galleryInfo.infoPartial")}</p>
      </Show>

      <section class="gv-section">
        <dl class="gv-rows">
          <Row label={t("galleryInfo.taken")}>
            <Show when={taken()} fallback={t("galleryInfo.takenUnknown")}>
              {taken()}
              <Show when={offset()}>
                <span class="gv-sub">{t("galleryInfo.utcOffset", { offset: offset() ?? "" })}</span>
              </Show>
            </Show>
          </Row>
          <Show when={item().deviceName ?? item().device}>
            {(name) => (
              <Row label={t("galleryInfo.device")}>
                {name()}
                <Show when={kindLabel()}>
                  <span class="gv-sub">{kindLabel()}</span>
                </Show>
              </Row>
            )}
          </Show>
        </dl>
      </section>

      <Show when={camera() || exif()?.lens || settings()}>
        <section class="gv-section">
          <dl class="gv-rows">
            <Show when={camera()}>
              <Row label={t("galleryInfo.camera")}>
                {camera()}
                <Show when={settings()}>
                  <span class="gv-sub">{settings()}</span>
                </Show>
              </Row>
            </Show>
            <Show when={!camera() && settings()}>
              <Row label={t("galleryInfo.camera")}>{settings()}</Row>
            </Show>
            <Show when={exif()?.lens}>
              {(lens) => <Row label={t("galleryInfo.lens")}>{lens()}</Row>}
            </Show>
          </dl>
        </section>
      </Show>

      <section class="gv-section">
        <dl class="gv-rows">
          <Show when={item().size > 0}>
            <Row label={t("galleryInfo.size")}>{formatBytes(item().size, locale())}</Row>
          </Show>
          <Show when={dimensions()}>
            <Row label={t("galleryInfo.dimensions")}>{dimensions()}</Row>
          </Show>
          <Show when={duration()}>
            <Row label={t("galleryInfo.duration")}>{duration()}</Row>
          </Show>
          <Show when={origin().filename}>
            {(name) => <Row label={t("galleryInfo.originalFile")}>{name()}</Row>}
          </Show>
          <Show when={origin().folder}>
            {(folder) => <Row label={t("galleryInfo.folder")}>{folder()}</Row>}
          </Show>
          <Show when={origin().importedFrom && origin().importedFrom !== origin().folder}>
            <Row label={t("galleryInfo.importedFrom")}>{origin().importedFrom}</Row>
          </Show>
        </dl>
      </section>

      <CopyInfo item={item()} />

      <Show when={item().place}>
        {(place) => (
          <section class="gv-section">
            <dl class="gv-rows">
              <Row label={t("galleryInfo.place")}>{placeLine(place()) || `${place().lat.toFixed(4)}, ${place().lon.toFixed(4)}`}</Row>
            </dl>
            <div class="gv-map">
              <LucinaMap
                interactive={false}
                pins={[{ id: "photo", lat: place().lat, lng: place().lon, title: place().name || item().title, tint: "accent" }]}
                label={t("galleryInfo.mapLabel")}
              />
            </div>
            <a class="gv-link" href={placeRoute({ title: place().name || item().title, lat: place().lat, lng: place().lon })}>
              {t("galleryInfo.openMap")}
            </a>
          </section>
        )}
      </Show>

      <section class="gv-section">
        <dl class="gv-rows">
          <Row label={t("galleryInfo.albums")}>
            <Show when={albumNames().length} fallback={<span class="gv-sub">{t("galleryInfo.noAlbums")}</span>}>
              <ul class="gv-chips">
                <For each={albumNames()}>{(name) => <li class="gv-chip">{name}</li>}</For>
              </ul>
            </Show>
          </Row>
          <Show when={loaded()?.category}>
            {(category) => (
              <Row label={t("galleryCategories.infoLabel")}>
                <CategoryValue itemId={item().id} category={category()} />
              </Row>
            )}
          </Show>
          <Show when={people().length}>
            <Row label={t("galleryInfo.people")}>
              <ul class="gv-chips">
                <For each={people()}>
                  {(person) => (
                    <li class="gv-chip">
                      <Show when={person.id} fallback={chipLabel(person, t("galleryPeople.unnamed"))}>
                        {(id) => (
                          <a
                            class="gv-chip__link"
                            href={personPath(id())}
                            onClick={(e) => {
                              if (!props.onPerson || e.metaKey || e.ctrlKey || e.shiftKey) return;
                              e.preventDefault();
                              props.onPerson(id());
                            }}
                          >
                            {chipLabel(person, t("galleryPeople.unnamed"))}
                          </a>
                        )}
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </Row>
          </Show>
        </dl>
      </section>

      <section class="gv-section gv-caption">
        <label class="gv-rows-label" for="gv-caption-text">
          {t("galleryInfo.caption")}
        </label>
        <textarea
          id="gv-caption-text"
          class="gv-caption-text"
          rows={3}
          maxLength={2000}
          placeholder={t("galleryInfo.captionPlaceholder")}
          value={draft()}
          disabled={saving()}
          onInput={(e) => setDraft(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              setDraft(item().caption ?? "");
              e.currentTarget.blur();
            } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              void saveCaption();
            }
          }}
        />
        <Show when={dirty()}>
          <div class="gv-caption-actions">
            <button type="button" class="gv-small-btn" disabled={saving()} onClick={() => setDraft(item().caption ?? "")}>
              {t("common.cancel")}
            </button>
            <button type="button" class="gv-small-btn gv-small-btn--primary" disabled={saving()} onClick={() => void saveCaption()}>
              {saving() ? t("common.saving") : t("galleryInfo.captionSave")}
            </button>
          </div>
        </Show>
      </section>
    </div>
  );
}
