import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { parseRich } from "../lib/rich";
import { chat, reloadChatFromStorage, type ChatMessage } from "../state/chat";
import { refreshSecretQueue, resetSecretQueue } from "../state/secretRequests";
import { stubFetch, type Call } from "../test/fetch";
import { RichReply } from "./RichReply";

const VALUE = "correct-horse-battery-staple-synthetic";
const CATALOG = {
  projects: [{ id: "p-os", name: "LucinaOS", slug: "lucinaos-x1", alias: "lucinaos", environments: [{ slug: "dev", name: "Dev" }, { slug: "prod", name: "Prod" }] }],
};

type Status = "queued" | "active" | "saved" | "dismissed" | "expired";
interface Row { id: string; name: string; project: string; environment: string; path: string; reason: string; who: string; status: Status; fingerprint?: string; action?: string }

function row(id: string, name: string, status: Status = "active", extra: Partial<Row> = {}): Row {
  return { id, name, project: "lucinaos", environment: "prod", path: "/", reason: "The laptop joins the Wi-Fi", who: "claude-code", status, ...extra };
}

/** A server with a queue; a save or a dismiss closes the request, as Lucina does. */
function server(rows: Row[], opts: { exists?: Set<string>; catalog?: typeof CATALOG } = {}): Call[] {
  return stubFetch((c) => {
    if (c.url.startsWith("/v1/timeline")) return { body: { items: [], nextBefore: null } };
    if (c.url === "/v1/secrets/catalog") return { body: opts.catalog ?? CATALOG };
    if (c.url === "/v1/secrets/requests") {
      return { body: { requests: rows, active: rows.find((r) => r.status === "active")?.id ?? null, queued: rows.filter((r) => r.status === "queued").length } };
    }
    const dismiss = /^\/v1\/secrets\/requests\/(.+)\/dismiss$/.exec(c.url);
    if (dismiss) {
      const r = rows.find((x) => x.id === dismiss[1])!;
      r.status = "dismissed";
      return { body: { status: "dismissed", nextItemId: null } };
    }
    if (c.url === "/v1/secrets" && c.method === "POST") {
      const b = c.body as { name: string; overwrite: boolean; request: string };
      if (opts.exists?.has(b.name) && !b.overwrite) return { status: 409, body: { error: { code: "exists", message: "exists" } } };
      const r = rows.find((x) => x.id === b.request);
      if (r) Object.assign(r, { status: "saved", name: b.name, fingerprint: "ab12cd34", action: b.overwrite ? "updated" : "created" });
      return { body: { key: b.name, project: { id: "p-os", name: "LucinaOS" }, environment: "prod", path: "/", action: "created", fingerprint: "ab12cd34", receipt: "Saved", nextItemId: null } };
    }
    return { status: 404, body: {} };
  });
}

function message(id: string, _name: string): ChatMessage {
  return { key: `k-${id}`, itemId: `itm-${id}`, role: "lucina", text: "card", createdAt: 1, channel: "web" } as ChatMessage;
}

function rich(id: string, name: string) {
  return parseRich({
    format: "v1",
    bubbles: [`claude-code asks for the secret ${name}`],
    actions: [
      { id: "secret-request", label: `Add ${name}`, kind: "secret", payload: { mode: "save", name, project: "lucinaos", environment: "prod", path: "/", target: "lucinaos/prod", request: id } },
      { id: "secret-request-dismiss", label: "Not now", kind: "dismiss", payload: {} },
    ],
  })!;
}

function storageText(): string {
  const all: string[] = [];
  for (const s of [localStorage, sessionStorage]) for (let i = 0; i < s.length; i++) all.push(s.getItem(s.key(i)!) ?? "");
  return all.join("\n");
}

const field = (name = "WIFI_PSK") => screen.findByLabelText(`Value of ${name}`) as Promise<HTMLTextAreaElement>;
const ok = () => screen.getByRole("button", { name: "OK" }) as HTMLButtonElement;

