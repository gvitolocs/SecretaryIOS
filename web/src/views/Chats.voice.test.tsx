import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { parseItem, type TimelineItem } from "../api/timeline";
import { putItems } from "../lib/chatdb";
import type { Conversation } from "../lib/chatSync";
import { clearChat, setChatDevice } from "../state/chat";
import { stubFetch, type Call } from "../test/fetch";
import ChatsView from "./Chats";

// The "Lucina Voice" chat (docs/lucinavoice-protocol.md section 7): the one thread all LucinaVoice speakers share.

const now = Date.now();
const voiceConversation: Conversation = { key: "voice", kind: "voice", title: "Lucina Voice", channel: "voice", count: 2, lastId: "v2", lastTs: now, lastRole: "lucina", lastText: "Sono le dieci e dieci." };
const agent: Conversation = { key: "agent:chatgpt", kind: "agent", title: "ChatGPT", channel: "web", count: 1, lastId: "a1", lastTs: now - 5000, lastRole: "user", lastText: "hi" };
const device: Conversation = { key: "device:other", kind: "device", title: "Other browser", channel: "web", device: "other", count: 7, lastId: "1", lastTs: now - 9000, lastRole: "lucina", lastText: "x" };

const heard: TimelineItem = {
  id: "v1",
  ts: now - 2000,
  channel: "voice",
  role: "user",
  text: "Che ore sono a Skibi gard vai",
  meta: { device: "esp", deviceName: "Kitchen speaker", voice: { turnId: "voice-1-ab", sttMs: 84 } },
};
// A reply as Lucina stores it: the speech-to-text time, the route (camelCase) and its layers.
const spoken = parseItem({
  id: "v2",
  ts: now - 1000,
  channel: "voice",
  role: "lucina",
  text: "Sono le dieci e dieci.",
  meta: {
    device: "esp",
    deviceName: "Kitchen speaker",
    voice: {
      turnId: "voice-1-ab",
      ms: { stt: 84 },
      route: {
        intent: "temporal_query",
        fast: true,
        source: "jev",
        confidence: 0.96,
        jevMs: 190,
        layers: [
          { layer: "intent", verdict: "temporal_query", confidence: 0.96, ms: 190, used: true },
          { layer: "vocab", verdict: "none", confidence: 0.4, ms: 120, used: false },
        ],
      },
    },
  },
})!;
// ... and one with every stage of the turn, for a Lucina that keeps them.
const everyStage: TimelineItem = {
  ...spoken,
  id: "v3",
  ts: now - 500,
  meta: { ...spoken.meta, voice: { turnId: "voice-2-cd", ms: { stt: 84, lucina_first: 310, tts_first: 640, first_audio: 700, total: 2100 }, corrections: [{ from: "skibi gard", to: "Skibigårdsvej" }], ambiguous: 1 } },
};

function renderChats(path: string) {
  const history = createMemoryHistory();
  history.set({ value: path, replace: true, scroll: false });
  render(() => (
    <MemoryRouter history={history}>
      <Route path="/chats/:key?" component={ChatsView} />
      <Route path="/" component={() => <p>Main chat</p>} />
    </MemoryRouter>
  ));
  return history;
}

type Handler = (c: Call) => { status?: number; body?: unknown } | undefined;

function api(over: Handler = () => undefined, conversations: Conversation[] = [voiceConversation, agent, device]) {
  return stubFetch((c) => {
    const special = over(c);
    if (special) return special;
    if (c.url.includes("/timeline/conversations")) return { body: { items: conversations } };
    if (c.url.includes("/timeline")) return { body: { items: [spoken, heard], cursor: "v2" } };
    return { status: 404, body: { error: { code: "not_found", message: "Not found." } } };
  });
}

beforeEach(() => {
  clearChat();
  setChatDevice("mine");
});

describe("the Lucina Voice chat in the chat list", () => {
  it("is labelled Voice, with a microphone, like the agent chats and unlike a device chat", async () => {
    api();
    renderChats("/chats");
    const row = (await screen.findByText("Lucina Voice")).closest("button")!;
    expect(within(row).getByText("Voice")).toBeInTheDocument();
    expect(within(row).queryByText("Device")).toBeNull();
    expect(row.querySelector(".conversation-icon")).not.toBeNull();
    expect(screen.getByText("ChatGPT").closest("button")).toHaveTextContent("Agent");
    expect(screen.getByText("Other browser").closest("button")).toHaveTextContent("Device");
    expect(within(row).getByText("Sono le dieci e dieci.")).toBeInTheDocument();
  });

  it("opens the thread read-only, with no way to continue it from here", async () => {
    const history = renderChats("/chats");
    api();
    fireEvent.click((await screen.findByText("Lucina Voice")).closest("button")!);
    await waitFor(() => expect(history.get()).toBe("/chats/voice"));
    expect(await screen.findByText("This conversation lives in Lucina Voice.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Continue here" })).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
  });
});

