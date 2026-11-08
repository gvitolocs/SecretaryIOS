import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import VoiceLearnView, { candidatesFrom } from "./VoiceLearn";

// The microphone side is a browser thing: replaced by a recorder the test drives.
const mic = vi.hoisted(() => ({ fail: null as null | "denied" | "unsupported" | "failed" }));
vi.mock("../lib/voiceSample", async () => {
  const { createSignal } = await import("solid-js");
  return {
    createSampleRecorder: (onClip: (wav: Blob) => void, onFailure: (why: string, detail?: string) => void) => {
      const [state, setState] = createSignal<"idle" | "recording" | "processing">("idle");
      const [seconds] = createSignal(3.4);
      return {
        state,
        seconds,
        start: async () => {
          if (mic.fail) onFailure(mic.fail, mic.fail === "failed" ? "cannot decode" : undefined);
          else setState("recording");
        },
        stop: () => {
          setState("idle");
          onClip(new Blob([new Uint8Array(48)], { type: "audio/wav" }));
        },
        cancel: () => setState("idle"),
      };
    },
  };
});

const skibi = { id: "vl_1", word: "Skibigårdsvej", soundsLike: ["skibi gård vej", "skibigor"], meaning: "home street", lang: "da", source: "app", updatedAt: "2026-11-01T10:00:00Z" };
const mikkel = { id: "vl_2", word: "Mikkel", soundsLike: [], lang: "it", source: "correction" };

const show = () => renderAt("/voice/learn", () => <Route path="/voice/learn" component={VoiceLearnView} />);

type Handler = (call: Call) => { status?: number; body?: unknown } | undefined;

/** A fake Voice Learn API over a mutable list. */
function api(over: Handler = () => undefined, initial: unknown[] = [skibi, mikkel]) {
  let list = [...initial] as Record<string, unknown>[];
  const calls = stubFetch((c) => {
    const special = over(c);
    if (special) return special;
    if (c.method === "GET") return { body: { entries: list, max: 500 } };
    if (c.method === "POST" && c.url.includes("/sample")) return { body: { heard: "skibi gard vej", corrections: [] } };
    if (c.method === "POST") {
      const made = { id: "vl_9", ...(c.body as object), soundsLike: (c.body as { soundsLike?: string[] }).soundsLike ?? [] };
      list = [...list, made];
      return { status: 201, body: made };
    }
    if (c.method === "PATCH") {
      const id = c.url.split("/").pop();
      list = list.map((e) => (e.id === id ? { ...e, ...(c.body as object) } : e));
      return { body: list.find((e) => e.id === id) };
    }
    if (c.method === "DELETE") {
      list = list.filter((e) => e.id !== c.url.split("/").pop());
      return { body: { deleted: true } };
    }
    return { status: 404, body: { error: { code: "not_found", message: "Not found." } } };
  });
  return calls;
}

beforeEach(() => {
  mic.fail = null;
  setLocale("en");
});

const open = async () => {
  const add = await screen.findByRole("button", { name: "Add a word" });
  await waitFor(() => expect(add).toBeEnabled()); // once the list has loaded
  fireEvent.click(add);
  return screen.findByRole("form", { name: "Word details" });
};

