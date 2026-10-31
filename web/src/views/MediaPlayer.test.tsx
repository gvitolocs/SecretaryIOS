import { fireEvent, render } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mediaElement } from "../state/media";
import { Player } from "./Media";

// Full screen is "full tab" (owner, 2026-10-31): the player's own header and footer step aside, the video fills the
// page, and inside Secretary Code the shell's focus mode hides its rail and titlebar. No browser Fullscreen API.
describe("the player's full screen", () => {
  let setFocusMode: ReturnType<typeof vi.fn>;
  let shellListener: ((on: boolean) => void) | null;

  beforeEach(() => {
    setFocusMode = vi.fn();
    shellListener = null;
    (window as { secretaryShell?: unknown }).secretaryShell = {
      setFocusMode,
      onFocusModeChange: (l: (on: boolean) => void) => {
        shellListener = l;
        return () => (shellListener = null);
      },
    };
  });
  afterEach(() => {
    delete (window as { secretaryShell?: unknown }).secretaryShell;
  });

  it("F enters it: the header and footer go, the native fullscreen button is off, the shell is asked for focus mode", () => {
    const { container } = render(() => <Player />);
    const root = container.querySelector(".media-player") as HTMLElement;
    expect(mediaElement().getAttribute("controlslist")).toBe("nofullscreen");
    fireEvent.keyDown(document, { key: "f" });
    expect(root.classList.contains("media-player--full")).toBe(true);
    expect(container.querySelector(".media-player__bar")).toBeNull();
    expect(container.querySelector(".media-player__controls")).toBeNull();
    expect(container.querySelector(".media-full")).not.toBeNull();
    expect(mediaElement().controls).toBe(false);
    expect(setFocusMode).toHaveBeenLastCalledWith(true);

    fireEvent.keyDown(document, { key: "F" });
    expect(root.classList.contains("media-player--full")).toBe(false);
    expect(setFocusMode).toHaveBeenLastCalledWith(false);
    expect(mediaElement().controls).toBe(true);
  });

  it("Esc leaves full screen first, and the exit button does too", () => {
    const { container, getAllByRole } = render(() => <Player />);
    const root = container.querySelector(".media-player") as HTMLElement;
    fireEvent.keyDown(document, { key: "f" });
    fireEvent.keyDown(document, { key: "Escape" });
    expect(root.classList.contains("media-player--full")).toBe(false);
    expect(container.querySelector(".media-player__bar")).not.toBeNull();

    fireEvent.keyDown(document, { key: "f" });
    fireEvent.click(getAllByRole("button", { name: "Exit full screen" })[0]!);
    expect(root.classList.contains("media-player--full")).toBe(false);
  });

  it("ends when the shell leaves focus mode by itself (its Esc or Exit focus)", () => {
    const { container } = render(() => <Player />);
    fireEvent.keyDown(document, { key: "f" });
    expect(shellListener).not.toBeNull();
    shellListener!(false);
    expect((container.querySelector(".media-player") as HTMLElement).classList.contains("media-player--full")).toBe(false);
    expect(mediaElement().controls).toBe(true);
  });

  it("ignores F while typing in a field", () => {
    const { container } = render(() => (
      <>
        <input aria-label="field" />
        <Player />
      </>
    ));
    fireEvent.keyDown(container.querySelector("input")!, { key: "f" });
    expect((container.querySelector(".media-player") as HTMLElement).classList.contains("media-player--full")).toBe(false);
  });
});
