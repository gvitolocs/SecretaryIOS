import { A, useNavigate } from "@solidjs/router";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { toApiError } from "../api/client";
import { gameApi, herNotes, placeName, type GameState } from "../api/game";
import { BackIcon } from "../components/Icons";
import { Button, Card, EmptyState, ErrorState, Page, PageGrid, PageHeader, PrimaryAction, useWide } from "../components/page";
import { t } from "../i18n";
import { requestPrefill } from "../state/chat";
import "../styles/pokemon.css";

// Lucina's Pokémon game (Plugins → Pokémon): the live screen about once a second, her team with HP,
// badges, objective and latest notes; Play/Pause for the owner, and a shortcut to ask her in chat.

export default function PokemonView() {
  const navigate = useNavigate();
  const { wide, trackSlot } = useWide();
  const [state, setState] = createSignal<GameState | null>(null);
  const [tick, setTick] = createSignal(0);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const loadState = async () => {
    try {
      setState(await gameApi.state());
      setError(null);
    } catch (e) {
      setError(toApiError(e).status === 503 ? t("plugins.pokemonUnreachable") : toApiError(e).message);
    }
  };

  onMount(() => {
    void loadState();
    let n = 0;
    const timer = window.setInterval(() => {
      if (document.hidden) return;
      setTick(Date.now());
      if (++n % 3 === 0) void loadState();
    }, 1000);
    onCleanup(() => window.clearInterval(timer));
  });

  const toggle = async () => {
    const s = state();
    if (!s) return;
    setBusy(true);
    try {
      await gameApi.setRunning(!s.running);
      await loadState();
    } catch (e) {
      setError(toApiError(e).message);
    } finally {
      setBusy(false);
    }
  };

  const hpLevel = (hp: number, max: number) => {
    const r = hp / Math.max(max, 1);
    return r > 0.5 ? "good" : r > 0.2 ? "warn" : "low";
  };

  const playLabel = () => (state()?.running ? t("plugins.pokemonPause") : t("plugins.pokemonPlay"));

  const gameBody = (desk: boolean) => (
    <>
      <Card class="pokemon-desk-game">
        <div class="pokemon-card">
          <div class="pokemon-screen">
            <img src={gameApi.screenshotUrl(tick())} alt={t("plugins.pokemonScreen")} width="160" height="144" />
          </div>
          <Show when={state()}>
            {(s) => (
              <>
                <p class="pokemon-status">
                  <span class="pokemon-dot" data-running={s().running ? "yes" : "no"} aria-hidden="true" />
                  <strong>{s().running ? t("plugins.pokemonPlaying") : t("plugins.pokemonPaused")}</strong>
                  <span class="pokemon-muted">· {placeName(s().map_name)}</span>
                  <span class="pokemon-badges">{t("plugins.pokemonBadges", { count: s().badges })}</span>
                </p>
                <Show when={s().objective}>
                  <p class="pokemon-objective">{s().objective}</p>
                </Show>
              </>
            )}
          </Show>
          <Show when={error()}>
            <Show
              when={desk}
              fallback={
                <p role="alert" class="pokemon-error">
                  {error()}
                </p>
              }
            >
              <ErrorState message={error()!} retryLabel={t("common.tryAgain")} onRetry={() => void loadState()} />
            </Show>
          </Show>
          <Show when={!desk}>
            <div class="pokemon-actions">
              <button type="button" class="button" disabled={!state() || busy()} onClick={() => void toggle()}>
                {playLabel()}
              </button>
              <button
                type="button"
                class="button primary"
                onClick={() => {
                  requestPrefill(t("plugins.pokemonAsk"));
                  navigate("/");
                }}
              >
                {t("plugins.pokemonAskLucina")}
              </button>
            </div>
          </Show>
          <Show when={desk && !state() && !error()}>
            <EmptyState text={t("plugins.pokemonUnreachable")} />
          </Show>
        </div>
      </Card>
      <Show when={desk}>
        <div class="pokemon-desk-ask">
          <Button
            onClick={() => {
              requestPrefill(t("plugins.pokemonAsk"));
              navigate("/");
            }}
          >
            {t("plugins.pokemonAskLucina")}
          </Button>
        </div>
      </Show>
      <Show when={(state()?.party.length ?? 0) > 0}>
        <h2>{t("plugins.pokemonTeam")}</h2>
        <ul class="pokemon-card pokemon-team">
          <For each={state()!.party}>
            {(m) => (
              <li>
                <div class="pokemon-mon">
                  <strong>{m.name}</strong>
                  <Show when={m.species && m.species !== m.name}>
                    <span class="pokemon-muted">{m.species}</span>
                  </Show>
                  <span class="pokemon-level">Lv {m.level}</span>
                </div>
                <div class="pokemon-hp" role="img" aria-label={`HP ${m.hp}/${m.max_hp}`}>
                  <span class="pokemon-hp-bar" data-level={hpLevel(m.hp, m.max_hp)} style={{ width: `${Math.round((100 * m.hp) / Math.max(m.max_hp, 1))}%` }} />
                </div>
                <span class="pokemon-muted pokemon-hp-text">
                  {m.hp}/{m.max_hp}
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={herNotes(state()?.events ?? []).length > 0}>
        <h2>{t("plugins.pokemonNotes")}</h2>
        <ul class="pokemon-card pokemon-notes">
          <For each={herNotes(state()!.events).slice(0, 8)}>{(note) => <li>{note}</li>}</For>
        </ul>
      </Show>
    </>
  );

  return (
    <section class="pokemon-page" classList={{ "pokemon-page--wide": wide() }} ref={trackSlot} aria-labelledby="pokemon-title">
      <Show
        when={wide()}
        fallback={
          <>
            <A href="/plugins" class="pokemon-back">
              <BackIcon /> {t("plugins.title")}
            </A>
            <h1 id="pokemon-title">{t("plugins.pokemon")}</h1>
            {gameBody(false)}
          </>
        }
      >
        <Page class="pokemon-desk">
          <PageHeader
            title={t("plugins.pokemon")}
            action={
              <PrimaryAction disabled={!state() || busy()} onClick={() => void toggle()}>
                {playLabel()}
              </PrimaryAction>
            }
          />
          <PageGrid layout="single">{gameBody(true)}</PageGrid>
        </Page>
      </Show>
    </section>
  );
}
