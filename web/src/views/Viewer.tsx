import { t } from "../i18n";
import "../styles/viewer.css";

/**
 * /viewer: the page of a Mac viewer window. Secretary Code loads it, then calls
 * `window.lucinaViewer.open(...)` (lib/desktopViewer.ts); the viewer covers this page.
 */
export default function ViewerView() {
  return (
    <main class="viewer-page">
      <p>{t("viewer.empty")}</p>
    </main>
  );
}
