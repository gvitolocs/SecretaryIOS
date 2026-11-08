import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FaceTarget } from "../../../api/gallery";
import { setLocale } from "../../../i18n";
import { peopleBackend, rawPerson } from "../../../test/peopleBackend";
import { FacePicker } from "./FacePicker";

const people = () => [
  rawPerson("p1", { name: "Alma", item_count: 9 }),
  rawPerson("p2", { name: "Zoë Larsen", item_count: 2 }),
  rawPerson("p3", { name: null, item_count: 1 }),
  rawPerson("p4", { name: "Bruno", item_count: 5 }),
];

function open(current: { id: string; name: string | null } | null = null, onAssign: (t: FaceTarget) => Promise<void> = async () => undefined) {
  const onCancel = vi.fn();
  const assign = vi.fn(onAssign);
  render(() => <FacePicker faceId="f1" current={current} onAssign={assign} onCancel={onCancel} />);
  return { assign, onCancel };
}
const dialog = () => screen.getByRole("dialog", { name: "Who is this?" });
const names = () => within(dialog()).getAllByRole("button").map((b) => b.querySelector(".gf-person__name")?.textContent).filter(Boolean);

beforeEach(() => setLocale("en"));
afterEach(() => vi.unstubAllGlobals());

describe("the picker for a face", () => {
  it("shows the face and the people to choose from, named ones first and unnamed ones last", async () => {
    const b = peopleBackend({ people: people() });
    open();
    await within(dialog()).findByRole("button", { name: /Alma/ });
    expect(names()).toEqual(["Alma", "Zoë Larsen", "Bruno", "Unnamed person"]);
    expect(dialog().querySelector(".gf-pick-face")?.getAttribute("src")).toBe("/v1/media/faces/f1/thumb");
    // It asks for the small groups too, so a person with a few faces can be chosen.
    expect(b.calls.find((c) => c.url.startsWith("/v1/media/people"))?.url).toBe("/v1/media/people?min_faces=1");
    expect(within(dialog()).queryByRole("button", { name: "Not this person" })).toBeNull();
  });

  it("searches by name, ignoring case and accents", async () => {
    peopleBackend({ people: people() });
    open();
    await within(dialog()).findByRole("button", { name: /Alma/ });
    fireEvent.input(within(dialog()).getByRole("searchbox", { name: "Search people" }), { target: { value: "zoe" } });
    expect(names()).toEqual(["Zoë Larsen"]);
    fireEvent.input(within(dialog()).getByRole("searchbox", { name: "Search people" }), { target: { value: "nobody" } });
    expect(within(dialog()).getByText("No one matches.")).toBeInTheDocument();
  });

  it("assigns the face to the chosen person", async () => {
    peopleBackend({ people: people() });
    const { assign } = open();
    fireEvent.click(await within(dialog()).findByRole("button", { name: /Bruno/ }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith({ personId: "p4" }));
  });

  it("makes a new person from a typed name, trimmed, and only when there is a name", async () => {
    peopleBackend({ people: people() });
    const { assign } = open();
    const create = within(dialog()).getByRole("button", { name: "Create" });
    expect(create).toBeDisabled();
    fireEvent.input(within(dialog()).getByLabelText("New person…"), { target: { value: "  Chiara  " } });
    expect(create).toBeEnabled();
    fireEvent.click(create);
    await waitFor(() => expect(assign).toHaveBeenCalledWith({ newPerson: "Chiara" }));
  });

  it("offers Not this person for a face that has one, and the person is not offered again", async () => {
    peopleBackend({ people: people() });
    const { assign } = open({ id: "p1", name: "Alma" });
    expect(within(dialog()).getByText("Now: Alma")).toBeInTheDocument();
    const current = await within(dialog()).findByRole("button", { name: /Alma/ });
    expect(current).toBeDisabled();
    fireEvent.click(within(dialog()).getByRole("button", { name: "Not this person" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith({ personId: null }));
  });

  it("stays open with an error when the server refuses, and works again afterwards", async () => {
    peopleBackend({ people: people() });
    let fail = true;
    const { assign } = open(null, async () => {
      if (fail) throw new Error("no");
    });
    fireEvent.click(await within(dialog()).findByRole("button", { name: /Alma/ }));
    expect(await within(dialog()).findByRole("alert")).toHaveTextContent("Couldn't save that");
    expect(within(dialog()).getByRole("button", { name: /Bruno/ })).toBeEnabled();
    fail = false;
    fireEvent.click(within(dialog()).getByRole("button", { name: /Bruno/ }));
    await waitFor(() => expect(assign).toHaveBeenCalledTimes(2));
  });

  it("says when the people cannot be loaded, and still lets a new person be made", async () => {
    const b = peopleBackend({ people: people() });
    b.failNext.set("GET /v1/media/people", 500);
    const { assign } = open();
    expect(await within(dialog()).findByText("Couldn't load the people.")).toBeInTheDocument();
    fireEvent.input(within(dialog()).getByLabelText("New person…"), { target: { value: "Elena" } });
    fireEvent.click(within(dialog()).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith({ newPerson: "Elena" }));
  });

  it("closes on Cancel and on Escape", async () => {
    peopleBackend({ people: people() });
    const { onCancel } = open();
    fireEvent.click(within(dialog()).getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(dialog(), { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(2);
  });
});
