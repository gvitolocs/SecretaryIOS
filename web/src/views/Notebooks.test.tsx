import { Route } from "@solidjs/router";
import { fireEvent, screen, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { notebook, stubNotebooks } from "../test/notebooksFixtures";
import { renderAt } from "../test/router";
import NotebooksView from "./Notebooks";
import { PLUGINS } from "./Plugins";

const routes = () => (
  <>
    <Route path="/notebooks" component={NotebooksView} />
    <Route path="/notebooks/:id" component={() => <p>Opened a notebook</p>} />
  </>
);

describe("Notebooks list", () => {
  it("lists the notebooks with what is in them, and credits Open Notebook", async () => {
    stubNotebooks(() => ({
      body: {
        notebooks: [
          notebook({ id: "nb1", name: "Research Methods", description: "Lecture notes", counts: { sources: 3, notes: 1, chats: 2, podcasts: 0 } }),
          notebook({ id: "nb2", name: "Trip to Japan", description: "", counts: { sources: 0, notes: 0, chats: 0, podcasts: 0 }, archived: false }),
        ],
      },
    }));
    renderAt("/notebooks", routes);
    const row = await screen.findByRole("link", { name: "Research Methods" });
    expect(row).toHaveAttribute("href", "/notebooks/nb1");
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Notebooks");
    expect(screen.getByText("2 notebooks")).toHaveClass("pg-context");
    expect(screen.getByText("Lecture notes · 3 sources · 1 note · 2 chats")).toBeInTheDocument();
    expect(screen.getByText("No sources · No notes · No chats")).toBeInTheDocument();
    const credit = screen.getByRole("link", { name: "Open Notebook by Luis Novo" });
    expect(credit).toHaveAttribute("href", "https://github.com/lfnovo/open-notebook");
    expect(credit.parentElement).toHaveTextContent("Based on Open Notebook by Luis Novo (MIT).");
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
  });

  it("shows the archived ones from the Archived section", async () => {
    const calls = stubNotebooks((c) => ({
      body: { notebooks: c.query.get("archived") === "only" ? [notebook({ id: "nb3", name: "Italian cooking", archived: true })] : [notebook()] },
    }));
    renderAt("/notebooks", routes);
    await screen.findByRole("link", { name: "Research Methods" });
    expect(calls[0]!.url).toBe("/v1/notebooks");
    fireEvent.click(screen.getByRole("button", { name: "Archived" }));
    const archived = await screen.findByRole("link", { name: "Italian cooking" });
    expect(archived.closest("li")).toHaveTextContent("Archived");
    expect(calls.at(-1)!.url).toBe("/v1/notebooks?archived=only");
    expect(screen.getByText("1 archived notebook")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Research Methods" })).toBeNull();
  });

  it("explains an empty list and offers the first notebook", async () => {
    stubNotebooks(() => ({ body: { notebooks: [] } }));
    renderAt("/notebooks", routes);
    expect(await screen.findByText("No notebooks yet")).toBeInTheDocument();
    expect(screen.getByText(/gathers sources/)).toBeInTheDocument();
    expect(screen.getByText("No notebooks")).toHaveClass("pg-context");
  });

  it("creates a notebook in a sheet and opens it", async () => {
    const calls = stubNotebooks((c) => (c.method === "POST" ? { status: 201, body: notebook({ id: "nb9", name: "CNS" }) } : { body: { notebooks: [] } }));
    renderAt("/notebooks", routes);
    await screen.findByText("No notebooks yet");
    fireEvent.click(document.querySelector<HTMLButtonElement>(".pg-cta")!);
    const dialog = await screen.findByRole("dialog", { name: "New notebook" });
    const create = within(dialog).getByRole("button", { name: "Create" });
    expect(create).toBeDisabled();
    fireEvent.input(within(dialog).getByLabelText("Name"), { target: { value: "  CNS  " } });
    fireEvent.input(within(dialog).getByLabelText("Description"), { target: { value: "Neuro course" } });
    expect(create).toBeEnabled();
    fireEvent.click(create);
    expect(await screen.findByText("Opened a notebook")).toBeInTheDocument();
    const post = calls.find((c) => c.method === "POST")!;
    expect(post).toMatchObject({ url: "/v1/notebooks", body: { name: "CNS", description: "Neuro course" } });
  });

  it("keeps the sheet and says why when creating fails", async () => {
    stubNotebooks((c) => (c.method === "POST" ? { status: 400, body: { error: { code: "bad_request", message: "name: too long" } } } : { body: { notebooks: [notebook()] } }));
    renderAt("/notebooks", routes);
    await screen.findByRole("link", { name: "Research Methods" });
    fireEvent.click(document.querySelector<HTMLButtonElement>(".pg-cta")!);
    const dialog = await screen.findByRole("dialog");
    fireEvent.input(within(dialog).getByLabelText("Name"), { target: { value: "x" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("name: too long");
    expect(screen.queryByText("Opened a notebook")).toBeNull();
  });

  it("says so when the list cannot be loaded, and tries again", async () => {
    let fail = true;
    const calls = stubNotebooks(() => (fail ? { status: 500, body: {} } : { body: { notebooks: [notebook()] } }));
    renderAt("/notebooks", routes);
    expect(await screen.findByRole("alert")).toHaveTextContent("The server had a problem");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByRole("link", { name: "Research Methods" });
    expect(calls).toHaveLength(2);
  });

  it("has a card on the Plugins page", () => {
    const entry = PLUGINS.find((p) => p.id === "notebooks");
    expect(entry?.href).toBe("/notebooks");
    expect(entry?.title()).toBe("Notebooks");
  });

  it("speaks Italian", async () => {
    const { setLocale } = await import("../i18n");
    setLocale("it");
    stubNotebooks(() => ({ body: { notebooks: [notebook()] } }));
    renderAt("/notebooks", routes);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Quaderni");
    expect(screen.getByRole("button", { name: "Nuovo quaderno" })).toBeInTheDocument();
    expect(await screen.findByText("1 quaderno")).toBeInTheDocument();
  });
});
