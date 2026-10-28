import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { fireEvent, render, screen } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { Markdown } from "./Markdown";

describe("Markdown: [note:<slug>] citations", () => {
  it("renders a link into Keep that opens in the app, not in a new tab", () => {
    render(() => <Markdown text="I saved it: [note:gift-ideas] and see [site](https://x.org)." />);
    const note = screen.getByRole("link", { name: "note:gift-ideas" });
    expect(note).toHaveAttribute("href", "/keep/note/gift-ideas");
    expect(note).not.toHaveAttribute("target");
    // Other links still open elsewhere.
    expect(screen.getByRole("link", { name: "site" })).toHaveAttribute("target", "_blank");
  });

  it("moves the router to the note on a click", async () => {
    const history = createMemoryHistory();
    history.set({ value: "/", scroll: false, replace: true });
    render(() => (
      <MemoryRouter history={history}>
        <Route path="/" component={() => <Markdown text="[note:gift-ideas]" />} />
        <Route path="/keep/note/:id" component={() => <p>Keep opened</p>} />
      </MemoryRouter>
    ));
    fireEvent.click(screen.getByRole("link", { name: "note:gift-ideas" }));
    expect(await screen.findByText("Keep opened")).toBeInTheDocument();
  });
});
