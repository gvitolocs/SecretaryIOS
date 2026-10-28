import { useParams } from "@solidjs/router";
import { Show } from "solid-js";
import GalleryView from "./Gallery";

/** /gallery/album/:id: the Gallery's own grid and viewer over one album (Lucina's chat links here). */
export default function GalleryAlbumView() {
  const params = useParams<{ id: string }>();
  // Another album is another list: start the view afresh.
  return (
    <Show when={params.id} keyed>
      {(id) => <GalleryView album={id} />}
    </Show>
  );
}
