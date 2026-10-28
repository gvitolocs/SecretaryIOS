import { useNavigate, useParams, useSearchParams } from "@solidjs/router";
import { For, Show, createEffect, createMemo, createResource, createSignal, on, onCleanup, type JSX } from "solid-js";
import { toApiError } from "../api/client";
import { notesApi, type Note } from "../api/notes";
import { prompterApi } from "../api/prompter";
import { Markdown } from "../components/Markdown";
import {
  Button,
  Card,
  EmptyState,
  ErrorState,
  ListRow,
  LoadingState,
  Page,
  PageGrid,
  PageHeader,
  PrimaryAction,
  RowList,
  Sheet,
} from "../components/page";
import { locale, t } from "../i18n";
import { desktopShell } from "../lib/desktopShell";
import {
  parseScript,
  pointCount,
  renumber,
  replaceTopTake,
  serializeScript,
  splitNote,
  withScript,
  withTake,
  type ScriptSection,
} from "../lib/prompterScript";
import { report, reportMarkdown } from "../lib/prompterTake";
import { PrompterLive, type TakeResult, type TakeSettings } from "./PrompterLive";
import "../styles/prompter.css";

// Lucina Prompter (docs/prompter.md): scripts of talking points (Keep notes labelled
// "prompter"), edited inline, then a live take where Lucina listens and crosses points
// out. Each take is written back to the script's note: what was said, in which order,
// what was missed, the transcript, and Lucina's summary on request.

const SETTINGS_KEY = "secretary.prompter";
const DEFAULTS: TakeSettings = {
  language: "auto",
  useJev: true,
  alsoRecord: false,
  autoAdvance: true,
  opacity: 0.85,
};

function loadSettings(): TakeSettings {
  try {
    return {
      ...DEFAULTS,
      ...(JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? "{}") as Partial<TakeSettings>),
    };
  } catch {
    return { ...DEFAULTS };
  }
}

const errText = (e: unknown) => toApiError(e).message;

/** The points as a list of fields, or as Markdown text. */
function PointsEditor(props: { sections: ScriptSection[]; onChange: (s: ScriptSection[]) => void; autofocus?: boolean }) {
  const [asText, setAsText] = createSignal(false);
  const [text, setText] = createSignal("");
  const edit = (f: (s: ScriptSection[]) => void) => {
    const copy = props.sections.map((s) => ({
      title: s.title,
      points: s.points.map((p) => ({ ...p })),
    }));
    f(copy);
    props.onChange(renumber(copy));
  };
  return (
    <div class="pr-editor">
      <div class="pr-editor-mode">
        <button
          type="button"
          class="pg-link"
          onClick={() => {
            if (!asText()) setText(serializeScript(props.sections));
            setAsText(!asText());
          }}
        >
          {asText() ? t("prompter.editAsList") : t("prompter.editAsText")}
        </button>
      </div>
      <Show
        when={!asText()}
        fallback={
          <label class="pg-field">
            <span class="pg-field-label">{t("prompter.textHint")}</span>
            <textarea
              class="pg-input pr-textarea"
              value={text()}
              rows={16}
              spellcheck
              onInput={(e) => {
                setText(e.currentTarget.value);
                props.onChange(parseScript(e.currentTarget.value));
              }}
            />
          </label>
        }
      >
        <For each={props.sections}>
          {(s, si) => (
            <section class="pr-edit-section">
              <div class="pr-edit-head">
                <input
                  class="pg-input pr-edit-title"
                  value={s.title}
                  placeholder={t("prompter.section", { n: si() + 1 })}
                  aria-label={t("prompter.sectionTitle")}
                  onChange={(e) => edit((c) => (c[si()]!.title = e.currentTarget.value.trim()))}
                />
                <button
                  type="button"
                  class="pg-icon-btn"
                  aria-label={t("prompter.removeSection")}
                  title={t("prompter.removeSection")}
                  onClick={() => edit((c) => c.splice(si(), 1))}
                >
                  ×
                </button>
              </div>
              <ol class="pr-edit-points">
                <For each={s.points}>
                  {(p, pi) => (
                    <li>
                      <input
                        class="pg-input"
                        value={p.text}
                        placeholder={t("prompter.pointPlaceholder")}
                        onChange={(e) => {
                          const v = e.currentTarget.value.trim();
                          edit((c) => (v ? (c[si()]!.points[pi()]!.text = v) : c[si()]!.points.splice(pi(), 1)));
                        }}
                        onKeyDown={(e) => {
                          if (e.key !== "Enter") return;
                          e.preventDefault();
                          const v = e.currentTarget.value.trim();
                          edit((c) => {
                            if (v) c[si()]!.points[pi()]!.text = v;
                            c[si()]!.points.splice(pi() + 1, 0, {
                              id: "",
                              text: "",
                            });
                          });
                          queueMicrotask(() =>
                            (e.currentTarget.closest("ol")?.querySelectorAll("input")[pi() + 1] as HTMLInputElement | undefined)?.focus(),
                          );
                        }}
                      />
                      <button
                        type="button"
                        class="pg-icon-btn"
                        aria-label={t("prompter.removePoint")}
                        title={t("prompter.removePoint")}
                        onClick={() => edit((c) => c[si()]!.points.splice(pi(), 1))}
                      >
                        ×
                      </button>
                    </li>
                  )}
                </For>
              </ol>
              <Button
                variant="ghost"
                onClick={() =>
                  edit((c) =>
                    c[si()]!.points.push({
                      id: "",
                      text: t("prompter.pointPlaceholder"),
                    }),
                  )
                }
              >
                + {t("prompter.addPoint")}
              </Button>
            </section>
          )}
        </For>
        <Button
          onClick={() =>
            edit((c) =>
              c.push({
                title: "",
                points: [{ id: "", text: t("prompter.pointPlaceholder") }],
              }),
            )
          }
        >
          + {t("prompter.addSection")}
        </Button>
      </Show>
    </div>
  );
}

