import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { fireEvent, render, screen } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
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

describe("Markdown: Notebooks citations", () => {
  const text = "Reliable means consistent [source:s1-3] and valid means meaningful [source:s1-5], again [source:s1-3] and [insight:i9].";
  const citations = [
    { ref: "source:s1-3", title: "RM Class 2", excerpt: "Consistency.", page: 7 },
    { ref: "source:s1-5", title: "RM Class 2", excerpt: "Meaning." },
  ];

  it("shows numbered chips: one number per citation, in order of first use, the same number for a repeat", () => {
    render(() => <Markdown text={text} citations={citations} onCite={() => {}} />);
    const chips = screen.getAllByRole("button");
    expect(chips.map((c) => c.textContent)).toEqual(["1", "2", "1", "3"]);
    expect(chips[0]).toHaveAttribute("aria-label", "Citation 1: RM Class 2");
    expect(chips[0]).toHaveAttribute("title", expect.stringContaining("p. 7"));
    expect(chips[3]).toHaveAttribute("aria-label", "Citation 3: insight:i9");
    expect(chips[3]).toHaveAttribute("data-kind", "insight");
  });

  it("calls onCite with the ref of the chip that was clicked", () => {
    const open = vi.fn();
    render(() => <Markdown text={text} citations={citations} onCite={open} />);
    fireEvent.click(screen.getAllByRole("button")[1]!);
    expect(open).toHaveBeenCalledWith("source:s1-5");
  });

  it("leaves them as plain text where nothing can open them", () => {
    render(() => <Markdown text="Said so [source:s1-3]." />);
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.getByText("Said so [source:s1-3].")).toBeInTheDocument();
  });
});
