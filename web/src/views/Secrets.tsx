import { useNavigate } from "@solidjs/router";
import { createEffect, createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { secretsApi } from "../api/secrets";
import { Button, Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageGrid, PageHeader, PrimaryAction, RowList, useWide } from "../components/page";
import { locale, t } from "../i18n";
import { closeSecretSheet, openSecretSheet, secretSheet } from "../state/secretSheet";
import ChatView from "./Chat";

export function SecretsView() {
  const { wide, track } = useWide();
  const navigate = useNavigate();
  const [project, setProject] = createSignal("");
  const [environment, setEnvironment] = createSignal("");
  const [path, setPath] = createSignal("/");
  const [catalog, { refetch: reloadCatalog }] = createResource(wide, () => secretsApi.catalog());
  const selected = () => catalog.error ? undefined : (catalog()?.find(p => p.id === project()) ?? catalog()?.[0]);
  const env = () => environment() || selected()?.environments[0]?.slug;
  const [folder, { refetch }] = createResource(
    () => wide() && selected() && env() ? { project: selected()!.id, environment: env()!, path: path() } : false,
    s => secretsApi.folder(s.project, s.environment, s.path),
  );
  let opened = false;
  let phoneSheet = false;
  onMount(() => { if (!wide()) { phoneSheet = true; openSecretSheet(); } });
  createEffect(() => {
    if (wide() && phoneSheet) { phoneSheet = false; closeSecretSheet(); }
  });
  createEffect(() => {
    if (secretSheet()) opened = true;
    else if (opened) {
      opened = false;
      if (!wide()) navigate("/", { replace: true });
      else { void reloadCatalog(); void refetch(); }
    }
  });
  onCleanup(() => { if (secretSheet()) closeSecretSheet(); });
  return <div ref={track}>
    <Show when={wide()} fallback={<ChatView />}>
      <Page>
        <PageHeader title={t("secrets.pageTitle")} context={t("secrets.pageContext")}
          action={<PrimaryAction onClick={() => openSecretSheet()}>{t("secrets.title")}</PrimaryAction>} />
        <PageGrid layout="single">
          <Show when={!catalog.loading} fallback={<LoadingState label={t("secrets.loadingProjects")} />}>
            <Show when={!catalog.error} fallback={<ErrorState message={t("secrets.loadFailed")} retryLabel={t("common.tryAgain")} onRetry={() => void reloadCatalog()} />}>
              <Card title={t("secrets.where")}>
                <label class="pg-field">{t("secrets.project")}<select class="pg-input" value={selected()?.id ?? ""} onChange={e => { setProject(e.currentTarget.value); setEnvironment(""); setPath("/"); }}>
                  <For each={catalog()}>{p => <option value={p.id}>{p.name}</option>}</For>
                </select></label>
                <label class="pg-field">{t("secrets.environment")}<select class="pg-input" value={env() ?? ""} onChange={e => { setEnvironment(e.currentTarget.value); setPath("/"); }}>
                  <For each={selected()?.environments}>{e => <option value={e.slug}>{e.name}</option>}</For>
                </select></label>
              </Card>
              <Show when={selected() && env()} fallback={<Card><EmptyState text={t("secrets.noProjects")} /></Card>}>
                <Show when={!folder.loading} fallback={<LoadingState label={t("common.loading")} />}>
                  <Show when={!folder.error} fallback={<ErrorState message={t("secrets.loadFailed")} retryLabel={t("common.tryAgain")} onRetry={() => void refetch()} />}>
                    <Card list title={path()} actions={<Show when={path() !== "/"}><Button variant="ghost" onClick={() => setPath(path().split("/").slice(0, -1).join("/") || "/")}>{t("secrets.up")}</Button></Show>}>
                      <RowList label={t("secrets.existing")}>
                        <For each={folder()?.folders}>{f => <ListRow title={<Button variant="ghost" onClick={() => setPath(`${path() === "/" ? "" : path()}/${f}`)}>{f}</Button>} sub={t("secrets.folder")} />}</For>
                        <For each={folder()?.secrets}>{s => <ListRow title={s.key} sub={`${selected()?.name} · ${env()} · ${path()}`} meta={s.updated_at ? <time dateTime={s.updated_at}>{new Date(s.updated_at).toLocaleString(locale())}</time> : undefined} />}</For>
                      </RowList>
                      <Show when={!folder()?.secrets.length && !folder()?.folders.length}><EmptyState text={t("secrets.existingNone")} /></Show>
                    </Card>
                  </Show>
                </Show>
              </Show>
            </Show>
          </Show>
        </PageGrid>
      </Page>
    </Show>
  </div>;
}
