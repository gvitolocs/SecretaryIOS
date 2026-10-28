import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { resetFacesAvailable } from "../components/gallery/faces/facesAvailable";
import { setLocale } from "../i18n";
import { peopleBackend, rawPerson } from "../test/peopleBackend";
import { renderAt } from "../test/router";
import GalleryPeopleView, { SCAN_POLL_MS } from "./GalleryPeople";

function mount() {
  return renderAt("/gallery/people", () => (
    <>
      <Route path="/gallery/people" component={GalleryPeopleView} />
      <Route path="/gallery/person/:id" component={() => <p>person page</p>} />
      <Route path="/gallery" component={() => <p>gallery page</p>} />
    </>
  ));
}

const three = () => [
  rawPerson("p1", { name: "Alma", face_count: 12, item_count: 9 }),
  rawPerson("p2", { name: "Bruno", face_count: 7, item_count: 1 }),
  rawPerson("p3", { name: null, face_count: 4, item_count: 3 }),
];
const grid = () => screen.getByRole("list", { name: "People" });
const tile = (name: string) => within(grid()).getByRole("button", { name }).closest(".pp-tile") as HTMLElement;

beforeEach(() => {
  setLocale("en");
  resetFacesAvailable();
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("while face recognition is off", () => {
  it("explains it, says it stays on the server, and turns it on from the button", async () => {
    const b = peopleBackend({ settings: { enabled: false, faces: 0 }, people: three() });
    mount();
    expect(await screen.findByRole("heading", { name: "Find the people in your photos" })).toBeInTheDocument();
    expect(screen.getByText(/runs only on this server/)).toBeInTheDocument();
    expect(screen.getByText(/off until you turn it on/)).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "People" })).toBeNull();
    // Nothing to delete yet, and no settings menu.
    expect(screen.queryByRole("button", { name: "Delete face data" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Face recognition settings" })).toBeNull();
    // Nothing but the settings was asked for.
    expect(b.calls.map((c) => c.url)).toEqual(["/v1/media/faces/settings"]);

    fireEvent.click(screen.getByRole("button", { name: "Turn on" }));
    expect(await screen.findByRole("list", { name: "People" })).toBeInTheDocument();
    expect(b.calls.find((c) => c.method === "PUT")?.body).toEqual({ enabled: true });
    expect(within(grid()).getByText("Alma")).toBeInTheDocument();
  });

  it("offers to delete what an earlier run found", async () => {
    const b = peopleBackend({ settings: { enabled: false, faces: 25 } });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Delete face data" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/Your photos are not touched/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete face data" }));
    await waitFor(() => expect(b.calls.some((c) => c.method === "DELETE" && c.url === "/v1/media/faces")).toBe(true));
    expect(await screen.findByText("Face data deleted.")).toBeInTheDocument();
  });

  it("says so when turning it on fails, and stays on the card", async () => {
    const b = peopleBackend({ settings: { enabled: false } });
    mount();
    const button = await screen.findByRole("button", { name: "Turn on" });
    b.failNext.set("PUT /v1/media/faces/settings", 500);
    fireEvent.click(button);
    expect(await screen.findByText("That didn't work. Try again.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Turn on" })).toBeEnabled();
  });
});