describe("the inline secret card", () => {
  beforeEach(() => {
    localStorage.clear();
    sessionStorage.clear();
    reloadChatFromStorage();
    resetSecretQueue();
  });

  it("parses the request and keeps the old sheet's fields", () => {
    const r = rich("sr-1", "WIFI_PSK");
    const a = r.actions[0]!;
    expect(a.kind).toBe("secret");
    if (a.kind === "secret") {
      expect(a.payload).toEqual({ mode: "save", name: "WIFI_PSK", project: "lucinaos", environment: "prod", path: "/", target: "lucinaos/prod", request: "sr-1" });
    }
  });

  it("paste, OK, saved: no confirm step, the value goes only in the save's body, and nowhere after", async () => {
    const rows = [row("sr-1", "WIFI_PSK")];
    const calls = server(rows);
    render(() => <RichReply message={message("sr-1", "WIFI_PSK")} rich={rich("sr-1", "WIFI_PSK")} />);
    const box = await field();
    expect(screen.getByText(/Enter the secret for WIFI_PSK/)).toBeTruthy();
    expect(screen.getByText("claude-code: The laptop joins the Wi-Fi")).toBeTruthy();
    expect(box.getAttribute("autocomplete")).toBe("off");
    expect(box.getAttribute("spellcheck")).toBe("false");
    expect(box.classList.contains("secret-value--hidden")).toBe(true);
    // Old-client bubble and buttons aren't shown.
    expect(screen.queryByText(/asks for the secret/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Add WIFI_PSK" })).toBeNull();
    expect(ok().disabled).toBe(true);

    fireEvent.input(box, { target: { value: `${VALUE}\n` } });
    await waitFor(() => expect(ok().disabled).toBe(false));
    fireEvent.click(ok());
    await screen.findByText("✅ WIFI_PSK saved. Fingerprint ab12cd34.");

    const saves = calls.filter((c) => c.url === "/v1/secrets");
    expect(saves).toHaveLength(1);
    expect(saves[0]!.body).toEqual({
      name: "WIFI_PSK", project: "p-os", environment: "prod", path: "/", overwrite: false, createFolder: false,
      value: VALUE, request: "sr-1", confirmed: true,
    });
    // Everything else the client did knows nothing of the value: no URL, no other body, no chat send.
    for (const c of calls.filter((x) => x.url !== "/v1/secrets")) {
      expect(c.url).not.toContain(VALUE);
      expect(JSON.stringify(c.body ?? null)).not.toContain(VALUE);
    }
    expect(calls.some((c) => c.url === "/v1/chat")).toBe(false);
    expect(storageText()).not.toContain(VALUE);
    expect(JSON.stringify(chat.messages)).not.toContain(VALUE);
    expect(document.body.innerHTML).not.toContain(VALUE);
    expect(screen.queryByLabelText("Value of WIFI_PSK")).toBeNull();
  });

  it("Enter in the field saves through the card, never through the chat", async () => {
    const rows = [row("sr-1", "WIFI_PSK")];
    const calls = server(rows);
    render(() => <RichReply message={message("sr-1", "WIFI_PSK")} rich={rich("sr-1", "WIFI_PSK")} />);
    const box = await field();
    fireEvent.input(box, { target: { value: VALUE } });
    await waitFor(() => expect(ok().disabled).toBe(false));
    fireEvent.keyDown(box, { key: "Enter" });
    await screen.findByText(/WIFI_PSK saved/);
    expect(calls.some((c) => c.url === "/v1/chat")).toBe(false);
  });

  it("a refresh of the queue keeps what he pasted", async () => {
    server([row("sr-1", "WIFI_PSK"), row("sr-2", "NEXT", "queued")]);
    render(() => <RichReply message={message("sr-1", "WIFI_PSK")} rich={rich("sr-1", "WIFI_PSK")} />);
    const box = await field();
    fireEvent.input(box, { target: { value: VALUE } });
    await refreshSecretQueue();
    await refreshSecretQueue();
    expect(await field()).toBe(box);
    expect(box.value).toBe(VALUE);
    // Reset for storage/DOM checks in the other tests.
    fireEvent.input(box, { target: { value: "" } });
  });

  it("replacing an existing secret takes one more tap", async () => {
    const rows = [row("sr-1", "WIFI_PSK")];
    const calls = server(rows, { exists: new Set(["WIFI_PSK"]) });
    render(() => <RichReply message={message("sr-1", "WIFI_PSK")} rich={rich("sr-1", "WIFI_PSK")} />);
    const box = await field();
    fireEvent.input(box, { target: { value: VALUE } });
    await waitFor(() => expect(ok().disabled).toBe(false));
    fireEvent.click(ok());
    const replace = (await screen.findByRole("button", { name: "Replace" })) as HTMLButtonElement;
    expect(rows[0]!.status).toBe("active");
    fireEvent.click(replace);
    await screen.findByText("✅ WIFI_PSK replaced. Fingerprint ab12cd34.");
    const saves = calls.filter((c) => c.url === "/v1/secrets").map((c) => (c.body as { overwrite: boolean }).overwrite);
    expect(saves).toEqual([false, true]);
  });

  it("Not now dismisses on the server and clears the value", async () => {
    const rows = [row("sr-1", "WIFI_PSK")];
    const calls = server(rows);
    render(() => <RichReply message={message("sr-1", "WIFI_PSK")} rich={rich("sr-1", "WIFI_PSK")} />);
    const box = await field();
    fireEvent.input(box, { target: { value: VALUE } });
    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    await screen.findByText("WIFI_PSK: not saved.");
    expect(calls.some((c) => c.url === "/v1/secrets/requests/sr-1/dismiss")).toBe(true);
    expect(calls.some((c) => c.url === "/v1/secrets" && c.method === "POST")).toBe(false);
    expect(document.body.innerHTML).not.toContain(VALUE);
  });

  it("a project the catalog doesn't have is never saved elsewhere: creating it takes a tap", async () => {
    const rows = [row("sr-1", "WIFI_PSK")];
    const calls = server(rows, { catalog: { projects: [] } });
    render(() => <RichReply message={message("sr-1", "WIFI_PSK")} rich={rich("sr-1", "WIFI_PSK")} />);
    const box = await field();
    fireEvent.input(box, { target: { value: VALUE } });
    await screen.findByRole("button", { name: "Create project" });
    expect(ok().disabled).toBe(true);
    expect(calls.some((c) => c.url === "/v1/secrets" && c.method === "POST")).toBe(false);
  });

  it("the owner's own blank request asks for the name and the place", async () => {
    const rows = [row("sr-2", "", "active", { project: "", environment: "", who: "You", reason: "You asked" })];
    const calls = server(rows);
    render(() => <RichReply message={message("sr-2", "")} rich={rich("sr-2", "Secret")} />);
    expect(await screen.findByText(/Enter a secret/)).toBeTruthy();
    const name = screen.getByPlaceholderText("OPENAI_API_KEY");
    fireEvent.input(name, { target: { value: "MY KEY" } });
    const box = (await screen.findByPlaceholderText("Paste it here")) as HTMLTextAreaElement;
    fireEvent.input(box, { target: { value: VALUE } });
    await waitFor(() => expect(ok().disabled).toBe(false));
    fireEvent.click(ok());
    await screen.findByText(/MY_KEY saved/);
    const body = calls.find((c) => c.url === "/v1/secrets")!.body as { name: string; project: string; environment: string };
    expect([body.name, body.project, body.environment]).toEqual(["MY_KEY", "p-os", "prod"]);
  });

  it("closed cards don't ask again, and an old action without a request still opens the sheet", async () => {
    const rows = [row("sr-1", "WIFI_PSK", "saved", { fingerprint: "ab12cd34" }), row("sr-3", "OLD", "expired")];
    server(rows);
    render(() => <RichReply message={message("sr-1", "WIFI_PSK")} rich={rich("sr-1", "WIFI_PSK")} />);
    await screen.findByText(/WIFI_PSK saved/);
    expect(screen.queryByPlaceholderText("Paste it here")).toBeNull();
    render(() => <RichReply message={message("sr-3", "OLD")} rich={rich("sr-3", "OLD")} />);
    await screen.findByText("OLD: no longer asked.");
    render(() => <RichReply message={message("sr-9", "GONE")} rich={rich("sr-9", "GONE")} />);
    await screen.findByText("GONE: no longer asked.");
    const legacy = parseRich({
      format: "v1",
      bubbles: ["Use the sheet"],
      actions: [{ id: "add-secret", label: "Add secret", kind: "secret", payload: { mode: "save" } }],
    })!;
    render(() => <RichReply message={message("x", "x")} rich={legacy} />);
    expect(screen.getByRole("button", { name: "Add secret" })).toBeTruthy();
    expect(screen.getByText("Use the sheet")).toBeTruthy();
  });

  it("one active card at a time: the card for a queued name appears only when the server posts it", async () => {
    const rows = [row("sr-1", "FIRST_KEY"), row("sr-2", "SECOND_KEY", "queued")];
    server(rows);
    render(() => <RichReply message={message("sr-1", "FIRST_KEY")} rich={rich("sr-1", "FIRST_KEY")} />);
    await field("FIRST_KEY");
    expect(screen.getByText("1 more after this one.")).toBeTruthy();
  });
});
