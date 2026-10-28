import { Route, type RouteSectionProps } from "@solidjs/router";
import { screen } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { approvalJson } from "../test/approvals";
import { MockEventSource } from "../test/eventsource";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import Shell from "./Shell";

// The Mac app (Secretary Code) embeds this page and shows no top or tab bar; the approval pop-up must still show.
describe("Shell: approval pop-up", () => {
  beforeEach(() => {
    vi.stubGlobal("EventSource", MockEventSource);
    stubFetch((c) =>
      c.url === "/v1/approvals?status=pending" ? { body: [approvalJson("apr_1")] } : { status: 404, body: {} },
    );
  });

  it("shows in the browser shell", async () => {
    renderAt("/", () => <Route path="*" component={(p: RouteSectionProps) => <Shell>{p.children}</Shell>} />);
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
    expect(document.querySelector(".topbar")).not.toBeNull();
    expect(document.querySelector(".tabbar")).not.toBeNull();
  });

  it("shows inside Secretary Code, where the bars are not rendered", async () => {
    window.secretaryShell = { showCode: vi.fn() };
    try {
      renderAt("/", () => <Route path="*" component={(p: RouteSectionProps) => <Shell>{p.children}</Shell>} />);
      expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
      expect(screen.getByText("Lucina is asking for your OK")).toBeInTheDocument();
      expect(document.querySelector(".topbar")).toBeNull();
      expect(document.querySelector(".tabbar")).toBeNull();
    } finally {
      delete window.secretaryShell;
      delete document.documentElement.dataset.shell;
    }
  });
});
