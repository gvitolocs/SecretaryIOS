import { createSignal } from "solid-js";
import { t } from "../../i18n";
import { dragHasFiles, imageFiles, pastedImages, type PickedImages } from "../../lib/keepPaste";
import { showSnackbar } from "../../state/keep";

/** Paste and drop handlers that hand a note's images to `onImages`, plus `over` for the drop highlight. */
export function imageTarget(onImages: (files: File[]) => void, enabled: () => boolean = () => true) {
  const [over, setOver] = createSignal(false);
  let depth = 0;
  const take = (p: PickedImages) => {
    if (p.files.length) onImages(p.files);
    if (p.rejected) showSnackbar(t("keep.imageUnsupported"));
  };
  const handlers = {
    onPaste: (e: ClipboardEvent) => {
      if (!enabled()) return;
      const p = pastedImages(e.clipboardData);
      if (!p.files.length && !p.rejected) return;
      e.preventDefault();
      take(p);
    },
    onDragEnter: (e: DragEvent) => {
      if (!enabled() || !dragHasFiles(e.dataTransfer)) return;
      e.preventDefault();
      depth++;
      setOver(true);
    },
    onDragOver: (e: DragEvent) => {
      if (!enabled() || !dragHasFiles(e.dataTransfer)) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    },
    onDragLeave: (e: DragEvent) => {
      if (!dragHasFiles(e.dataTransfer)) return;
      depth = Math.max(0, depth - 1);
      if (!depth) setOver(false);
    },
    onDrop: (e: DragEvent) => {
      if (!dragHasFiles(e.dataTransfer)) return;
      e.preventDefault();
      e.stopPropagation();
      depth = 0;
      setOver(false);
      if (enabled()) take(imageFiles(e.dataTransfer));
    },
  };
  return { over, handlers, take };
}
