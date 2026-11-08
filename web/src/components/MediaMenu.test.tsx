import { Route } from "@solidjs/router";
import { cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderAt } from "../test/router";
import { mediaAt, MediaMenuHost } from "./MediaMenu";

const copyFromUrl = vi.fn(async (_url: string) => true);
const fileFromUrl = vi.fn(async (_url: string, name: string) => new File(["x"], `${name}.jpg`, { type: "image/jpeg" }));
vi.mock("../lib/copyImage", () => ({ copyFromUrl: (url: string) => copyFromUrl(url), fileFromUrl: (url: string, name: string) => fileFromUrl(url, name) }));

const frame = () => new Promise((r) => setTimeout(r, 20));

/** jsdom loads no pictures: give one its own size. */
function sized(img: HTMLImageElement, width: number): HTMLImageElement {
  Object.defineProperty(img, "naturalWidth", { configurable: true, value: width });
  return img;
}

function page(html: string): HTMLElement {
  const root = document.createElement("div");
  root.innerHTML = html;
  document.body.append(root);
  root.querySelectorAll("img").forEach((img) => sized(img, Number(img.dataset.w ?? 600)));
  return root;
}

describe("mediaAt", () => {
  afterEach(() => document.body.replaceChildren());

  it("finds the picture under the pointer, or the one inside the tile that was clicked", () => {
    const root = page(`<img id="a" src="/p/a.jpg" alt="Cat"><button id="tile"><span>badge</span><img src="/p/b.jpg"></button>`);
    expect(mediaAt(root.querySelector("#a"))).toEqual({ src: "http://localhost:3000/p/a.jpg", link: undefined, name: "Cat" });
    expect(mediaAt(root.querySelector("#tile span"))?.src).toBe("http://localhost:3000/p/b.jpg");
  });

  it("takes the page's hints: the full-size picture, the page it belongs to and its name", () => {
    const root = page(
      `<button data-media-src="/full.jpg" data-media-link="https://www.pixiv.net/artworks/1" data-media-name="Sky"><img id="t" src="/thumb.jpg"></button>`,
    );
    expect(mediaAt(root.querySelector("#t"))).toEqual({ src: "/full.jpg", link: "https://www.pixiv.net/artworks/1", name: "Sky" });
  });

  it("uses a link around the picture as its link", () => {
    const root = page(`<a href="https://instagram.com/p/x"><img id="i" src="/p.jpg"></a>`);
    expect(mediaAt(root.querySelector("#i"))?.link).toBe("https://instagram.com/p/x");
  });

  it("leaves icons, fields, opted-out areas and empty spots alone", () => {
    const root = page(
      `<button id="icon"><img data-w="24" src="/i.png"></button><input id="f"><div data-media-off><img id="off" src="/o.jpg"></div><p id="text">hi</p>`,
    );
    expect(mediaAt(root.querySelector("#icon img"))).toBeNull();
    expect(mediaAt(root.querySelector("#f"))).toBeNull();
    expect(mediaAt(root.querySelector("#off"))).toBeNull();
    expect(mediaAt(root.querySelector("#text"))).toBeNull();
  });

  it("uses a video's poster", () => {
    const root = page(`<video id="v" poster="/poster.jpg"></video>`);
    expect(mediaAt(root.querySelector("#v"))?.src).toBe("http://localhost:3000/poster.jpg");
  });
});

describe("MediaMenuHost", () => {
  beforeEach(() => {
    copyFromUrl.mockClear();
    fileFromUrl.mockClear();
  });
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
    vi.unstubAllGlobals();
  });

  function setup(html: string) {
    renderAt("/pixiv", () => <Route path="*" component={() => <MediaMenuHost />} />);
    return page(html);
  }

  it("opens on a right-click over a picture and copies it", async () => {
    const root = setup(`<button data-media-link="https://www.pixiv.net/artworks/7"><img id="p" src="/v1/pixiv/img/7.jpg"></button>`);
    const event = fireEvent.contextMenu(root.querySelector("#p")!, { clientX: 40, clientY: 50 });
    expect(event).toBe(false); // the browser's own menu is not shown
    const menu = await screen.findByRole("menu", { name: "Picture actions" });
    expect([...menu.querySelectorAll("[role=menuitem]")].map((b) => b.textContent)).toEqual([
      "Copy image",
      "Copy link",
      "Download",
      "Ask Lucina about this picture",
    ]);
    fireEvent.pointerDown(screen.getByRole("menuitem", { name: "Copy image" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Copy image" }));
    expect(copyFromUrl).toHaveBeenCalledWith("http://localhost:3000/v1/pixiv/img/7.jpg");
    expect(await screen.findByRole("status")).toHaveTextContent("Image copied.");
  });

  it("copies the picture's page as its link", async () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    const root = setup(`<button data-media-link="https://www.pixiv.net/artworks/7"><img id="p" src="/7.jpg"></button>`);
    fireEvent.contextMenu(root.querySelector("#p")!, { clientX: 40, clientY: 50 });
    const row = await screen.findByRole("menuitem", { name: "Copy link" });
    fireEvent.pointerDown(row);
    fireEvent.click(row);
    expect(writeText).toHaveBeenCalledWith("https://www.pixiv.net/artworks/7");
  });

  it("says so when the copy fails", async () => {
    copyFromUrl.mockResolvedValueOnce(false);
    const root = setup(`<img id="p" src="/7.jpg">`);
    fireEvent.contextMenu(root.querySelector("#p")!, { clientX: 40, clientY: 50 });
    const row = await screen.findByRole("menuitem", { name: "Copy image" });
    fireEvent.pointerDown(row);
    fireEvent.click(row);
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't copy.");
  });

  it("stays out of the way of a page's own menu, and of everything that is not a picture", async () => {
    const root = setup(`<img id="p" src="/7.jpg"><p id="t">text</p>`);
    root.querySelector("#p")!.addEventListener("contextmenu", (e) => e.preventDefault());
    fireEvent.contextMenu(root.querySelector("#p")!, { clientX: 40, clientY: 50 });
    expect(fireEvent.contextMenu(root.querySelector("#t")!, { clientX: 40, clientY: 50 })).toBe(true);
    await frame();
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
