import { A } from "@solidjs/router";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { toApiError, type ApiError } from "../api/client";
import {
  LEARN_LANGS,
  MAX_ENTRIES,
  MAX_FIELD_CHARS,
  splitSoundsLike,
  voiceLearnApi,
  type LearnDraft,
  type LearnEntry,
  type LearnLang,
  type SampleResult,
} from "../api/voiceLearn";
import { BackIcon, EditIcon, MicIcon, PlusIcon, TrashIcon } from "../components/Icons";
import { Button, Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageHeader, PrimaryAction, RowList } from "../components/page";
import { t, type MessageKey } from "../i18n";
import { createSampleRecorder, type SampleFailure } from "../lib/voiceSample";
import "../styles/voice.css";
import "../styles/voice-learn.css";

// Voice Learn (Plugins → Voice devices → Voice Learn): the words Lucina should recognise in voice turns, voice
// notes and memos (docs/lucinavoice-protocol.md section 8). The list, the add / edit form and the "Record a
// sample" helper: say the word into the phone, see what the speech-to-text heard, and add that as "sounds like".

const LANG_TEXT: Record<LearnLang, MessageKey> = { it: "voiceLearn.langIt", en: "voiceLearn.langEn", da: "voiceLearn.langDa" };

/** What the form edits; `id` is set when an existing entry is being changed. */
interface Form {
  id: string | null;
  word: string;
  soundsLike: string;
  meaning: string;
  lang: LearnLang | "";
}

const blank = (): Form => ({ id: null, word: "", soundsLike: "", meaning: "", lang: "" });
const fromEntry = (e: LearnEntry): Form => ({ id: e.id, word: e.word, soundsLike: e.soundsLike.join(", "), meaning: e.meaning ?? "", lang: e.lang ?? "" });

