import { Route } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderAt } from "../test/router";
import PluginsView from "../views/Plugins";

describe("Code plugin", () => {
  afterEach(() => {
    delete window.secretaryShell;
  });

  it("is hidden in a browser", () => {
    renderAt("/plugins", () => <Route path="/plugins" component={PluginsView} />);
    expect(screen.queryByRole("button", { name: /^Code/ })).toBeNull();
  });

  it("asks Secretary Code to show its coding view", () => {
    const showCode = vi.fn();
    window.secretaryShell = { isDesktop: true, showCode };
    renderAt("/plugins", () => <Route path="/plugins" component={PluginsView} />);
    fireEvent.click(screen.getByRole("button", { name: /^Code/ }));
    expect(showCode).toHaveBeenCalledOnce();
  });
});
