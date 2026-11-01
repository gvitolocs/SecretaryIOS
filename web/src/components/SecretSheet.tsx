// "Add secret": name, value and a live project → environment → folder picker
// over every project Lucina's broker can reach (names only), a confirmation
// that names the place, and the receipt. Full-screen sheet on phones, a
// centred panel on wide screens (the Mac app embeds the same page).
//
// The value stays in this component's signal: never in storage, the chat
// cache, a URL or a log. It is sent once, in the body of the save.

import { createEffect, createMemo, createResource, createSignal, For, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { toApiError, type ApiError } from "../api/client";
import { secretsApi, type SecretProject, type SecretReceipt } from "../api/secrets";
import { t } from "../i18n";
import { joinFolder, NAME_RE, normalizeFolder, parentFolder, projectKeys, resolveTarget } from "../lib/secrets";
import { closeSecretSheet, lastPlace, rememberPlace, secretSheet, type SecretSheetInput } from "../state/secretSheet";

type Step = "form" | "confirm" | "saving" | "done" | "project" | "projectConfirm" | "projectSaving" | "projectDone";

export function SecretSheet() {
  return (
    <Show when={secretSheet()} keyed>
      {(input) => <SheetBody input={input} />}
    </Show>
  );
}

function SheetBody(props: { input: SecretSheetInput }) {
  const startProject = props.input.mode === "project";
  const [step, setStep] = createSignal<Step>(startProject ? "project" : "form");
  const [name, setName] = createSignal(props.input.name ?? "");
  const [value, setValue] = createSignal(props.input.value ?? "");
  const [held, setHeld] = createSignal(props.input.held);
  const [heldGone, setHeldGone] = createSignal(false);
  const [showValue, setShowValue] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [projectId, setProjectId] = createSignal<string | null>(null);
  const [env, setEnv] = createSignal<string | null>(null);
  const [path, setPath] = createSignal("/");
  /** Folders under the last existing one that the save creates. */
  const [newFolder, setNewFolder] = createSignal<string | null>(null);
  const [folderDraft, setFolderDraft] = createSignal<string | null>(null);
  const [projectName, setProjectName] = createSignal(props.input.projectName ?? "");
  const [error, setError] = createSignal<string | null>(null);
  const [receipt, setReceipt] = createSignal<SecretReceipt | null>(null);
  const [created, setCreated] = createSignal<{ name: string; envs: string } | null>(null);
  let first: HTMLInputElement | undefined;

  const [projects, { refetch: reloadProjects }] = createResource(() => secretsApi.catalog());
  const project = createMemo(() => projects()?.find((p) => p.id === projectId()) ?? null);
  const filtered = createMemo(() => {
    const q = query().trim().toLowerCase();
    const all = projects() ?? [];
    return q ? all.filter((p) => p.name.toLowerCase().includes(q) || projectKeys(p).some((k) => k.startsWith(q))) : all;
  });
  /** The folder being listed: the last one that exists. */
  const listedPath = () => (newFolder() ? parentFolder(path()) : path());
  const [folder] = createResource(
    () => (projectId() && env() ? { p: projectId()!, e: env()!, path: listedPath() } : null),
    (k) => secretsApi.folder(k.p, k.e, k.path),
  );
  const existing = createMemo(() => (newFolder() ? [] : folder()?.secrets.map((s) => s.key) ?? []));
  const overwrite = createMemo(() => existing().includes(name().trim()));
  const nameOk = () => NAME_RE.test(name().trim());
  const valueOk = () => (held() && !heldGone()) || (value().length > 0 && value().length <= 65_536);
  const canReview = () => nameOk() && valueOk() && !!projectId() && !!env();
  const where = () => `${project()?.name ?? "?"} / ${env() ?? "?"} / ${path()}`;

  // Pick the starting place once the projects arrive: the `/secret` target,
  // the card's names, the last place used, else the first project.
  createEffect(
    on(projects, (list) => {
      if (!list || projectId()) return;
      const input = props.input;
      const byTarget = input.target ? resolveTarget(input.target, list) : null;
      const last = lastPlace();
      const pick = (id?: string | null) => (id ? list.find((p) => p.id === id) : undefined);
      const p = byTarget?.project ?? pick(input.project) ?? pick(last?.project) ?? list[0];
      if (!p) return;
      choose(p, byTarget?.environment ?? input.environment ?? (last?.project === p.id ? last.environment : undefined));
      const start = byTarget?.path ?? (input.path ? normalizeFolder(input.path) : null) ?? (last?.project === p.id ? last.path : null);
      if (start) setPath(start);
    }),
  );

  function choose(p: SecretProject, preferEnv?: string) {
    setProjectId(p.id);
    const envs = p.environments.map((e) => e.slug);
    setEnv(preferEnv && envs.includes(preferEnv) ? preferEnv : envs.includes("prod") ? "prod" : envs[0] ?? null);
    setPath("/");
    setNewFolder(null);
    setQuery("");
  }

  function enter(child: string) {
    setNewFolder(null);
    setPath(joinFolder(path(), child));
  }

  function useNewFolder() {
    const n = (folderDraft() ?? "").trim();
    const next = normalizeFolder(joinFolder(listedPath(), n));
    if (!n || n.includes("/") || !next) return;
    setPath(next);
    setNewFolder(n);
    setFolderDraft(null);
  }

  function describe(e: ApiError): string {
    if (e.code === "exists") return t("secrets.errorExists", { name: name().trim() });
    if (e.code === "secrets_unconfigured") return t("secrets.errorUnconfigured");
    if (e.code === "upstream_unauthorized") return t("secrets.errorOrgRole");
    if (e.code === "held_expired") {
      setHeld(undefined);
      setHeldGone(true);
      return t("secrets.valueHeldExpired");
    }
    return e.message;
  }

  async function save() {
    if (!canReview() || step() === "saving") return;
    setStep("saving");
    setError(null);
    try {
      const h = held();
      const r = await secretsApi.save({
        name: name().trim(),
        project: projectId()!,
        environment: env()!,
        path: path(),
        overwrite: overwrite(),
        createFolder: !!newFolder(),
        ...(h && !heldGone() ? { held: h } : { value: value() }),
      });
      setValue("");
      rememberPlace({ project: projectId()!, environment: env()!, path: path() });
      setReceipt(r);
      setStep("done");
    } catch (e) {
      setError(describe(toApiError(e)));
      setStep("form");
    }
  }

  async function createProject() {
    const n = projectName().trim();
    if (!n || step() === "projectSaving") return;
    setStep("projectSaving");
    setError(null);
    try {
      const r = await secretsApi.createProject(n);
      await reloadProjects();
      const p = projects()?.find((x) => x.id === r.project.id) ?? r.project;
      choose(p);
      setCreated({ name: p.name, envs: p.environments.map((e) => e.slug).join(", ") });
      setStep(startProject ? "projectDone" : "form");
    } catch (e) {
      setError(describe(toApiError(e)));
      setStep("project");
    }
  }

  function another() {
    setName("");
    setValue("");
    setHeld(undefined);
    setReceipt(null);
    setStep("form");
    queueMicrotask(() => first?.focus());
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") closeSecretSheet();
  }
  onMount(() => {
    document.addEventListener("keydown", onKey);
    queueMicrotask(() => first?.focus());
  });
  onCleanup(() => {
    document.removeEventListener("keydown", onKey);
    setValue("");
  });

  const title = () => (step().startsWith("project") ? t("secrets.newProjectTitle") : t("secrets.title"));

  return (
    <div class="picker-overlay" onClick={(e) => e.target === e.currentTarget && closeSecretSheet()}>
      <div class="picker-sheet secret-sheet" role="dialog" aria-modal="true" aria-labelledby="secret-title">
        <header class="picker-head">
          <h2 id="secret-title" class="section-title">
            🔐 {title()}
          </h2>
          <button type="button" class="icon-btn" aria-label={t("common.close")} onClick={closeSecretSheet}>
            ×
          </button>
        </header>
        <Show when={error()}>
          <p class="field-error" role="alert">
            {error()}
          </p>
        </Show>
        <Switch>
          <Match when={step() === "form"}>
            <div class="settings-form secret-form">
              <label class="field-label" for="secret-name">
                {t("secrets.name")}
              </label>
              <input
                ref={first}
                id="secret-name"
                class="text-input secret-mono"
                placeholder={t("secrets.namePlaceholder")}
                value={name()}
                onInput={(e) => setName(e.currentTarget.value.replace(/\s/g, "_"))}
                autocomplete="off"
                autocapitalize="characters"
                spellcheck={false}
                aria-invalid={name() && !nameOk() ? "true" : "false"}
              />
              <span class="muted small">{t("secrets.nameHint")}</span>
              <label class="field-label" for="secret-value">
                {t("secrets.value")}
              </label>
              <Show
                when={held() && !heldGone()}
                fallback={
                  <div class="secret-value-row">
                    <textarea
                      id="secret-value"
                      class="text-input secret-mono secret-value"
                      classList={{ "secret-value--hidden": !showValue() }}
                      rows="2"
                      value={value()}
                      onInput={(e) => setValue(e.currentTarget.value)}
                      autocomplete="off"
                      autocapitalize="off"
                      spellcheck={false}
                      data-1p-ignore
                      data-lpignore="true"
                    />
                    <button type="button" class="btn btn--small" aria-pressed={showValue() ? "true" : "false"} onClick={() => setShowValue((s) => !s)}>
                      {showValue() ? t("secrets.hide") : t("secrets.show")}
                    </button>
                  </div>
                }
              >
                <p class="secret-held" role="status">
                  {t("secrets.valueHeld")}
                </p>
              </Show>
              <Show when={heldGone()}>
                <span class="muted small">{t("secrets.valueHeldExpired")}</span>
              </Show>

              <h3 class="field-label">{t("secrets.where")}</h3>
              <Switch>
                <Match when={projects.loading}>
                  <p class="muted" role="status">
                    {t("secrets.loadingProjects")}
                  </p>
                </Match>
                <Match when={projects.error}>
                  <p class="field-error" role="alert">
                    {describe(toApiError(projects.error))}
                  </p>
                  <button type="button" class="btn btn--small" onClick={() => void reloadProjects()}>
                    {t("common.tryAgain")}
                  </button>
                </Match>
                <Match when={projects()}>
                  <input
                    class="text-input"
                    type="search"
                    placeholder={t("secrets.searchProjects")}
                    aria-label={t("secrets.searchProjects")}
                    value={query()}
                    onInput={(e) => setQuery(e.currentTarget.value)}
                  />
                  <div class="secret-projects" role="listbox" aria-label={t("secrets.project")}>
                    <For each={filtered()} fallback={<p class="muted small">{t("secrets.noProjects")}</p>}>
                      {(p) => (
                        <button
                          type="button"
                          role="option"
                          class="chip"
                          classList={{ "chip--active": p.id === projectId() }}
                          aria-selected={p.id === projectId() ? "true" : "false"}
                          onClick={() => choose(p)}
                        >
                          {p.name}
                        </button>
                      )}
                    </For>
                    <button type="button" class="chip secret-new-project" onClick={() => setStep("project")}>
                      ＋ {t("secrets.newProject")}
                    </button>
                  </div>
                  <Show when={project()}>
                    {(p) => (
                      <>
                        <span class="field-label" id="secret-env">
                          {t("secrets.environment")}
                        </span>
                        <div class="segmented" role="group" aria-labelledby="secret-env">
                          <For each={p().environments}>
                            {(e) => (
                              <button
                                type="button"
                                class="segmented-btn"
                                classList={{ "segmented-btn--active": env() === e.slug }}
                                aria-pressed={env() === e.slug ? "true" : "false"}
                                onClick={() => {
                                  setEnv(e.slug);
                                  setPath("/");
                                  setNewFolder(null);
                                }}
                              >
                                {e.slug}
                              </button>
                            )}
                          </For>
                        </div>
                        <span class="field-label">{t("secrets.folder")}</span>
                        <div class="secret-folder">
                          <code class="secret-path">{path()}</code>
                          <Show when={path() !== "/"}>
                            <button
                              type="button"
                              class="btn btn--small"
                              onClick={() => {
                                setPath(parentFolder(path()));
                                setNewFolder(null);
                              }}
                            >
                              ↑ {t("secrets.up")}
                            </button>
                          </Show>
                        </div>
                        <Show when={newFolder()}>
                          <span class="muted small">{t("secrets.newFolderPending", { path: path() })}</span>
                        </Show>
                        <Show when={!newFolder()}>
                          <div class="secret-subfolders">
                            <For each={folder()?.folders ?? []}>
                              {(f) => (
                                <button type="button" class="chip" onClick={() => enter(f)}>
                                  📁 {f}
                                </button>
                              )}
                            </For>
                            <Show
                              when={folderDraft() !== null}
                              fallback={
                                <button type="button" class="chip" onClick={() => setFolderDraft("")}>
                                  ＋ {t("secrets.newFolder")}
                                </button>
                              }
                            >
                              <span class="secret-newfolder">
                                <input
                                  class="text-input"
                                  aria-label={t("secrets.newFolderName")}
                                  placeholder={t("secrets.newFolderName")}
                                  value={folderDraft() ?? ""}
                                  onInput={(e) => setFolderDraft(e.currentTarget.value)}
                                  onKeyDown={(e) => e.key === "Enter" && (e.preventDefault(), useNewFolder())}
                                />
                                <button type="button" class="btn btn--small" onClick={useNewFolder}>
                                  {t("secrets.newFolderAdd")}
                                </button>
                              </span>
                            </Show>
                          </div>
                        </Show>
                        <span class="field-label">{t("secrets.existing")}</span>
                        <div class="secret-existing" aria-live="polite">
                          <For each={existing()} fallback={<span class="muted small">{folder.loading ? t("common.loading") : t("secrets.existingNone")}</span>}>
                            {(k) => (
                              <button
                                type="button"
                                class="secret-key"
                                classList={{ "secret-key--match": k === name().trim() }}
                                onClick={() => setName(k)}
                              >
                                {k}
                              </button>
                            )}
                          </For>
                        </div>
                        <Show when={overwrite()}>
                          <p class="secret-warn" role="status">
                            ⚠️ {t("secrets.existingWillReplace", { name: name().trim() })}
                          </p>
                        </Show>
                      </>
                    )}
                  </Show>
                </Match>
              </Switch>
              <Show when={created()}>
                {(c) => <p class="secret-ok">✅ {t("secrets.projectCreated", { name: c().name, envs: c().envs })}</p>}
              </Show>
              <div class="secret-actions">
                <button type="button" class="btn btn--primary" disabled={!canReview()} onClick={() => setStep("confirm")}>
                  {t("secrets.review")}
                </button>
              </div>
            </div>
          </Match>

          <Match when={step() === "confirm" || step() === "saving"}>
            <div class="secret-confirm">
              <p class="secret-confirm-title">
                {t("secrets.confirmTitle", { name: name().trim(), where: where() })}
              </p>
              <Show when={overwrite()}>
                <p class="secret-warn">⚠️ {t("secrets.confirmReplace", { name: name().trim() })}</p>
              </Show>
              <Show when={newFolder()}>
                <p class="muted small">{t("secrets.newFolderPending", { path: path() })}</p>
              </Show>
              <div class="secret-actions">
                <button type="button" class="btn" disabled={step() === "saving"} onClick={() => setStep("form")}>
                  {t("secrets.back")}
                </button>
                <button type="button" class="btn btn--primary" classList={{ "btn--danger": overwrite() }} disabled={step() === "saving"} onClick={() => void save()}>
                  {step() === "saving" ? t("common.saving") : overwrite() ? t("secrets.replace") : t("secrets.save")}
                </button>
              </div>
            </div>
          </Match>

          <Match when={step() === "done" && receipt()}>
            {(r) => (
              <div class="secret-receipt" role="status">
                <p class="secret-ok">
                  ✅{" "}
                  {t("secrets.receipt", {
                    verb: r().action === "updated" ? t("secrets.verbUpdated") : t("secrets.verbSaved"),
                    name: r().key,
                    where: `${r().project.name} / ${r().environment} / ${r().path}`,
                  })}
                </p>
                <p class="muted small secret-mono">{t("secrets.fingerprint", { fp: r().fingerprint })}</p>
                <div class="secret-actions">
                  <button type="button" class="btn" onClick={another}>
                    {t("secrets.addAnother")}
                  </button>
                  <button type="button" class="btn btn--primary" onClick={closeSecretSheet}>
                    {t("secrets.done")}
                  </button>
                </div>
              </div>
            )}
          </Match>

          <Match when={step() === "project"}>
            <div class="settings-form secret-form">
              <label class="field-label" for="secret-project-name">
                {t("secrets.newProjectName")}
              </label>
              <input
                ref={first}
                id="secret-project-name"
                class="text-input"
                value={projectName()}
                maxLength={64}
                onInput={(e) => setProjectName(e.currentTarget.value)}
                autocomplete="off"
              />
              <p class="muted small">{t("secrets.newProjectNote")}</p>
              <div class="secret-actions">
                <Show when={!startProject}>
                  <button type="button" class="btn" onClick={() => setStep("form")}>
                    {t("secrets.back")}
                  </button>
                </Show>
                <button type="button" class="btn btn--primary" disabled={!projectName().trim()} onClick={() => setStep("projectConfirm")}>
                  {t("secrets.review")}
                </button>
              </div>
            </div>
          </Match>

          <Match when={step() === "projectConfirm" || step() === "projectSaving"}>
            <div class="secret-confirm">
              <p class="secret-confirm-title">{t("secrets.confirmCreateProject", { name: projectName().trim() })}</p>
              <p class="muted small">{t("secrets.newProjectNote")}</p>
              <div class="secret-actions">
                <button type="button" class="btn" disabled={step() === "projectSaving"} onClick={() => setStep("project")}>
                  {t("secrets.back")}
                </button>
                <button type="button" class="btn btn--primary" disabled={step() === "projectSaving"} onClick={() => void createProject()}>
                  {step() === "projectSaving" ? t("secrets.creating") : t("secrets.createProject")}
                </button>
              </div>
            </div>
          </Match>

          <Match when={step() === "projectDone" && created()}>
            {(c) => (
              <div class="secret-receipt" role="status">
                <p class="secret-ok">✅ {t("secrets.projectCreated", { name: c().name, envs: c().envs })}</p>
                <p class="muted small">{t("secrets.newProjectNote")}</p>
                <div class="secret-actions">
                  <button type="button" class="btn" onClick={() => setStep("form")}>
                    {t("secrets.attach")}
                  </button>
                  <button type="button" class="btn btn--primary" onClick={closeSecretSheet}>
                    {t("secrets.done")}
                  </button>
                </div>
              </div>
            )}
          </Match>
        </Switch>
      </div>
    </div>
  );
}
