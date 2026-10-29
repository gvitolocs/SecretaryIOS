import { Route } from "@solidjs/router";
import { screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import Desk from "./Desk";

const desk = {
  mode: "default",
  pluginInstalled: true,
  model: { provider: "google", default: "gemini-2.0-flash" },
  backends: [{ name: "default", label: "Parent model", available: true, reason: "" }],
  modelRoutes: [],
  defaultRoute: null,
  profileRoutes: [],
  jobs: [],
  updatedAt: new Date().toISOString(),
  sandbox: "/home/lucina",
  androidSandbox: "android",
  plugins: [],
};

function serve() {
  return stubFetch((c) => {
    if (c.url === "/v1/hermes" && c.method === "GET") return { body: desk };
    return { status: 404 };
  });
}

describe("desktop My desk", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header and desk cards", async () => {
    serve();
    renderAt("/desk", () => <Route path="/desk" component={Desk} />);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("My desk");
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    expect(await screen.findByRole("heading", { level: 2, name: "Hermes Agent" })).toBeInTheDocument();
    expect(document.querySelector(".desk-head")).toBeNull();
  });
});
