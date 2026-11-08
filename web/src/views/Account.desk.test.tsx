import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { setSessionForTests } from "../state/session";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import { AccountView } from "./ShellSheets";
afterEach(() => vi.unstubAllGlobals());
function renderPage(path = "/account") {
  stubWide();
  setSessionForTests({ status: "authed", deviceName: "Desktop" });
  return renderAt(path, () => <Route path="/account" component={AccountView} />);
}
it("shows the general section as Cards under a page header with an account rail", async () => {
  stubFetch(() => ({ status: 404, body: { error: { code: "not_found", message: "Unavailable" } } }));
  const { container } = renderPage();
  expect(await screen.findByRole("heading", { level: 1, name: "Account" })).toBeInTheDocument();
  const nav = screen.getByRole("navigation", { name: "Account sections" });
  expect(within(nav).getByRole("link", { name: "General" })).toHaveAttribute("aria-current", "page");
  expect(within(nav).getByRole("link", { name: "Import" })).not.toHaveAttribute("aria-current");
  expect(container.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
  // The phone dialog is gone.
  expect(container.querySelector(".picker-overlay, .account-sheet, .segmented, [role=dialog]")).toBeNull();
  for (const name of ["Language", "Appearance", "Change password"]) expect(screen.getByRole("region", { name })).toBeInTheDocument();
  expect(screen.queryByRole("region", { name: "Import" })).toBeNull();
  expect(container.querySelector(".pg-rail .pg-card")).toHaveTextContent("Desktop");
  expect(container.querySelector(".pg-rail .pg-btn")).toHaveTextContent("Log out");
});
it("switches sections: notifications keep their Cards, an absent approvals section stays out", async () => {
  stubFetch(() => ({ status: 404, body: { error: { code: "not_found", message: "Unavailable" } } }));
  renderPage("/account?section=notifications");
  const nav = await screen.findByRole("navigation", { name: "Account sections" });
  expect(within(nav).getByRole("link", { name: "Notification settings" })).toHaveAttribute("aria-current", "page");
  expect(screen.getByRole("region", { name: "Notification settings" })).toBeInTheDocument();
  expect(screen.queryByRole("region", { name: "Change password" })).toBeNull();
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
});
it("shows the import section", async () => {
  stubFetch(() => ({ body: {} }));
  renderPage("/account?section=import");
  expect(await screen.findByRole("region", { name: "Import" })).toBeInTheDocument();
  expect(screen.queryByRole("region", { name: "Language" })).toBeNull();
});
it("uses the same password validation on desktop", async () => {
  stubFetch(() => ({ body: {} }));
  renderPage();
  fireEvent.submit(await screen.findByRole("form", { name: "Change password" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Enter your current password.");
});
