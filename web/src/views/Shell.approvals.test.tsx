import { Route, type RouteSectionProps } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { approvalJson, NOW } from "../test/approvals";
import { MockEventSource } from "../test/eventsource";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import Shell from "./Shell";

// The Mac app (Secretary Code) embeds this page and shows no top or tab bar; the approval pop-up must still show.
describe("Shell: approval pop-up", () => {
  beforeEach(() => {
    // The fixture expires five minutes after NOW: pin the clock there (timers stay real), or the card shows
    // "This request expired" once the wall clock passes it.
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(NOW);
    vi.stubGlobal("EventSource", MockEventSource);
    stubFetch((c) =>
      c.url === "/v1/approvals?status=pending" ? { body: [approvalJson("apr_1")] } : { status: 404, body: {} },
    );
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("tapping the active Chat tab dispatches latest without navigating", () => {
    const latest = vi.fn();
    window.addEventListener("secretary:chat-latest", latest);
    try {
      renderAt("/", () => <Route path="*" component={(p: RouteSectionProps) => <Shell>{p.children}</Shell>} />);
      const link = document.querySelector<HTMLAnchorElement>('.tabbar a[href="/"]')!;
      expect(fireEvent.click(link)).toBe(false);
      expect(fireEvent.click(link)).toBe(false);
      expect(latest).toHaveBeenCalledTimes(2);
      expect(link).toHaveAttribute("aria-current", "page");
    } finally {
      window.removeEventListener("secretary:chat-latest", latest);
    }
  });

  it("shows in the browser shell", async () => {
    renderAt("/", () => <Route path="*" component={(p: RouteSectionProps) => <Shell>{p.children}</Shell>} />);
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
    expect(screen.getByText("Lucina is asking for your OK")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Restart the tunnel (apr_1)" })).toBeInTheDocument();
    expect(screen.queryByText("This request expired.")).toBeNull();
    expect(document.querySelector(".topbar")).not.toBeNull();
    expect(document.querySelector(".tabbar")).not.toBeNull();
  });

  it("shows inside Secretary Code, where the bars are not rendered", async () => {
    window.secretaryShell = { showCode: vi.fn() };
    try {
      renderAt("/", () => <Route path="*" component={(p: RouteSectionProps) => <Shell>{p.children}</Shell>} />);
      expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
      expect(screen.getByText("Lucina is asking for your OK")).toBeInTheDocument();
      expect(screen.getByRole("heading", { name: "Restart the tunnel (apr_1)" })).toBeInTheDocument();
      expect(screen.queryByText("This request expired.")).toBeNull();
      expect(document.querySelector(".topbar")).toBeNull();
      expect(document.querySelector(".tabbar")).toBeNull();
    } finally {
      delete window.secretaryShell;
      delete document.documentElement.dataset.shell;
    }
  });
});
