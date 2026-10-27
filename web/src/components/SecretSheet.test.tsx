import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import ChatView from "../views/Chat";
import { reloadChatFromStorage } from "../state/chat";
import { closeSecretSheet, openSecretSheet, secretSheet } from "../state/secretSheet";
import { stubFetch, type Call } from "../test/fetch";
import { SecretSheet } from "./SecretSheet";

const VALUE = "sk-proj-SyntheticValue0123456789AbCdEfGh";

const CATALOG = {
  projects: [
    { id: "p-poko", name: "Pokobot", slug: "pokobot", alias: "poko", environments: [{ slug: "dev", name: "Dev" }, { slug: "prod", name: "Prod" }] },
    { id: "p-orch", name: "Orchestra", slug: "orchestra", environments: [{ slug: "dev", name: "Dev" }] },
  ],
};

function server(): Call[] {
  return stubFetch((c) => {
    if (c.url.startsWith("/v1/timeline")) return { body: { items: [], nextBefore: null } };
    if (c.url === "/v1/secrets/catalog") return { body: CATALOG };
    if (c.url.startsWith("/v1/secrets/names")) {
      const path = new URL(c.url, "http://x").searchParams.get("path");
      return {
        body: path === "/Pokobot"
          ? { folders: ["sub"], secrets: [{ key: "EXISTING_KEY" }] }
          : { folders: ["Pokobot"], secrets: [] },
      };
    }
    if (c.url === "/v1/secrets" && c.method === "POST") {
      const b = c.body as { name: string };
      return {
        body: {
          key: b.name, project: { id: "p-poko", name: "Pokobot" }, environment: "prod", path: "/Pokobot",
          action: "created", fingerprint: "ab12cd34", receipt: "Saved",
        },
      };
    }
    if (c.url === "/v1/secrets/projects") return { body: { project: { id: "p-new", name: "Poko Shorts", environments: [{ slug: "dev", name: "Dev" }] }, receipt: "x" } };
    return { status: 404, body: {} };
  });
}

function storageText(): string {
  const all: string[] = [];
  for (const s of [localStorage, sessionStorage]) for (let i = 0; i < s.length; i++) all.push(s.getItem(s.key(i)!) ?? "");
  return all.join("\n");
}

