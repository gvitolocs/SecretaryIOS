// "Enter the secret for NAME": the inline card of a request in Lucina's queue. Paste, OK, saved; the
// server then posts the next card. No confirm step when the card names a known project and the secret
// doesn't exist yet: replacing an existing one (the broker answers `exists`) takes one more tap.
//
// The value lives in this component's signal only: never in storage, the chat cache, a draft, a URL or a
// log, and it never goes through the chat's send path. It leaves once, in the body of the save.

import { createEffect, createMemo, createResource, createSignal, For, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { toApiError, type ApiError } from "../api/client";
import { secretsApi, type SecretRequestEntry } from "../api/secrets";
import { t } from "../i18n";
import { findProject, NAME_RE, normalizeFolder } from "../lib/secrets";
import { loadLatest, onTimelineItem } from "../state/chat";
import { lastPlace, rememberPlace } from "../state/secretSheet";
import { refreshSecretQueue, secretQueue, secretQueueFailed } from "../state/secretRequests";

export function SecretCard(props: { request: string; autoFocus?: boolean; fallback: string }) {
  onMount(() => {
    void refreshSecretQueue();
    // Saved or dismissed on another device: every secret item in the timeline may change a card.
    const stop = onTimelineItem((item) => {
      if (String((item.meta as { kind?: unknown } | undefined)?.kind ?? "").startsWith("secret_")) void refreshSecretQueue();
    });
    onCleanup(stop);
  });
  const entry = createMemo(() => secretQueue()?.requests.find((r) => r.id === props.request));

  return (
    <section class="rw secret-card" aria-label={t("secrets.cardLabel")}>
      <Switch
        fallback={
          <p class="muted small" role="status">
            {t("secrets.cardClosed", { name: props.fallback })}
          </p>
        }
      >
        <Match when={!secretQueue() && !secretQueueFailed()}>
          <p class="muted small" role="status">
            {t("common.loading")}
          </p>
        </Match>
        <Match when={!secretQueue() && secretQueueFailed()}>
          <p class="field-error" role="alert">
            {t("secrets.cardOffline")}
          </p>
          <button type="button" class="btn btn--small" onClick={() => void refreshSecretQueue()}>
            {t("common.tryAgain")}
          </button>
        </Match>
        {/* Keyed by the id: a refresh of the queue must not rebuild the form (and lose what he pasted). */}
        <Match when={entry()?.status === "active" ? entry()!.id : null} keyed>
          {(_id) => <ActiveCard entry={entry()!} autoFocus={props.autoFocus} />}
        </Match>
        <Match when={entry()?.status === "saved" && entry()}>
          {(e) => (
            <p class="secret-ok" role="status">
              ✅ {t(e().action === "updated" ? "secrets.cardUpdated" : "secrets.cardSaved", { name: e().name, fp: e().fingerprint ?? "?" })}
            </p>
          )}
        </Match>
        <Match when={entry()?.status === "dismissed" && entry()}>
          {(e) => (
            <p class="muted small" role="status">
              {t("secrets.cardDismissed", { name: e().name || t("secrets.cardSomeSecret") })}
            </p>
          )}
        </Match>
      </Switch>
    </section>
  );
}

type Phase = "idle" | "saving" | "replace";

function ActiveCard(props: { entry: SecretRequestEntry; autoFocus?: boolean }) {
  const e = props.entry;
  const [value, setValue] = createSignal("");
  const [name, setName] = createSignal(e.name);
  const [phase, setPhase] = createSignal<Phase>("idle");
  const [error, setError] = createSignal<string | null>(null);
  const [projectId, setProjectId] = createSignal<string | null>(null);
  const [env, setEnv] = createSignal<string | null>(e.environment || null);
  const [path, setPath] = createSignal(e.path || "/");
  const [creating, setCreating] = createSignal(false);
  let field: HTMLTextAreaElement | undefined;

  const [projects, { refetch }] = createResource(() => secretsApi.catalog());
  const project = createMemo(() => projects()?.find((p) => p.id === projectId()) ?? null);
  /** The card names a project the catalog doesn't have: never save it somewhere else. */
  const missing = createMemo(() => !!e.project && !!projects() && !findProject(e.project, projects()!));
  const envMissing = createMemo(() => !!e.environment && !!project() && !project()!.environments.some((x) => x.slug === e.environment));
  const normalizedPath = () => normalizeFolder(path());

  createEffect(
    on(projects, (list) => {
      if (!list || projectId()) return;
      if (e.project) {
        const named = findProject(e.project, list);
        if (named) setProjectId(named.id);
        return;
      }
      const last = lastPlace();
      const p = list.find((x) => x.id === last?.project);
      if (p) {
        setProjectId(p.id);
        setEnv(last!.environment);
        setPath(last!.path);
      } else if (list[0]) {
        pick(list[0].id);
      }
    }),
  );

  function pick(id: string) {
    setProjectId(id);
    const envs = projects()?.find((p) => p.id === id)?.environments.map((x) => x.slug) ?? [];
    setEnv(envs.includes("prod") ? "prod" : envs[0] ?? null);
  }

  // A value pasted from a file often ends in a newline that isn't part of it.
  const secret = () => value().replace(/[\r\n]+$/, "");
  const ready = () => !!secret() && secret().length <= 65_536 && NAME_RE.test(name().trim()) && !!projectId() && !!env() && !!normalizedPath() && !envMissing();

  function describe(err: ApiError): string {
    if (err.code === "secrets_unconfigured") return t("secrets.errorUnconfigured");
    if (err.code === "upstream_unauthorized") return t("secrets.errorOrgRole");
    return err.message;
  }

  async function save(overwrite: boolean) {
    if (!ready() || phase() === "saving") return;
    setPhase("saving");
    setError(null);
    const folder = normalizedPath()!;
    try {
      const r = await secretsApi.save({
        name: name().trim(),
        project: projectId()!,
        environment: env()!,
        path: folder,
        overwrite,
        createFolder: folder !== "/",
        value: secret(),
        request: e.id,
      });
      setValue("");
      rememberPlace({ project: projectId()!, environment: env()!, path: folder });
      // The card turns "saved"; the next one comes through the stream, or this catch-up.
      await refreshSecretQueue();
      if ((r as { nextItemId?: string | null }).nextItemId) void loadLatest();
    } catch (err) {
      const api = toApiError(err);
      if (api.code === "exists") {
        setPhase("replace");
        return;
      }
      setError(describe(api));
      setPhase("idle");
    }
  }

  async function notNow() {
    setValue("");
    setError(null);
    try {
      const r = await secretsApi.dismissRequest(e.id);
      await refreshSecretQueue();
      if ((r as { nextItemId?: string | null }).nextItemId) void loadLatest();
    } catch (err) {
      setError(describe(toApiError(err)));
    }
  }

  async function createProject() {
    setCreating(true);
    setError(null);
    try {
      await secretsApi.createProject(e.project);
      await refetch();
    } catch (err) {
      setError(describe(toApiError(err)));
    } finally {
      setCreating(false);
    }
  }

  onMount(() => {
    if (props.autoFocus) queueMicrotask(() => field?.focus());
  });
  onCleanup(() => setValue(""));

  const queued = () => secretQueue()?.queued ?? 0;
  const place = () => `${project()?.name ?? e.project} / ${env() ?? "?"} / ${normalizedPath() ?? path()}`;

  return (
    <div class="secret-card-form">
      <h4 class="rw-card-title">🔐 {e.name ? t("secrets.cardTitle", { name: e.name }) : t("secrets.cardTitleBlank")}</h4>
      <Show when={e.who && e.name}>
        <p class="rw-sub">{t("secrets.cardWhy", { who: e.who, reason: e.reason })}</p>
      </Show>
      <Show when={e.project && e.environment}>
        <p class="muted small secret-mono">{place()}</p>
      </Show>
      <Show when={!e.name}>
        <label class="field-label">
          {t("secrets.name")}
          <input
            class="text-input secret-mono"
            placeholder={t("secrets.namePlaceholder")}
            value={name()}
            onInput={(ev) => setName(ev.currentTarget.value.replace(/\s/g, "_"))}
            autocomplete="off"
            autocapitalize="characters"
            spellcheck={false}
            data-1p-ignore
            data-lpignore="true"
          />
        </label>
      </Show>
      <Show when={!e.project || !e.environment}>
        <Show when={projects()} fallback={<p class="muted small">{projects.error ? describe(toApiError(projects.error)) : t("secrets.loadingProjects")}</p>}>
          {(list) => (
            <div class="secret-card-place">
              <label class="field-label">
                {t("secrets.project")}
                <select class="text-input" value={projectId() ?? ""} onChange={(ev) => pick(ev.currentTarget.value)}>
                  <For each={list()}>{(p) => <option value={p.id}>{p.name}</option>}</For>
                </select>
              </label>
              <label class="field-label">
                {t("secrets.environment")}
                <select class="text-input" value={env() ?? ""} onChange={(ev) => setEnv(ev.currentTarget.value)}>
                  <For each={project()?.environments ?? []}>{(x) => <option value={x.slug}>{x.slug}</option>}</For>
                </select>
              </label>
              <label class="field-label">
                {t("secrets.folder")}
                <input class="text-input secret-mono" value={path()} onInput={(ev) => setPath(ev.currentTarget.value)} autocomplete="off" spellcheck={false} />
              </label>
            </div>
          )}
        </Show>
      </Show>
      <Show when={missing()}>
        <p class="secret-warn" role="status">
          ⚠️ {t("secrets.cardProjectMissing", { name: e.project })}{" "}
          <button type="button" class="btn btn--small" disabled={creating()} onClick={() => void createProject()}>
            {creating() ? t("secrets.creating") : t("secrets.createProject")}
          </button>
        </p>
      </Show>
      <Show when={projects.error && e.project}>
        <p class="field-error" role="alert">
          {describe(toApiError(projects.error))}{" "}
          <button type="button" class="btn btn--small" onClick={() => void refetch()}>
            {t("common.tryAgain")}
          </button>
        </p>
      </Show>
      <Show when={envMissing()}>
        <p class="field-error" role="alert">
          {t("secrets.cardNoEnv", { env: e.environment })}
        </p>
      </Show>
      {/* A textarea so a PEM key keeps its newlines; masked like a password. No <form>: nothing to autofill or remember. */}
      <textarea
        ref={field}
        class="text-input secret-mono secret-value secret-value--hidden secret-card-value"
        rows="1"
        aria-label={t("secrets.cardValue", { name: name() || t("secrets.value") })}
        placeholder={t("secrets.cardPaste")}
        value={value()}
        disabled={phase() === "saving"}
        onInput={(ev) => {
          setValue(ev.currentTarget.value);
          if (phase() === "replace") setPhase("idle");
        }}
        onKeyDown={(ev) => {
          // Never the composer's Enter: this field has its own OK.
          ev.stopPropagation();
          if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) {
            ev.preventDefault();
            if (phase() === "idle") void save(false);
          }
        }}
        autocomplete="off"
        autocapitalize="off"
        autocorrect="off"
        spellcheck={false}
        data-1p-ignore
        data-lpignore="true"
        data-form-type="other"
      />
      <Show when={error()}>
        <p class="field-error" role="alert">
          {error()}
        </p>
      </Show>
      <Show when={phase() === "replace"}>
        <p class="secret-warn" role="status">
          ⚠️ {t("secrets.confirmReplace", { name: name().trim() })}
        </p>
      </Show>
      <div class="secret-actions">
        <button type="button" class="btn" disabled={phase() === "saving"} onClick={() => void notNow()}>
          {t("secrets.cardNotNow")}
        </button>
        <Show
          when={phase() === "replace"}
          fallback={
            <button type="button" class="btn btn--primary" disabled={!ready() || phase() === "saving"} onClick={() => void save(false)}>
              {phase() === "saving" ? t("common.saving") : t("secrets.cardOk")}
            </button>
          }
        >
          <button type="button" class="btn btn--primary btn--danger" onClick={() => void save(true)}>
            {t("secrets.replace")}
          </button>
        </Show>
      </div>
      <Show when={queued() > 0}>
        <p class="muted small">{t("secrets.cardMore", { count: queued() })}</p>
      </Show>
    </div>
  );
}
