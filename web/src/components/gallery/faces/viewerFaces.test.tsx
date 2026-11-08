import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../../../i18n";
import { type Call, stubFetch } from "../../../test/fetch";
import { rawPerson } from "../../../test/peopleBackend";
import { facesAvailable, resetFacesAvailable } from "./facesAvailable";
import { createViewerFaces } from "./viewerFaces";

const fit = { left: "100px", top: "50px", width: "400px", height: "300px" };
const rawFace = (id: string, extra: Record<string, unknown> = {}) => ({ id, box: { x: 0.25, y: 0.5, w: 0.1, h: 0.2 }, score: 0.9, source: "ml", person: null, ...extra });

interface Backend {
  calls: Call[];
  enabled: boolean;
  faces: Record<string, Record<string, unknown>[]>;
  failFaces: boolean;
  /** What PUT faces/{id} answers with (the face by default). */
  answer?: (face: Record<string, unknown>, body: Record<string, unknown>) => unknown;
}

function backend(over: Partial<Backend> = {}): Backend {
  const b: Backend = { calls: [], enabled: true, faces: { a: [rawFace("fa1"), rawFace("fa2", { box: { x: 0.6, y: 0.1, w: 0.1, h: 0.1 }, person: { id: "p1", name: "Alma" } })], b: [rawFace("fb1")] }, failFaces: false, ...over };
  b.calls = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/faces/settings") return { body: { enabled: b.enabled } };
    const list = /^\/v1\/media\/items\/([^/]+)\/faces$/.exec(url.pathname);
    if (list) return b.failFaces ? { status: 500, body: { error: { code: "x", message: "x" } } } : { body: { faces: b.faces[list[1]!] ?? [] } };
    if (url.pathname === "/v1/media/people") return { body: { people: [rawPerson("p1", { name: "Alma" }), rawPerson("p2", { name: "Bruno" })] } };
    const put = /^\/v1\/media\/faces\/([^/]+)$/.exec(url.pathname);
    if (put && call.method === "PUT") {
      const all = Object.values(b.faces).flat();
      const face = all.find((f) => f.id === put[1])!;
      const body = call.body as Record<string, unknown>;
      face.person = body.new_person ? { id: "p_new", name: body.new_person } : body.person_id ? { id: body.person_id, name: body.person_id === "p2" ? "Bruno" : "Alma" } : null;
      face.source = "manual";
      return { body: b.answer ? b.answer(face, body) : face };
    }
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
  return b;
}

function Harness(props: { hooks?: Parameters<typeof createViewerFaces>[1]; placed?: boolean }) {
  const [id, setId] = createSignal("a");
  const faces = createViewerFaces(id, props.hooks);
  return (
    <>
      {faces.button()}
      {faces.layer({ current: () => true, style: () => fit, placed: () => props.placed !== false })}
      {faces.picker()}
      <button type="button" onClick={() => setId("b")}>
        next photo
      </button>
      <output data-testid="revision">{faces.revision()}</output>
      <output data-testid="picking">{String(faces.picking())}</output>
      <output data-testid="on">{String(faces.on())}</output>
    </>
  );
}

const toggle = () => screen.queryByRole("button", { name: "Faces" });
const faceCalls = (b: Backend) => b.calls.filter((c) => /\/items\/[^/]+\/faces$/.test(c.url)).map((c) => /items\/([^/]+)\/faces/.exec(c.url)![1]);

beforeEach(() => {
  setLocale("en");
  resetFacesAvailable();
  try {
    localStorage.clear();
  } catch {
    // ignore
  }
});
afterEach(() => vi.unstubAllGlobals());

