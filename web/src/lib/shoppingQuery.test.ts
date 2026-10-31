import { describe, expect, it } from "vitest";
import { joinSpec, parseQuery } from "./shoppingQuery";

describe("parseQuery", () => {
  it("reads counts written as x2, 2x, ×3 or a bare number", () => {
    expect(parseQuery("pane x2")).toEqual({ text: "pane", spec: "x2" });
    expect(parseQuery("2x latte")).toEqual({ text: "latte", spec: "x2" });
    expect(parseQuery("Uova ×6")).toEqual({ text: "Uova", spec: "x6" });
    expect(parseQuery("uova 6")).toEqual({ text: "uova", spec: "x6" });
  });

  it("reads amounts with units, with or without a space", () => {
    expect(parseQuery("farina 1 kg")).toEqual({ text: "farina", spec: "1 kg" });
    expect(parseQuery("latte 2l")).toEqual({ text: "latte", spec: "2 l" });
    expect(parseQuery("prosciutto crudo 200gr")).toEqual({ text: "prosciutto crudo", spec: "200 g" });
    expect(parseQuery("olio 0.5 lt")).toEqual({ text: "olio", spec: "0,5 l" });
  });

  it("takes everything after a comma as the spec", () => {
    expect(parseQuery("pane, integrale")).toEqual({ text: "pane", spec: "integrale" });
    expect(parseQuery("pane x2, integrale")).toEqual({ text: "pane", spec: "x2 integrale" });
  });

  it("leaves names alone, and never empties the text", () => {
    expect(parseQuery("Pane integrale")).toEqual({ text: "Pane integrale", spec: "" });
    expect(parseQuery("7up")).toEqual({ text: "7up", spec: "" });
    expect(parseQuery("x2")).toEqual({ text: "x2", spec: "" });
    expect(parseQuery("6")).toEqual({ text: "6", spec: "" });
    expect(parseQuery("uova 0")).toEqual({ text: "uova 0", spec: "" });
    expect(parseQuery("   ")).toEqual({ text: "", spec: "" });
  });
});

describe("joinSpec", () => {
  it("joins the typed spec and the chips once each", () => {
    expect(joinSpec("x2", ["bio"])).toBe("x2 bio");
    expect(joinSpec("", ["x2", "x2"])).toBe("x2");
    expect(joinSpec("", [])).toBe("");
  });
});
