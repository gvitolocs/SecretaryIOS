// /models and /models/:id: the private 3D gallery (models Lucina made with Meshy),
// each with its prompt, clips, credits and files; open one in the 3D viewer or delete it.

import { A, useNavigate, useParams } from "@solidjs/router";
import { createResource, createSignal, For, Show } from "solid-js";
import { deleteModel, type GalleryModel, listModels, thumbName } from "../api/models";
import Model3dViewer from "../components/Model3dViewer";
import { t } from "../i18n";
import { blockFromModel, modelFileUrl } from "../lib/model3d";

function dateLabel(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

export default function ModelsView() {
  const params = useParams();
  const navigate = useNavigate();
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

  return (
    <main class="models">
      <header class="models-head">
        <h1>{t("models.title")}</h1>
        <Show when={notice()}>
          <p class="models-notice" role="status">{notice()}</p>
        </Show>
      </header>
      <Show when={selected()}>
        {(m) => {
          const block = blockFromModel(m());
          return (
            <section class="models-detail">
              <A href="/models" class="models-back">← {t("models.back")}</A>
              <Show when={block}>{(b) => <Model3dViewer block={b()} open />}</Show>
              <dl class="models-facts">
                <Show when={m().prompt}>
                  <dt>{t("models.prompt")}</dt>
                  <dd>{m().prompt}</dd>
                </Show>
                <Show when={m().animations.length}>
                  <dt>{t("models.animations")}</dt>
                  <dd>{m().animations.join(", ")}</dd>
                </Show>
                <dt>{t("models.formats")}</dt>
                <dd>{m().files.filter((f) => f.name.startsWith("model.")).map((f) => f.name.slice(6).toUpperCase()).join(", ")}</dd>
                <dd>{t("models.made", { date: dateLabel(m().createdAt) })}</dd>
              </dl>
              <button type="button" class="btn btn--danger" onClick={() => void remove(m())}>
                {t("models.delete")}
              </button>
            </section>
          );
        }}
      </Show>
      <Show when={!selected()}>
        <Show when={!models.loading} fallback={<p class="empty-sub">{t("models.loadingList")}</p>}>
          <Show
            when={(models() ?? []).length > 0}
            fallback={
              <div class="empty">
                <p class="empty-title">{t("models.empty")}</p>
                <p class="empty-sub">{t("models.emptySub")}</p>
              </div>
            }
          >
            <ul class="models-grid">
              <For each={models()}>
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
          </Show>
        </Show>
      </Show>
    </main>
  );
}
