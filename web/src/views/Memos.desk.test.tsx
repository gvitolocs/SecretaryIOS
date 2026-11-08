import { Route } from "@solidjs/router";
import { screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import MemosView from "./Memos";

function serve() {
  return stubFetch((c) => {
    if (c.url.startsWith("/v1/memos?")) {
      return {
        body: {
          memos: [
            {
              id: "m1",
              clientId: "c1",
              title: "Standup notes",
              recordedAt: "2026-10-28T10:00:00Z",
              durationSec: 120,
              sizeBytes: 1000,
              receivedBytes: 1000,
              status: "done",
              preview: "Ship the page system",
              language: "en",
            },
          ],
        },
      };
    }
    return { status: 404 };
  });
}

describe("desktop Memos", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header, split list and pick-one empty state", async () => {
    serve();
    renderAt("/memos", () => <Route path="/memos/:id?" component={MemosView} />);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Memos");
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    expect(await screen.findByRole("link", { name: "Standup notes" })).toHaveAttribute("href", "/memos/m1");
    expect(screen.getByText("Pick a memo from the list.")).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Memos" })).toBeInTheDocument();
    const list = screen.getByRole("navigation", { name: "Memos" });
    expect(within(list).getByRole("link", { name: "Standup notes" })).toBeInTheDocument();
  });
});
