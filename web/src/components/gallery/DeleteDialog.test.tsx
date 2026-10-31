import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import { setLocale } from "../../i18n";
import { galleryItem } from "../../test/galleryFixtures";
import { DeleteDialog, type DeleteDialogProps } from "./DeleteDialog";

vi.mock("../../api/gallery", async (original) => ({ ...(await original<typeof import("../../api/gallery")>()), batch: vi.fn() }));
const batch = vi.mocked(api.batch);

function open(items = [galleryItem("a")], over: Partial<DeleteDialogProps> = {}) {
  const onDone = vi.fn();
  const onCancel = vi.fn();
  render(() => <DeleteDialog items={items} onDone={onDone} onCancel={onCancel} {...over} />);
  return { onDone, onCancel };
}

beforeEach(() => {
  setLocale("en");
  batch.mockReset();
});

describe("DeleteDialog", () => {
  it("says the originals stay and the Trash is emptied after 30 days", () => {
    open();
    const dialog = screen.getByRole("dialog", { name: "Move to Trash?" });
    expect(dialog).toHaveAccessibleDescription(/Lucina's Trash/);
    expect(dialog).toHaveTextContent("It moves to Lucina's Trash.");
    expect(dialog).toHaveTextContent("The originals stay on Test iPhone.");
    expect(dialog).toHaveTextContent("The Trash is emptied after 30 days");
  });

  it("offers 'Also delete from <device>' only for items that have a device, unchecked", () => {
    open();
    const box = screen.getByRole("checkbox", { name: /Also delete from Test iPhone/ });
    expect(box).not.toBeChecked();
    expect(screen.getByText("Each device asks you to confirm before anything is removed from it.")).toBeInTheDocument();
  });

  it("has no device option for chat items, and names every device when there are several", () => {
    const { unmount } = render(() => <DeleteDialog items={[galleryItem("c", { device: null, deviceName: null, source: "web-chat" })]} onDone={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.queryByText(/The originals stay/)).toBeNull();
    unmount();
    open([galleryItem("a"), galleryItem("b", { device: "dev-2", deviceName: "Test Mac" }), galleryItem("c")]);
    expect(screen.getByRole("dialog", { name: "Move 3 items to Trash?" })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /Also delete from Test iPhone and Test Mac/ })).toBeInTheDocument();
  });

  it("moves every item to the Trash in one batch and leaves the device alone by default", async () => {
    batch.mockResolvedValue({ done: ["a", "b"], failed: [] });
    const { onDone } = open([galleryItem("a"), galleryItem("b")]);
    fireEvent.click(screen.getByRole("button", { name: "Move to Trash" }));
    await waitFor(() => expect(onDone).toHaveBeenCalledWith({ trashed: ["a", "b"], purged: [], failed: [] }));
    expect(batch).toHaveBeenCalledTimes(1);
    expect(batch).toHaveBeenCalledWith("trash", ["a", "b"], { alsoFromDevice: false });
  });

  it("asks the device too when the option is ticked", async () => {
    batch.mockResolvedValue({ done: ["a"], failed: [] });
    open();
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "Move to Trash" }));
    await waitFor(() => expect(batch).toHaveBeenCalledWith("trash", ["a"], { alsoFromDevice: true }));
  });

  it("keeps the dialog open with an error when the request fails, then lets it retry", async () => {
    batch.mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce({ done: ["a"], failed: [] });
    const { onDone } = open();
    fireEvent.click(screen.getByRole("button", { name: "Move to Trash" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't finish");
    expect(onDone).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Move to Trash" }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
  });

  it("reports a partial result to the caller and stays put when nothing was done", async () => {
    batch.mockResolvedValueOnce({ done: [], failed: [{ id: "a", error: "boom" }] });
    const { onDone } = open();
    fireEvent.click(screen.getByRole("button", { name: "Move to Trash" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("1 item couldn't be removed.");
    expect(onDone).not.toHaveBeenCalled();
    batch.mockResolvedValueOnce({ done: ["a"], failed: [{ id: "b", error: "boom" }] });
    fireEvent.click(screen.getByRole("button", { name: "Move to Trash" }));
    await waitFor(() => expect(onDone).toHaveBeenCalledWith({ trashed: ["a"], purged: [], failed: [{ id: "b", error: "boom" }] }));
  });

  it("closes on Esc and Cancel, focuses Cancel first, and traps Tab", () => {
    const { onCancel } = open();
    const cancel = screen.getByRole("button", { name: "Cancel" });
    expect(cancel).toHaveFocus();
    const [box, , confirm] = [screen.getByRole("checkbox"), cancel, screen.getByRole("button", { name: "Move to Trash" })];
    confirm.focus();
    fireEvent.keyDown(confirm, { key: "Tab" });
    expect(box).toHaveFocus();
    fireEvent.keyDown(box, { key: "Tab", shiftKey: true });
    expect(confirm).toHaveFocus();
    fireEvent.keyDown(confirm, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  describe("delete forever (the Trash)", () => {
    it("needs a tick before the button works, and says it cannot be undone", async () => {
      batch.mockResolvedValue({ done: ["a", "b"], failed: [] });
      const { onDone } = open([galleryItem("a"), galleryItem("b")], { purge: true });
      expect(screen.getByRole("dialog", { name: "Delete 2 items forever?" })).toHaveTextContent("It can't be undone");
      expect(screen.getByText("Originals on your devices are not touched.")).toBeInTheDocument();
      expect(screen.queryByRole("checkbox", { name: /Also delete from/ })).toBeNull();
      const button = screen.getByRole("button", { name: "Delete forever" });
      expect(button).toBeDisabled();
      fireEvent.click(button);
      expect(batch).not.toHaveBeenCalled();
      fireEvent.click(screen.getByRole("checkbox", { name: "I understand this can't be undone" }));
      expect(button).toBeEnabled();
      fireEvent.click(button);
      await waitFor(() => expect(onDone).toHaveBeenCalledWith({ trashed: [], purged: ["a", "b"], failed: [] }));
      expect(batch).toHaveBeenCalledWith("purge", ["a", "b"]);
    });
  });
});