export default function PrompterView() {
  const params = useParams<{ id?: string }>();
  const [search] = useSearchParams<{ live?: string; overlay?: string }>();
  const navigate = useNavigate();
  const overlay = () => search.overlay === "1";
  const [scripts, { refetch }] = createResource(() => prompterApi.scripts());
  const [note, setNote] = createSignal<Note | null>(null);
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const [title, setTitle] = createSignal("");
  const [draft, setDraft] = createSignal<ScriptSection[]>([]);
  const [dirty, setDirty] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [notice, setNotice] = createSignal<{
    text: string;
    error?: boolean;
  } | null>(null);
  const [settings, setSettings] = createSignal<TakeSettings>(loadSettings());
  const [live, setLive] = createSignal(false);
  const [result, setResult] = createSignal<{
    r: TakeResult;
    md: string;
    before: string;
  } | null>(null);
  const [summary, setSummary] = createSignal<{
    busy: boolean;
    text?: string;
    error?: string;
  }>({ busy: false });
  const [sheet, setSheet] = createSignal<"draft" | "keep" | null>(null);
  const [midEdit, setMidEdit] = createSignal<{
    sections: ScriptSection[];
    resolve: (s: ScriptSection[] | null) => void;
  } | null>(null);

  const saveSettings = (patch: Partial<TakeSettings>) => {
    const next = { ...settings(), ...patch };
    setSettings(next);
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(next));
    } catch {
      /* private mode: settings last for this page */
    }
  };

  createEffect(
    on(
      () => params.id,
      async (id) => {
        setResult(null);
        setLoadError(null);
        if (!id) {
          setNote(null);
          return;
        }
        try {
          const n = await prompterApi.get(id);
          setNote(n);
          setTitle(n.title);
          setDraft(parseScript(n.content));
          setDirty(false);
          if (search.live === "1" && pointCount(parseScript(n.content))) setLive(true);
        } catch (e) {
          setLoadError(errText(e));
        }
      },
    ),
  );

  async function save(): Promise<Note | null> {
    const n = note();
    if (!n) return null;
    if (!dirty()) return n;
    setSaving(true);
    try {
      const updated = await prompterApi.save(n, {
        title: title().trim() || t("prompter.untitled"),
        content: withScript(n.content, draft()),
      });
      setNote(updated);
      setDirty(false);
      setNotice({ text: t("prompter.saved") });
      void refetch();
      return updated;
    } catch (e) {
      setNotice({
        text: t("prompter.saveFailed", { error: errText(e) }),
        error: true,
      });
      return null;
    } finally {
      setSaving(false);
    }
  }

  const onKeys = (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "s" && note() && !live()) {
      e.preventDefault();
      void save();
    }
  };
  window.addEventListener("keydown", onKeys);
  onCleanup(() => window.removeEventListener("keydown", onKeys));

  async function start() {
    if (!pointCount(draft())) {
      setNotice({ text: t("prompter.noPoints"), error: true });
      return;
    }
    if (dirty() && !(await save())) return;
    setResult(null);
    setSummary({ busy: false });
    setLive(true);
  }

  function openOverlay() {
    const n = note();
    if (!n) return;
    desktopShell()?.openPrompterOverlay?.(`/prompter/${encodeURIComponent(n.slug)}?live=1&overlay=1`, { opacity: settings().opacity });
  }

  async function newScript(content = `## ${t("prompter.section", { n: 1 })}\n- ${t("prompter.pointPlaceholder")}`, name = t("prompter.untitled")) {
    try {
      const n = await prompterApi.create(name, content);
      await refetch();
      navigate(`/prompter/${encodeURIComponent(n.slug)}`);
    } catch (e) {
      setNotice({
        text: t("prompter.saveFailed", { error: errText(e) }),
        error: true,
      });
    }
  }

  async function finish(r: TakeResult) {
    setLive(false);
    const n = note();
    if (!n) return;
    const transcript = r.finals
      .map((f) => f.text)
      .join(" ")
      .trim();
    const lat = [...r.latencies].sort((a, b) => a - b);
    const md = reportMarkdown(r.take, {
      startedAt: r.startedAt,
      durationMs: r.durationMs,
      transcript,
      jevCalls: r.jevCalls,
      medianLatencyMs: lat.length ? lat[lat.length >> 1]! : null,
      locale: locale(),
      memo: r.recorded
        ? `${t("prompter.recordingSaved")} (/memos, ${r.startedAt.toLocaleTimeString(locale(), { hour: "2-digit", minute: "2-digit" })})`
        : undefined,
    });
    setResult({ r, md, before: n.content });
    setSummary({ busy: false });
    if (overlay()) desktopShell()?.closePrompterOverlay?.();
    try {
      const fresh = await prompterApi.get(n.id).catch(() => n);
      const updated = await prompterApi.save(fresh, {
        content: withTake(fresh.content, md),
      });
      setNote(updated);
      setResult({ r, md, before: fresh.content });
      setNotice({ text: t("prompter.takeSaved") });
    } catch (e) {
      setNotice({
        text: t("prompter.takeSaveFailed", { error: errText(e) }),
        error: true,
      });
    }
  }

  async function summarise() {
    const res = result();
    const n = note();
    if (!res || !n) return;
    setSummary({ busy: true });
    try {
      const text = await prompterApi.summary({
        title: n.title,
        script: serializeScript(res.r.take.sections),
        report: res.md.split("**Transcript**")[0] ?? res.md,
        transcript: res.r.finals.map((f) => f.text).join(" "),
      });
      setSummary({ busy: false, text });
      const fresh = await prompterApi.get(n.id).catch(() => n);
      const withSummary = `${res.md}\n\n**${t("prompter.summary")}**\n\n${text.trim()}`;
      const content = replaceTopTake(fresh.content, res.md, withSummary);
      if (content) {
        setNote(await prompterApi.save(fresh, { content }));
        setResult({ ...res, md: withSummary });
      }
    } catch (e) {
      setSummary({ busy: false, error: errText(e) });
    }
  }

  const count = createMemo(() => pointCount(draft()));
  const takes = createMemo(() => splitNote(note()?.content ?? "").takes);
  const rep = createMemo(() => (result() ? report(result()!.r.take) : null));

  const rail = (): JSX.Element => (
    <>
      <Card
        title={t("prompter.scriptsTitle")}
        list
        actions={
          <Button variant="ghost" onClick={() => void newScript()}>
            + {t("prompter.newScript")}
          </Button>
        }
      >
        <Show when={!scripts.loading || scripts()} fallback={<LoadingState label={t("common.loading")} />}>
          <Show when={(scripts() ?? []).length} fallback={<p class="pr-rail-empty pg-muted">{t("prompter.empty")}</p>}>
            <RowList label={t("prompter.scriptsTitle")}>
              <For each={scripts()}>
                {(s) => (
                  <ListRow
                    class={note()?.id === s.id ? "pr-row-current" : ""}
                    title={s.title || t("prompter.untitled")}
                    href={`/prompter/${encodeURIComponent(s.slug)}`}
                    sub={new Date(s.updatedAt).toLocaleDateString(locale(), {
                      day: "numeric",
                      month: "short",
                    })}
                  />
                )}
              </For>
            </RowList>
          </Show>
        </Show>
        <div class="pr-rail-actions">
          <Button variant="ghost" onClick={() => setSheet("draft")}>
            ✦ {t("prompter.draft")}
          </Button>
          <Button variant="ghost" onClick={() => setSheet("keep")}>
            {t("prompter.fromKeep")}
          </Button>
        </div>
      </Card>
      <Card title={t("prompter.settings")}>
        <div class="pr-settings">
          <label class="pg-field">
            <span class="pg-field-label">{t("prompter.language")}</span>
            <select class="pg-input" value={settings().language} onChange={(e) => saveSettings({ language: e.currentTarget.value })}>
              <option value="auto">{t("prompter.languageAuto")}</option>
              <option value="en">English</option>
              <option value="it">Italiano</option>
              <option value="da">Dansk</option>
            </select>
          </label>
          <label class="pr-check">
            <input type="checkbox" checked={settings().useJev} onChange={(e) => saveSettings({ useJev: e.currentTarget.checked })} />
            <span>
              {t("prompter.useJev")}
              <small class="pg-muted">{t("prompter.useJevHint")}</small>
            </span>
          </label>
          <label class="pr-check">
            <input type="checkbox" checked={settings().autoAdvance} onChange={(e) => saveSettings({ autoAdvance: e.currentTarget.checked })} />
            <span>{t("prompter.autoAdvance")}</span>
          </label>
          <label class="pr-check">
            <input type="checkbox" checked={settings().alsoRecord} onChange={(e) => saveSettings({ alsoRecord: e.currentTarget.checked })} />
            <span>{t("prompter.alsoRecord")}</span>
          </label>
          <Show when={desktopShell()?.openPrompterOverlay}>
            <label class="pg-field">
              <span class="pg-field-label">
                {t("prompter.opacity")} {Math.round(settings().opacity * 100)}%
              </span>
              <input
                type="range"
                min="0.3"
                max="1"
                step="0.05"
                value={settings().opacity}
                onInput={(e) => saveSettings({ opacity: Number(e.currentTarget.value) })}
              />
            </label>
          </Show>
        </div>
      </Card>
    </>
  );

  return (
    <>
      <Show when={live() && note()}>
        <PrompterLive
          sections={draft()}
          settings={settings()}
          overlay={overlay()}
          onEnd={(r) => void finish(r)}
          onEdit={(sections) => new Promise((resolve) => setMidEdit({ sections, resolve }))}
        />
      </Show>
      <Show when={!(live() && overlay())}>
        <Page class="pr-page" label={t("prompter.title")}>
          <PageHeader
            title={t("prompter.title")}
            context={
              note()
                ? `${note()!.title || t("prompter.untitled")} · ${t("prompter.pointsCount", { count: count() })}`
                : t("prompter.scripts", { count: scripts()?.length ?? 0 })
            }
            action={
              <Show when={note()}>
                <PrimaryAction onClick={() => void start()} disabled={!count()}>
                  ● {t("prompter.startTake")}
                </PrimaryAction>
              </Show>
            }
          />
          <Show when={notice()}>
            <div class="pg-notice" classList={{ "pr-notice-ok": !notice()!.error }} role="status">
              <span>{notice()!.text}</span>
              <button type="button" class="pg-icon-btn" aria-label={t("common.dismiss")} onClick={() => setNotice(null)}>
                ×
              </button>
            </div>
          </Show>
          <PageGrid rail={rail()} railLabel={t("prompter.title")}>
            <Show when={result()}>
              <Card
                title={t("prompter.said", {
                  said: rep()!.said.length,
                  total: rep()!.total,
                })}
                class="pr-result"
              >
                <div class="pr-result-body">
                  <Show when={rep()!.outOfOrder}>
                    <p class="pg-muted">{t("prompter.outOfOrder", { count: rep()!.outOfOrder })}</p>
                  </Show>
                  <ol class="pr-said">
                    <For each={rep()!.said}>{(d) => <li>{d.text}</li>}</For>
                  </ol>
                  <Show when={rep()!.missed.length}>
                    <h3 class="pr-sub">{t("prompter.missed")}</h3>
                    <ul class="pr-missed-list">
                      <For each={rep()!.missed}>{(m) => <li>{m.text}</li>}</For>
                    </ul>
                  </Show>
                  <Show when={rep()!.notReached.length}>
                    <h3 class="pr-sub">{t("prompter.notReached")}</h3>
                    <ul class="pr-missed-list pr-missed-list--later">
                      <For each={rep()!.notReached}>{(m) => <li>{m.text}</li>}</For>
                    </ul>
                  </Show>
                  <Show when={summary().text}>
                    <h3 class="pr-sub">{t("prompter.summary")}</h3>
                    <Markdown text={summary().text!} />
                  </Show>
                  <Show when={summary().error}>
                    <p class="pr-error-text">{t("prompter.summaryFailed", { error: summary().error! })}</p>
                  </Show>
                  <div class="pr-result-actions">
                    <Show when={!summary().text}>
                      <Button onClick={() => void summarise()} disabled={summary().busy || !result()!.r.finals.length}>
                        {summary().busy ? t("prompter.summarising") : `✦ ${t("prompter.summarise")}`}
                      </Button>
                    </Show>
                    <Button onClick={() => void start()}>{t("prompter.another")}</Button>
                    <Button variant="ghost" onClick={() => setResult(null)}>
                      {t("prompter.backToScript")}
                    </Button>
                  </div>
                </div>
              </Card>
            </Show>
            <Show
              when={params.id}
              fallback={
                <Card>
                  <EmptyState
                    title={t("prompter.title")}
                    text={(scripts() ?? []).length ? t("prompter.pick") : t("prompter.empty")}
                    action={<Button onClick={() => void newScript()}>+ {t("prompter.newScript")}</Button>}
                  />
                </Card>
              }
            >
              <Show
                when={!loadError()}
                fallback={
                  <ErrorState
                    message={loadError()!}
                    retryLabel={t("common.tryAgain")}
                    onRetry={() => navigate(`/prompter/${params.id}`, { replace: true })}
                  />
                }
              >
                <Show when={note()} fallback={<LoadingState label={t("common.loading")} />}>
                  <Card
                    title={t("prompter.points")}
                    actions={
                      <>
                        <Show when={desktopShell()?.openPrompterOverlay}>
                          <Button variant="ghost" onClick={openOverlay} title={t("prompter.overlayHint")}>
                            {t("prompter.overlayOpen")}
                          </Button>
                        </Show>
                        <Button onClick={() => void save()} disabled={!dirty() || saving()}>
                          {saving() ? t("prompter.saving") : t("prompter.save")}
                        </Button>
                      </>
                    }
                  >
                    <label class="pg-field pr-title-field">
                      <span class="pg-field-label">{t("prompter.titleLabel")}</span>
                      <input
                        class="pg-input"
                        value={title()}
                        onInput={(e) => {
                          setTitle(e.currentTarget.value);
                          setDirty(true);
                        }}
                      />
                    </label>
                    <PointsEditor
                      sections={draft()}
                      onChange={(s) => {
                        setDraft(s);
                        setDirty(true);
                      }}
                    />
                  </Card>
                  <Card title={t("prompter.takes")}>
                    <Show when={takes()} fallback={<p class="pg-muted">{t("prompter.noTakes")}</p>}>
                      <Markdown text={takes()} class="pr-takes" />
                    </Show>
                  </Card>
                </Show>
              </Show>
            </Show>
          </PageGrid>
        </Page>
      </Show>
      <DraftSheet
        open={sheet() === "draft"}
        onClose={() => setSheet(null)}
        onDrafted={(md, name) => void newScript(md, name).then(() => setSheet(null))}
      />
      <KeepSheet
        open={sheet() === "keep"}
        onClose={() => setSheet(null)}
        onPick={async (n) => {
          const adopted = await prompterApi.adopt(n);
          await refetch();
          setSheet(null);
          navigate(`/prompter/${encodeURIComponent(adopted.slug)}`);
        }}
      />
      <Sheet
        open={!!midEdit()}
        size="wide"
        title={t("prompter.edit")}
        onClose={() => {
          midEdit()?.resolve(null);
          setMidEdit(null);
        }}
        footer={
          <Button
            onClick={() => {
              const m = midEdit();
              if (!m) return;
              setDraft(m.sections);
              setDirty(true);
              m.resolve(m.sections);
              setMidEdit(null);
              void save();
            }}
          >
            {t("prompter.save")}
          </Button>
        }
      >
        <Show when={midEdit()}>
          <PointsEditor sections={midEdit()!.sections} onChange={(s) => setMidEdit({ ...midEdit()!, sections: s })} />
        </Show>
      </Sheet>
    </>
  );
}

