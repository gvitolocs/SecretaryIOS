import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import type { StudioExtras, WardrobeStage } from "@secretary/lucina-stage";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FakeExtras, FakeStage } from "../test/stage";
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

describe("Lucina's studio extras", () => {
  beforeEach(() => localStorage.clear());

  function setupExtras() {
    const stage = new FakeStage(document.createElement("canvas"), {});
    const extras = new FakeExtras();
    render(() => (
      <LucinaStudio
        stage={() => stage as unknown as WardrobeStage}
        extras={() => extras as unknown as StudioExtras}
        muted={() => true}
        fetchInit={{}}
        onClose={() => {}}
        onPhoto={() => {}}
      />
    ));
    return { stage, extras };
  }

  it("sets hand shapes per hand", () => {
    const { extras } = setupExtras();
    fireEvent.click(screen.getByRole("tab", { name: "Hands" }));
    fireEvent.click(screen.getByRole("button", { name: "Peace" }));
    expect(extras.setHand).toHaveBeenLastCalledWith("both", "peace");
    fireEvent.click(screen.getByRole("button", { name: "Left" }));
    fireEvent.click(screen.getByRole("button", { name: "Fist" }));
    expect(extras.setHand).toHaveBeenLastCalledWith("l", "fist");
    fireEvent.click(screen.getByRole("button", { name: "Natural" }));
    expect(extras.setHand).toHaveBeenLastCalledWith("l", null);
  });

  it("edits, saves and uses a pose as idle; leaving the tab ends editing", () => {
    const { extras } = setupExtras();
    fireEvent.click(screen.getByRole("tab", { name: "Pose" }));
    fireEvent.click(screen.getByRole("button", { name: "Edit pose" }));
    expect(extras.editPose).toHaveBeenLastCalledWith(true);
    fireEvent.input(screen.getByRole("textbox", { name: "Pose name" }), { target: { value: "Victory" } });
    fireEvent.click(screen.getByRole("button", { name: "Save pose" }));
    expect(extras.savePose).toHaveBeenCalledWith("Victory");
    fireEvent.click(screen.getByRole("button", { name: "Use as idle" }));
    expect(extras.setIdlePose).toHaveBeenCalledWith("Victory");
    fireEvent.click(screen.getByRole("tab", { name: "Light" }));
    expect(extras.editPose).toHaveBeenLastCalledWith(false);
  });

  it("face sliders and saved faces", () => {
    const { extras } = setupExtras();
    fireEvent.click(screen.getByRole("tab", { name: "Face" }));
    fireEvent.input(screen.getByRole("slider", { name: "shy" }), { target: { value: "0.6" } });
    expect(extras.setMorph).toHaveBeenCalledWith("shy", 0.6);
    fireEvent.input(screen.getByRole("textbox", { name: "Face name" }), { target: { value: "Blush" } });
    fireEvent.click(screen.getByRole("button", { name: "Save face" }));
    expect(extras.saveFace).toHaveBeenCalledWith("Blush");
    fireEvent.click(screen.getByRole("button", { name: "Blush" }));
    expect(extras.applyFace).toHaveBeenCalledWith("Blush");
  });

  it("adds, adjusts and removes props; sliders lock the camera while dragged", async () => {
    const { extras, stage } = setupExtras();
    fireEvent.click(screen.getByRole("tab", { name: "Props" }));
    fireEvent.click(screen.getByRole("button", { name: "Add: Guitar" }));
    await waitFor(() => expect(extras.addProp).toHaveBeenCalledWith("guitar"));
    const size = await screen.findByRole("slider", { name: "Size" });
    fireEvent.pointerDown(size);
    expect(stage.setInteractive).toHaveBeenLastCalledWith(false);
    fireEvent.input(size, { target: { value: "1.5" } });
    fireEvent.pointerUp(size);
    expect(stage.setInteractive).toHaveBeenLastCalledWith(true);
    expect(extras.updateProp).toHaveBeenCalledWith("p0", { scale: 1.5 });
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    expect(extras.removeProp).toHaveBeenCalledWith("p0");
  });

  it("camera roll, filters and gaze", () => {
    const { extras } = setupExtras();
    fireEvent.click(screen.getByRole("tab", { name: "Camera" }));
    fireEvent.input(screen.getByRole("slider", { name: "Roll" }), { target: { value: "15" } });
    expect(extras.setRoll).toHaveBeenCalledWith(15);
    fireEvent.click(screen.getByRole("button", { name: "Sepia" }));
    expect(extras.setFilter).toHaveBeenCalledWith({ id: "sepia" });
    fireEvent.input(screen.getByRole("slider", { name: "Strength" }), { target: { value: "0.4" } });
    expect(extras.setFilter).toHaveBeenLastCalledWith({ strength: 0.4 });
    fireEvent.click(screen.getByRole("button", { name: "Custom" }));
    fireEvent.input(screen.getByRole("slider", { name: "Left / right" }), { target: { value: "-0.5" } });
    expect(extras.setGaze).toHaveBeenLastCalledWith({ x: -0.5 });
  });

  it("flick ring, live stamps and dance moves", () => {
    const { extras } = setupExtras();
    fireEvent.click(screen.getByRole("tab", { name: "Fun" }));
    fireEvent.click(screen.getByRole("button", { name: "Hold and flick" }));
    expect(extras.setFlick).toHaveBeenCalledWith("hold");
    fireEvent.click(screen.getByRole("button", { name: "Live stamps" }));
    expect(extras.setLiveStamps).toHaveBeenCalledWith(true);
    fireEvent.click(screen.getByRole("button", { name: "Twist" }));
    expect(extras.setDanceMove).toHaveBeenCalledWith("twist");
    fireEvent.click(screen.getByRole("button", { name: "Pop a stamp" }));
    expect(extras.popStamp).toHaveBeenCalled();
  });

  it("an image background stays on the device and comes back with the studio", async () => {
    const { extras } = setupExtras();
    fireEvent.click(screen.getByRole("tab", { name: "Background" }));
    const file = new File(["x"], "bg.png", { type: "image/png" });
    const input = document.querySelector('input[type="file"]') as HTMLInputElement;
    Object.defineProperty(input, "files", { value: [file] });
    fireEvent.change(input);
    await waitFor(() => expect(extras.setBackgroundImage).toHaveBeenCalled());
    await waitFor(() => expect(localStorage.getItem("secretary.studio.background")).toBe("image"));
    fireEvent.click(screen.getByRole("button", { name: "Café" }));
    expect(extras.setBackgroundImage).toHaveBeenLastCalledWith(null);
  });
});
