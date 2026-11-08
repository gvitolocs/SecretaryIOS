import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { render, screen, waitFor } from "@solidjs/testing-library";
import type { JSX } from "solid-js";
import { expect, it, vi } from "vitest";
import { putItems } from "./lib/chatdb";
import { clearChat, setChatDevice } from "./state/chat";
import { setSessionForTests } from "./state/session";
import { deferred, stubFetch } from "./test/fetch";
import App from "./App";
import ChatView from "./views/Chat";
vi.mock("./views/Shell", () => ({ default: (props: { children?: JSX.Element }) => <main>{props.children}</main> }));
it("renders the previous device's cached chat while the session request is pending", async () => {
 clearChat(); setChatDevice("cached-device"); setSessionForTests({ status: "loading" });
 await putItems([{ id: "1", ts: Date.now(), channel: "web", role: "lucina", text: "Already on disk", meta: { device: "cached-device" } }]);
 const gate = deferred<void>();
 stubFetch(async c => {
   await gate.promise;
   return { body: c.url === "/v1/session" ? { authenticated: true, deviceId: "cached-device" } : { items: [], cursor: "1", hasMore: false } };
 });
 const history = createMemoryHistory();
 render(() => <MemoryRouter history={history} root={App}><Route path="/" component={ChatView} /></MemoryRouter>);
 expect(await screen.findByText("Already on disk")).toBeInTheDocument();
 gate.resolve();
 await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
});
