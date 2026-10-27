import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createMemo, createResource, createSignal, For, onMount, Show } from "solid-js";
import { formatTime, mediaApi, type MediaItem } from "../api/player";
import { BackIcon, CheckIcon, RepeatIcon, SearchIcon } from "../components/Icons";
import { t } from "../i18n";
import { continueItems, isFinished, media, playQueue, progressOf, ready, syncProgress } from "../state/media";
import { MiniPlayer, Player, Poster } from "./Media";
import "../styles/films.css";

// Films (/films, a Plugins card; owner, 2026-10-26): the film folders on the server
// (`scope=films`, "Film" and "Dansk film"), apart from the Gallery (phone backups) and the
// Drive (the archive). Continue watching comes first; a click plays from the saved minute.
// The iPhone's screen is ios/Secretary/Features/Media/FilmsView.swift.

interface Folder {
  id: string;
  title: string;
  films: MediaItem[];
}

/** The folders, biggest first, each with its films in title order. */
async function loadFolders(): Promise<Folder[]> {
  const albums = (await mediaApi.filmFolders()).sort((a, b) => b.count - a.count);
  const folders = await Promise.all(
    albums.map(async (album) => ({
      id: album.id,
      title: album.title,
      films: (await mediaApi.filmsIn(album.id)).sort((a, b) => a.title.localeCompare(b.title, undefined, { numeric: true })),
    })),
  );
  return folders.filter((f) => f.films.length > 0);
}

/** "Up - Dansk dub + Dansk subs" → the title and the edition. */
export function splitTitle(title: string): { name: string; edition: string | null } {
  const at = title.indexOf(" - ");
  return at < 0 ? { name: title, edition: null } : { name: title.slice(0, at), edition: title.slice(at + 3) };
}

/** One film at a time: the next file in a folder is another film, not a next episode. */
function playFilm(film: MediaItem, fromStart = false): void {
  void playQueue([film], 0, { fromStart });
  media.setOpen(true);
}

function FilmCard(props: { film: MediaItem; remaining?: boolean }) {
  const progress = () => progressOf(props.film);
  const finished = () => isFinished(props.film);
  const name = () => splitTitle(props.film.title);
  const detail = () => {
    const parts: string[] = [];
    const seconds = (props.film.duration_ms ?? 0) / 1000;
    const p = progress();
    if (props.remaining && p !== null && seconds > 0) parts.push(t("films.left", { time: formatTime(seconds * (1 - p)) }));
    else if (seconds > 0) parts.push(formatTime(seconds));
    const edition = name().edition;
    if (edition) parts.push(edition);
    return parts.join(" · ");
  };
  return (
    <div class="film-card" data-media-id={props.film.id}>
      <button type="button" class="film-card__play" onClick={() => playFilm(props.film)}>
        <span class="film-card__poster">
          <Poster url={props.film.poster_url} kind="video" class="film-card__image" />
          <Show when={progress() !== null}>
            <span class="film-card__progress" aria-hidden="true">
              <span style={{ width: `${Math.round((progress() ?? 0) * 100)}%` }} />
            </span>
          </Show>
          <Show when={finished()}>
            <span class="film-card__badge" role="img" aria-label={t("media.watched")}>
              <CheckIcon />
            </span>
          </Show>
        </span>
        <span class="film-card__title">{name().name}</span>
        <span class="film-card__detail">{detail()}</span>
      </button>
      <Show when={progress() !== null || finished()}>
        <button
          type="button"
          class="film-card__restart"
          aria-label={`${t("films.fromStart")}: ${props.film.title}`}
          title={t("films.fromStart")}
          onClick={() => playFilm(props.film, true)}
        >
          <RepeatIcon />
        </button>
      </Show>
    </div>
  );
}

export default function FilmsView() {
  const [query, setQuery] = createSignal("");
  const [folders] = createResource(loadFolders);
  onMount(() => {
    void ready();
    void syncProgress();
  });

  const allFilms = createMemo(() => {
    const seen = new Set<string>();
    return (folders() ?? []).flatMap((f) => f.films).filter((film) => !seen.has(film.id) && !!seen.add(film.id));
  });
  const shown = createMemo(() => {
    const q = query().trim().toLowerCase();
    const all = folders() ?? [];
    if (!q) return all;
    return all.map((f) => ({ ...f, films: f.films.filter((film) => film.title.toLowerCase().includes(q)) })).filter((f) => f.films.length > 0);
  });
  const resume = () => (query().trim() ? [] : continueItems(allFilms()));

  // `/films/play/<id>` (a film Lucina found): play it once the folders are in, then stay on /films.
  const params = useParams<{ id?: string }>();
  const navigate = useNavigate();
  createEffect(() => {
    const id = params.id;
    if (!id || folders.loading) return;
    const film = allFilms().find((f) => f.id === id);
    navigate("/films", { replace: true });
    if (film) playFilm(film);
  });

  return (
    <section class="films" aria-labelledby="films-title" aria-busy={folders.loading}>
      <A href="/plugins" class="media-back">
        <BackIcon /> {t("media.back")}
      </A>
      <h1 id="films-title">{t("films.title")}</h1>
      <label class="media-search">
        <SearchIcon />
        <input type="search" placeholder={t("films.search")} aria-label={t("films.search")} value={query()} onInput={(e) => setQuery(e.currentTarget.value)} />
      </label>

      <Show when={folders.error}>
        <p class="media-muted" role="alert">{String(folders.error?.message ?? folders.error)}</p>
      </Show>
      <Show when={!folders.loading && !folders.error && (folders() ?? []).length === 0}>
        <div class="films-empty">
          <p class="films-empty__title">{t("films.empty")}</p>
          <p class="media-muted">{t("films.emptyDetail")}</p>
        </div>
      </Show>

      <Show when={resume().length > 0}>
        <section class="films-section" aria-labelledby="films-continue">
          <h2 id="films-continue" class="media-section">{t("films.continue")}</h2>
          <div class="films-row">
            <For each={resume()}>{(film) => <FilmCard film={film} remaining />}</For>
          </div>
        </section>
      </Show>

      <For each={shown()}>
        {(folder) => (
          <section class="films-section" aria-labelledby={`films-${folder.id}`}>
            <h2 id={`films-${folder.id}`} class="media-section films-section__header">
              <span>{folder.title}</span>
              <span class="films-section__count">{folder.films.length}</span>
            </h2>
            <div class="films-grid">
              <For each={folder.films}>{(film) => <FilmCard film={film} />}</For>
            </div>
          </section>
        )}
      </For>

      <MiniPlayer />
      <Show when={media.open() && media.current()}>
        <Player />
      </Show>
    </section>
  );
}
