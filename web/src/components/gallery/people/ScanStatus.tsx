import { Show } from "solid-js";
import type { FaceSettings } from "../../../api/gallery";
import { t } from "../../../i18n";
import { scanProgress } from "../../../lib/galleryPeople";
import "../../../styles/gallery-people.css";

/** How far the face scan has got: a bar while photos are waiting (with the reason when it is paused), one line when done. */
export function ScanStatus(props: { settings: FaceSettings }) {
  const progress = () => scanProgress(props.settings);
  const line = () => {
    const p = progress();
    const params = { scanned: p.scanned, pending: p.pending };
    switch (p.phase) {
      case "hot":
        return t("galleryPeople.scanHot", params);
      case "busy":
        return t("galleryPeople.scanBusy", params);
      case "scanning":
        return t("galleryPeople.scanning", params);
      default:
        return t("galleryPeople.scanDone", params);
    }
  };
  return (
    <div class="pp-scan" classList={{ "pp-scan--paused": progress().phase === "hot" || progress().phase === "busy" }} role="status">
      <Show when={progress().pending > 0}>
        <div class="pp-bar" role="progressbar" aria-label={t("galleryPeople.scanProgress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress().percent}>
          <span class="pp-bar__fill" style={{ width: `${progress().percent}%` }} />
        </div>
      </Show>
      <p class="pp-scan__line">{line()}</p>
    </div>
  );
}
