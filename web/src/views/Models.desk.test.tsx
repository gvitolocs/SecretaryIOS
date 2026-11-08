import { Route } from "@solidjs/router";
import { screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import ModelsView from "./Models";

function serve() {
  return stubFetch((c) => {
    if (c.url.startsWith("/v1/models")) {
      return {
        body: {
          models: [
            {
              id: "md1",
              title: "Crystal fox",
              prompt: "a fox",
              createdAt: "2026-10-01T12:00:00Z",
              animations: [],
              files: [{ name: "model.glb", size: 1 }],
              credits: 2,
            },
          ],
        },
      };
    }
    return { status: 404 };
  });
}

describe("desktop Models", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header and full grid card", async () => {
    serve();
    renderAt("/models", () => <Route path="/models/:id?" component={ModelsView} />);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent(/3D|Models/i);
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    expect(await screen.findByTestId("model-card")).toBeInTheDocument();
    expect(document.querySelector(".models-desk-surface")).toBeInTheDocument();
  });
});
