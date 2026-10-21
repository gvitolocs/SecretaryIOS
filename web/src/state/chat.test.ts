import { beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { chat, cleanMessage, JOB_POLL_MS, reloadChatFromStorage, sendMessage } from "./chat";

describe("chat jobs", () => {
  beforeEach(() => reloadChatFromStorage());

  it("polls a job every 3 s until it finishes, then keeps the result", async () => {
    vi.useFakeTimers();
    let polls = 0;
    const calls = stubFetch((c) => {
      if (c.url === "/v1/chat") return { body: { reply: "On it.", action: "task.run", job: { id: "j1", status: "queued" } } };
      polls++;
      return polls < 2
        ? { body: { id: "j1", status: "running", action: "task.run" } }
        : { body: { id: "j1", status: "done", action: "task.run", result: { summary: "All done." } } };
    });

    await sendMessage("do the job");
    const reply = () => chat.messages[1]!;
    expect(reply().job).toMatchObject({ id: "j1", status: "queued" });

    await vi.advanceTimersByTimeAsync(JOB_POLL_MS);
    expect(reply().job?.status).toBe("running");
    await vi.advanceTimersByTimeAsync(JOB_POLL_MS);
    expect(reply().job).toMatchObject({ status: "done", result: { summary: "All done." } });

    await vi.advanceTimersByTimeAsync(JOB_POLL_MS * 3);
    expect(calls.filter((c) => c.url === "/v1/jobs/j1")).toHaveLength(2);
  });

  it("records a job's error and imageUrl", async () => {
    vi.useFakeTimers();
    stubFetch((c) =>
      c.url === "/v1/chat"
        ? { body: { reply: "Drawing.", action: "generate_image", job: { id: "j2", status: "queued" } } }
        : { body: { id: "j2", status: "error", action: "generate_image", result: null, error: "interrupted", imageUrl: null } },
    );
    await sendMessage("draw a cat");
    await vi.advanceTimersByTimeAsync(JOB_POLL_MS);
    expect(chat.messages[1]!.job).toMatchObject({ status: "error", error: "interrupted", action: "generate_image" });
  });

  it("cleans messages to the server's rules", () => {
    expect(cleanMessage("  a\r\nb\u0000c\td\u0007 ")).toBe("a\nbc\td");
  });

  it("does not send over-long messages", async () => {
    const calls = stubFetch(() => ({ body: { reply: "x", job: null } }));
    expect(await sendMessage("x".repeat(4001))).toBe(false);
    expect(calls).toHaveLength(0);
  });

  it("marks interrupted sends as failed after a reload", () => {
    sessionStorage.setItem(
      "secretary.chat.v1",
      JSON.stringify([{ id: "m1", role: "user", text: "hi", createdAt: 1, status: "sending" }]),
    );
    reloadChatFromStorage();
    expect(chat.messages[0]).toMatchObject({ status: "error" });
  });

  it("survives broken sessionStorage", () => {
    const spy = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("QuotaExceeded");
    });
    stubFetch(() => ({ body: { reply: "ok", job: null } }));
    return sendMessage("hello").then(() => {
      expect(chat.messages).toHaveLength(2);
      spy.mockRestore();
    });
  });
});
