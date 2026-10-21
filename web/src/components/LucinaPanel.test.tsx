import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { reloadChatFromStorage } from "../state/chat";
import { deferred, stubFetch, type Call, type Handler } from "../test/fetch";
import { fake, FakeVoiceError, loadStage, resetFakeStage } from "../test/stage";
import ChatView from "../views/Chat";

vi.mock("../lib/stageLoader", () => import("../test/stage"));

function serve(chatReply: Handler): Call[] {
  return stubFetch((c) => (c.url.startsWith("/v1/timeline") ? { body: { items: [], nextBefore: null } } : chatReply(c)));
}

const posts = (calls: Call[]) => calls.filter((c) => c.url === "/v1/chat");

async function mounted() {
  render(() => <ChatView />);
  await waitFor(() => expect(fake.stages).toHaveLength(1));
  return fake.stages[0]!;
}

describe("Lucina stage in Chat", () => {
  beforeEach(() => {
    reloadChatFromStorage();
    resetFakeStage();
  });

  it("loads the stage lazily with the private assets, muted on a first visit", async () => {
    serve(() => ({ body: { reply: "x", job: null } }));
    const stage = await mounted();
    expect(loadStage).toHaveBeenCalledTimes(1);
    expect(stage.canvas).toBeInstanceOf(HTMLCanvasElement);
    expect(stage.options).toMatchObject({
      modelUrl: "/v1/assets/lucina.glb",
      manifestUrl: "/v1/assets/manifest.json",
      fetchInit: { credentials: "include", headers: { "X-Secretary-CSRF": "1" } },
      muted: true,
    });
    expect(stage.setMuted).toHaveBeenLastCalledWith(true);
    expect(fake.voiceOptions[0]).toMatchObject({ ttsUrl: "/v1/tts", lang: "it-IT" });
    expect(screen.getByRole("button", { name: "Unmute Lucina" })).toHaveAttribute("aria-pressed", "true");
    // The one-time hint.
    expect(await screen.findByText("Tap Lucina to talk")).toBeInTheDocument();
    expect(localStorage.getItem("secretary.stage.hintSeen")).toBe("1");
  });

  it("thinks while a message is in flight and stays silent while muted", async () => {
    const gate = deferred<void>();
    serve(async () => {
      await gate.promise;
      return { body: { reply: "Ecco fatto.", job: null } };
    });
    const stage = await mounted();
    const box = screen.getByLabelText("Message Lucina");
    fireEvent.input(box, { target: { value: "ciao" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(stage.state).toBe("thinking"));
    gate.resolve();
    await screen.findByText("Ecco fatto.");
    await waitFor(() => expect(stage.state).toBe("idle"));
    expect(fake.speak).not.toHaveBeenCalled();
    expect(stage.speak).not.toHaveBeenCalled();
  });

  it("speaks replies once unmuted, and the choice is remembered", async () => {
    serve(() => ({ body: { reply: "Ecco fatto.", job: null } }));
    const stage = await mounted();
    fireEvent.click(screen.getByRole("button", { name: "Unmute Lucina" }));
    expect(stage.setMuted).toHaveBeenLastCalledWith(false);
    expect(stage.unlockAudio).toHaveBeenCalled();
    expect(localStorage.getItem("lucina-stage:muted")).toBe("0");

    const box = screen.getByLabelText("Message Lucina");
    fireEvent.input(box, { target: { value: "ciao" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(stage.speak).toHaveBeenCalledTimes(1));
    expect(fake.speak).toHaveBeenCalledWith("Ecco fatto.", expect.objectContaining({ lang: "it-IT" }));

    fireEvent.click(screen.getByRole("button", { name: "Mute Lucina" }));
    expect(stage.stopSpeaking).toHaveBeenCalled();
    expect(localStorage.getItem("lucina-stage:muted")).toBe("1");
  });

  it("listens when Lucina is tapped and sends what was heard", async () => {
    const heard = deferred<void>();
    fake.listen.mockImplementation(async () => {
      await heard.promise;
      return "  aggiungi il latte  ";
    });
    const calls = serve(() => ({ body: { reply: "Fatto.", job: null } }));
    const stage = await mounted();
    stage.tap();
    expect(await screen.findByText("Listening…")).toBeInTheDocument();
    expect(stage.state).toBe("listening");
    expect(fake.listen).toHaveBeenCalledWith(expect.objectContaining({ lang: "it-IT" }));

    heard.resolve();
    await screen.findByText("Fatto.");
    expect((posts(calls)[0]!.body as { message: string }).message).toBe("aggiungi il latte");
    expect(stage.states).toContain("thinking");
  });

  it("moves focus to the composer when voice input is not supported", async () => {
    fake.listen.mockRejectedValue(new FakeVoiceError("not-supported"));
    serve(() => ({ status: 404 }));
    const stage = await mounted();
    fireEvent.click(screen.getByRole("button", { name: "Talk to Lucina" }));
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Message Lucina")));
    expect(await screen.findByText(/Type your message instead/)).toBeInTheDocument();
    expect(stage.state).toBe("idle");
  });

  it("hides the stage and frees it, and remembers that", async () => {
    serve(() => ({ status: 404 }));
    const stage = await mounted();
    fireEvent.click(screen.getByRole("button", { name: "Hide Lucina" }));
    expect(stage.dispose).toHaveBeenCalledTimes(1);
    expect(document.querySelector(".stage")).toBeNull();
    expect(localStorage.getItem("secretary.stage.hidden")).toBe("1");

    fireEvent.click(screen.getByRole("button", { name: "Show Lucina" }));
    await waitFor(() => expect(fake.stages).toHaveLength(2));
  });

  it("starts hidden when it was hidden before, without loading three.js", async () => {
    localStorage.setItem("secretary.stage.hidden", "1");
    serve(() => ({ status: 404 }));
    render(() => <ChatView />);
    expect(screen.getByRole("button", { name: "Show Lucina" })).toBeInTheDocument();
    await Promise.resolve();
    expect(loadStage).not.toHaveBeenCalled();
  });

  it("shows her portrait when WebGL is unavailable", async () => {
    fake.failStage = true;
    serve(() => ({ status: 404 }));
    render(() => <ChatView />);
    await waitFor(() => expect(document.querySelector(".stage-portrait img")).toHaveAttribute("src", "/icon-512.png"));
    expect(document.querySelector(".stage-canvas")).toBeNull();
    // Tapping the portrait still starts listening.
    fireEvent.click(document.querySelector(".stage-portrait")!);
    await waitFor(() => expect(fake.listen).toHaveBeenCalledTimes(1));
  });

  it("disposes the stage when Chat unmounts", async () => {
    serve(() => ({ status: 404 }));
    render(() => <ChatView />);
    await waitFor(() => expect(fake.stages).toHaveLength(1));
    const stage = fake.stages[0]!;
    const { cleanup } = await import("@solidjs/testing-library");
    cleanup();
    expect(stage.dispose).toHaveBeenCalledTimes(1);
  });
});
