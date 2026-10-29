import { createSignal, Show } from "solid-js";
import type { Slide } from "../lib/instagram";
import InstagramTiles from "./InstagramTiles";
import InstagramViewer from "./InstagramViewer";

// An opened highlight, story, post or Archive item: its tiles and the full-size viewer, one at a time. A highlight opens
// on its tiles; a tile opens the viewer there. The viewer's grid button and G go back to the tiles (and G there back to
// the viewer). Once the tiles have been shown, Esc or a swipe down in the viewer goes back to them; Esc on the tiles
// closes it all.

export default function InstagramOpened(props: {
  slides: Slide[];
  start: number;
  /** Open on the tiles (a highlight) instead of the viewer. */
  tiles?: boolean;
  username: string;
  title?: string;
  archived?: boolean;
  hasMore?: boolean;
  loading?: boolean;
  onClose: () => void;
  onNearEnd?: () => void;
}) {
  const [mode, setMode] = createSignal<"tiles" | "viewer">(props.tiles ? "tiles" : "viewer");
  const [index, setIndex] = createSignal(props.start);
  const [fromTiles, setFromTiles] = createSignal(!!props.tiles);
  const showTiles = () => {
    setFromTiles(true);
    setMode("tiles");
  };

  return (
    <Show
      when={mode() === "viewer"}
      fallback={
        <InstagramTiles
          slides={props.slides}
          username={props.username}
          title={props.title}
          focus={index()}
          hasMore={props.hasMore}
          loading={props.loading}
          onOpen={(i) => {
            setIndex(i);
            setMode("viewer");
          }}
          onClose={() => props.onClose()}
          onNearEnd={props.onNearEnd}
        />
      }
    >
      <InstagramViewer
        slides={props.slides}
        start={index()}
        username={props.username}
        archived={props.archived}
        onIndex={setIndex}
        onGrid={props.slides.length > 1 || fromTiles() ? showTiles : undefined}
        onClose={() => (fromTiles() ? setMode("tiles") : props.onClose())}
        onNearEnd={props.onNearEnd}
      />
    </Show>
  );
}
