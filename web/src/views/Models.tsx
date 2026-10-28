// /models and /models/:id: the private 3D gallery (models Lucina made with Meshy),
// each with its prompt, clips, credits and files; open one in the 3D viewer or delete it.

import { A, useNavigate, useParams } from "@solidjs/router";
import { createResource, createSignal, For, Show } from "solid-js";
import { deleteModel, type GalleryModel, listModels, thumbName } from "../api/models";
import Model3dViewer from "../components/Model3dViewer";
import { Button, Card, EmptyState, LoadingState, Page, PageGrid, PageHeader, useWide } from "../components/page";
import { t } from "../i18n";
import { blockFromModel, modelFileUrl } from "../lib/model3d";
import "../styles/models.css";

function dateLabel(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

function ModelGrid(props: { models: GalleryModel[] }) {
  return (
    <ul class="models-grid">
      <For each={props.models}>
        {(m) => {
          const thumb = thumbName(m);
          return (
            <li>
              <A href={`/models/${m.id}`} class="models-card" data-testid="model-card">
                <span class="models-thumb">
                  <Show when={thumb} fallback={<span aria-hidden="true">🧊</span>}>
                    {(n) => <img src={modelFileUrl(m.id, n())} alt="" loading="lazy" />}
                  </Show>
                </span>
                <span class="models-card-title">{m.title}</span>
                <span class="models-card-sub">
                  {dateLabel(m.createdAt)}
                  {m.credits != null ? ` · ${t("models.credits", { count: m.credits })}` : ""}
                </span>
              </A>
            </li>
          );
        }}
      </For>
    </ul>
  );
}

function ModelDetail(props: { model: GalleryModel; desk: boolean; onRemove: () => void }) {
  const block = () => blockFromModel(props.model);
  return (
    <section class="models-detail">
      <Show
        when={props.desk}
        fallback={
          <A href="/models" class="models-back">
            ← {t("models.back")}
          </A>
        }
      >
        <A href="/models" class="pg-link pg-back">
          {t("models.back")}
        </A>
        <h2 class="trd-detail-title">{props.model.title}</h2>
      </Show>
      <Show when={block()}>{(b) => <Model3dViewer block={b()} open />}</Show>
      <dl class="models-facts">
        <Show when={props.model.prompt}>
          <dt>{t("models.prompt")}</dt>
          <dd>{props.model.prompt}</dd>
        </Show>
        <Show when={props.model.animations.length}>
          <dt>{t("models.animations")}</dt>
          <dd>{props.model.animations.join(", ")}</dd>
        </Show>
        <dt>{t("models.formats")}</dt>
        <dd>{props.model.files.filter((f) => f.name.startsWith("model.")).map((f) => f.name.slice(6).toUpperCase()).join(", ")}</dd>
        <dd>{t("models.made", { date: dateLabel(props.model.createdAt) })}</dd>
      </dl>
      <Show
        when={props.desk}
        fallback={
          <button type="button" class="btn btn--danger" onClick={props.onRemove}>
            {t("models.delete")}
          </button>
        }
      >
        <Button variant="danger" onClick={props.onRemove}>
          {t("models.delete")}
        </Button>
      </Show>
    </section>
  );
}

export default function ModelsView() {
  const params = useParams();
  const navigate = useNavigate();
  const { wide, track } = useWide();
  const [models, { refetch }] = createResource(() => listModels());
  const [notice, setNotice] = createSignal("");
  const selected = () => (models() ?? []).find((m) => m.id === params.id) ?? null;

  async function remove(m: GalleryModel) {
    if (!confirm(t("models.deleteConfirm", { title: m.title }))) return;
    await deleteModel(m.id);
    setNotice(t("models.deleted", { title: m.title }));
    navigate("/models");
    void refetch();
  }

  const gallery = (desk: boolean) => (
    <Show
      when={selected()}
      fallback={
        <Show
          when={!models.loading}
          fallback={
            <Show when={desk} fallback={<p class="empty-sub">{t("models.loadingList")}</p>}>
              <LoadingState label={t("models.loadingList")} />
            </Show>
          }
        >
          <Show
            when={(models() ?? []).length > 0}
            fallback={
              <Show
                when={desk}
                fallback={
                  <div class="empty">
                    <p class="empty-title">{t("models.empty")}</p>
                    <p class="empty-sub">{t("models.emptySub")}</p>
                  </div>
                }
              >
                <EmptyState title={t("models.empty")} text={t("models.emptySub")} />
              </Show>
            }
          >
            <ModelGrid models={models() ?? []} />
          </Show>
        </Show>
      }
    >
      {(m) => <ModelDetail model={m()} desk={desk} onRemove={() => void remove(m())} />}
    </Show>
  );

  return (
    <main class="models" classList={{ "models--wide": wide() }} ref={track}>
      <Show
        when={wide()}
        fallback={
          <>
            <header class="models-head">
              <h1>{t("models.title")}</h1>
              <Show when={notice()}>
                <p class="models-notice" role="status">
                  {notice()}
                </p>
              </Show>
            </header>
            {gallery(false)}
          </>
        }
      >
        <Page class="models-desk">
          <PageHeader title={t("models.title")} context={notice() || undefined} />
          <Show when={models.loading && !selected()}>
            <LoadingState label={t("models.loadingList")} />
          </Show>
          <Show when={!models.loading || selected()}>
            <PageGrid layout="full">
              <Card class="models-desk-surface">{gallery(true)}</Card>
            </PageGrid>
          </Show>
        </Page>
      </Show>
    </main>
  );
}