describe("the people", () => {
  it("shows each person's face, name (or Add a name), photo count and a link to their page", async () => {
    peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    const alma = tile("Rename Alma");
    expect(within(alma).getByText("9 photos")).toBeInTheDocument();
    expect(alma.querySelector("img")?.getAttribute("src")).toBe("/v1/media/people/p1/thumb?v=1");
    expect(within(tile("Rename Bruno")).getByText("1 photo")).toBeInTheDocument();
    // An unnamed person invites a name.
    expect(within(tile("Rename Unnamed person")).getByRole("button", { name: "Rename Unnamed person" })).toHaveTextContent("Add a name");
    expect(within(alma).getByRole("link", { name: "Open the photos of Alma" })).toHaveAttribute("href", "/gallery/person/p1");
  });

  it("opens the person's page from the face", async () => {
    peopleBackend({ people: three() });
    mount();
    fireEvent.click(await screen.findByRole("link", { name: "Open the photos of Bruno" }));
    expect(await screen.findByText("person page")).toBeInTheDocument();
  });

  it("asks for the people with the hidden ones only when Show hidden is on", async () => {
    const b = peopleBackend({ people: [...three(), rawPerson("p4", { name: "Dario", hidden: true })] });
    mount();
    await screen.findByRole("list", { name: "People" });
    expect(within(grid()).queryByText("Dario")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show hidden" }));
    expect(await within(grid()).findByText("Dario")).toBeInTheDocument();
    expect(b.calls.filter((c) => c.url.startsWith("/v1/media/people?")).map((c) => c.url)).toEqual(["/v1/media/people?include_hidden=true"]);
    expect(within(tile("Rename Dario")).getByText(/Hidden/)).toBeInTheDocument();
  });

  it("says when nobody has been found yet", async () => {
    peopleBackend({ people: [] });
    mount();
    expect(await screen.findByText(/No people yet/)).toBeInTheDocument();
  });

  it("offers a retry when the people cannot be loaded", async () => {
    const b = peopleBackend({ people: three() });
    b.failNext.set("GET /v1/media/people", 500);
    mount();
    expect(await screen.findByText(/Couldn't load the people/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("list", { name: "People" })).toBeInTheDocument();
  });

  it("offers a retry when the settings cannot be loaded", async () => {
    const b = peopleBackend({ people: three() });
    b.failNext.set("GET /v1/media/faces/settings", 500);
    mount();
    expect(await screen.findByText(/Couldn't load the people/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("list", { name: "People" })).toBeInTheDocument();
  });
});

describe("the scan", () => {
  it("shows a bar with how many photos are done while some wait", async () => {
    peopleBackend({ settings: { scanned: 30, pending: 70, running: true }, people: three() });
    mount();
    const bar = await screen.findByRole("progressbar", { name: "Photos looked at" });
    expect(bar).toHaveAttribute("aria-valuenow", "30");
    expect(screen.getByText("Looking at your photos: 30 done, 70 to go.")).toBeInTheDocument();
  });

  it("says why it is paused", async () => {
    peopleBackend({ settings: { scanned: 30, pending: 70, paused_hot: true }, people: three() });
    mount();
    expect(await screen.findByText(/Paused while the server cools down: 30 done, 70 to go/)).toBeInTheDocument();
  });

  it("says when the server is busy", async () => {
    peopleBackend({ settings: { scanned: 30, pending: 70, paused_busy: true }, people: three() });
    mount();
    expect(await screen.findByText(/Paused while the server is busy/)).toBeInTheDocument();
  });

  it("has no bar once every photo has been looked at", async () => {
    peopleBackend({ settings: { scanned: 100, pending: 0 }, people: three() });
    mount();
    expect(await screen.findByText("All 100 photos have been looked at.")).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("asks again while photos wait, and shows the people the scan has found by then", async () => {
    // Only the timeouts are faked: the page's poll is a timeout, and the test library polls with an interval.
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const b = peopleBackend({ settings: { scanned: 30, pending: 70, running: true, people: 1, faces: 5 }, people: [rawPerson("p1", { name: "Alma" })] });
    mount();
    await screen.findByRole("list", { name: "People" });
    b.settings = { ...b.settings, scanned: 80, pending: 20, people: 2, faces: 12 };
    b.people = [...b.people, rawPerson("p2", { name: "Bruno" })];
    await vi.advanceTimersByTimeAsync(SCAN_POLL_MS + 10);
    expect(await screen.findByText("Looking at your photos: 80 done, 20 to go.")).toBeInTheDocument();
    expect(await within(grid()).findByText("Bruno")).toBeInTheDocument();
  });
});

describe("renaming", () => {
  it("turns the name into a field; Enter saves it and the new name shows", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(within(tile("Rename Unnamed person")).getByRole("button", { name: "Rename Unnamed person" }));
    const input = await screen.findByLabelText("Name of this person");
    expect(input).toHaveFocus();
    fireEvent.input(input, { target: { value: "  Chiara " } });
    fireEvent.submit(input.closest("form")!);
    await waitFor(() => expect(within(grid()).getByText("Chiara")).toBeInTheDocument());
    const patch = b.calls.find((c) => c.method === "PATCH")!;
    expect(patch.url).toBe("/v1/media/people/p3");
    expect(patch.body).toEqual({ name: "Chiara" });
    expect(screen.queryByLabelText("Name of this person")).toBeNull();
  });

  it("leaves the name alone on Escape and does not ask the server when nothing changed", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(within(tile("Rename Alma")).getByRole("button", { name: "Rename Alma" }));
    const input = await screen.findByLabelText("Name of this person");
    fireEvent.input(input, { target: { value: "Someone else" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(within(grid()).getByText("Alma")).toBeInTheDocument();
    fireEvent.click(within(tile("Rename Alma")).getByRole("button", { name: "Rename Alma" }));
    fireEvent.submit((await screen.findByLabelText("Name of this person")).closest("form")!);
    await waitFor(() => expect(screen.queryByLabelText("Name of this person")).toBeNull());
    expect(b.calls.some((c) => c.method === "PATCH")).toBe(false);
  });

  it("removes the name when the field is emptied", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(within(tile("Rename Bruno")).getByRole("button", { name: "Rename Bruno" }));
    const input = await screen.findByLabelText("Name of this person");
    fireEvent.input(input, { target: { value: "   " } });
    fireEvent.submit(input.closest("form")!);
    await waitFor(() => expect(b.calls.find((c) => c.method === "PATCH")?.body).toEqual({ name: null }));
    await waitFor(() => expect(within(grid()).getAllByText("Add a name")).toHaveLength(2));
  });

  it("keeps the field open with an error when the server refuses", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(within(tile("Rename Alma")).getByRole("button", { name: "Rename Alma" }));
    const input = await screen.findByLabelText("Name of this person");
    fireEvent.input(input, { target: { value: "Almas" } });
    b.failNext.set("PATCH /v1/media/people/p1", 500);
    fireEvent.submit(input.closest("form")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("That didn't work");
    expect(screen.getByLabelText("Name of this person")).toHaveValue("Almas");
  });
});

describe("hiding", () => {
  it("hides a person (they leave the list) and shows them again from Show hidden", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(within(tile("Rename Bruno")).getByRole("button", { name: "Hide Bruno" }));
    await waitFor(() => expect(within(grid()).queryByText("Bruno")).toBeNull());
    expect(b.calls.find((c) => c.method === "PATCH")).toMatchObject({ url: "/v1/media/people/p2", body: { hidden: true } });

    fireEvent.click(screen.getByRole("button", { name: "Show hidden" }));
    fireEvent.click(await within(grid()).findByRole("button", { name: "Show Bruno again" }));
    await waitFor(() => expect(b.calls.filter((c) => c.method === "PATCH").pop()?.body).toEqual({ hidden: false }));
    await waitFor(() => expect(within(tile("Rename Bruno")).queryByText(/Hidden/)).toBeNull());
  });

  it("says so when hiding fails and keeps the person", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    b.failNext.set("PATCH /v1/media/people/p1", 500);
    fireEvent.click(within(tile("Rename Alma")).getByRole("button", { name: "Hide Alma" }));
    expect(await screen.findByText("That didn't work. Try again.")).toBeInTheDocument();
    expect(within(grid()).getByText("Alma")).toBeInTheDocument();
  });
});

describe("merging", () => {
  async function selectTwo() {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(screen.getByRole("button", { name: "Select" }));
    return b;
  }

  it("selects people instead of opening them, and merges only with two or more", async () => {
    await selectTwo();
    expect(screen.queryByRole("link", { name: /Open the photos of/ })).toBeNull();
    expect(screen.getByRole("button", { name: /Merge/ })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Select Alma" }));
    expect(screen.getByRole("button", { name: "Select Alma" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("1 selected")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Merge/ })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Select Unnamed person" }));
    expect(screen.getByText("2 selected")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Merge/ })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Select Alma" }));
    expect(screen.getByText("1 selected")).toBeInTheDocument();
  });

  it("asks which person to keep (the named one first), merges into the chosen one, and removes the others", async () => {
    const b = await selectTwo();
    fireEvent.click(screen.getByRole("button", { name: "Select Alma" }));
    fireEvent.click(screen.getByRole("button", { name: "Select Unnamed person" }));
    fireEvent.click(screen.getByRole("button", { name: /Merge/ }));
    const dialog = await screen.findByRole("dialog", { name: "Merge people" });
    const alma = within(dialog).getByRole("radio", { name: /Keep Alma/ });
    expect(alma).toBeChecked();
    fireEvent.click(within(dialog).getByRole("radio", { name: /Keep Unnamed person/ }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Merge 2 people" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const call = b.calls.find((c) => c.method === "POST")!;
    expect(call.url).toBe("/v1/media/people/p3/merge");
    expect(call.body).toEqual({ from: ["p1"] });
    expect(within(grid()).queryByText("Alma")).toBeNull();
    expect(within(grid()).getByText("Bruno")).toBeInTheDocument();
    expect(screen.getByText("Merged 2 people into one.")).toBeInTheDocument();
    // Selection mode is over.
    expect(screen.getByRole("button", { name: "Select" })).toBeInTheDocument();
  });

  it("keeps the dialog with an error when the merge fails", async () => {
    const b = await selectTwo();
    fireEvent.click(screen.getByRole("button", { name: "Select Alma" }));
    fireEvent.click(screen.getByRole("button", { name: "Select Bruno" }));
    fireEvent.click(screen.getByRole("button", { name: /Merge/ }));
    const dialog = await screen.findByRole("dialog", { name: "Merge people" });
    b.failNext.set("POST /v1/media/people/p1/merge", 500);
    fireEvent.click(within(dialog).getByRole("button", { name: "Merge 2 people" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("That didn't work");
    expect(within(grid()).getByText("Bruno")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("leaves selection mode with Done", async () => {
    await selectTwo();
    fireEvent.click(screen.getByRole("button", { name: "Select Alma" }));
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    expect(screen.getByRole("link", { name: "Open the photos of Alma" })).toBeInTheDocument();
  });
});

describe("the settings menu", () => {
  it("turns recognition off and shows the card again, keeping the data", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(screen.getByRole("button", { name: "Face recognition settings" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Turn off" }));
    expect(await screen.findByRole("heading", { name: "Find the people in your photos" })).toBeInTheDocument();
    expect(b.calls.find((c) => c.method === "PUT")?.body).toEqual({ enabled: false });
    expect(screen.getByText(/What it found is kept until you delete it/)).toBeInTheDocument();
    // There is something to delete.
    expect(screen.getByRole("button", { name: "Delete face data" })).toBeInTheDocument();
  });

  it("asks before deleting the face data, and deletes it when confirmed", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(screen.getByRole("button", { name: "Face recognition settings" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete face data" }));
    const dialog = await screen.findByRole("dialog", { name: "Delete all face data?" });
    expect(b.calls.some((c) => c.method === "DELETE")).toBe(false);
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(b.calls.some((c) => c.method === "DELETE")).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Face recognition settings" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete face data" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Delete face data" }));
    expect(await screen.findByText("Face data deleted.")).toBeInTheDocument();
    expect(b.calls.some((c) => c.method === "DELETE" && c.url === "/v1/media/faces")).toBe(true);
    expect(await screen.findByRole("heading", { name: "Find the people in your photos" })).toBeInTheDocument();
  });

  it("keeps the dialog open with an error when the deletion fails", async () => {
    const b = peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    fireEvent.click(screen.getByRole("button", { name: "Face recognition settings" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete face data" }));
    const dialog = await screen.findByRole("dialog");
    b.failNext.set("DELETE /v1/media/faces", 500);
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete face data" }));
    expect(await within(dialog).findByRole("alert")).toBeInTheDocument();
    expect(screen.getByRole("list", { name: "People" })).toBeInTheDocument();
  });
});

describe("faces without a person", () => {
  const loose = (n: number) => Array.from({ length: n }, (_, i) => ({ id: `f${i}`, item_id: `i${i}`, thumb_url: `/v1/media/faces/f${i}/thumb`, score: 0.9 }));

  it("lists them at the bottom, page by page", async () => {
    peopleBackend({ people: three(), unassigned: loose(3) });
    mount();
    expect(await screen.findByRole("heading", { name: "Faces without a person" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "A face without a person. Choose who it is." })).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: "Show more" }));
    await waitFor(() => expect(screen.getAllByRole("button", { name: /A face without a person/ })).toHaveLength(3));
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("has no such section when every face has a person", async () => {
    peopleBackend({ people: three(), unassigned: [] });
    mount();
    await screen.findByRole("list", { name: "People" });
    expect(screen.queryByRole("heading", { name: "Faces without a person" })).toBeNull();
  });

  it("gives a face to a person from the picker and takes it off the list", async () => {
    const b = peopleBackend({ people: three(), unassigned: loose(2) });
    mount();
    await screen.findByRole("heading", { name: "Faces without a person" });
    fireEvent.click(screen.getAllByRole("button", { name: /A face without a person/ })[0]!);
    const dialog = await screen.findByRole("dialog", { name: "Who is this?" });
    fireEvent.click(await within(dialog).findByRole("button", { name: /Bruno/ }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(b.calls.find((c) => c.method === "PUT" && c.url === "/v1/media/faces/f0")?.body).toEqual({ person_id: "p2" });
    expect(screen.getAllByRole("button", { name: /A face without a person/ })).toHaveLength(1);
  });

  it("makes a new person from a face", async () => {
    const b = peopleBackend({ people: three(), unassigned: loose(1) });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: /A face without a person/ }));
    const dialog = await screen.findByRole("dialog", { name: "Who is this?" });
    fireEvent.input(within(dialog).getByLabelText("New person…"), { target: { value: "Elena" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(b.calls.find((c) => c.method === "PUT" && c.url === "/v1/media/faces/f0")?.body).toEqual({ new_person: "Elena" }));
    await waitFor(() => expect(screen.queryByRole("heading", { name: "Faces without a person" })).toBeNull());
  });
});

describe("leaving", () => {
  it("has a way back to the Gallery", async () => {
    peopleBackend({ people: three() });
    mount();
    await screen.findByRole("list", { name: "People" });
    expect(screen.getByRole("link", { name: "Gallery" })).toHaveAttribute("href", "/gallery");
  });
});
