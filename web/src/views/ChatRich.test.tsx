import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { Route } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { parseRich, speechText } from "../lib/rich";
import { chat, onLucinaReply, reloadChatFromStorage, type ChatMessage } from "../state/chat";
import { RichReply } from "../components/RichReply";
import { stubFetch, type Call, type Handler } from "../test/fetch";
import { renderAt } from "../test/router";
import ChatView from "./Chat";

const EMPTY_PAGE = { body: { items: [], nextBefore: null } };

function serve(rest: Handler, timeline: Handler = () => EMPTY_PAGE): Call[] {
  return stubFetch((c) => (c.url.startsWith("/v1/timeline") ? timeline(c) : rest(c)));
}

const posts = (calls: Call[]) => calls.filter((c) => c.url === "/v1/chat");

function type(text: string) {
  const box = screen.getByLabelText("Message Lucina") as HTMLTextAreaElement;
  fireEvent.input(box, { target: { value: text } });
  fireEvent.keyDown(box, { key: "Enter" });
  return box;
}

/** The owner's 2026-10-22 "inventario" question, answered the way it should look (DeepSeek JSON mode, fixture). */
const INVENTARIO = {
  reply: "plain fallback",
  action: "reply",
  job: null,
  format: "v1",
  lang: "it",
  bubbles: [
    "Ecco l'inventario di ciò che è live ora, senza aver avviato nulla ✅",
    "In sintesi: trigger, canali, memoria e tool ci sono; approvazioni strutturate e morning brief **no** ⚠️",
  ],
  blocks: [
    {
      type: "checklist",
      title: "Live adesso",
      items: [
        { text: "Notifiche app: push PWA, iOS, inbox", done: true },
        { text: "Check-in proattivi 8-22", done: true },
        { text: "Morning brief automatico" },
      ],
    },
    { type: "card", emoji: "⚠️", title: "Limiti certi", tone: "warning", body: "Niente email. Ragiono solo via API DeepSeek." },
    { type: "list", items: ["uno", "due", "tre", "quattro", "cinque", "sei", "sette"].map((text) => ({ text })) },
    { type: "table", columns: ["a", "b"], rows: [["1", "2"]] },
    { type: "stat", items: [{ label: "Peso", value: "82 kg", delta: "-0.4" }] },
    { type: "link_preview", url: "javascript:alert(1)", title: "evil" },
  ],
  actions: [
    { id: "details", label: "Dettagli", emoji: "🔎", kind: "reply", payload: { text: "Dammi i dettagli" } },
    { id: "hint-reminder", label: "Promemoria", emoji: "⏰", kind: "reply", payload: { text: "Ricordami alle 21:30 di", prefill: true } },
    { id: "notes", label: "Note", kind: "route", payload: { path: "/notes" } },
  ],
};

