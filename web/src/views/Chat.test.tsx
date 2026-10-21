import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { chat, reloadChatFromStorage } from "../state/chat";
import { deferred, stubFetch } from "../test/fetch";
import ChatView from "./Chat";

function type(text: string) {
  const box = screen.getByLabelText("Message Lucina") as HTMLTextAreaElement;
  fireEvent.input(box, { target: { value: text } });
  fireEvent.keyDown(box, { key: "Enter" });
  return box;
}

describe("Chat", () => {
  beforeEach(() => reloadChatFromStorage());

  it("shows an optimistic bubble, a thinking indicator, then the reply as plain text", async () => {
    const gate = deferred<void>();
    const calls = stubFetch(async () => {
      await gate.promise;
      return { body: { reply: "Line one\n<b>not bold</b> https://example.com", action: null, job: null } };
    });
    render(() => <ChatView />);
    const box = type("Hello Lucina");

    expect(await screen.findByText("Hello Lucina")).toBeInTheDocument();
    expect(screen.getAllByText("Lucina is thinking…").length).toBeGreaterThan(0);
    expect(box).toHaveAttribute("readonly");
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();

    gate.resolve();
    const link = await screen.findByRole("link", { name: "https://example.com" });
    expect(link).toHaveAttribute("href", "https://example.com/");
    expect(link).toHaveAttribute("rel", expect.stringContaining("noopener"));
    // Markup in replies stays text.
    expect(document.querySelector(".msg--lucina b")).toBeNull();
    expect(document.querySelector(".msg--lucina .bubble")?.textContent).toBe(
      "Line one\n<b>not bold</b> https://example.com",
    );
    // The live region announces the new reply.
    expect(document.querySelector("[aria-live=polite]")?.textContent).toContain("Lucina: Line one");

    const body = calls[0]!.body as { message: string; sessionId: string; clientMessageId: string };
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
    expect(body.message).toBe("Hello Lucina");
    expect(body.sessionId).toMatch(/[0-9a-f-]{36}/);
    expect(body.clientMessageId).toMatch(/[0-9a-f-]{36}/);
    await waitFor(() => expect(box).not.toHaveAttribute("readonly"));

    // Mirrored to sessionStorage.
    const stored = JSON.parse(sessionStorage.getItem("secretary.chat.v1") ?? "[]") as unknown[];
    expect(stored).toHaveLength(2);
  });

  it("Shift+Enter does not send", () => {
    const calls = stubFetch(() => ({ body: { reply: "x", job: null } }));
    render(() => <ChatView />);
    const box = screen.getByLabelText("Message Lucina");
    fireEvent.input(box, { target: { value: "draft" } });
    fireEvent.keyDown(box, { key: "Enter", shiftKey: true });
    expect(calls).toHaveLength(0);
  });

  it("offers retry on failure and resends with the same clientMessageId", async () => {
    let attempt = 0;
    const calls = stubFetch(() => {
      attempt++;
      return attempt === 1
        ? { status: 502, body: { error: { code: "lucina_unreachable", message: "down" } } }
        : { body: { reply: "Back again.", action: null, job: null } };
    });
    render(() => <ChatView />);
    type("Are you there?");

    const retry = await screen.findByRole("button", { name: "Retry" });
    expect(screen.getByText("Lucina is offline. Not delivered.")).toBeInTheDocument();
    fireEvent.click(retry);

    expect(await screen.findByText("Back again.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    const ids = calls.map((c) => (c.body as { clientMessageId: string }).clientMessageId);
    expect(ids[0]).toBe(ids[1]);
    expect(chat.messages.map((m) => m.role)).toEqual(["user", "lucina"]);
  });
});
