import { Route } from "@solidjs/router";
import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import { renderAt } from "../../test/router";
import { Button, Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageGrid, PageHeader, PrimaryAction, RowList, SectionNav, Sheet } from ".";

describe("page system", () => {
  it("renders the header row: title, context, sections and one accent action", () => {
    const onStart = vi.fn();
    renderAt("/a", () => (
      <Route
        path="/a"
        component={() => (
          <Page>
            <PageHeader
              title="Training"
              context="Wednesday, October 28"
              nav={
                <SectionNav
                  label="Sections"
                  items={[
                    { href: "/a", label: "Overview", current: true },
                    { href: "/b", label: "Journal", current: false },
                    { href: "/c", label: "Settings", current: false, icon: <svg /> },
                  ]}
                />
              }
              action={<PrimaryAction onClick={onStart}>Start</PrimaryAction>}
            />
          </Page>
        )}
      />
    ));
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Training");
    expect(screen.getByText("Wednesday, October 28")).toHaveClass("pg-context");
    const nav = screen.getByRole("navigation", { name: "Sections" });
    const links = within(nav).getAllByRole("link");
    expect(links.map((a) => a.getAttribute("aria-label") ?? a.textContent)).toEqual(["Overview", "Journal", "Settings"]);
    expect(links[0]).toHaveAttribute("aria-current", "page");
    expect(links[1]).not.toHaveAttribute("aria-current");
    // An icon item is named by its label.
    expect(links[2]).toHaveAttribute("title", "Settings");
    fireEvent.click(screen.getByRole("button", { name: "Start" }));
    expect(onStart).toHaveBeenCalledTimes(1);
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
  });

  it("makes section items without a route into buttons", () => {
    const onSelect = vi.fn();
    render(() => (
      <SectionNav
        label="Views"
        items={[
          { label: "Grid", current: true, onSelect },
          { label: "List", current: false, onSelect },
        ]}
      />
    ));
    const buttons = within(screen.getByRole("navigation", { name: "Views" })).getAllByRole("button");
    expect(buttons[0]).toHaveAttribute("aria-current", "page");
    fireEvent.click(buttons[1]!);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it("lays out a main column with a rail, or a single column", () => {
    const { container, unmount } = render(() => (
      <PageGrid rail={<p>Rail</p>} railLabel="Details">
        <p>Main</p>
      </PageGrid>
    ));
    expect(container.querySelector(".pg-grid--rail")).not.toBeNull();
    expect(screen.getByRole("complementary", { name: "Details" })).toHaveTextContent("Rail");
    unmount();
    const single = render(() => (
      <PageGrid>
        <p>Only</p>
      </PageGrid>
    ));
    expect(single.container.querySelector(".pg-grid--single")).not.toBeNull();
    expect(screen.queryByRole("complementary")).toBeNull();
  });

  it("names a card's region by its title and lists rows edge to edge", () => {
    render(() => (
      <Card list title="Routines" actions={<Button variant="ghost">New routine</Button>}>
        <RowList label="Routine list">
          <ListRow title="Push" badge="Next" sub="Bench Press · Overhead Press" meta="Last done 2 days ago" trailing={<Button>⋯</Button>} />
          <ListRow title="Pull" href="/gym/history/1" />
        </RowList>
      </Card>
    ));
    const card = screen.getByRole("region", { name: "Routines" });
    expect(card).toHaveClass("pg-card", "pg-card--list");
    expect(within(card).getByRole("button", { name: "New routine" })).toHaveClass("pg-btn", "pg-btn--ghost");
    const rows = within(screen.getByRole("list", { name: "Routine list" })).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]!).getByText("Next")).toHaveClass("pg-badge");
    expect(within(rows[0]!).getByText("Last done 2 days ago")).toHaveClass("pg-row-meta");
    expect(within(rows[1]!).getByRole("link", { name: "Pull" })).toHaveAttribute("href", "/gym/history/1");
  });

  it("shows empty, loading and error states with one action", () => {
    const retry = vi.fn();
    render(() => (
      <>
        <EmptyState title="Nothing yet" text="Your workouts show up here." action={<button type="button">Start</button>} />
        <LoadingState label="Loading" />
        <ErrorState message="The server is down" retryLabel="Try again" onRetry={retry} />
      </>
    ));
    expect(screen.getByText("Nothing yet")).toHaveClass("pg-empty-title");
    expect(screen.getByText("Your workouts show up here.")).toHaveClass("pg-empty-text");
    expect(screen.getByRole("status")).toHaveTextContent("Loading");
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("The server is down");
    fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("opens a sheet as a modal dialog that Escape, the backdrop and Close dismiss", async () => {
    const [open, setOpen] = createSignal(false);
    const onClose = vi.fn(() => setOpen(false));
    render(() => (
      <>
        <button type="button" onClick={() => setOpen(true)}>
          Add
        </button>
        <Sheet open={open()} onClose={onClose} title="New measurement" tools={<input aria-label="Search" />} footer={<button type="button">Save</button>}>
          <p>Body</p>
        </Sheet>
      </>
    ));
    expect(screen.queryByRole("dialog")).toBeNull();
    const opener = screen.getByRole("button", { name: "Add" });
    opener.focus();
    fireEvent.click(opener);
    const dialog = screen.getByRole("dialog", { name: "New measurement" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    // Focus moves to the first control in the tools.
    await waitFor(() => expect(document.activeElement).toBe(within(dialog).getByRole("textbox", { name: "Search" })));
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("dialog")).toBeNull();
    // Focus goes back to the opener.
    expect(document.activeElement).toBe(opener);

    fireEvent.click(opener);
    fireEvent.click(document.querySelector(".pg-sheet-overlay")!);
    expect(onClose).toHaveBeenCalledTimes(2);
    fireEvent.click(opener);
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledTimes(3);
  });
});
