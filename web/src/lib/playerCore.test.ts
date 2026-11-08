/// <reference types="node" />
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { coreFromBytes } from "./playerCore";

// The committed player-core.wasm (web/scripts/build-player-core.sh) through the same ABI the
// page uses: the engine answers, the subtitle choice is remembered, and state survives a reload.
const bytes = readFileSync(join(process.cwd(), "src/lib/player-core.wasm"));

describe("player core (wasm)", () => {
  it("queues, resumes and parses like the iPhone's engine", async () => {
    const core = await coreFromBytes(bytes);
    const items = [{ id: "a", kind: "video", title: "Film" }, { id: "b", kind: "audio", title: "Song" }];
    const set = core.request({ cmd: "queue_set", items, start: 0 }, 1_000);
    expect((set.queue as { item: { id: string } }).item.id).toBe("a");
    expect(core.request({ cmd: "position", id: "a", position_ms: 600_000, duration_ms: 3_600_000, playing: false }, 2_000).sync).toBe(true);
    expect(core.request({ cmd: "resume", id: "a" }, 3_000).position_ms).toBe(598_000);
    const cues = core.request({ cmd: "subtitles", text: "1\n00:00:01,000 --> 00:00:02,500\nHej\n" }).cues as { text: string }[];
    expect(cues[0]!.text).toBe("Hej");
    expect(core.request({ cmd: "nope" }).code).toBe("invalid_command");
  });

  it("remembers the subtitle choice and the language across a reload", async () => {
    const core = await coreFromBytes(bytes);
    const tracks = [
      { id: "x0", lang: "da", forced: false, kind: "text" },
      { id: "e2", lang: "dan", forced: false, kind: "image" },
    ];
    expect(core.request({ cmd: "subtitle_pick", id: "film", tracks }).track).toBeNull();
    core.request({ cmd: "subtitle_choose", id: "film", track: "e2", lang: "dan" });
    const reloaded = await coreFromBytes(bytes, core.state());
    expect(reloaded.request({ cmd: "subtitle_pick", id: "film", tracks }).track).toBe("e2");
    // Another film: the language picks its text track first.
    expect(reloaded.request({ cmd: "subtitle_pick", id: "other", tracks }).track).toBe("x0");
  });

  it("keeps large requests and answers intact", async () => {
    const core = await coreFromBytes(bytes);
    const srt = Array.from({ length: 3000 }, (_, i) => `${i + 1}\n00:${String(Math.floor(i / 60) % 60).padStart(2, "0")}:${String(i % 60).padStart(2, "0")},000 --> 00:${String(Math.floor(i / 60) % 60).padStart(2, "0")}:${String(i % 60).padStart(2, "0")},900\nLinje ${i} æøå\n`).join("\n");
    const cues = core.request({ cmd: "subtitles", text: srt }).cues as { text: string }[];
    expect(cues).toHaveLength(3000);
    expect(cues[2999]!.text).toBe("Linje 2999 æøå");
  });
});
