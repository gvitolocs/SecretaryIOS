import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { WIDE_QUERY } from "../lib/media";
import { chat, reloadChatFromStorage } from "../state/chat";
import { reloadStageFromStorage } from "../state/stage";
import { deferred, stubFetch, type Call, type Handler } from "../test/fetch";
import { fake, FakeVoiceError, loadStage, resetFakeStage } from "../test/stage";
import ChatView from "../views/Chat";
import Shell from "../views/Shell";
import { bubbleDuration } from "./LucinaDock";

// No WebGL or audio in tests: the Lucina stage is a fake.
vi.mock("../lib/stageLoader", () => import("../test/stage"));

/** matchMedia whose WIDE_QUERY answer can be flipped at runtime. */
function viewport(wide: boolean) {
  const listeners = new Set<(e: MediaQueryListEvent) => void>();
  const state = { wide };
  vi.stubGlobal("matchMedia", (q: string) => ({
    get matches() {
      return q === WIDE_QUERY ? state.wide : false;
    },
    media: q,
    addEventListener: (_: string, fn: (e: MediaQueryListEvent) => void) => q === WIDE_QUERY && listeners.add(fn),
    removeEventListener: (_: string, fn: (e: MediaQueryListEvent) => void) => listeners.delete(fn),
  }));
  return {
    set(next: boolean) {
      state.wide = next;
      for (const fn of listeners) fn({ matches: next } as MediaQueryListEvent);
    },
  };
}

function serve(chatReply: Handler = () => ({ status: 404 })): Call[] {
  return stubFetch((c) => {
    if (c.url.startsWith("/v1/timeline")) return { body: { items: [], nextBefore: null } };
    if (c.url === "/v1/health") return { body: { ok: true, lucina: { reachable: true } } };
    if (c.url === "/v1/chat") return chatReply(c);
    return { status: 404 };
  });
}

const posts = (calls: Call[]) => calls.filter((c) => c.url === "/v1/chat");

function renderShell(path = "/") {
  const history = createMemoryHistory();
  history.set({ value: path, scroll: false, replace: true });
  render(() => (
    <MemoryRouter history={history} root={(p) => <Shell>{p.children}</Shell>}>
      <Route path="/" component={ChatView} />
      <Route path="/shopping" component={() => <p>Shopping page</p>} />
      <Route path="/training" component={() => <p>Training page</p>} />
      <Route path="/plugins" component={() => <p>Plugins page</p>} />
    </MemoryRouter>
  ));
}

function go(name: string) {
  fireEvent.click(screen.getAllByRole("link", { name })[0]!);
}

const dock = () => document.querySelector<HTMLElement>("aside.dock");

async function mounted(path = "/") {
  renderShell(path);
  await waitFor(() => expect(fake.stages).toHaveLength(1));
  return fake.stages[0]!;
}

function say(text: string) {
  const box = screen.getByLabelText("Message Lucina");
  fireEvent.input(box, { target: { value: text } });
  fireEvent.keyDown(box, { key: "Enter" });
}

