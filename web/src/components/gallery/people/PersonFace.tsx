import { Show } from "solid-js";
import "../../../styles/gallery-people.css";

/** A person's round face thumbnail; a plain disc when the server has no crop yet. */
export function PersonFace(props: { src: string | null; class?: string }) {
  return (
    <span class={`pp-face ${props.class ?? ""}`} aria-hidden="true">
      <Show when={props.src}>{(src) => <img class="pp-face__img" src={src()} alt="" loading="lazy" draggable={false} />}</Show>
    </span>
  );
}