/** The word(s) to offer as "sounds like" from a sample: what the engine first heard, else what it heard. */
export function candidatesFrom(sample: SampleResult, word: string): string[] {
  const clean = (s: string) => s.replace(/^[\s"“”«»'.,;:!?¿¡-]+|[\s"“”«»'.,;:!?¿¡-]+$/g, "").slice(0, MAX_FIELD_CHARS);
  const w = word.trim().toLowerCase();
  const fixed = sample.corrections.filter((c) => !w || c.to.toLowerCase() === w).map((c) => clean(c.from));
  const list = fixed.length ? fixed : [clean(sample.heard)];
  return [...new Set(list.filter(Boolean))];
}

export default function VoiceLearnView() {
  const [entries, setEntries] = createSignal<LearnEntry[] | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [form, setForm] = createSignal<Form | null>(null);
  const [removing, setRemoving] = createSignal<string | null>(null);
  const [rowError, setRowError] = createSignal("");

  async function load() {
    if (loading()) return;
    setLoading(true);
    try {
      setEntries(await voiceLearnApi.list());
      setError(null);
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setLoading(false);
    }
  }
  onMount(() => void load());

  async function remove(e: LearnEntry) {
    setRowError("");
    try {
      await voiceLearnApi.remove(e.id);
      setEntries((list) => (list ?? []).filter((x) => x.id !== e.id));
      setRemoving(null);
    } catch (err) {
      setRowError(t("voiceLearn.removeFailed", { error: toApiError(err).message }));
    }
  }

  const full = () => (entries()?.length ?? 0) >= MAX_ENTRIES;

  return (
    <Page class="vd-page vl-page" label={t("voiceLearn.title")}>
      <A href="/voice" class="pg-link pg-back">
        <BackIcon /> {t("voiceLearn.back")}
      </A>
      <PageHeader
        title={t("voiceLearn.title")}
        context={t("voiceLearn.explain")}
        action={
          <PrimaryAction icon={<PlusIcon />} disabled={!entries() || full() || !!form()} onClick={() => setForm(blank())}>
            {t("voiceLearn.add")}
          </PrimaryAction>
        }
      />

      <Show when={form()}>
        {(f) => (
          <EntryForm
            initial={f()}
            onSaved={() => {
              setForm(null);
              void load();
            }}
            onCancel={() => setForm(null)}
          />
        )}
      </Show>

      <Show
        when={entries()}
        fallback={
          <Show when={error()} fallback={<LoadingState label={t("voiceLearn.loading")} />}>
            <ErrorState message={t("voiceLearn.loadError")} retryLabel={t("voiceLearn.retry")} onRetry={() => void load()} />
          </Show>
        }
      >
        {(list) => (
          <>
            <Show when={error()}>
              <p class="vd-banner" role="alert">{t("voiceLearn.refreshError", { error: error()!.message })}</p>
            </Show>
            <Show when={rowError()}>
              <p class="vd-banner" role="alert">{rowError()}</p>
            </Show>
            <Show
              when={list().length}
              fallback={
                <Card>
                  <EmptyState title={t("voiceLearn.empty")} text={t("voiceLearn.emptyText")} />
                </Card>
              }
            >
              <Card list label={t("voiceLearn.list")}>
                <RowList label={t("voiceLearn.list")}>
                  <For each={list()}>
                    {(e) => (
                      <ListRow
                        title={e.word}
                        badge={e.lang ? t(LANG_TEXT[e.lang]) : undefined}
                        sub={
                          <>
                            <Show when={e.soundsLike.length}>
                              <span class="vl-sounds">{t("voiceLearn.soundsLikeValue", { list: e.soundsLike.join(", ") })}</span>
                            </Show>
                            <Show when={e.meaning}>
                              <span class="vl-meaning">{e.meaning}</span>
                            </Show>
                            <Show when={e.source === "correction"}>
                              <span class="vl-source">{t("voiceLearn.fromCorrection")}</span>
                            </Show>
                          </>
                        }
                        trailing={
                          <Show
                            when={removing() === e.id}
                            fallback={
                              <span class="vl-actions">
                                <button type="button" class="pg-icon-btn" aria-label={t("voiceLearn.editWord", { word: e.word })} disabled={!!form()} onClick={() => setForm(fromEntry(e))}>
                                  <EditIcon />
                                </button>
                                <button type="button" class="pg-icon-btn" aria-label={t("voiceLearn.removeWord", { word: e.word })} onClick={() => { setRowError(""); setRemoving(e.id); }}>
                                  <TrashIcon />
                                </button>
                              </span>
                            }
                          >
                            <span class="vl-actions vl-confirm" role="group" aria-label={t("voiceLearn.confirmRemove", { word: e.word })}>
                              <span>{t("voiceLearn.confirmRemove", { word: e.word })}</span>
                              <Button variant="danger" onClick={() => void remove(e)}>
                                {t("voiceLearn.confirmYes")}
                              </Button>
                              <Button variant="ghost" onClick={() => setRemoving(null)}>
                                {t("voiceLearn.cancel")}
                              </Button>
                            </span>
                          </Show>
                        }
                      />
                    )}
                  </For>
                </RowList>
              </Card>
              <p class="pg-muted vl-count">{t("voiceLearn.count", { count: list().length, max: MAX_ENTRIES })}</p>
            </Show>
          </>
        )}
      </Show>
    </Page>
  );
}

// ---- The add / edit form, with the sample recorder ----------------------------------------

function EntryForm(props: { initial: Form; onSaved: () => void; onCancel: () => void }) {
  const [f, setF] = createSignal<Form>(props.initial);
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");
  const set = <K extends keyof Form>(k: K, v: Form[K]) => setF((x) => ({ ...x, [k]: v }));

  async function submit(e: Event) {
    e.preventDefault();
    const word = f().word.trim();
    if (!word) {
      setError(t("voiceLearn.needWord"));
      return;
    }
    const soundsLike = splitSoundsLike(f().soundsLike);
    const meaning = f().meaning.trim();
    setSaving(true);
    setError("");
    try {
      if (f().id) {
        // An edit sends every field, so a cleared one is cleared.
        const patch: LearnDraft = { word, soundsLike, meaning, lang: f().lang || null };
        await voiceLearnApi.update(f().id!, patch);
      } else {
        await voiceLearnApi.create({ word, ...(soundsLike.length ? { soundsLike } : {}), ...(meaning ? { meaning } : {}), ...(f().lang ? { lang: f().lang as LearnLang } : {}) });
      }
      props.onSaved();
    } catch (err) {
      // Lucina's own validation message (too long, duplicate, full) is shown as it is.
      setError(t("voiceLearn.saveFailed", { error: toApiError(err).message }));
    } finally {
      setSaving(false);
    }
  }

  return (
    <Card title={f().id ? t("voiceLearn.editWord", { word: props.initial.word }) : t("voiceLearn.add")} class="vl-form-card">
      <form class="vl-form" aria-label={t("voiceLearn.form")} onSubmit={(e) => void submit(e)}>
        <div class="pg-field">
          <label class="pg-field-label" for="vl-word">{t("voiceLearn.word")}</label>
          <input id="vl-word" class="pg-input" value={f().word} maxlength={MAX_FIELD_CHARS} autocomplete="off" placeholder={t("voiceLearn.wordHint")} onInput={(e) => set("word", e.currentTarget.value)} disabled={saving()} />
        </div>
        <div class="pg-field">
          <label class="pg-field-label" for="vl-sounds">{t("voiceLearn.soundsLike")}</label>
          <input id="vl-sounds" class="pg-input" value={f().soundsLike} autocomplete="off" aria-describedby="vl-sounds-hint" onInput={(e) => set("soundsLike", e.currentTarget.value)} disabled={saving()} />
          <span id="vl-sounds-hint" class="pg-muted vl-hint">{t("voiceLearn.soundsLikeHint")}</span>
        </div>
        <div class="pg-field">
          <label class="pg-field-label" for="vl-meaning">{t("voiceLearn.meaning")}</label>
          <input id="vl-meaning" class="pg-input" value={f().meaning} maxlength={MAX_FIELD_CHARS} autocomplete="off" placeholder={t("voiceLearn.meaningHint")} onInput={(e) => set("meaning", e.currentTarget.value)} disabled={saving()} />
        </div>
        <div class="pg-field">
          <label class="pg-field-label" for="vl-lang">{t("voiceLearn.lang")}</label>
          <select id="vl-lang" class="pg-input" value={f().lang} onChange={(e) => set("lang", e.currentTarget.value as LearnLang | "")} disabled={saving()}>
            <option value="">{t("voiceLearn.langAny")}</option>
            <For each={LEARN_LANGS}>{(l) => <option value={l}>{t(LANG_TEXT[l])}</option>}</For>
          </select>
        </div>

        <SampleBox
          lang={f().lang || undefined}
          word={f().word}
          onAdd={(words) => set("soundsLike", splitSoundsLike([f().soundsLike, ...words].join(", ")).join(", "))}
        />

        <Show when={error()}>
          <p class="vl-error" role="alert">{error()}</p>
        </Show>
        <div class="vl-form-actions">
          <button type="submit" class="pg-cta" disabled={saving()}>
            {saving() ? t("voiceLearn.saving") : t("voiceLearn.save")}
          </button>
          <Button variant="ghost" onClick={() => props.onCancel()} disabled={saving()}>
            {t("voiceLearn.cancel")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

// ---- Record a sample ----------------------------------------------------------------------------

const FAILURES: Record<SampleFailure, MessageKey> = {
  unsupported: "voiceLearn.sampleUnsupported",
  denied: "voiceLearn.sampleDenied",
  failed: "voiceLearn.sampleFailed",
};

function SampleBox(props: { lang: LearnLang | undefined; word: string; onAdd: (words: string[]) => void }) {
  const [listening, setListening] = createSignal(false);
  const [result, setResult] = createSignal<SampleResult | null>(null);
  const [problem, setProblem] = createSignal("");

  const recorder = createSampleRecorder(
    (wav) => void listen(wav),
    (why, detail) => setProblem(why === "failed" ? t(FAILURES.failed, { error: detail ?? "" }) : t(FAILURES[why])),
  );
  onCleanup(recorder.cancel);

  async function listen(wav: Blob) {
    setListening(true);
    try {
      const heard = await voiceLearnApi.sample(wav, props.lang);
      setResult(heard);
    } catch (e) {
      const err = toApiError(e);
      setProblem(err.code === "no_speech" ? t("voiceLearn.sampleNoSpeech") : t("voiceLearn.sampleFailed", { error: err.message }));
    } finally {
      setListening(false);
    }
  }

  function record() {
    setProblem("");
    setResult(null);
    void recorder.start();
  }

  const candidates = () => (result() ? candidatesFrom(result()!, props.word) : []);

  return (
    <div class="vl-sample">
      <p class="pg-muted vl-hint">{t("voiceLearn.sampleHint")}</p>
      <div class="vl-sample-row">
        <Show
          when={recorder.state() === "recording"}
          fallback={
            <Button disabled={recorder.state() !== "idle" || listening()} onClick={record}>
              <MicIcon /> {result() || problem() ? t("voiceLearn.sampleAgain") : t("voiceLearn.sample")}
            </Button>
          }
        >
          <Button variant="danger" onClick={() => recorder.stop()}>
            {t("voiceLearn.sampleStop")}
          </Button>
          <span class="vl-sample-status" role="status">{t("voiceLearn.sampleRecording", { seconds: Math.floor(recorder.seconds()) })}</span>
        </Show>
        <Show when={recorder.state() === "processing" || listening()}>
          <span class="vl-sample-status" role="status">{t("voiceLearn.sampleListening")}</span>
        </Show>
      </div>
      <Show when={problem()}>
        <p class="vl-error" role="alert">{problem()}</p>
      </Show>
      <Show when={result()}>
        {(r) => (
          <div class="vl-heard" role="status">
            <p>{t("voiceLearn.sampleHeard", { heard: r().heard })}</p>
            <div class="vl-sample-row">
              <For each={candidates()}>
                {(c) => (
                  <Button
                    onClick={() => {
                      props.onAdd([c]);
                      setResult(null);
                    }}
                  >
                    {t("voiceLearn.sampleAdd")}: {c}
                  </Button>
                )}
              </For>
              <Button variant="ghost" onClick={() => setResult(null)}>
                {t("voiceLearn.sampleDismiss")}
              </Button>
            </div>
          </div>
        )}
      </Show>
    </div>
  );
}