describe("Lucina message format v1 in the chat", () => {
  beforeEach(() => {
    localStorage.clear();
    reloadChatFromStorage();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("parses only what fits the contract", () => {
    const m = parseRich(INVENTARIO)!;
    expect(m.bubbles).toHaveLength(2);
    expect(m.blocks.map((b) => b.type)).toEqual(["checklist", "card", "list", "table", "stat"]);
    expect(parseRich({ ...INVENTARIO, format: "v2" })).toBeNull();
    expect(parseRich({ format: "v1", bubbles: [] })).toBeNull();
    const evil = parseRich({
      format: "v1",
      bubbles: ["x"],
      actions: [
        { id: "a", label: "Out", kind: "route", payload: { path: "https://evil.example" } },
        { id: "b", label: "Up", kind: "route", payload: { path: "/../x" } },
        { id: "c", label: "Ok", kind: "confirm", payload: { answer: "maybe", text: "x" } },
        { id: "d", label: "Yes", kind: "confirm", payload: { answer: "yes", text: "sì" } },
      ],
    })!;
    expect(evil.actions.map((a) => a.id)).toEqual(["d"]);
    expect(speechText(m)).toBe(
      "Ecco l'inventario di ciò che è live ora, senza aver avviato nulla ✅ In sintesi: trigger, canali, memoria e tool ci sono; approvazioni strutturate e morning brief no ⚠️",
    );
  });

  it("renders bubbles, widgets and buttons; the voice gets only the bubbles", async () => {
    const spoken: string[] = [];
    const off = onLucinaReply((t) => spoken.push(t));
    serve(() => ({ body: INVENTARIO }));
    render(() => <ChatView />);
    type("inventario");
    // Bubbles appear one by one; widgets and buttons after the last one.
    await screen.findByText("Ecco l'inventario di ciò che è live ora, senza aver avviato nulla ✅");
    await waitFor(() => expect(document.querySelectorAll(".bubble--rich")).toHaveLength(2), { timeout: 3000 });
    await screen.findByText("Limiti certi");
    expect(document.querySelector(".bubble--rich strong")?.textContent).toBe("no");
    expect(spoken).toEqual([speechText(parseRich(INVENTARIO)!)]);
    off();

    const checklist = document.querySelector(".rw-checklist")!;
    expect(checklist.querySelectorAll("li.is-done")).toHaveLength(2);
    expect(checklist.textContent).toContain("to do");
    expect(document.querySelector(".rw-card")).toHaveAttribute("data-tone", "warning");
    // Long lists collapse after 5 items.
    expect(document.querySelectorAll(".rw-list li")).toHaveLength(5);
    fireEvent.click(screen.getByRole("button", { name: "Show 2 more" }));
    expect(document.querySelectorAll(".rw-list li")).toHaveLength(7);
    expect(document.querySelector(".rw-table td")?.textContent).toBe("1");
    expect(document.querySelector(".rw-stat-value")?.textContent).toBe("82 kg");
    // The unsafe link preview was dropped; the plain fallback text is not shown.
    expect(document.querySelector(".rw-link")).toBeNull();
    expect(screen.queryByText("plain fallback")).toBeNull();
    expect(screen.getByRole("group", { name: "Quick replies" })).toBeInTheDocument();
  });

  it("a reply button sends its text, then the buttons disable and show the choice", async () => {
    let n = 0;
    const calls = serve(() => (++n === 1 ? { body: INVENTARIO } : { body: { reply: "Ecco i dettagli.", job: null } }));
    render(() => <ChatView />);
    type("inventario");
    const details = await screen.findByRole("button", { name: /Dettagli/ }, { timeout: 3000 });
    fireEvent.click(details);
    await screen.findByText("Ecco i dettagli.");
    expect((posts(calls)[1]!.body as { message: string }).message).toBe("Dammi i dettagli");
    const chosen = screen.getByRole("button", { name: "Dettagli (chosen)" });
    expect(chosen).toBeDisabled();
    expect(chosen).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: /Promemoria/ })).toBeDisabled();
  });

  it("a prefill button fills the composer instead of sending", async () => {
    const calls = serve(() => ({ body: INVENTARIO }));
    render(() => <ChatView />);
    type("inventario");
    fireEvent.click(await screen.findByRole("button", { name: /Promemoria/ }, { timeout: 3000 }));
    const box = screen.getByLabelText("Message Lucina") as HTMLTextAreaElement;
    await waitFor(() => expect(box.value).toBe("Ricordami alle 21:30 di "));
    expect(posts(calls)).toHaveLength(1);
  });

  it("confirm buttons answer the pending confirmation with the yes/no words", async () => {
    const confirm = {
      reply: "Confermi?",
      action: "calendar_confirm",
      job: null,
      format: "v1",
      lang: "it",
      bubbles: ["Confermi? Aggiungo «Dentista» domani alle 9:30 su Google Calendar."],
      actions: [
        { id: "confirm-yes", label: "Sì", emoji: "✅", kind: "confirm", payload: { answer: "yes", text: "sì" } },
        { id: "confirm-no", label: "No", emoji: "✖️", kind: "confirm", payload: { answer: "no", text: "no" } },
        { id: "confirm-edit", label: "Modifica", emoji: "✏️", kind: "confirm", payload: { answer: "edit", text: "Modifica:" } },
      ],
    };
    let n = 0;
    const calls = serve(() => (++n === 1 ? { body: confirm } : { body: { reply: "Fatto: è in calendario.", job: null } }));
    render(() => <ChatView />);
    type("aggiungi dentista domani alle 9:30");
    fireEvent.click(await screen.findByRole("button", { name: /Sì/ }));
    await screen.findByText("Fatto: è in calendario.");
    expect((posts(calls)[1]!.body as { message: string }).message).toBe("sì");
    expect(screen.getByRole("button", { name: "Sì (chosen)" })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Modifica/ })).toBeDisabled();
  });

  it("history renders at once, old plain replies lose their asterisks, typed answers count as the choice", async () => {
    const now = Date.now();
    const at = (ago: number) => new Date(now - ago).toISOString();
    const items = [
      { id: "4", ts: at(1000), channel: "web", role: "user", text: "Dammi i dettagli" },
      { id: "3", ts: at(2000), channel: "web", role: "lucina", text: "fallback", meta: { ...INVENTARIO, format: "v1" } },
      { id: "2", ts: at(3000), channel: "web", role: "user", text: "inventario" },
      { id: "1", ts: at(4000), channel: "telegram", role: "lucina", text: "**Inventario fattuale**\n\n- uno\n- due" },
    ];
    serve(() => ({ status: 404 }), () => ({ body: { items, nextBefore: null } }));
    render(() => <ChatView />);
    await screen.findByText("Limiti certi");
    expect(document.querySelectorAll(".bubble--rich")).toHaveLength(2);
    expect(document.querySelector(".bubble--typing")).toBeNull();
    // The old wall: bold rendered, no literal "**".
    const old = screen.getByText("Inventario fattuale");
    expect(old.tagName).toBe("STRONG");
    expect(document.querySelector('.msg[data-key="s:1"]')?.textContent).not.toContain("**");
    // The owner typed the button's text: that button is the choice.
    expect(screen.getByRole("button", { name: "Dettagli (chosen)" })).toBeDisabled();
  });

  it("reveals bubbles one by one with a typing pause, or all at once with reduced motion", () => {
    vi.useFakeTimers();
    const rich = parseRich({ format: "v1", bubbles: ["Uno.", "Due.", "Tre."], blocks: [{ type: "quote", text: "q" }] })!;
    const message: ChatMessage = { key: "r:x", role: "lucina", text: "", createdAt: Date.now(), channel: "web", rich, reveal: true };
    const { unmount } = render(() => <RichReply message={message} rich={rich} />);
    expect(document.querySelectorAll(".bubble--rich")).toHaveLength(1);
    expect(document.querySelector(".rw-quote")).toBeNull();
    vi.advanceTimersByTime(200);
    expect(document.querySelector(".bubble--typing")).not.toBeNull();
    vi.advanceTimersByTime(3000);
    expect(document.querySelectorAll(".bubble--rich")).toHaveLength(3);
    expect(document.querySelector(".bubble--typing")).toBeNull();
    expect(document.querySelector(".rw-quote")).not.toBeNull();
    unmount();

    vi.stubGlobal("matchMedia", (q: string) => ({ matches: q.includes("reduce"), media: q, addEventListener() {}, removeEventListener() {} }));
    render(() => <RichReply message={message} rich={rich} />);
    expect(document.querySelectorAll(".bubble--rich")).toHaveLength(3);
    vi.unstubAllGlobals();
  });

  it("renders a place block as a place card opening the in-app map", async () => {
    stubFetch(() => ({ status: 404, body: {} }));
    const rich = parseRich({
      format: "v1",
      bubbles: ["Domani sera:"],
      blocks: [{ type: "place", title: "Hack Night", address: "Folkehuset Møllestien, Aarhus", lat: 56.1611, lng: 10.2006, kind: "event" }],
    })!;
    const message: ChatMessage = { key: "r:p", role: "lucina", text: "", createdAt: Date.now(), channel: "web", rich };
    render(() => <RichReply message={message} rich={rich} />);
    const card = screen.getByRole("link", { name: /Hack Night/ });
    expect(card).toHaveClass("place-card");
    expect(card).toHaveTextContent("Folkehuset Møllestien, Aarhus");
    expect(card.getAttribute("href")).toMatch(/^\/map\/place\?lat=56\.1611&lng=10\.2006&title=Hack\+Night/);
  });

  it("a route button navigates in the app", async () => {
    const rich = parseRich(INVENTARIO)!;
    const message: ChatMessage = { key: "s:9", itemId: "9", role: "lucina", text: "", createdAt: Date.now(), channel: "web", rich };
    renderAt("/", () => (
      <>
        <Route path="/" component={() => <RichReply message={message} rich={rich} />} />
        <Route path="/notes" component={() => <p>Notes page</p>} />
      </>
    ));
    fireEvent.click(await screen.findByRole("button", { name: "Note" }));
    expect(await screen.findByText("Notes page")).toBeInTheDocument();
    expect(chat.messages).toHaveLength(0);
  });
});
