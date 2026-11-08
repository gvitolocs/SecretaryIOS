import { Route } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderAt } from "../test/router";
import PluginsView from "../views/Plugins";
import { embedded, showWebChrome } from "./desktopShell";

describe("Code plugin", () => {
  afterEach(() => {
    delete window.secretaryShell;
    sessionStorage.clear();
    window.history.replaceState(null, "", "/");
  });

  it("counts as embedded with ?shell=1 and remembers it after the query is gone", () => {
    expect(embedded()).toBe(false);
    window.history.replaceState(null, "", "/computer?shell=1");
    expect(embedded()).toBe(true);
    window.history.replaceState(null, "", "/computer");
    expect(embedded()).toBe(true);
    expect(showWebChrome()).toBe(false);
  });

  it("is hidden in a browser", () => {
    renderAt("/plugins", () => <Route path="/plugins" component={PluginsView} />);
    expect(screen.queryByRole("button", { name: /^Code/ })).toBeNull();
  });

  it("keeps the web app's top bar and tab bar in a browser and drops them inside Secretary Code", () => {
    expect(showWebChrome()).toBe(true);
    window.secretaryShell = { version: 1, showCode: vi.fn(), show: vi.fn() };
    expect(showWebChrome()).toBe(false);
  });

  it("asks Secretary Code to show its coding view", () => {
    const showCode = vi.fn();
    window.secretaryShell = { isDesktop: true, showCode };
    renderAt("/plugins", () => <Route path="/plugins" component={PluginsView} />);
    fireEvent.click(screen.getByRole("button", { name: /^Code/ }));
    expect(showCode).toHaveBeenCalledOnce();
  });
});
