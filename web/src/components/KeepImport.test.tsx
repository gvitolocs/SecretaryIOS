import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api/client";
import { parseImportReport, type ImportOptions, type ImportReport } from "../api/keep";
import { KeepImport, formatBytes } from "./KeepImport";

const report = (over: Partial<ImportReport> = {}): ImportReport =>
  parseImportReport({
    dryRun: true,
    files: { json: 3, html: 3, attachments: 1, ignored: 0 },
    notes: { total: 3, new: 2, unchanged: 1, lists: 1, pinned: 1, archived: 1 },
    labels: { total: 2, new: ["GodLevel", "Chill"] },
    attachments: { total: 2, new: 1, missing: ["Keep/gone.jpg"], bytes: 2_500_000 },
    errors: [{ file: "Keep/bad.json", error: "not a Keep note" }],
    sample: [{ title: "Active recall", labels: ["GodLevel"], color: "default", kind: "text" }],
    ...over,
  });

const zip = (name = "takeout-2026.zip") => new File(["PK"], name, { type: "application/zip" });
const choose = (file: File) => fireEvent.change(screen.getByTestId("import-file"), { target: { files: [file] } });

describe("Settings → Import → Google Keep", () => {
  it("explains the Takeout steps", () => {
    render(() => <KeepImport run={vi.fn()} />);
    expect(screen.getByText(/takeout\.google\.com/)).toBeInTheDocument();
    expect(screen.getByText(/Deselect all/)).toBeInTheDocument();
    expect(screen.getByText(/“Keep”/)).toBeInTheDocument();
    expect(screen.getByText(/Choose “\.zip”/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Check" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Import" })).toBeDisabled();
  });

  it("checks first (a dry run), shows the report, then imports the same file", async () => {
    const run = vi.fn(async (_f: Blob, o: ImportOptions) => {
      o.onProgress?.(1);
      return report({ dryRun: o.dryRun });
    });
    const file = zip();
    render(() => <KeepImport run={run} />);
    choose(file);
    expect(screen.getByText(/takeout-2026\.zip/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Check" }));
    expect(await screen.findByText("Check result (nothing imported yet)")).toBeInTheDocument();
    expect(run).toHaveBeenCalledWith(file, expect.objectContaining({ dryRun: true }));
    const view = screen.getByTestId("import-report");
    expect(view).toHaveTextContent("Notes3");
    expect(view).toHaveTextContent("New2");
    expect(view).toHaveTextContent("Labels: 2 (2 new)");
    expect(view).toHaveTextContent("GodLevel, Chill");
    expect(view).toHaveTextContent("Images and audio: 2 (1 new, 2.4 MB)");
    expect(view).toHaveTextContent("1 files named in notes are missing from the zip");
    expect(view).toHaveTextContent("1 files could not be read");
    expect(view).toHaveTextContent("Keep/bad.json: not a Keep note");
    expect(screen.getByText(/Press “Import”/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Import" }));
    expect(await screen.findByText("Import finished")).toBeInTheDocument();
    expect(run).toHaveBeenLastCalledWith(file, expect.objectContaining({ dryRun: false }));
    expect(run).toHaveBeenCalledTimes(2);
  });

  it("shows the upload progress, and disables the buttons meanwhile", async () => {
    let finish!: (r: ImportReport) => void;
    let report$: ((f: number) => void) | undefined;
    const run = vi.fn(
      (_f: Blob, o: ImportOptions) =>
        new Promise<ImportReport>((resolve) => {
          report$ = o.onProgress;
          finish = resolve;
        }),
    );
    render(() => <KeepImport run={run} />);
    choose(zip());
    fireEvent.click(screen.getByRole("button", { name: "Check" }));
    report$?.(0.42);
    expect(await screen.findByText("Uploading 42%")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Check" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Import" })).toBeDisabled();
    report$?.(1);
    expect(await screen.findByText("The server is reading the notes…")).toBeInTheDocument();
    finish(report());
    await waitFor(() => expect(screen.getByRole("button", { name: "Check" })).toBeEnabled());
  });

  it("refuses a file that is not a .zip", () => {
    const run = vi.fn();
    render(() => <KeepImport run={run} />);
    choose(new File(["x"], "notes.txt"));
    expect(screen.getByRole("alert")).toHaveTextContent("Choose the .zip that Google Takeout made.");
    expect(screen.getByRole("button", { name: "Check" })).toBeDisabled();
    expect(run).not.toHaveBeenCalled();
  });

  it("says when another import is running, and shows other failures", async () => {
    const run = vi.fn().mockRejectedValueOnce(new ApiError(409, "import_running", "x")).mockRejectedValueOnce(new ApiError(500, "internal", "boom"));
    render(() => <KeepImport run={run} />);
    choose(zip());
    fireEvent.click(screen.getByRole("button", { name: "Import" }));
    expect(await screen.findByText("Another import is already running. Try again in a minute.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Check" }));
    expect(await screen.findByText("The import failed: boom")).toBeInTheDocument();
  });

  it("formats sizes", () => {
    expect(formatBytes(900)).toBe("900 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(5_000_000)).toBe("4.8 MB");
    expect(formatBytes(3 * 1024 ** 3)).toBe("3.0 GB");
  });
});
