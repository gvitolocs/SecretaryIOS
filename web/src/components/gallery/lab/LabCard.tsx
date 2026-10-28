import { A } from "@solidjs/router";
import { Match, Show, Switch } from "solid-js";
import { type LabJob } from "../../../api/gallery";
import { locale, t } from "../../../i18n";
import { progressLine, statusKey, type LabLine } from "../../../lib/lab";
import { thumbHashToImage } from "../../../lib/thumbhash";
import { CompareSlider } from "./CompareSlider";
import "../../../styles/gallery-viewer.css";
import "../../../styles/gallery-lab.css";
import { LabIcon } from "./LabIcons";

export interface LabCardProps {
  job: LabJob;
  /** An action on this job is running. */
  busy: "accept" | "discard" | "retry" | "cancel" | null;
  /** A short message about the last action on this job. */
  error: string | null;
  onAccept: (job: LabJob) => void;
  onDiscard: (job: LabJob) => void;
  onRetry: (job: LabJob) => void;
  /** Cancel a queued job, or remove a failed one. */
  onRemove: (job: LabJob) => void;
}

function lineText(l: LabLine): string {
  switch (l.key) {
    case "lab.queuePosition":
      return t(l.key, l.params);
    case "lab.generating":
      return t(l.key, l.params);
    default:
      return t(l.key);
  }
}

function when(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  try {
    return new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeStyle: "short" }).format(d);
  } catch {
    return d.toLocaleString();
  }
}

/**
 * One edit in the Lab: its picture (before and after once done), the request, where it is, and what can be done with it:
 * Accept (a new picture in the library; the original stays) and Discard for a finished one, Retry for a failed one,
 * Cancel for one still in the queue.
 */
export function LabCard(props: LabCardProps) {
  const job = () => props.job;
  const ratio = () => {
    const j = job();
    return j.width && j.height ? j.width / j.height : null;
  };
  const placeholder = () => thumbHashToImage(job().original?.thumbhash ?? undefined)?.url ?? null;
  const line = () => progressLine(job());
  const idle = () => props.busy === null;

  return (
    <li class="lab-card" classList={{ [`lab-card--${job().status}`]: true }} data-status={job().status} data-job={job().id}>
      <div class="lab-card-head">
        <span class="lab-badge" classList={{ [`lab-badge--${job().status}`]: true }}>
          {t(statusKey(job().status))}
        </span>
        <Show when={job().createdAt}>
          <span class="lab-when">{t("lab.createdAt", { when: when(job().createdAt) })}</span>
        </Show>
      </div>

      <div class="lab-media">
        <Switch>
          <Match when={job().status === "done" && job().urls.before && job().urls.after}>
            <CompareSlider
              before={job().urls.before as string}
              after={job().urls.after as string}
              beforeLabel={t("lab.before")}
              afterLabel={t("lab.after")}
              label={t("lab.compare")}
              ratio={ratio()}
            />
          </Match>
          <Match when={job().status === "accepted" && job().urls.after}>
            <img class="lab-single" src={job().urls.after as string} alt={t("lab.after")} style={{ "aspect-ratio": ratio() ? String(ratio()) : "4 / 3" }} />
          </Match>
          <Match when={job().urls.before}>
            <div class="lab-pending" style={{ "aspect-ratio": ratio() ? String(ratio()) : "4 / 3", "background-image": placeholder() ? `url(${placeholder()})` : undefined }}>
              <img class="lab-single" classList={{ "lab-dim": job().status !== "failed" }} src={job().urls.before as string} alt={t("lab.before")} />
              <Show when={line()}>
                {(l) => (
                  <div class="lab-progress" role="status">
                    <span class="lab-progress-text">{lineText(l())}</span>
                    <Show when={job().status === "running" && job().phase === "generating"}>
                      <progress class="lab-bar" max="100" value={job().progress} aria-label={t("lab.statusRunning")} />
                    </Show>
                  </div>
                )}
              </Show>
            </div>
          </Match>
          <Match when={true}>
            <p class="lab-gone">{t("lab.originalGone")}</p>
          </Match>
        </Switch>
      </div>

      <div class="lab-body">
        <p class="lab-prompt-heading">{t("lab.promptHeading")}</p>
        <p class="lab-prompt-text">{job().prompt}</p>
        <Show when={job().referenceItemId}>
          <p class="lab-ref-line">
            <Show when={job().urls.reference}>{(src) => <img class="lab-ref-mini" src={src()} alt="" />}</Show>
            <span>{t("lab.withReference")}</span>
          </p>
        </Show>
        <Show when={job().status === "failed" && job().error}>
          {(reason) => (
            <p class="gv-error" role="alert">
              {t("lab.failedReason", { reason: reason() })}
            </p>
          )}
        </Show>
        <Show when={job().status === "accepted"}>
          <p class="lab-note">{t("lab.acceptedNote")}</p>
        </Show>
        <Show when={props.error}>
          {(message) => (
            <p class="gv-error" role="alert">
              {message()}
            </p>
          )}
        </Show>
      </div>

      <div class="lab-actions">
        <Switch>
          <Match when={job().status === "done"}>
            <button type="button" class="btn btn--primary btn--small" disabled={!idle()} onClick={() => props.onAccept(job())}>
              {props.busy === "accept" ? t("lab.accepting") : t("lab.accept")}
            </button>
            <button type="button" class="btn btn--small" title={t("lab.discardHint")} disabled={!idle()} onClick={() => props.onDiscard(job())}>
              {props.busy === "discard" ? t("lab.discarding") : t("lab.discard")}
            </button>
          </Match>
          <Match when={job().status === "failed"}>
            <button type="button" class="btn btn--primary btn--small" disabled={!idle()} onClick={() => props.onRetry(job())}>
              {props.busy === "retry" ? t("lab.retrying") : t("lab.retry")}
            </button>
            <button type="button" class="btn btn--small" disabled={!idle()} onClick={() => props.onRemove(job())}>
              {t("lab.removeJob")}
            </button>
          </Match>
          <Match when={job().status === "queued"}>
            <button type="button" class="btn btn--small" disabled={!idle()} onClick={() => props.onRemove(job())}>
              {t("lab.cancelJob")}
            </button>
          </Match>
          <Match when={job().status === "accepted"}>
            <A class="btn btn--small" href="/gallery">
              <LabIcon />
              {t("lab.openInGallery")}
            </A>
          </Match>
        </Switch>
      </div>
    </li>
  );
}
