import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { ChatMessage } from "../state/chat";
import { MessageItem } from "./Chat";

function userMessage(over: Partial<ChatMessage>): ChatMessage {
  return { key: "k1", itemId: "itm_1", role: "user", text: "", createdAt: 1, channel: "telegram", status: "sent", ...over } as ChatMessage;
}

describe("voice note bubble", () => {
  it("shows no file name and no legacy caption, only the player with a Voice message label", () => {
    const { container } = render(() => (
      <MessageItem
        message={userMessage({
          text: "\u{1F4CE} voice-note.wav",
          attachments: [{ kind: "voice", name: "voice-note.wav", mediaId: "m1", durationMs: 5000 }],
        })}
      />
    ));
    expect(container.textContent).not.toContain("voice-note.wav");
    expect(container.textContent).not.toContain("\u{1F4CE}");
    expect(container.textContent).toContain("0:05");
    const play = container.querySelector(".audio-toggle")!;
    expect(play.getAttribute("aria-label")).toContain("Voice message 0:05");
  });

  it("keeps a real caption and the names of other attachments", () => {
    const { container } = render(() => (
      <MessageItem
        message={userMessage({
          text: "Listen",
          attachments: [
            { kind: "voice", name: "voice-note.wav", mediaId: "m1", durationMs: 5000 },
            { kind: "pdf", name: "report.pdf", mediaId: "m2" },
          ],
        })}
      />
    ));
    expect(container.textContent).toContain("Listen");
    expect(container.textContent).toContain("report.pdf");
    expect(container.textContent).not.toContain("voice-note.wav");
  });
});
