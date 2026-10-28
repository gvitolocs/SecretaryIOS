import { useParams } from "@solidjs/router";
import { Show } from "solid-js";
import GalleryView from "./Gallery";

/** /gallery/person/:id: the Gallery's own grid and viewer over the photos of one person. */
export default function GalleryPersonView() {
  const params = useParams<{ id: string }>();
  // Another person is another list: start the view afresh.
  return (
    <Show when={params.id} keyed>
      {(id) => <GalleryView person={id} />}
    </Show>
  );
}
