import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import type { WardrobeStage } from "@secretary/lucina-stage";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FakeStage } from "../test/stage";
import { LucinaStudio, restoreStudio } from "./LucinaStudio";

function setup(muted = true) {
  const stage = new FakeStage(document.createElement("canvas"), {});
  const onPhoto = vi.fn();
  const onClose = vi.fn();
  render(() => (
    <LucinaStudio stage={() => stage as unknown as WardrobeStage} muted={() => muted} fetchInit={{}} onClose={onClose} onPhoto={onPhoto} />
  ));
  return { stage, onPhoto, onClose };
}

describe("Lucina's studio", () => {
  beforeEach(() => localStorage.clear());

  it("moves the camera to the presets", () => {
    const { stage } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Face" }));
    fireEvent.click(screen.getByRole("button", { name: "Full body" }));
    fireEvent.click(screen.getByRole("button", { name: "Reset view" }));
    expect(stage.cameraPreset.mock.calls).toEqual([["face"], ["full"]]);
    expect(stage.resetView).toHaveBeenCalled();
  });

  it("lists her poses without the broken clips, and plays them", () => {
    const { stage } = setup();
    fireEvent.click(screen.getByRole("tab", { name: "Poses" }));
    expect(screen.queryByRole("button", { name: "bow" })).toBeNull();
    expect(screen.queryByRole("button", { name: "dance" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "status loop" }));
    expect(stage.play).toHaveBeenCalledWith("status_loop", { loop: true });
    fireEvent.click(screen.getByRole("button", { name: "tilt" }));
    expect(stage.gesture).toHaveBeenCalledWith("tilt");
  });

  it("sets and remembers the background and the light", () => {
    const { stage } = setup();
    fireEvent.click(screen.getByRole("tab", { name: "Background" }));
    fireEvent.click(screen.getByRole("button", { name: "Café" }));
    expect(stage.setBackground).toHaveBeenCalledWith("room:cafe");
    fireEvent.click(screen.getByRole("tab", { name: "Light" }));
    fireEvent.click(screen.getByRole("button", { name: "Sunset" }));
    expect(stage.setLighting).toHaveBeenCalledWith("sunset");
    const next = new FakeStage(document.createElement("canvas"), {});
    restoreStudio(next as unknown as WardrobeStage);
    expect(next.background).toBe("room:cafe");
    expect(next.lighting).toBe("sunset");
  });

  it("dances silently while muted; music only when her voice is on", async () => {
    const muted = setup(true);
    fireEvent.click(screen.getByRole("button", { name: "Dance" }));
    await waitFor(() => expect(muted.stage.setDance).toHaveBeenCalledWith(true, {}));
    fireEvent.click(screen.getByRole("button", { name: "Stop dancing" }));
    await waitFor(() => expect(muted.stage.setDance).toHaveBeenLastCalledWith(false));
  });

  it("opens photo mode", () => {
    const { onPhoto } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Photo" }));
    expect(onPhoto).toHaveBeenCalled();
  });
});