describe("the Lucina Voice thread", () => {
  it("shows what was heard and what was said, and a timing line that opens into the stages and the layers", async () => {
    await putItems([heard, spoken]);
    const calls = api();
    renderChats("/chats/voice");
    expect(await screen.findByText(heard.text)).toBeInTheDocument();
    expect(screen.getByText(spoken.text)).toBeInTheDocument();
    await waitFor(() => expect(calls.some((c) => c.url.includes("conversation=voice"))).toBe(true));
    expect(screen.getAllByText("Heard by Kitchen speaker")).toHaveLength(1);

    // The layers that fired stay visible; the stages are behind the "Timing" button.
    const layers = screen.getByRole("list", { name: "Jev layers" });
    expect(within(layers).getByText("Intent")).toBeInTheDocument();
    expect(within(layers).getByText("temporal_query")).toBeInTheDocument();
    const vocab = within(layers).getAllByRole("listitem")[1]!;
    expect(vocab).toHaveAttribute("data-used", "false");
    expect(vocab).toHaveAttribute("title", "Vocabulary · none · confidence 40% · 120 ms · not used");

    const timing = screen.getByRole("button", { name: /Timing/ });
    expect(timing).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(timing);
    expect(timing).toHaveAttribute("aria-expanded", "true");
    const list = screen.getByRole("list", { name: "Time from the end of speech" });
    expect([...list.querySelectorAll("li")].map((li) => li.textContent)).toEqual(["STT 84 ms", "Routed as temporal_query (fast)", "Jev 190 ms"]);
    fireEvent.click(timing);
    expect(screen.queryByRole("list", { name: "Time from the end of speech" })).toBeNull();
  });

  it("lists every stage, the total and the Voice Learn counts when the turn has them", async () => {
    await putItems([heard, everyStage]);
    api((c) => (c.url.includes("/timeline") && !c.url.includes("conversations") ? { body: { items: [everyStage, heard], cursor: "v3" } } : undefined));
    renderChats("/chats/voice");
    const timing = await screen.findByRole("button", { name: /Timing/ });
    expect(timing).toHaveTextContent("Timing · 2,100 ms");
    fireEvent.click(timing);
    const list = screen.getByRole("list", { name: "Time from the end of speech" });
    expect([...list.querySelectorAll("li")].map((li) => li.textContent)).toEqual([
      "STT 84 ms",
      "Lucina first text 310 ms",
      "TTS first audio 640 ms",
      "First audio 700 ms",
      "Total 2,100 ms",
      "Voice Learn fixed 1",
      "1 unsure",
    ]);
  });

  it("falls back to the STT time when the turn has only that", async () => {
    const onlyStt: TimelineItem = { ...spoken, meta: { deviceName: "Kitchen speaker", voice: { sttMs: 84 } } };
    await putItems([heard, onlyStt]);
    api((c) => (c.url.includes("/timeline") && !c.url.includes("conversations") ? { body: { items: [onlyStt, heard], cursor: "v2" } } : undefined));
    renderChats("/chats/voice");
    fireEvent.click(await screen.findByRole("button", { name: "Timing" }));
    expect(screen.getByText("Speech to text 84 ms")).toBeInTheDocument();
  });

  it("offers 'Add to Voice Learn' after a transcript edit and adds the suggested entry", async () => {
    await putItems([heard, spoken]);
    const calls = api((c) => {
      if (c.url.endsWith("/v1/voice/learn/suggest")) return { body: { candidates: [{ word: "Skibigårdsvej", soundsLike: ["skibi gard vai"], lang: "da" }] } };
      if (c.url.endsWith("/v1/voice/learn") && c.method === "POST") return { status: 201, body: { id: "vl_1", word: "Skibigårdsvej", soundsLike: ["skibi gard vai"] } };
      return undefined;
    });
    renderChats("/chats/voice");
    fireEvent.click(await screen.findByRole("button", { name: "Edit transcript" }));
    const box = await screen.findByLabelText("Corrected text");
    expect(box).toHaveValue(heard.text);

    // The same text asks nothing.
    fireEvent.click(screen.getByRole("button", { name: "Suggest a word" }));
    expect(await screen.findByText("That is the same text.")).toBeInTheDocument();
    expect(calls.some((c) => c.url.includes("/learn/suggest"))).toBe(false);

    fireEvent.input(box, { target: { value: "Che ore sono a Skibigårdsvej" } });
    fireEvent.click(screen.getByRole("button", { name: "Suggest a word" }));
    const suggestion = await screen.findByRole("group", { name: "Suggested for Voice Learn" });
    expect(suggestion).toHaveTextContent("Skibigårdsvej · sounds like skibi gard vai");
    expect(calls.find((c) => c.url.endsWith("/learn/suggest"))).toMatchObject({ method: "POST", body: { transcript: heard.text, corrected: "Che ore sono a Skibigårdsvej" } });

    fireEvent.input(within(suggestion).getByLabelText("Meaning"), { target: { value: "home street" } });
    fireEvent.click(within(suggestion).getByRole("button", { name: "Add to Voice Learn" }));
    expect(await within(suggestion).findByText("Added to Voice Learn.")).toBeInTheDocument();
    expect(calls.find((c) => c.method === "POST" && c.url.endsWith("/v1/voice/learn"))!.body).toEqual({ word: "Skibigårdsvej", soundsLike: ["skibi gard vai"], meaning: "home street", lang: "da" });
    // History is not rewritten: the message still shows what was heard.
    expect(screen.getByText(heard.text)).toBeInTheDocument();
  });

  it("says when there is nothing to learn, and when the suggestion or the add fails", async () => {
    await putItems([heard, spoken]);
    let mode: "none" | "fail" | "addfail" = "none";
    api((c) => {
      if (c.url.endsWith("/learn/suggest")) {
        if (mode === "none") return { body: { candidates: [] } };
        return mode === "fail" ? { status: 502, body: { error: { code: "lucina_unreachable", message: "Lucina is not reachable" } } } : { body: [{ word: "Mikkel", soundsLike: [] }] };
      }
      if (c.url.endsWith("/v1/voice/learn") && c.method === "POST") return { status: 400, body: { error: { code: "full", message: "the vocabulary is full" } } };
      return undefined;
    });
    renderChats("/chats/voice");
    fireEvent.click(await screen.findByRole("button", { name: "Edit transcript" }));
    const box = await screen.findByLabelText("Corrected text");
    const suggest = () => fireEvent.click(screen.getByRole("button", { name: "Suggest a word" }));

    fireEvent.input(box, { target: { value: "Altro testo" } });
    suggest();
    expect(await screen.findByText("Nothing to learn from that change.")).toBeInTheDocument();

    mode = "fail";
    suggest();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't get a suggestion: Lucina is not reachable");

    mode = "addfail";
    suggest();
    const suggestion = await screen.findByRole("group", { name: "Suggested for Voice Learn" });
    fireEvent.click(within(suggestion).getByRole("button", { name: "Add to Voice Learn" }));
    expect(await within(suggestion).findByRole("alert")).toHaveTextContent("Couldn't add it: the vocabulary is full");
    expect(within(suggestion).getByRole("button", { name: "Add to Voice Learn" })).not.toBeDisabled();
  });

  it("cancelling the edit closes the form", async () => {
    await putItems([heard, spoken]);
    api();
    renderChats("/chats/voice");
    fireEvent.click(await screen.findByRole("button", { name: "Edit transcript" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    expect(screen.queryByLabelText("Corrected text")).toBeNull();
    expect(screen.getByRole("button", { name: "Edit transcript" })).toBeInTheDocument();
  });

  it("leaves other conversations exactly as they were: no timing line, no edit action", async () => {
    const other: TimelineItem = { id: "o1", ts: now, channel: "web", role: "user", text: "A message from the other browser", meta: { device: "other", deviceName: "Other browser" } };
    await putItems([other]);
    api((c) => (c.url.includes("/timeline") && !c.url.includes("conversations") ? { body: { items: [other], cursor: "o1" } } : undefined));
    renderChats("/chats/device%3Aother");
    expect(await screen.findByText(other.text)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit transcript" })).toBeNull();
    expect(screen.queryByRole("button", { name: /Timing/ })).toBeNull();
  });
});