describe("Voice Learn page", () => {
  it("says what it does and lists the words with what they sound like, their meaning and language", async () => {
    const calls = api();
    show();
    expect(await screen.findByRole("heading", { level: 1, name: "Voice Learn" })).toBeInTheDocument();
    expect(screen.getByText("Words Lucina should recognise in voice turns, voice notes and memos.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Voice devices/ })).toHaveAttribute("href", "/voice");
    expect(calls[0]).toMatchObject({ method: "GET", url: "/v1/voice/learn" });

    const list = await screen.findByRole("list", { name: "Vocabulary" });
    const rows = within(list).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]!).getByText("Skibigårdsvej")).toBeInTheDocument();
    expect(within(rows[0]!).getByText("Danish")).toBeInTheDocument();
    expect(within(rows[0]!).getByText("sounds like skibi gård vej, skibigor")).toBeInTheDocument();
    expect(within(rows[0]!).getByText("home street")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("Italian")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("From a correction")).toBeInTheDocument();
    expect(screen.getByText("2 of 500 words")).toBeInTheDocument();
  });

  it("shows an empty state and an error with a retry", async () => {
    api(() => undefined, []);
    show();
    expect(await screen.findByText("No words yet")).toBeInTheDocument();
    expect(screen.getByText(/Add a name or a word Lucina keeps mishearing/)).toBeInTheDocument();
  });

  it("recovers from a failed load", async () => {
    let fail = true;
    stubFetch(() => (fail ? { status: 502, body: { error: { code: "lucina_unreachable", message: "Lucina is not reachable" } } } : { body: { entries: [skibi] } }));
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the vocabulary.");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("Skibigårdsvej")).toBeInTheDocument();
  });

  it("adds a word, with Danish characters, and reloads the list", async () => {
    const calls = api();
    show();
    const form = await open();
    // An empty word is stopped before any request.
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent("Type the word first.");
    expect(calls.filter((c) => c.method === "POST")).toHaveLength(0);

    fireEvent.input(within(form).getByLabelText("Word"), { target: { value: "Blåbærgrød" } });
    fireEvent.input(within(form).getByLabelText("Sounds like"), { target: { value: "blo ber grød, bloberger ,," } });
    fireEvent.input(within(form).getByLabelText("Meaning"), { target: { value: "min morgenmad" } });
    fireEvent.change(within(form).getByLabelText("Language"), { target: { value: "da" } });
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));

    await waitFor(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.method === "POST")).toMatchObject({
      url: "/v1/voice/learn",
      body: { word: "Blåbærgrød", soundsLike: ["blo ber grød", "bloberger"], meaning: "min morgenmad", lang: "da" },
    });
    expect(await screen.findByText("Blåbærgrød")).toBeInTheDocument();
    expect(screen.queryByRole("form", { name: "Word details" })).toBeNull();
  });

  it("sends only what was filled in for a new word", async () => {
    const calls = api();
    show();
    const form = await open();
    fireEvent.input(within(form).getByLabelText("Word"), { target: { value: "Lucina" } });
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.method === "POST")!.body).toEqual({ word: "Lucina" });
  });

  it("shows the API's validation message and keeps the form", async () => {
    api((c) => (c.method === "POST" ? { status: 400, body: { error: { code: "word_too_long", message: "word must be at most 100 characters" } } } : undefined));
    show();
    const form = await open();
    fireEvent.input(within(form).getByLabelText("Word"), { target: { value: "x" } });
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent("Couldn't save: word must be at most 100 characters");
    expect(within(form).getByLabelText("Word")).toHaveValue("x");
    // The full list refuses a new word up front.
    expect(within(form).getByRole("button", { name: "Save" })).not.toBeDisabled();
  });

  it("edits a word and clears the fields that were emptied", async () => {
    const calls = api();
    show();
    fireEvent.click(await screen.findByRole("button", { name: "Edit Skibigårdsvej" }));
    const form = await screen.findByRole("form", { name: "Word details" });
    expect(within(form).getByLabelText("Word")).toHaveValue("Skibigårdsvej");
    expect(within(form).getByLabelText("Sounds like")).toHaveValue("skibi gård vej, skibigor");
    expect(within(form).getByLabelText("Language")).toHaveValue("da");
    fireEvent.input(within(form).getByLabelText("Meaning"), { target: { value: "" } });
    fireEvent.change(within(form).getByLabelText("Language"), { target: { value: "" } });
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH")).toBe(true));
    expect(calls.find((c) => c.method === "PATCH")).toMatchObject({
      url: "/v1/voice/learn/vl_1",
      body: { word: "Skibigårdsvej", soundsLike: ["skibi gård vej", "skibigor"], meaning: "", lang: null },
    });
    await waitFor(() => expect(screen.queryByRole("form", { name: "Word details" })).toBeNull());
  });

  it("asks before deleting, and deletes", async () => {
    const calls = api();
    show();
    fireEvent.click(await screen.findByRole("button", { name: "Delete Mikkel" }));
    const confirm = await screen.findByRole("group", { name: "Delete “Mikkel”?" });
    fireEvent.click(within(confirm).getByRole("button", { name: "Cancel" }));
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
    expect(screen.getByText("Mikkel")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Delete Mikkel" }));
    fireEvent.click(within(await screen.findByRole("group", { name: "Delete “Mikkel”?" })).getByRole("button", { name: "Yes, delete" }));
    await waitFor(() => expect(screen.queryByText("Mikkel")).toBeNull());
    expect(calls.find((c) => c.method === "DELETE")!.url).toBe("/v1/voice/learn/vl_2");
  });

  it("says when a delete failed", async () => {
    api((c) => (c.method === "DELETE" ? { status: 404, body: { error: { code: "entry_not_found", message: "no such entry" } } } : undefined));
    show();
    fireEvent.click(await screen.findByRole("button", { name: "Delete Mikkel" }));
    fireEvent.click(within(await screen.findByRole("group", { name: "Delete “Mikkel”?" })).getByRole("button", { name: "Yes, delete" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't delete: no such entry");
    expect(screen.getByText("Mikkel")).toBeInTheDocument();
  });

  it("records a sample, shows what Lucina heard and adds it as 'sounds like' in one tap", async () => {
    const calls = api();
    show();
    const form = await open();
    fireEvent.input(within(form).getByLabelText("Word"), { target: { value: "Skibigårdsvej" } });
    fireEvent.change(within(form).getByLabelText("Language"), { target: { value: "da" } });
    fireEvent.click(within(form).getByRole("button", { name: "Record a sample" }));
    expect(await within(form).findByText("Recording… 3 s")).toBeInTheDocument();
    fireEvent.click(within(form).getByRole("button", { name: "Stop" }));

    expect(await within(form).findByText("Lucina heard: “skibi gard vej”")).toBeInTheDocument();
    const sample = calls.find((c) => c.url.includes("/sample"))!;
    expect(sample).toMatchObject({ method: "POST", url: "/v1/voice/learn/sample?lang=da" });
    expect(sample.headers["content-type"]).toBe("audio/wav");

    fireEvent.click(within(form).getByRole("button", { name: "Add as “sounds like”: skibi gard vej" }));
    expect(within(form).getByLabelText("Sounds like")).toHaveValue("skibi gard vej");
    expect(within(form).queryByText(/Lucina heard/)).toBeNull();
    // Nothing was saved by that: only the form changed.
    expect(calls.filter((c) => c.method === "POST" && !c.url.includes("/sample"))).toHaveLength(0);
  });

  it("offers what the engine first heard when it had already fixed the word", async () => {
    api((c) => (c.url.includes("/sample") ? { body: { heard: "Skibigårdsvej", corrections: [{ from: "skibi gard", to: "Skibigårdsvej" }] } } : undefined));
    show();
    const form = await open();
    fireEvent.input(within(form).getByLabelText("Word"), { target: { value: "Skibigårdsvej" } });
    fireEvent.input(within(form).getByLabelText("Sounds like"), { target: { value: "skibigor" } });
    fireEvent.click(within(form).getByRole("button", { name: "Record a sample" }));
    fireEvent.click(await within(form).findByRole("button", { name: "Stop" }));
    fireEvent.click(await within(form).findByRole("button", { name: "Add as “sounds like”: skibi gard" }));
    expect(within(form).getByLabelText("Sounds like")).toHaveValue("skibigor, skibi gard");
  });

  it("explains a silent sample, a refused microphone and a failed decode", async () => {
    api((c) => (c.url.includes("/sample") ? { status: 422, body: { error: { code: "no_speech", message: "no speech was heard" } } } : undefined));
    show();
    const form = await open();
    fireEvent.click(within(form).getByRole("button", { name: "Record a sample" }));
    fireEvent.click(await within(form).findByRole("button", { name: "Stop" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent("No speech was heard. Try again a little closer to the microphone.");

    mic.fail = "denied";
    fireEvent.click(within(form).getByRole("button", { name: "Record again" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent("The microphone isn't available. Allow it in the browser and try again.");
    mic.fail = "failed";
    fireEvent.click(within(form).getByRole("button", { name: "Record again" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent("Couldn't listen to the sample: cannot decode");
    mic.fail = "unsupported";
    fireEvent.click(within(form).getByRole("button", { name: "Record again" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent("This browser can't record from the microphone.");
  });

  it("speaks Italian and Danish", async () => {
    api();
    setLocale("it");
    const { unmount } = show();
    expect(await screen.findByText("Parole che Lucina deve riconoscere nei turni vocali, nelle note vocali e nei memo.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Aggiungi una parola" })).toBeInTheDocument();
    unmount();
    setLocale("da");
    show();
    expect(await screen.findByText("Ord, som Lucina skal genkende i stemmesvar, talebeskeder og memoer.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Tilføj et ord" })).toBeInTheDocument();
  });
});

describe("candidatesFrom", () => {
  it("offers what the engine first heard for the word, else what it heard, cleaned", () => {
    expect(candidatesFrom({ heard: "Skibigårdsvej.", corrections: [{ from: "skibi gard", to: "Skibigårdsvej" }, { from: "other", to: "Else" }] }, "skibigårdsvej")).toEqual(["skibi gard"]);
    expect(candidatesFrom({ heard: "“skibi gard vej!”", corrections: [] }, "Skibigårdsvej")).toEqual(["skibi gard vej"]);
    expect(candidatesFrom({ heard: " ", corrections: [] }, "")).toEqual([]);
    expect(candidatesFrom({ heard: "x", corrections: [{ from: "a", to: "B" }, { from: "c", to: "D" }] }, "")).toEqual(["a", "c"]);
  });
});
