import { describe, expect, it } from "vitest";
import { parseItem } from "../api/timeline";
import { blockFromModel, modelFileUrl, parseModel3dBlock, parseModel3dBlocks } from "./model3d";

describe("model3d blocks", () => {
  it("keeps valid fields and drops the rest", () => {
    expect(
      parseModel3dBlock({
        type: "model3d",
        id: "mdl_0123456789abcdef",
        title: "  Knight\n",
        prompt: "a knight",
        animations: ["Casual Walk", 3, ""],
        credits: 41,
        formats: ["glb", "exe", "usdz", "glb"],
        poster: true,
        video: "yes",
        url: "https://evil.example/x.glb",
      }),
    ).toEqual({
      type: "model3d",
      id: "mdl_0123456789abcdef",
      title: "Knight",
      prompt: "a knight",
      animations: ["Casual Walk"],
      credits: 41,
      formats: ["glb", "usdz"],
      poster: true,
    });
    expect(parseModel3dBlock({ type: "model3d", id: "../../etc/passwd" })).toBeNull();
    expect(parseModel3dBlock({ type: "card", id: "mdl_0123456789abcdef" })).toBeNull();
    expect(parseModel3dBlocks([{ type: "card" }, { type: "model3d", id: "mdl_0123456789abcdef" }])).toHaveLength(1);
  });

  it("builds only same-origin file URLs", () => {
    expect(modelFileUrl("mdl_0123456789abcdef", "model.glb")).toBe("/v1/models/mdl_0123456789abcdef/files/model.glb");
    expect(() => modelFileUrl("https://evil", "model.glb")).toThrow();
  });

  it("rides on timeline items with meta.format v1", () => {
    const item = parseItem({
      id: "01J",
      ts: "2026-10-22T10:00:00Z",
      channel: "web",
      role: "lucina",
      text: "Il tuo modello 3D è pronto",
      meta: { format: "v1", kind: "meshy", blocks: [{ type: "model3d", id: "mdl_0123456789abcdef", title: "Knight" }] },
    });
    expect(item?.meta?.blocks?.[0]?.id).toBe("mdl_0123456789abcdef");
    const noFormat = parseItem({ id: "01K", ts: 1, channel: "web", role: "lucina", text: "x", meta: { blocks: [{ type: "model3d", id: "mdl_0123456789abcdef" }] } });
    expect(noFormat?.meta?.blocks).toBeUndefined();
  });

  it("turns a gallery entry into a block", () => {
    const b = blockFromModel({
      id: "mdl_0123456789abcdef",
      title: "Chest",
      files: [{ name: "model.glb" }, { name: "thumb.png" }, { name: "preview.mp4" }],
      meshy: { rigTaskId: "r" },
    });
    expect(b).toMatchObject({ poster: true, video: true, rigged: true });
  });
});
