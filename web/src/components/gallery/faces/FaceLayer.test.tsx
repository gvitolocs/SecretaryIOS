import { fireEvent, render, screen } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ItemFace } from "../../../api/gallery";
import { setLocale } from "../../../i18n";
import { FaceLayer } from "./FaceLayer";

const face = (id: string, box: ItemFace["box"], person: ItemFace["person"] = null): ItemFace => ({ id, box, score: 0.9, source: "ml", person });
const A = face("fa", { x: 0.25, y: 0.5, w: 0.1, h: 0.2 }, { id: "p1", name: "Alma" });
const B = face("fb", { x: 0.6, y: 0.1, w: 0.1, h: 0.1 });
const C = face("fc", { x: 0.1, y: 0.1, w: 0.1, h: 0.1 }, { id: "p2", name: null });
const fit = { left: "100px", top: "50px", width: "400px", height: "300px" };

function draw(style: Record<string, string> | undefined, faces: ItemFace[], placed = true, onPick = vi.fn()) {
  render(() => <FaceLayer faces={() => faces} style={() => style} placed={() => placed} onPick={onPick} />);
  return onPick;
}
const boxOf = (name: RegExp | string) => screen.getByRole("button", { name });
const rect = (el: HTMLElement) => ({ left: el.style.left, top: el.style.top, width: el.style.width, height: el.style.height });

beforeEach(() => setLocale("en"));

describe("the face boxes over the picture", () => {
  it("puts each box on its face at a fit and labels it with the person", () => {
    draw(fit, [A, B, C]);
    expect(rect(boxOf("Face of Alma. Change who it is."))).toEqual({ left: "200px", top: "200px", width: "40px", height: "60px" });
    expect(boxOf("Face of Alma. Change who it is.")).toHaveTextContent("Alma");
    // A face with no person asks who it is; a person with no name says so.
    expect(boxOf("A face without a person. Choose who it is.")).toHaveTextContent("Who is this?");
    expect(boxOf("Face of Unnamed person. Change who it is.")).toHaveTextContent("Unnamed person");
  });

  it("follows the zoom and pan: the viewer places the picture bigger and the boxes grow with it", () => {
    draw({ left: "-300px", top: "-100px", width: "1600px", height: "1200px" }, [A]);
    expect(rect(boxOf(/Face of Alma/))).toEqual({ left: "100px", top: "500px", width: "160px", height: "240px" });
  });

  it("turns with the picture", () => {
    draw({ left: "150px", top: "0px", width: "400px", height: "300px", transform: "rotate(90deg)" }, [A]);
    // The unturned picture is 400 x 300 around (350, 150); the face centre (-80, 30) from it lands at (-30, -80).
    expect(rect(boxOf(/Face of Alma/))).toEqual({ left: "290px", top: "50px", width: "60px", height: "40px" });
  });

  it("draws nothing until the viewer has placed the picture", () => {
    draw(undefined, [A], false);
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.getByTestId("face-layer")).toBeInTheDocument();
  });

  it("tells which face was clicked", () => {
    const onPick = draw(fit, [A, B]);
    fireEvent.click(boxOf("A face without a person. Choose who it is."));
    expect(onPick).toHaveBeenCalledWith(B);
    fireEvent.click(boxOf(/Face of Alma/));
    expect(onPick).toHaveBeenLastCalledWith(A);
  });

  it("marks a face with no person differently so it can be found", () => {
    draw(fit, [A, B]);
    expect(boxOf("A face without a person. Choose who it is.").className).toContain("gf-box--open");
    expect(boxOf(/Face of Alma/).className).not.toContain("gf-box--open");
  });
});
