import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { resetShopping } from "../state/shopping";
import { deferred, stubFetch } from "../test/fetch";
import ShoppingView from "./Shopping";

const ITEMS = [
  { id: "1", name: "Milk", quantity: "2", checked: false },
  { id: "2", rawName: "Eggs", checked: false },
  { id: "3", product: "Basil", status: "purchased" },
];

function listOf(name: string): string | null {
  return screen.getByLabelText(new RegExp(`^${name}`)).closest("ul")?.getAttribute("aria-label") ?? null;
}

describe("Shopping", () => {
  beforeEach(() => resetShopping());

  it("splits unchecked and bought items and shows the empty state", async () => {
    stubFetch(() => ({ body: ITEMS }));
    render(() => <ShoppingView />);
    await screen.findByLabelText(/^Milk/);
    expect(listOf("Milk")).toBe("To buy");
    expect(listOf("Eggs")).toBe("To buy");
    expect(listOf("Basil")).toBe("Bought");
    expect(screen.getByText("Bought (1)")).toBeInTheDocument();
  });

  it("shows an empty state", async () => {
    stubFetch(() => ({ body: [] }));
    render(() => <ShoppingView />);
    expect(await screen.findByText("Your list is empty.")).toBeInTheDocument();
  });

  it("toggles optimistically and rolls back on error", async () => {
    const gate = deferred<void>();
    const calls = stubFetch(async (c) => {
      if (c.method === "GET") return { body: ITEMS };
      await gate.promise;
      return { status: 500, body: { error: { code: "internal", message: "Nope." } } };
    });
    render(() => <ShoppingView />);
    fireEvent.click(await screen.findByLabelText(/^Milk/));

    // Optimistic: moved to Bought before the server answers.
    await waitFor(() => expect(listOf("Milk")).toBe("Bought"));
    expect(calls.at(-1)).toMatchObject({ method: "PATCH", url: "/v1/shopping/1", body: { checked: true } });

    gate.resolve();
    expect(await screen.findByRole("alert")).toHaveTextContent('Couldn\'t update "Milk".');
    await waitFor(() => expect(listOf("Milk")).toBe("To buy"));
    expect(screen.getByLabelText(/^Milk/)).not.toBeChecked();
  });

  it("sends the status vocabulary for status-shaped items", async () => {
    const calls = stubFetch((c) => (c.method === "GET" ? { body: ITEMS } : { body: {} }));
    render(() => <ShoppingView />);
    fireEvent.click(await screen.findByLabelText(/^Basil/));
    await waitFor(() => expect(listOf("Basil")).toBe("To buy"));
    expect(calls.find((c) => c.method === "PATCH")).toMatchObject({ url: "/v1/shopping/3", body: { status: "needed" } });
  });

  it("adds an item optimistically", async () => {
    const gate = deferred<void>();
    const calls = stubFetch(async (c) => {
      if (c.method === "GET") return { body: ITEMS };
      await gate.promise;
      return { status: 201, body: { id: "9", rawName: "Tea", checked: false } };
    });
    render(() => <ShoppingView />);
    await screen.findByLabelText(/^Milk/);
    const input = screen.getByLabelText("Add an item");
    fireEvent.input(input, { target: { value: "Tea" } });
    fireEvent.submit(input.closest("form")!);

    expect(await screen.findByLabelText(/^Tea/)).toBeDisabled();
    expect(input).toHaveValue("");
    gate.resolve();
    await waitFor(() => expect(screen.getByLabelText(/^Tea/)).not.toBeDisabled());
    expect(calls.find((c) => c.method === "POST")).toMatchObject({ url: "/v1/shopping", body: { rawName: "Tea" } });
  });
});
