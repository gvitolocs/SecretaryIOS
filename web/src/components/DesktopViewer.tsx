import { createMemo, Show } from "solid-js";
import { uploadToLucina } from "../api/mediaUpload";
import { desktopDevice, desktopSession, lucinaViewer, replaceShown, setDesktopIndex, type OpenFile } from "../lib/desktopViewer";
import { PhotoViewer, type ViewerItem } from "./PhotoViewer";

const HEIC = /image\/hei[cf]/i;
const HEIC_NAME = /\.hei[cf]$/i;
const VIDEO_NAME = /\.(mov|mp4|m4v|webm)$/i;

async function bytesOf(file: OpenFile): Promise<Blob> {
  if (file.blob) return file.blob;
  const res = await fetch(file.url ?? file.src);
  if (!res.ok) throw new Error(`read ${res.status}`);
  return res.blob();
}

/**
 * The viewer the desktop shell opens (`window.lucinaViewer`, lib/desktopViewer.ts): local
 * files in the gallery's PhotoViewer, with "Save to Lucina" as this Mac. Mounted once per page.
 */
export function DesktopViewerHost() {
  const converting = new Set<number>();
  const items = createMemo<ViewerItem[]>(() =>
    (desktopSession()?.files ?? []).map((f) => ({
      src: f.src,
      name: f.name,
      mime: f.mime,
      blob: f.blob,
      kind: f.mime?.startsWith("video/") || VIDEO_NAME.test(f.name) ? "video" : "photo",
    })),
  );

  // Browsers other than Safari can't show HEIC: show a JPEG copy, save the original.
  const onImageError = async (index: number) => {
    const file = desktopSession()?.files[index];
    if (!file || converting.has(index) || !(HEIC.test(file.mime ?? "") || HEIC_NAME.test(file.name))) return;
    converting.add(index);
    try {
      const { default: convert } = await import("heic2any");
      const result = await convert({ blob: await bytesOf(file), toType: "image/jpeg", quality: 0.92 });
      replaceShown(index, Array.isArray(result) ? result[0]! : result);
    } catch {
      // Left broken; "Open with Preview" in the shell still works.
    }
  };

  const save = async (index: number, onProgress: (sent: number, total: number) => void) => {
    const s = desktopSession();
    const file = s?.files[index];
    const device = desktopDevice();
    if (!file || !device) throw new Error("nothing to save");
    return uploadToLucina(await bytesOf(file), {
      name: file.name,
      device,
      takenAt: file.takenAt,
      assetId: file.assetId,
      onProgress,
    });
  };

  return (
    <Show when={desktopSession()}>
      {(s) => (
        <PhotoViewer
          items={items()}
          index={s().index}
          onIndex={setDesktopIndex}
          onClose={() => lucinaViewer.close()}
          onImageError={(i) => void onImageError(i)}
          actions={{
            share: true,
            saveToLucina: desktopDevice() ? (_item, onProgress) => save(s().index, onProgress) : undefined,
          }}
        />
      )}
    </Show>
  );
}
