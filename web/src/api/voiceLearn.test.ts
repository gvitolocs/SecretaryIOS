import { describe, expect, it } from "vitest";
import { stubFetch } from "../test/fetch";
import { parseEntries, parseSample, parseSaved, parseSuggestions, splitSoundsLike, voiceLearnApi } from "./voiceLearn";

const entry = { id: "vl_1", word: "Skibigårdsvej", soundsLike: ["skibi gård vej", "skibigor"], meaning: "home street", lang: "da", source: "app", updatedAt: "2026-11-01T10:00:00Z" };

describe("Voice Learn parsers", () => {
  it("read a bare array or an object holding the list, and drop what they do not understand", () => {
    for (const raw of [[entry], { entries: [entry] }, { items: [entry] }, { words: [entry] }]) {
      expect(parseEntries(raw)).toEqual([{ id: "vl_1", word: "Skibigårdsvej", soundsLike: ["skibi gård vej", "skibigor"], meaning: "home street", lang: "da", source: "app", updatedAt: "2026-11-01T10:00:00Z" }]);
    }
    const messy = [{ id: 7, word: " Mikkel ", soundsLike: "mikel, Mikkel,  mikel", lang: "fr", source: "x" }, { id: "no-word" }, { word: "no id" }, null, "x"];
    expect(parseEntries(messy)).toEqual([{ id: "7", word: "Mikkel", soundsLike: ["mikel", "Mikkel"] }]);
    expect(parseEntries(null)).toEqual([]);
    expect(parseEntries({ entries: "nope" })).toEqual([]);
  });

  it("cut long fields and keep Danish and Italian characters", () => {
    const [e] = parseEntries([{ id: "1", word: "x".repeat(300), soundsLike: ["perché", "blåbærgrød", "È"] }]);
    expect(e!.word).toHaveLength(100);
    expect(e!.soundsLike).toEqual(["perché", "blåbærgrød", "È"]);
  });

  it("read a saved entry bare or wrapped", () => {
    expect(parseSaved(entry)?.id).toBe("vl_1");
    expect(parseSaved({ entry })?.id).toBe("vl_1");
    expect(parseSaved({ ok: true })).toBeNull();
  });

  it("read suggestions as a list, under candidates, or as one entry", () => {
    const s = { word: "Skibigårdsvej", soundsLike: ["skibi gard vai"], meaning: "home", lang: "da" };
    const want = [{ word: "Skibigårdsvej", soundsLike: ["skibi gard vai"], meaning: "home", lang: "da" }];
    expect(parseSuggestions([s])).toEqual(want);
    expect(parseSuggestions({ candidates: [s] })).toEqual(want);
    expect(parseSuggestions({ suggestions: [s] })).toEqual(want);
    expect(parseSuggestions(s)).toEqual(want);
    expect(parseSuggestions({ candidates: [] })).toEqual([]);
    expect(parseSuggestions([{ soundsLike: ["x"] }, 3])).toEqual([]);
  });

  it("read a sample answer", () => {
    expect(parseSample({ heard: " skibi gard ", corrections: [{ from: "skibi gard", to: "Skibigårdsvej" }, { from: "", to: "x" }, 4] })).toEqual({ heard: "skibi gard", corrections: [{ from: "skibi gard", to: "Skibigårdsvej" }] });
    expect(parseSample(undefined)).toEqual({ heard: "", corrections: [] });
  });

  it("split a comma list without empties or duplicates", () => {
    expect(splitSoundsLike("a, b ,,a,  c")).toEqual(["a", "b", "c"]);
    expect(splitSoundsLike("")).toEqual([]);
  });
});

describe("voiceLearnApi", () => {
  it("calls the proxied routes with the right methods and bodies", async () => {
    const calls = stubFetch((c) => {
      if (c.method === "GET") return { body: { entries: [entry] } };
      if (c.url.endsWith("/suggest")) return { body: { candidates: [{ word: "Skibigårdsvej", soundsLike: [] }] } };
      if (c.url.includes("/sample")) return { body: { heard: "skibi gard vej", corrections: [] } };
      if (c.method === "DELETE") return { status: 200, body: { deleted: true } };
      return { status: c.method === "POST" ? 201 : 200, body: entry };
    });
    expect((await voiceLearnApi.list())[0]!.word).toBe("Skibigårdsvej");
    expect((await voiceLearnApi.create({ word: "Skibigårdsvej", lang: "da" }))?.id).toBe("vl_1");
    expect((await voiceLearnApi.update("vl_1", { meaning: "", lang: null }))?.id).toBe("vl_1");
    await voiceLearnApi.remove("a b/c");
    expect(await voiceLearnApi.suggest("ciao skibi", "ciao Skibigårdsvej")).toEqual([{ word: "Skibigårdsvej", soundsLike: [] }]);
    const wav = new Blob([new Uint8Array(48)], { type: "audio/wav" });
    expect(await voiceLearnApi.sample(wav, "da")).toEqual({ heard: "skibi gard vej", corrections: [] });
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET /v1/voice/learn",
      "POST /v1/voice/learn",
      "PATCH /v1/voice/learn/vl_1",
      "DELETE /v1/voice/learn/a%20b%2Fc",
      "POST /v1/voice/learn/suggest",
      "POST /v1/voice/learn/sample?lang=da",
    ]);
    expect(calls[1]!.body).toEqual({ word: "Skibigårdsvej", lang: "da" });
    expect(calls[2]!.body).toEqual({ meaning: "", lang: null });
    expect(calls[4]!.body).toEqual({ transcript: "ciao skibi", corrected: "ciao Skibigårdsvej" });
    expect(calls[5]!.headers["content-type"]).toBe("audio/wav");
    expect(calls[1]!.headers["x-secretary-csrf"]).toBe("1");
  });
});
