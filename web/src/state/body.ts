import { createStore, reconcile } from "solid-js/store";
import {
  getBodyState,
  sendBodyCommand,
  type BodyCommand,
  type BodyResponse,
  type BodyView,
} from "../api/body";
import { createEngineChannel, currentTimeZone } from "./engine";

interface BodyStoreState {
  view: BodyView | null;
  loaded: boolean;
  loadError: string | null;
  actionError: string | null;
}

const [body, setBody] = createStore<BodyStoreState>({
  view: null,
  loaded: false,
  loadError: null,
  actionError: null,
});

export { body };

const channel = createEngineChannel<BodyView, BodyCommand, BodyResponse>({
  getState: () => getBodyState(currentTimeZone()),
  send: sendBodyCommand,
  revisionOf: (view) => view.revision,
  onView: (view) => {
    setBody("view", reconcile(view));
    setBody({ loaded: true, loadError: null });
  },
  onLoadError: (message) => setBody("loadError", message),
  onActionError: (message) => setBody("actionError", message),
});

export function refreshBody(): Promise<void> {
  return channel.refresh().then(() => undefined);
}

/** The returned promise is the per-action pending signal for views. */
export function runBody(command: BodyCommand): Promise<BodyResponse | null> {
  setBody("actionError", null);
  return channel.run(command);
}

export function dismissBodyError(): void {
  setBody("actionError", null);
}

export function resetBody(): void {
  channel.reset();
  setBody({ view: null, loaded: false, loadError: null, actionError: null });
}
