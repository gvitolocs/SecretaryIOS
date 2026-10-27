import { createMemo, createResource } from "solid-js";
import { previewUrl, type GalleryItem } from "../../api/gallery";
import { supportsJxl } from "../../lib/jxl";
import { PhotoViewer, type ViewerItem } from "../PhotoViewer";

export interface GalleryViewerProps {
  items: GalleryItem[];
  index: number;
  onIndex: (index: number) => void;
  onClose: () => void;
  onChanged: (item: GalleryItem) => void;
  onRemoved: (id: string) => void;
  trash?: boolean;
}

/** The Gallery's photos in the shared PhotoViewer (the viewer builder adds the info panel and the actions). */
export function GalleryViewer(props: GalleryViewerProps) {
  const [jxl] = createResource(supportsJxl);
  const items = createMemo<ViewerItem[]>(() =>
    props.items.map((item) => ({
      src: previewUrl(item, jxl() ?? false),
      original: item.urls.original,
      name: item.title,
      mime: item.mime,
      kind: item.kind,
    })),
  );
  return <PhotoViewer items={items()} index={props.index} onIndex={props.onIndex} onClose={props.onClose} actions={{ share: true }} />;
}
