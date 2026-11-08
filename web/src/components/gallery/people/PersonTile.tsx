import { A } from "@solidjs/router";
import { Show } from "solid-js";
import type { Person } from "../../../api/gallery";
import { personName } from "../../../lib/galleryPeople";
import { HideEyeIcon } from "../faces/FaceIcons";
import { InlineName } from "./InlineName";
import { PersonFace } from "./PersonFace";
import { groupText, memberPath, useGroupKind } from "../../../lib/galleryGroup";

export interface PersonTileProps {
  person: Person;
  selecting: boolean;
  selected: boolean;
  onToggle: () => void;
  onRename: (name: string | null) => Promise<void>;
  onHide: () => void;
}

/** One person on the People page: the face, the name (a click renames), the photo count, and hide / unhide. */
export function PersonTile(props: PersonTileProps) {
  const kind = useGroupKind();
  const pt = groupText(kind);
  const label = () => personName(props.person, pt("unnamed"));
  return (
    <div class="pp-tile" classList={{ "pp-tile--hidden": props.person.hidden, "pp-tile--selected": props.selected }}>
      <div class="pp-tile__face">
        <Show
          when={props.selecting}
          fallback={
            <A class="pp-open" href={memberPath(kind, props.person.id)} aria-label={pt("open", { name: label() })}>
              <PersonFace src={props.person.thumbUrl} />
            </A>
          }
        >
          <button type="button" class="pp-open" aria-pressed={props.selected} aria-label={pt("pick", { name: label() })} onClick={props.onToggle}>
            <PersonFace src={props.person.thumbUrl} />
            <span class="pp-check" aria-hidden="true">
              {props.selected ? "✓" : ""}
            </span>
          </button>
        </Show>
        <Show when={!props.selecting}>
          <button
            type="button"
            class="pp-hide"
            aria-label={props.person.hidden ? pt("unhide", { name: label() }) : pt("hide", { name: label() })}
            title={props.person.hidden ? pt("unhide", { name: label() }) : pt("hide", { name: label() })}
            onClick={props.onHide}
          >
            <HideEyeIcon off={!props.person.hidden} />
          </button>
        </Show>
      </div>
      <InlineName name={props.person.name} onSave={props.onRename} label={pt("rename", { name: label() })} />
      <span class="pp-count">
        {pt("photos", { count: props.person.itemCount })}
        <Show when={props.person.hidden}> · {pt("hidden")}</Show>
      </span>
    </div>
  );
}
