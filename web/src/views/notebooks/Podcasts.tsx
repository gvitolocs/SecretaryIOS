import { createEffect, createSignal, For, Show } from "solid-js";
import { toApiError } from "../../api/client";
import { notebooksApi, PODCAST_PENDING, type Podcast, type PodcastProfile } from "../../api/notebooks";
import { Button, Sheet } from "../../components/page";
import { t } from "../../i18n";
import { formatDate } from "../../lib/units";
import { clock, PODCAST_STATUS_KEY, statusTone } from "./common";

const MIN_MINUTES = 2;
const MAX_MINUTES = 30;

function Episode(props: { podcast: Podcast; onRemove: () => void }) {
  const p = () => props.podcast;
  return (
    <li class="nb-episode">
      <div class="nb-episode-head">
        <strong class="nb-episode-title">{p().title}</strong>
        <span class="nb-status" data-tone={statusTone(p().status)} data-busy={PODCAST_PENDING.includes(p().status) ? "" : undefined}>
          {t(PODCAST_STATUS_KEY[p().status])}
        </span>
      </div>
      <p class="pg-muted nb-episode-sub">
        {p().episode_profile} · {p().speaker_profile}
        <Show when={p().duration_s}>{` · ${clock(p().duration_s!)}`}</Show>
        {` · ${formatDate(Date.parse(p().created_at))}`}
      </p>
      <Show when={p().status === "failed"}>
        <p class="nb-error" role="alert">{p().error ?? t("notebooks.failedGeneric")}</p>
      </Show>
      <Show when={p().status === "ready" && p().audio}>
        <audio controls preload="none" src={notebooksApi.audioUrl(p().id)} aria-label={p().title} />
      </Show>
      <Show when={p().transcript.length}>
        <details class="nb-transcript">
          <summary class="pg-link">{t("notebooks.transcript")}</summary>
          <ul>
            <For each={p().transcript}>
              {(line) => (
                <li>
                  <strong>{line.speaker}</strong> {line.text}
                </li>
              )}
            </For>
          </ul>
        </details>
      </Show>
      <div class="nb-episode-actions">
        <Button variant="ghost" onClick={props.onRemove}>
          {t("common.delete")}
        </Button>
      </div>
    </li>
  );
}

/** The podcast sheet: make an episode from the notebook (profiles, briefing, length) and listen to the ones made. */
export function PodcastSheet(props: { open: boolean; notebookId: string; podcasts: Podcast[]; onClose: () => void; onChanged: () => void }) {
  const [episodes, setEpisodes] = createSignal<PodcastProfile[]>([]);
  const [speakers, setSpeakers] = createSignal<PodcastProfile[]>([]);
  const [episode, setEpisode] = createSignal("");
  const [speaker, setSpeaker] = createSignal("");
  const [briefing, setBriefing] = createSignal("");
  const [minutes, setMinutes] = createSignal(8);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  createEffect(() => {
    if (!props.open) return;
    setError(null);
    setBusy(false);
    void notebooksApi.podcastProfiles().then(
      (r) => {
        setEpisodes(r.episode_profiles);
        setSpeakers(r.speaker_profiles);
        if (!episode()) setEpisode(r.episode_profiles[0]?.id ?? "");
        if (!speaker()) setSpeaker(r.speaker_profiles[0]?.id ?? "");
      },
      (e) => setError(toApiError(e).message),
    );
  });

  async function create(e: Event) {
    e.preventDefault();
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      await notebooksApi.createPodcast(props.notebookId, {
        ...(episode() ? { episode_profile: episode() } : {}),
        ...(speaker() ? { speaker_profile: speaker() } : {}),
        ...(briefing().trim() ? { briefing: briefing().trim() } : {}),
        minutes: Math.min(MAX_MINUTES, Math.max(MIN_MINUTES, Math.round(minutes()) || 8)),
      });
      setBriefing("");
      props.onChanged();
    } catch (err) {
      setError(toApiError(err).message);
    } finally {
      setBusy(false);
    }
  }

  async function remove(p: Podcast) {
    if (!confirm(t("notebooks.deletePodcastConfirm", { title: p.title }))) return;
    try {
      await notebooksApi.removePodcast(p.id);
    } catch (e) {
      setError(toApiError(e).message);
    }
    props.onChanged();
  }

  return (
    <Sheet
      open={props.open}
      onClose={props.onClose}
      size="wide"
      class="nb-sheet"
      title={t("notebooks.podcast")}
      footer={
        <Button type="submit" form="nb-podcast-form" disabled={busy()}>
          {busy() ? t("notebooks.creating") : t("notebooks.createPodcast")}
        </Button>
      }
    >
      <form id="nb-podcast-form" class="nb-form" onSubmit={create}>
        <Show when={error()}>
          <p class="pg-notice" role="alert">{error()}</p>
        </Show>
        <p class="pg-muted">{t("notebooks.podcastIntro")}</p>
        <div class="nb-form-row">
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.episodeProfile")}</span>
            <select class="pg-input" onChange={(e) => setEpisode(e.currentTarget.value)}>
              <For each={episodes()}>
                {(p) => (
                  <option value={p.id} selected={episode() === p.id} title={p.description}>
                    {p.name}
                  </option>
                )}
              </For>
            </select>
          </label>
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.speakerProfile")}</span>
            <select class="pg-input" onChange={(e) => setSpeaker(e.currentTarget.value)}>
              <For each={speakers()}>
                {(p) => (
                  <option value={p.id} selected={speaker() === p.id} title={p.description}>
                    {p.name}
                  </option>
                )}
              </For>
            </select>
          </label>
          <label class="pg-field nb-minutes">
            <span class="pg-field-label">{t("notebooks.minutes")}</span>
            <input
              class="pg-input"
              type="number"
              min={MIN_MINUTES}
              max={MAX_MINUTES}
              value={minutes()}
              onInput={(e) => setMinutes(Number(e.currentTarget.value))}
            />
          </label>
        </div>
        <label class="pg-field">
          <span class="pg-field-label">{t("notebooks.briefing")}</span>
          <textarea
            class="pg-input nb-textarea"
            rows={3}
            placeholder={t("notebooks.briefingPlaceholder")}
            value={briefing()}
            onInput={(e) => setBriefing(e.currentTarget.value)}
          />
        </label>
      </form>
      <h3 class="nb-reader-h nb-episodes-h">{t("notebooks.episodes")}</h3>
      <Show when={props.podcasts.length} fallback={<p class="pg-muted">{t("notebooks.episodesEmpty")}</p>}>
        <ul class="nb-episodes">
          <For each={props.podcasts}>{(p) => <Episode podcast={p} onRemove={() => void remove(p)} />}</For>
        </ul>
      </Show>
    </Sheet>
  );
}
