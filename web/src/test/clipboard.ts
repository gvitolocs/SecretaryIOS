// A DataTransfer for paste and drop tests (jsdom has none): items, files, types and getData like Chromium's.

export function transfer(opts: { files?: File[]; text?: string; html?: string; itemsOnly?: boolean }): DataTransfer {
  const files = opts.files ?? [];
  const data: Record<string, string> = {};
  if (opts.text !== undefined) data["text/plain"] = opts.text;
  if (opts.html !== undefined) data["text/html"] = opts.html;
  const items = [
    ...Object.keys(data).map((type) => ({ kind: "string", type, getAsFile: () => null })),
    ...files.map((f) => ({ kind: "file", type: f.type, getAsFile: () => f })),
  ];
  return {
    items,
    files: opts.itemsOnly ? [] : files,
    types: [...Object.keys(data), ...(files.length ? ["Files"] : [])],
    getData: (type: string) => data[type] ?? "",
    dropEffect: "none",
  } as unknown as DataTransfer;
}

export const png = (name = "image.png", type = "image/png") => new File([new Uint8Array([137, 80, 78, 71])], name, { type });
