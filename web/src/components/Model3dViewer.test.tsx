import { render, screen, waitFor } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";

vi.mock("../api/models", () => ({
  getModel: vi.fn(async () => ({
    id: "mdl_0123456789abcdef",
    title: "Knight",
    files: [{ name: "model.glb", bytes: 10 }, { name: "thumb.png", bytes: 5 }, { name: "model.usdz", bytes: 5 }],
    animations: ["Casual Walk"],
    formats: ["glb", "usdz", "fbx"],
  })),
  thumbName: (m: { files: { name: string }[] }) => (m.files.some((f) => f.name === "thumb.png") ? "thumb.png" : null),
}));

import Model3dViewer from "./Model3dViewer";

describe("Model3dViewer", () => {
  it("shows the poster, the facts and the download links before any 3D loads", async () => {
    render(() => (
      <Model3dViewer
        block={{ type: "model3d", id: "mdl_0123456789abcdef", title: "Knight", animations: ["Casual Walk"], credits: 41, formats: ["glb", "usdz", "fbx"], poster: true }}
      />
    ));
    expect(screen.getByText("Knight")).toBeInTheDocument();
    expect(screen.getByText(/Casual Walk · 41 credits/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /View in 3D: Knight/ })).toBeInTheDocument();
    await waitFor(() => expect(document.querySelector(".model3d-poster img")?.getAttribute("src")).toBe("/v1/models/mdl_0123456789abcdef/files/thumb.png"));
    const glb = screen.getByText("Download GLB").closest("a");
    expect(glb?.getAttribute("href")).toBe("/v1/models/mdl_0123456789abcdef/files/model.glb");
    // USDZ is on disk: AR Quick Look; FBX is listed but missing: no link
    await waitFor(() => expect(screen.getByText("View in AR").closest("a")?.getAttribute("rel")).toBe("ar"));
    expect(screen.queryByText("Download FBX")).toBeNull();
    expect(screen.getByText("Gallery").closest("a")?.getAttribute("href")).toBe("/models");
  });
});
