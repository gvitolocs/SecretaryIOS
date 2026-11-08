// /v1/media to a real media library (lucina-media on nezopt), the way p1's Caddy does it: it adds
// the upstream key and streams bodies both ways (Range, 206, HLS segments). Opt-in, for
// checking the web player against real files:
//   MOCK_MEDIA_URL=http://100.107.164.33:8830 MOCK_MEDIA_KEY_FILE=~/.config/secretary-media/upstream.key npm run dev:mock

import { readFileSync } from "node:fs";
import http from "node:http";

const FORWARDED = ["range", "if-range", "if-none-match", "if-modified-since", "content-type", "content-length", "accept"];

export function proxyMedia(req, res, url, fail) {
  const base = process.env.MOCK_MEDIA_URL;
  const keyFile = process.env.MOCK_MEDIA_KEY_FILE;
  if (!base || !keyFile) return fail(res, 503, "media_not_configured", "set MOCK_MEDIA_URL and MOCK_MEDIA_KEY_FILE to proxy /v1/media");
  let key;
  try {
    key = readFileSync(keyFile.replace(/^~(?=\/)/, process.env.HOME ?? "~"), "utf8").trim();
  } catch {
    return fail(res, 503, "media_not_configured", "MOCK_MEDIA_KEY_FILE is unreadable");
  }
  const headers = { "x-media-upstream-key": key };
  for (const h of FORWARDED) if (req.headers[h]) headers[h] = req.headers[h];
  const upstream = http.request(new URL(`${url.pathname}${url.search}`, base), { method: req.method, headers }, (r) => {
    res.writeHead(r.statusCode ?? 502, r.headers);
    r.pipe(res);
  });
  upstream.on("error", () => {
    if (!res.headersSent) fail(res, 502, "media_unreachable", "the media library is not reachable");
  });
  req.pipe(upstream);
}