describe("Add secret", () => {
  beforeEach(() => {
    closeSecretSheet();
    localStorage.clear();
    sessionStorage.clear();
    reloadChatFromStorage();
  });

  it("/secret opens the sheet instead of sending, and the value never reaches the chat or storage", async () => {
    const calls = server();
    render(() => (
      <>
        <ChatView />
        <SecretSheet />
      </>
    ));
    const box = screen.getByLabelText("Message Lucina") as HTMLTextAreaElement;
    fireEvent.input(box, { target: { value: `/secret POKO_KEY ${VALUE} poko/prod/Pokobot` } });
    fireEvent.keyDown(box, { key: "Enter" });

    expect(await screen.findByRole("dialog", { name: /Add secret/ })).toBeInTheDocument();
    expect(box.value).toBe("");
    expect(calls.some((c) => c.url === "/v1/chat")).toBe(false);
    expect(document.querySelector(".msg")).toBeNull();
    // The target picked Pokobot / prod / /Pokobot and lists what is there.
    expect(await screen.findByText("EXISTING_KEY")).toBeInTheDocument();
    expect(screen.getByText("/Pokobot")).toBeInTheDocument();
    expect(storageText()).not.toContain(VALUE);

    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(screen.getByText("Save POKO_KEY to Pokobot / prod / /Pokobot?")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(/Saved POKO_KEY in Pokobot \/ prod \/ \/Pokobot/)).toBeInTheDocument();
    expect(screen.getByText("Fingerprint ab12cd34")).toBeInTheDocument();

    const save = calls.find((c) => c.url === "/v1/secrets")!;
    expect(save.headers["x-secretary-csrf"]).toBe("1");
    expect(save.body).toEqual({
      name: "POKO_KEY", project: "p-poko", environment: "prod", path: "/Pokobot",
      overwrite: false, createFolder: false, value: VALUE, confirmed: true,
    });
    // Only names in storage: the last place, for next time.
    expect(storageText()).not.toContain(VALUE);
    expect(JSON.parse(localStorage.getItem("secretary.secret.last.v1")!)).toEqual({ project: "p-poko", environment: "prod", path: "/Pokobot" });
    // No chat request at any point carried the value.
    expect(JSON.stringify(calls.filter((c) => c.url !== "/v1/secrets"))).not.toContain(VALUE);
  });

  it("asks before sending a message that holds a token", async () => {
    const calls = server();
    render(() => (
      <>
        <ChatView />
        <SecretSheet />
      </>
    ));
    const box = screen.getByLabelText("Message Lucina") as HTMLTextAreaElement;
    fireEvent.input(box, { target: { value: `usa questa chiave ${VALUE}` } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(await screen.findByText(/This looks like a secret/)).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/chat")).toBe(false);
    expect(document.querySelector(".msg")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Save as secret" }));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(secretSheet()?.value).toBe(VALUE);
    expect(secretSheet()?.name).toBe("OPENAI_API_KEY");
    expect(box.value).toBe("usa questa chiave [secret:token]");
    expect(storageText()).not.toContain(VALUE);
  });

  it("sends the masked text when asked", async () => {
    const calls = server();
    render(() => <ChatView />);
    const box = screen.getByLabelText("Message Lucina") as HTMLTextAreaElement;
    fireEvent.input(box, { target: { value: `token ${VALUE}` } });
    fireEvent.keyDown(box, { key: "Enter" });
    fireEvent.click(await screen.findByRole("button", { name: "Send it hidden" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/chat")).toBe(true));
    const chat = calls.find((c) => c.url === "/v1/chat")!;
    expect((chat.body as { message: string }).message).toBe("token [secret:token]");
    expect(storageText()).not.toContain(VALUE);
  });

  it("shows existing names, makes an overwrite explicit and can make a folder", async () => {
    const calls = server();
    render(() => <SecretSheet />);
    openSecretSheet({ name: "EXISTING_KEY", value: "v-1234567", target: "poko/prod/Pokobot" });
    expect(await screen.findByText(/EXISTING_KEY exists here/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(screen.getByText(/This replaces the current value of EXISTING_KEY/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    // A new folder under /Pokobot.
    fireEvent.click(screen.getByRole("button", { name: /New folder/ }));
    fireEvent.input(screen.getByLabelText("Folder name"), { target: { value: "bots" } });
    fireEvent.click(screen.getByRole("button", { name: "Use" }));
    expect(screen.getByText("/Pokobot/bots will be created")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText(/Saved EXISTING_KEY/);
    const save = calls.find((c) => c.url === "/v1/secrets")!;
    expect(save.body).toMatchObject({ path: "/Pokobot/bots", createFolder: true, overwrite: false });
  });

  it("a held value from Lucina's card is sent by id, and a new project needs a confirmation", async () => {
    const calls = server();
    render(() => <SecretSheet />);
    openSecretSheet({ held: "held-0123456789ab", name: "GH_TOKEN", target: "orchestra" });
    expect(await screen.findByText(/Lucina holds this value for you/)).toBeInTheDocument();
    await screen.findByText("No secrets in this folder yet");
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText(/Saved GH_TOKEN/);
    expect(calls.find((c) => c.url === "/v1/secrets")!.body).toMatchObject({ held: "held-0123456789ab", project: "p-orch", environment: "dev" });
    expect((calls.find((c) => c.url === "/v1/secrets")!.body as Record<string, unknown>).value).toBeUndefined();

    closeSecretSheet();
    openSecretSheet({ mode: "project", projectName: "Poko Shorts" });
    fireEvent.click(await screen.findByRole("button", { name: "Review" }));
    expect(screen.getByText("Create the Infisical project Poko Shorts?")).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/secrets/projects")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    expect(await screen.findByText(/Project Poko Shorts created/)).toBeInTheDocument();
    expect(calls.find((c) => c.url === "/v1/secrets/projects")!.body).toEqual({ name: "Poko Shorts", confirmed: true });
  });
});