describe("Lucina in the shell", () => {
  beforeEach(() => {
    reloadChatFromStorage();
    reloadStageFromStorage();
    resetFakeStage();
  });

  it("keeps one stage for the whole session while navigating", async () => {
    viewport(true);
    serve();
    const stage = await mounted();
    for (const page of ["Shopping", "Training", "Plugins", "Chat", "Training"]) {
      go(page);
      await waitFor(() => expect(dock()).not.toBeNull());
    }
    expect(await screen.findByText("Training page")).toBeInTheDocument();
    expect(loadStage).toHaveBeenCalledTimes(1);
    expect(fake.stages).toHaveLength(1);
    expect(stage.dispose).not.toHaveBeenCalled();
    expect(document.querySelectorAll("canvas.stage-canvas")).toHaveLength(1);
  });

  it("inside Secretary Code: hidden on plugin pages, the stage still mounted", async () => {
    viewport(true);
    serve();
    window.secretaryShell = { version: 1, showCode: vi.fn() };
    try {
      const stage = await mounted("/shopping");
      expect(dock()).toHaveAttribute("hidden");
      expect(dock()).toHaveClass("dock--offstage");
      expect(stage.dispose).not.toHaveBeenCalled();
    } finally {
      delete window.secretaryShell;
    }
  });

  it("inside Secretary Code: on stage in the chat", async () => {
    viewport(true);
    serve();
    window.secretaryShell = { version: 1, showCode: vi.fn() };
    try {
      await mounted("/");
      expect(dock()).not.toHaveAttribute("hidden");
      expect(dock()).not.toHaveClass("dock--offstage");
    } finally {
      delete window.secretaryShell;
    }
  });

  it("desktop: a full-body column beside the page, which keeps its own column", async () => {
    viewport(true);
    serve();
    const stage = await mounted("/gym");
    expect(dock()).toHaveClass("dock--side");
    expect(stage.options.framing).toBe("full");
    // The page and Lucina are siblings in the shell body: nothing overlays the page.
    const body = document.querySelector(".shell-body")!;
    expect([...body.children].map((c) => c.tagName)).toEqual(["MAIN", "ASIDE"]);
    expect(dock()!.querySelector(".dock-stage")).not.toHaveAttribute("hidden");
    expect(screen.queryByRole("button", { name: "Open Lucina" })).toBeNull();
  });

  it("mobile: a bust strip on Chat, a floating avatar elsewhere that expands", async () => {
    viewport(false);
    serve();
    const stage = await mounted("/");
    expect(dock()).toHaveClass("dock--strip");
    expect(stage.options.framing).toBe("bust");

    go("Shopping");
    await screen.findByText("Shopping page");
    expect(dock()).toHaveClass("dock--float");
    expect(dock()!.querySelector(".dock-stage")).toHaveAttribute("hidden");
    fireEvent.click(screen.getByRole("button", { name: "Open Lucina" }));
    expect(dock()).toHaveClass("dock--open");
    expect(dock()!.querySelector(".dock-stage")).not.toHaveAttribute("hidden");

    fireEvent.click(screen.getByRole("button", { name: "Minimize Lucina" }));
    expect(dock()).not.toHaveClass("dock--open");
    expect(screen.getByRole("button", { name: "Open Lucina" })).toBeInTheDocument();
    expect(fake.stages).toHaveLength(1);
    expect(stage.dispose).not.toHaveBeenCalled();
  });

  it("follows the breakpoint live without re-creating the stage", async () => {
    const vp = viewport(false);
    serve();
    const stage = await mounted("/gym");
    expect(dock()).toHaveClass("dock--float");
    vp.set(true);
    expect(dock()).toHaveClass("dock--side");
    expect(stage.setFraming).toHaveBeenLastCalledWith("full");
    vp.set(false);
    expect(dock()).toHaveClass("dock--float");
    expect(stage.setFraming).toHaveBeenLastCalledWith("bust");
    expect(fake.stages).toHaveLength(1);
  });

  it("talks from any page: the reply shows as a bubble and lands in the timeline", async () => {
    viewport(true);
    const calls = serve(() => ({ body: { reply: "Latte aggiunto.", job: null } }));
    fake.listen.mockImplementation(async () => "aggiungi il latte");
    const stage = await mounted("/shopping");
    fireEvent.click(screen.getByRole("button", { name: "Talk to Lucina" }));
    expect(await screen.findByText("Latte aggiunto.")).toBeInTheDocument();
    expect((posts(calls)[0]!.body as { message: string }).message).toBe("aggiungi il latte");
    expect(chat.messages.map((m) => [m.role, m.text])).toEqual([
      ["user", "aggiungi il latte"],
      ["lucina", "Latte aggiunto."],
    ]);
    expect(stage.states).toContain("thinking");

    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByText("Latte aggiunto.")).toBeNull();

    // On Chat the transcript shows it; no bubble.
    go("Chat");
    await screen.findByLabelText("Message Lucina");
    say("ancora");
    await waitFor(() => expect(screen.getAllByText("Latte aggiunto.")).toHaveLength(2));
    expect(document.querySelector(".dock-bubble")).toBeNull();
  });

  it("dismisses the bubble by itself after a while", () => {
    expect(bubbleDuration("ok")).toBe(6_000);
    expect(bubbleDuration("x".repeat(200))).toBe(12_000);
    expect(bubbleDuration("x".repeat(5_000))).toBe(20_000);
  });

  it("loads the stage lazily with the private assets, muted on a first visit", async () => {
    viewport(true);
    serve();
    const stage = await mounted();
    expect(stage.options).toMatchObject({
      modelUrl: "/v1/assets/lucina.glb",
      manifestUrl: "/v1/assets/manifest.json",
      fetchInit: { credentials: "include", headers: { "X-Secretary-CSRF": "1" } },
      muted: true,
    });
    expect(stage.setMuted).toHaveBeenLastCalledWith(true);
    expect(fake.voiceOptions[0]).toMatchObject({ ttsUrl: "/v1/tts", lang: "en-US" });
    expect(screen.getByRole("button", { name: "Unmute Lucina" })).toHaveAttribute("aria-pressed", "true");
    expect(await screen.findByText("Drag to turn her, pinch to zoom, tap her. The mic button talks.")).toBeInTheDocument();
    expect(localStorage.getItem("secretary.stage.hintSeen")).toBe("1");
  });

  it("thinks while a message is in flight and stays silent while muted", async () => {
    viewport(true);
    const gate = deferred<void>();
    serve(async () => {
      await gate.promise;
      return { body: { reply: "Ecco fatto.", job: null } };
    });
    const stage = await mounted();
    say("ciao");
    await waitFor(() => expect(stage.state).toBe("thinking"));
    gate.resolve();
    await screen.findByText("Ecco fatto.");
    await waitFor(() => expect(stage.state).toBe("idle"));
    expect(fake.speak).not.toHaveBeenCalled();
    expect(stage.speak).not.toHaveBeenCalled();
  });

  it("speaks replies once unmuted, and the choice is remembered", async () => {
    viewport(true);
    serve(() => ({ body: { reply: "Ecco fatto.", job: null } }));
    const stage = await mounted();
    fireEvent.click(screen.getByRole("button", { name: "Unmute Lucina" }));
    expect(stage.setMuted).toHaveBeenLastCalledWith(false);
    expect(stage.unlockAudio).toHaveBeenCalled();
    expect(localStorage.getItem("lucina-stage:muted")).toBe("0");

    say("ciao");
    await waitFor(() => expect(stage.speak).toHaveBeenCalledTimes(1));
    expect(fake.speak).toHaveBeenCalledWith("Ecco fatto.", expect.objectContaining({ lang: "en-US" }));

    fireEvent.click(screen.getByRole("button", { name: "Mute Lucina" }));
    expect(stage.stopSpeaking).toHaveBeenCalled();
    expect(localStorage.getItem("lucina-stage:muted")).toBe("1");
  });

  it("listens from the mic button and shows that it is listening", async () => {
    viewport(true);
    const heard = deferred<void>();
    fake.listen.mockImplementation(async () => {
      await heard.promise;
      return "  aggiungi il latte  ";
    });
    const calls = serve(() => ({ body: { reply: "Fatto.", job: null } }));
    const stage = await mounted();
    fireEvent.click(screen.getByRole("button", { name: "Talk to Lucina" }));
    expect(await screen.findByText("Listening…")).toBeInTheDocument();
    expect(stage.state).toBe("listening");
    heard.resolve();
    await screen.findByText("Fatto.");
    expect((posts(calls)[0]!.body as { message: string }).message).toBe("aggiungi il latte");
  });

  it("is interactive; a touch speaks her short line only when her voice is on", async () => {
    viewport(true);
    serve();
    const stage = await mounted();
    expect(stage.options.interactive).toBe(true);
    // Muted (the first-visit default): the reaction plays, no TTS request, no sound.
    stage.touch("head", "pat");
    await Promise.resolve();
    expect(fake.speak).not.toHaveBeenCalled();
    expect(fake.listen).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Unmute Lucina" }));
    stage.touch("head", "pat");
    await waitFor(() => expect(stage.speak).toHaveBeenCalledTimes(1));
    expect(fake.speak).toHaveBeenCalledWith("Ehehe…", expect.anything());
    // The same line again comes from the cache.
    stage.touch("head", "pat");
    await waitFor(() => expect(stage.speak).toHaveBeenCalledTimes(2));
    expect(fake.speak).toHaveBeenCalledTimes(1);
  });

  it("without voice input, takes you to the Chat composer", async () => {
    viewport(true);
    fake.listen.mockRejectedValue(new FakeVoiceError("not-supported"));
    serve();
    await mounted("/gym");
    fireEvent.click(screen.getByRole("button", { name: "Talk to Lucina" }));
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Message Lucina")));
    expect(await screen.findByText(/Type your message instead/)).toBeInTheDocument();
  });

  it("hides the stage and frees it; the header brings her back", async () => {
    viewport(true);
    serve();
    const stage = await mounted();
    fireEvent.click(screen.getByRole("button", { name: "Hide Lucina" }));
    expect(stage.dispose).toHaveBeenCalledTimes(1);
    expect(dock()).toBeNull();
    expect(localStorage.getItem("secretary.stage.hidden")).toBe("1");

    go("Shopping");
    await screen.findByText("Shopping page");
    expect(fake.stages).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Show Lucina" }));
    await waitFor(() => expect(fake.stages).toHaveLength(2));
  });

  it("starts hidden when it was hidden before, without loading three.js", async () => {
    viewport(true);
    localStorage.setItem("secretary.stage.hidden", "1");
    reloadStageFromStorage();
    serve();
    renderShell("/");
    expect(screen.getByRole("button", { name: "Show Lucina" })).toBeInTheDocument();
    await Promise.resolve();
    expect(loadStage).not.toHaveBeenCalled();
  });

  it("shows her portrait when WebGL is unavailable, and tapping it still listens", async () => {
    viewport(true);
    fake.failStage = true;
    serve();
    renderShell("/");
    await waitFor(() => expect(document.querySelector(".stage-portrait img")).toHaveAttribute("src", "/icon-512.png"));
    expect(document.querySelector(".stage-canvas")).toBeNull();
    fireEvent.click(document.querySelector(".stage-portrait")!);
    await waitFor(() => expect(fake.listen).toHaveBeenCalledTimes(1));
  });

  it("disposes the stage when the shell unmounts (logout)", async () => {
    viewport(true);
    serve();
    const stage = await mounted();
    const { cleanup } = await import("@solidjs/testing-library");
    cleanup();
    expect(stage.dispose).toHaveBeenCalledTimes(1);
  });
});

describe("Lucina speaks the app's language", () => {
  beforeEach(() => {
    reloadChatFromStorage();
    reloadStageFromStorage();
    resetFakeStage();
  });

  it("uses it-IT / da-DK for recognition and TTS, and translates her controls", async () => {
    viewport(true);
    serve(() => ({ body: { reply: "Fatto.", job: null } }));
    setLocale("it");
    await mounted();
    expect(fake.voiceOptions[0]).toMatchObject({ lang: "it-IT" });
    expect(screen.getByRole("button", { name: "Riattiva l'audio di Lucina" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Parla con Lucina" }));
    await waitFor(() => expect(fake.listen).toHaveBeenCalledWith(expect.objectContaining({ lang: "it-IT" })));

    setLocale("da");
    fireEvent.click(screen.getByRole("button", { name: "Slå lyden til for Lucina" }));
    await screen.findAllByText("Fatto.");
    const box = screen.getByLabelText("Skriv til Lucina");
    fireEvent.input(box, { target: { value: "hej" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(fake.speak).toHaveBeenLastCalledWith("Fatto.", expect.objectContaining({ lang: "da-DK" })));
  });
});
