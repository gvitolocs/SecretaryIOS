import { createMemoryHistory, MemoryRouter } from "@solidjs/router";
import { render } from "@solidjs/testing-library";
import type { JSX } from "solid-js";

/** Render route definitions inside a memory router starting at `path`. */
export function renderAt(path: string, routes: () => JSX.Element) {
  const history = createMemoryHistory();
  history.set({ value: path, scroll: false, replace: true });
  return render(() => <MemoryRouter history={history}>{routes()}</MemoryRouter>);
}