function DraftSheet(props: { open: boolean; onClose: () => void; onDrafted: (markdown: string, title: string) => void }) {
  const [topic, setTopic] = createSignal("");
  const [doc, setDoc] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  async function go() {
    setBusy(true);
    setError(null);
    try {
      const md = await prompterApi.draft(topic(), doc(), locale());
      props.onDrafted(md, topic().trim().slice(0, 120) || t("prompter.untitled"));
    } catch (e) {
      setError(t("prompter.draftFailed", { error: errText(e) }));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Sheet
      open={props.open}
      onClose={props.onClose}
      title={t("prompter.draft")}
      footer={
        <Button onClick={() => void go()} disabled={busy() || (!topic().trim() && !doc().trim())}>
          {busy() ? t("prompter.drafting") : `✦ ${t("prompter.draftGo")}`}
        </Button>
      }
    >
      <div class="pr-settings">
        <label class="pg-field">
          <span class="pg-field-label">{t("prompter.draftTopic")}</span>
          <input class="pg-input" value={topic()} placeholder={t("prompter.draftTopicHint")} onInput={(e) => setTopic(e.currentTarget.value)} />
        </label>
        <label class="pg-field">
          <span class="pg-field-label">{t("prompter.draftDocument")}</span>
          <textarea
            class="pg-input pr-textarea"
            rows={10}
            value={doc()}
            placeholder={t("prompter.draftDocumentHint")}
            onInput={(e) => setDoc(e.currentTarget.value)}
          />
        </label>
        <Show when={error()}>
          <p class="pr-error-text" role="alert">
            {error()}
          </p>
        </Show>
      </div>
    </Sheet>
  );
}

function KeepSheet(props: { open: boolean; onClose: () => void; onPick: (n: Note) => Promise<void> }) {
  const [q, setQ] = createSignal("");
  const [list] = createResource(
    () => (props.open ? q() : null),
    (query) => notesApi.list(query),
  );
  const [busy, setBusy] = createSignal<string | null>(null);
  return (
    <Sheet
      open={props.open}
      onClose={props.onClose}
      title={t("prompter.fromKeep")}
      tools={<input class="pg-input" type="search" value={q()} placeholder={t("prompter.keepSearch")} onInput={(e) => setQ(e.currentTarget.value)} />}
    >
      <Show when={!list.loading || list()} fallback={<LoadingState label={t("common.loading")} />}>
        <Show when={(list() ?? []).length} fallback={<p class="pg-muted">{t("prompter.keepEmpty")}</p>}>
          <RowList>
            <For each={list()}>
              {(n) => (
                <ListRow
                  title={n.title || n.slug}
                  sub={n.excerpt}
                  trailing={
                    <Button
                      disabled={!!busy()}
                      onClick={async () => {
                        setBusy(n.id);
                        try {
                          await props.onPick(await notesApi.get(n.id));
                        } finally {
                          setBusy(null);
                        }
                      }}
                    >
                      {t("prompter.keepUse")}
                    </Button>
                  }
                />
              )}
            </For>
          </RowList>
        </Show>
      </Show>
    </Sheet>
  );
}
