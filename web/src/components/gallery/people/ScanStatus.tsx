import { Show } from "solid-js";
import type { FaceSettings } from "../../../api/gallery";
import { scanProgress } from "../../../lib/galleryPeople";
import "../../../styles/gallery-people.css";
import { groupText, useGroupKind } from "../../../lib/galleryGroup";

/** How far the face scan has got: a bar while photos are waiting (with the reason when it is paused), one line when done. */
export function ScanStatus(props: { settings: FaceSettings }) {
  const kind = useGroupKind();
  const pt = groupText(kind);
  const progress = () => scanProgress(props.settings);
  const line = () => {
    const p = progress();
    const params = { scanned: p.scanned, pending: p.pending };
    switch (p.phase) {
      case "hot":
        return pt("scanHot", params);
      case "busy":
        return pt("scanBusy", params);
      case "scanning":
        return pt("scanning", params);
      default:
        return pt("scanDone", params);
    }
  };
  return (
    <div class="pp-scan" classList={{ "pp-scan--paused": progress().phase === "hot" || progress().phase === "busy" }} role="status">
      <Show when={progress().pending > 0}>
        <div class="pp-bar" role="progressbar" aria-label={pt("scanProgress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress().percent}>
          <span class="pp-bar__fill" style={{ width: `${progress().percent}%` }} />
        </div>
      </Show>
      <p class="pp-scan__line">{line()}</p>
    </div>
  );
}