describe("faces in the viewer", () => {
  it("offers nothing while recognition is off, and asks for no faces", async () => {
    const b = backend({ enabled: false });
    render(() => <Harness />);
    await waitFor(() => expect(b.calls.length).toBeGreaterThan(0));
    await new Promise((r) => setTimeout(r, 10));
    expect(toggle()).toBeNull();
    expect(faceCalls(b)).toEqual([]);
  });

  it("offers nothing when the server cannot say (no such route)", async () => {
    stubFetch(() => ({ status: 404, body: { error: { code: "not_found", message: "no" } } }));
    render(() => <Harness />);
    await new Promise((r) => setTimeout(r, 20));
    expect(toggle()).toBeNull();
  });

  it("asks the server once a minute at most, however many viewers open", async () => {
    const b = backend();
    expect(await facesAvailable()).toBe(true);
    expect(await facesAvailable()).toBe(true);
    expect(b.calls.filter((c) => c.url === "/v1/media/faces/settings")).toHaveLength(1);
    resetFacesAvailable();
    await facesAvailable();
    expect(b.calls.filter((c) => c.url === "/v1/media/faces/settings")).toHaveLength(2);
  });

  it("has a Faces toggle that is off at first: no faces are asked for until it is pressed", async () => {
    const b = backend();
    render(() => <Harness />);
    const button = await screen.findByRole("button", { name: "Faces" });
    expect(button).toHaveAttribute("aria-pressed", "false");
    expect(button).toHaveAttribute("title", "Show faces (P)");
    expect(faceCalls(b)).toEqual([]);
    expect(screen.queryByTestId("face-layer")).toBeNull();

    fireEvent.click(button);
    expect(button).toHaveAttribute("aria-pressed", "true");
    expect(await screen.findByRole("button", { name: "Face of Alma. Change who it is." })).toBeInTheDocument();
    expect(faceCalls(b)).toEqual(["a"]);
    expect(screen.getAllByRole("button", { name: "A face without a person. Choose who it is." })).toHaveLength(1);

    fireEvent.click(button);
    expect(screen.queryByTestId("face-layer")).toBeNull();
  });

  it("remembers that the faces were on, and shows them when the next viewer opens", async () => {
    backend();
    const first = render(() => <Harness />);
    fireEvent.click(await screen.findByRole("button", { name: "Faces" }));
    await screen.findByTestId("face-layer");
    first.unmount();
    render(() => <Harness />);
    expect(await screen.findByRole("button", { name: "Faces" })).toHaveAttribute("aria-pressed", "true");
    expect(await screen.findByTestId("face-layer")).toBeInTheDocument();
  });

  it("draws the boxes only once the viewer has placed the picture", async () => {
    backend();
    render(() => <Harness placed={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "Faces" }));
    await screen.findByTestId("face-layer");
    expect(screen.queryByRole("button", { name: /Face of|A face without/ })).toBeNull();
  });

  it("asks for the next photo's faces and never shows the last photo's on it", async () => {
    const b = backend();
    render(() => <Harness />);
    fireEvent.click(await screen.findByRole("button", { name: "Faces" }));
    await screen.findByRole("button", { name: /Face of Alma/ });
    fireEvent.click(screen.getByRole("button", { name: "next photo" }));
    await waitFor(() => expect(faceCalls(b)).toEqual(["a", "b"]));
    await waitFor(() => expect(screen.queryByRole("button", { name: /Face of Alma/ })).toBeNull());
    expect(await screen.findAllByRole("button", { name: "A face without a person. Choose who it is." })).toHaveLength(1);
  });

  it("tells the viewer when the faces cannot be loaded", async () => {
    const onError = vi.fn();
    backend({ failFaces: true });
    render(() => <Harness hooks={{ onError }} />);
    fireEvent.click(await screen.findByRole("button", { name: "Faces" }));
    await waitFor(() => expect(onError).toHaveBeenCalledWith("Couldn't load the faces."));
    expect(screen.queryByRole("button", { name: /Face of|A face without/ })).toBeNull();
  });

  describe("the picker", () => {
    async function openPicker(name: RegExp | string) {
      fireEvent.click(await screen.findByRole("button", { name: "Faces" }));
      fireEvent.click(await screen.findByRole("button", { name }));
      return screen.findByRole("dialog", { name: "Who is this?" });
    }

    it("opens for a clicked face, takes the viewer's keys while open, and closes without a change on Cancel", async () => {
      const b = backend();
      render(() => <Harness />);
      const dialog = await openPicker("A face without a person. Choose who it is.");
      expect(screen.getByTestId("picking")).toHaveTextContent("true");
      fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
      expect(screen.queryByRole("dialog")).toBeNull();
      expect(screen.getByTestId("picking")).toHaveTextContent("false");
      expect(b.calls.some((c) => c.method === "PUT")).toBe(false);
    });

    it("assigns an unassigned face: the box takes the name at once and the people count as changed", async () => {
      const onPeopleChanged = vi.fn();
      const b = backend();
      render(() => <Harness hooks={{ onPeopleChanged }} />);
      const dialog = await openPicker("A face without a person. Choose who it is.");
      fireEvent.click(await within(dialog).findByRole("button", { name: /Bruno/ }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      expect(b.calls.find((c) => c.method === "PUT")).toMatchObject({ url: "/v1/media/faces/fa1", body: { person_id: "p2" } });
      expect(await screen.findByRole("button", { name: "Face of Bruno. Change who it is." })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "A face without a person. Choose who it is." })).toBeNull();
      expect(onPeopleChanged).toHaveBeenCalledTimes(1);
      expect(screen.getByTestId("revision")).toHaveTextContent("1");
    });

    it("corrects a wrong face with Not this person", async () => {
      const b = backend();
      render(() => <Harness />);
      const dialog = await openPicker(/Face of Alma/);
      expect(within(dialog).getByText("Now: Alma")).toBeInTheDocument();
      fireEvent.click(within(dialog).getByRole("button", { name: "Not this person" }));
      await waitFor(() => expect(b.calls.find((c) => c.method === "PUT")?.body).toEqual({ person_id: null }));
      await waitFor(() => expect(screen.getAllByRole("button", { name: "A face without a person. Choose who it is." })).toHaveLength(2));
    });

    it("makes a new person from the picker", async () => {
      const b = backend();
      render(() => <Harness />);
      const dialog = await openPicker("A face without a person. Choose who it is.");
      fireEvent.input(within(dialog).getByLabelText("New person…"), { target: { value: "Chiara" } });
      fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
      expect(await screen.findByRole("button", { name: "Face of Chiara. Change who it is." })).toBeInTheDocument();
      expect(b.calls.find((c) => c.method === "PUT")?.body).toEqual({ new_person: "Chiara" });
    });

    it("asks for the photo's faces again when the server's answer is not a face", async () => {
      const b = backend({ answer: () => ({ ok: true }) });
      render(() => <Harness />);
      const dialog = await openPicker("A face without a person. Choose who it is.");
      fireEvent.click(await within(dialog).findByRole("button", { name: /Bruno/ }));
      await waitFor(() => expect(faceCalls(b)).toEqual(["a", "a"]));
      expect(await screen.findByRole("button", { name: "Face of Bruno. Change who it is." })).toBeInTheDocument();
    });

    it("keeps the picker open with an error when the server refuses, and changes nothing", async () => {
      const onPeopleChanged = vi.fn();
      backend();
      render(() => <Harness hooks={{ onPeopleChanged }} />);
      const dialog = await openPicker("A face without a person. Choose who it is.");
      const people = await within(dialog).findByRole("button", { name: /Bruno/ });
      stubFetch((call) => (call.method === "PUT" ? { status: 500, body: { error: { code: "x", message: "x" } } } : { body: { people: [rawPerson("p2", { name: "Bruno" })], faces: [], enabled: true } }));
      fireEvent.click(people);
      expect(await within(dialog).findByRole("alert")).toHaveTextContent("Couldn't save that");
      expect(onPeopleChanged).not.toHaveBeenCalled();
      expect(screen.getByTestId("revision")).toHaveTextContent("0");
    });

    it("closes when the photo changes", async () => {
      backend();
      render(() => <Harness />);
      await openPicker("A face without a person. Choose who it is.");
      fireEvent.click(screen.getByRole("button", { name: "next photo" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    });
  });
});
