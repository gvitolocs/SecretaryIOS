import { A } from "@solidjs/router";
import { createSignal, For, Match, Show, Switch } from "solid-js";
import { createLabJob, LAB_PROMPT_MAX, type GalleryItem, type LabJob } from "../../../api/gallery";
import { ApiError } from "../../../api/client";
import { t } from "../../../i18n";
import { LAB_SUGGESTIONS, promptValid, withSuggestion } from "../../../lib/lab";
import "../../../styles/gallery-lab.css";
import { Modal } from "../Modal";
import { ReferencePicker } from "./ReferencePicker";

export interface LabSheetProps {
  /** The picture to edit. */
  item: GalleryItem;
  onClose: () => void;
  /** The job was queued. */
  onQueued?: (job: LabJob) => void;
}

type Step = "form" | "pick" | "sent";

/**
 * "Edit with Lucina…": a prompt, an optional reference photo from the Gallery and a few ideas. Sending queues an edit on
 * the Lab (a model on the owner's own computer, one job at a time, in the background); the sheet then says so and links
 * to the Lab. Nothing here waits for the result.
 */
export function LabSheet(props: LabSheetProps) {
  const [step, setStep] = createSignal<Step>("form");
  const [prompt, setPrompt] = createSignal("");
  const [reference, setReference] = createSignal<GalleryItem | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let field: HTMLTextAreaElement | undefined;

  const count = () => [...prompt()].length;
  const canSend = () => promptValid(prompt()) && !busy();

  const send = async (e: SubmitEvent) => {
    e.preventDefault();
    if (!canSend()) return;
    setBusy(true);
    setError(null);
    try {
      const job = await createLabJob(props.item.id, { prompt: prompt(), referenceItemId: reference()?.id ?? null });
      props.onQueued?.(job);
      setStep("sent");
    } catch (err) {
      setError(err instanceof ApiError && err.status === 429 ? t("lab.queueFull") : t("lab.createFailed"));
    } finally {
      setBusy(false);
    }
  };

  const suggest = (text: string) => {
    setPrompt(withSuggestion(prompt(), text).slice(0, LAB_PROMPT_MAX));
    field?.focus();
  };

  return (
    <Modal labelledBy="lab-sheet-title" busy={busy()} onClose={props.onClose} initialFocus={() => field}>
      <Switch>
        <Match when={step() === "pick"}>
          <ReferencePicker
            exclude={props.item.id}
            onPick={(item) => {
              setReference(item);
              setStep("form");
            }}
            onBack={() => setStep("form")}
          />
        </Match>
        <Match when={step() === "sent"}>
          <h2 class="gv-dialog-title" id="lab-sheet-title">
            {t("lab.sheetTitle")}
          </h2>
          <p class="gv-dialog-body" role="status">
            {t("lab.queued")}
          </p>
          <div class="gv-dialog-actions">
            <button type="button" class="btn" onClick={() => props.onClose()}>
              {t("lab.dismiss")}
            </button>
            <A class="btn btn--primary" href="/gallery/lab" onClick={() => props.onClose()}>
              {t("lab.openLab")}
            </A>
          </div>
        </Match>
        <Match when={step() === "form"}>
          <form class="lab-form" onSubmit={(e) => void send(e)}>
            <h2 class="gv-dialog-title" id="lab-sheet-title">
              {t("lab.sheetTitle")}
            </h2>
            <p class="gv-dialog-sub">{t("lab.sheetIntro")}</p>
            <label class="lab-label" for="lab-prompt">
              {t("lab.promptLabel")}
            </label>
            <textarea
              id="lab-prompt"
              ref={field}
              class="text-input gv-textarea lab-prompt"
              rows={3}
              maxLength={LAB_PROMPT_MAX}
              value={prompt()}
              placeholder={t("lab.promptPlaceholder")}
              disabled={busy()}
              onInput={(e) => setPrompt(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void send(e as unknown as SubmitEvent);
              }}
            />
            <p class="lab-count" aria-hidden="true">
              {t("lab.promptCount", { count: count(), max: LAB_PROMPT_MAX })}
            </p>
            <p class="lab-label" id="lab-ideas">
              {t("lab.suggestionsLabel")}
            </p>
            <ul class="lab-chips" aria-labelledby="lab-ideas">
              <For each={LAB_SUGGESTIONS}>
                {(key) => (
                  <li>
                    <button type="button" class="lab-chip" disabled={busy()} onClick={() => suggest(t(key))}>
                      {t(key)}
                    </button>
                  </li>
                )}
              </For>
            </ul>
            <p class="lab-label">{t("lab.referenceLabel")}</p>
            <Show
              when={reference()}
              fallback={
                <div class="lab-ref">
                  <button type="button" class="btn btn--small" disabled={busy()} onClick={() => setStep("pick")}>
                    {t("lab.referencePick")}
                  </button>
                  <span class="lab-hint">{t("lab.referenceHint")}</span>
                </div>
              }
            >
              {(ref) => (
                <div class="lab-ref">
                  <img class="lab-ref-thumb" src={ref().urls.thumb ?? ref().urls.large ?? ""} alt={ref().title} />
                  <div class="lab-ref-actions">
                    <button type="button" class="btn btn--small" disabled={busy()} onClick={() => setStep("pick")}>
                      {t("lab.referenceChange")}
                    </button>
                    <button type="button" class="btn btn--small" disabled={busy()} onClick={() => setReference(null)}>
                      {t("lab.referenceRemove")}
                    </button>
                  </div>
                </div>
              )}
            </Show>
            <Show when={error()}>
              {(message) => (
                <p class="gv-error" role="alert">
                  {message()}
                </p>
              )}
            </Show>
            <div class="gv-dialog-actions">
              <button type="button" class="btn" disabled={busy()} onClick={() => props.onClose()}>
                {t("lab.cancel")}
              </button>
              <button type="submit" class="btn btn--primary" disabled={!canSend()}>
                {busy() ? t("lab.sending") : t("lab.send")}
              </button>
            </div>
          </form>
        </Match>
      </Switch>
    </Modal>
  );
}
