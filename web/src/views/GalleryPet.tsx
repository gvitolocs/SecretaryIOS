import { useParams } from "@solidjs/router";
import { Show } from "solid-js";
import GalleryView from "./Gallery";

/** /gallery/pet/:id: the Gallery's own grid and viewer over the photos of one pet. */
export default function GalleryPetView() {
  const params = useParams<{ id: string }>();
  // Another pet is another list: start the view afresh.
  return (
    <Show when={params.id} keyed>
      {(id) => <GalleryView pet={id} desktop />}
    </Show>
  );
}
