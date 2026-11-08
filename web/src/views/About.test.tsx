import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { Fact } from "../api/about";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import About from "./About";

const ID = "af-0123456789abcdef";
const fact = (overrides: Partial<Fact> = {}): Fact => ({
  id: ID,
  text: "Lives in Aarhus",
  category: "home",
  source: { kind: "chat" },
  learnedAt: "2026-11-05T08:00:00Z",
  lastConfirmed: "2026-11-05T08:00:00Z",
  status: "active",
  changes: [],
  ...overrides,
});
const routes = () => <Route path="/about" component={About} />;

function aboutFetch(opts: { failAdd?: boolean; refused?: boolean } = {}) {
  return stubFetch(c => {
    if (c.method === "GET" && c.url === "/v1/about") {
      return {
        body: {
          facts: [
            fact(),
            fact({ id: "af-1111111111111111", text: "Works as a gardener", category: "work", source: { kind: "document" }, learnedAt: "2026-10-01T08:00:00Z", validUntil: { date: "2027-01-01" } }),
          ],
          history: [
            fact({ id: "af-2222222222222222", text: "Lived in Odense", status: "expired", endedAt: "2026-09-01T08:00:00Z" }),
            fact({ id: "af-3333333333333333", text: "Allergic to nuts", status: "retracted", endedAt: "2026-08-01T08:00:00Z" }),
          ],
        },
      };
    }
    if (c.method === "POST" && c.url === "/v1/about") {
      if (opts.refused) return { status: 400, body: { error: { code: "secret_refused", message: "Secrets go to Infisical, not here." } } };
      if (opts.failAdd) return { status: 500, body: {} };
      return { body: fact({ id: "af-4444444444444444", source: { kind: "owner-edit" } }) };
    }
    if (c.method === "PATCH") return { body: fact({ text: (c.body as { text: string }).text }) };
    if (c.method === "POST" && c.url.endsWith("/end")) return { body: fact({ status: "expired" }) };
    if (c.method === "POST" && c.url.endsWith("/retract")) return { body: fact({ status: "retracted" }) };
    return { status: 404, body: {} };
  });
}

describe("About", () => {
  it("renders active facts grouped by category with source and until", async () => {
    aboutFetch();
    renderAt("/about", routes);
    expect(await screen.findByText("Lives in Aarhus")).toBeInTheDocument();
    expect(screen.getByText("Home")).toBeInTheDocument();
    expect(screen.getByText("Work")).toBeInTheDocument();
    expect(screen.getByText("Works as a gardener")).toBeInTheDocument();
    expect(screen.getByText("until 2027-01-01")).toBeInTheDocument();
    const row = screen.getByText("Lives in Aarhus").closest("li")!;
    expect(row).toHaveTextContent("from chat, Nov 5");
    // History is collapsed.
    const history = screen.getByText("History", { selector: "summary" }).closest("details")!;
    expect(history.open).toBe(false);
    expect(history.querySelectorAll("li")).toHaveLength(2);
  });

  it("edits a fact inline and sends PATCH", async () => {
    const calls = aboutFetch();
    renderAt("/about", routes);
    const row = (await screen.findByText("Lives in Aarhus")).closest("li")!;
    fireEvent.click(within(row).getByRole("button", { name: "Edit" }));
    const input = within(row).getByDisplayValue("Lives in Aarhus");
    fireEvent.input(input, { target: { value: "Lives in Copenhagen" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.some(c => c.method === "PATCH")).toBe(true));
    const patch = calls.find(c => c.method === "PATCH")!;
    expect(patch.url).toBe(`/v1/about/${ID}`);
    expect(patch.body).toMatchObject({ text: "Lives in Copenhagen", category: "home", validUntil: "", validUntilCondition: "" });
  });

  it("asks before retracting, then sends the retract", async () => {
    const calls = aboutFetch();
    renderAt("/about", routes);
    await screen.findByText("Lives in Aarhus");
    fireEvent.click(within((screen.getByText("Lives in Aarhus")).closest("li")!).getByRole("button", { name: "Wrong" }));
    expect(calls.filter(c => c.method === "POST" && c.url.endsWith("/retract"))).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Sure?" }));
    await screen.findByText("History", { selector: "summary" });
    const retract = calls.find(c => c.method === "POST" && c.url.endsWith("/retract"))!;
    expect(retract.url).toBe(`/v1/about/${ID}/retract`);
  });

  it("ends a fact with the end route", async () => {
    const calls = aboutFetch();
    renderAt("/about", routes);
    await screen.findByText("Lives in Aarhus");
    fireEvent.click(within((screen.getByText("Lives in Aarhus")).closest("li")!).getByRole("button", { name: "No longer true" }));
    await screen.findByText("History", { selector: "summary" });
    expect(calls.some(c => c.method === "POST" && c.url === `/v1/about/${ID}/end`)).toBe(true);
  });

  it("toggles the history open and shows the change log of a row", async () => {
    stubFetch(c => {
      if (c.method === "GET") {
        return {
          body: {
            facts: [fact({ changes: [{ at: "2026-11-06T08:00:00Z", by: "owner", action: "edited", note: "fixed city", before: "Lives in Odense" }] })],
            history: [fact({ id: "af-3333333333333333", text: "Allergic to nuts", status: "retracted", endedAt: "2026-08-01T08:00:00Z" })],
          },
        };
      }
      return { body: {} };
    });
    const { container } = renderAt("/about", routes);
    await screen.findByText("Lives in Aarhus");
    const history = screen.getByText("History", { selector: "summary" }).closest("details")!;
    fireEvent.click(screen.getByText("History", { selector: "summary" }));
    expect(history.open).toBe(true);
    expect(screen.getByText("Allergic to nuts").closest("li")).toHaveTextContent("until Aug 1");
    expect(container.querySelector(".about-retracted")).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Changes" }));
    expect(screen.getByText(/edited/)).toHaveTextContent("was: Lives in Odense");
  });

  it("shows the refused-secret message as is", async () => {
    aboutFetch({ refused: true });
    renderAt("/about", routes);
    await screen.findByText("Lives in Aarhus");
    fireEvent.input(screen.getByLabelText("What should Lucina know?"), { target: { value: "my password is 1234" } });
    fireEvent.click(screen.getByRole("button", { name: "Add fact" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Secrets go to Infisical, not here.");
  });
});
