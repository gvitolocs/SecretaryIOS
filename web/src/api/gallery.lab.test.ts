import { describe, expect, it } from "vitest";
import { rawJob } from "../test/labFixtures";
import { stubFetch } from "../test/fetch";
import { ApiError } from "./client";
import { acceptLabJob, cancelLabJob, createLabJob, discardLabJob, getLabJob, listLab, parseLabJob, retryLabJob } from "./gallery";

describe("parseLabJob", () => {
  it("reads the server's job", () => {
    const job = parseLabJob(
      rawJob("lab_1", {
        status: "done",
        progress: 100,
        model: "klein4b",
        result: { hash: "h", bytes: 10, width: 1024, height: 768 },
        reference_item_id: "itm_r",
        urls: { before: "/v1/media/lab/lab_1/before", after: "/v1/media/lab/lab_1/result", reference: "/v1/media/items/itm_r/thumb?size=256" },
      }),
    );
    expect(job).toMatchObject({
      id: "lab_1",
      itemId: "itm_a",
      referenceItemId: "itm_r",
      status: "done",
      progress: 100,
      model: "klein4b",
      width: 1024,
      height: 768,
      urls: { before: "/v1/media/lab/lab_1/before", after: "/v1/media/lab/lab_1/result", reference: "/v1/media/items/itm_r/thumb?size=256" },
      original: { id: "itm_a", title: "a.jpg", rotation: 0, trashed: false },
    });
  });

  it("drops what is malformed and never trusts a foreign URL", () => {
    expect(parseLabJob(null)).toBeNull();
    expect(parseLabJob({ id: "x" })).toBeNull();
    expect(parseLabJob(rawJob("lab_1", { status: "bogus" }))).toBeNull();
    const job = parseLabJob(rawJob("lab_1", { progress: 250, phase: "weird", original: null, urls: { before: "https://evil.example/x.png", after: "//evil.example/y" } }))!;
    expect(job.progress).toBe(100);
    expect(job.phase).toBeNull();
    expect(job.original).toBeNull();
    expect(job.urls).toEqual({ before: null, after: null, reference: null });
  });
});

describe("the Lab's requests", () => {
  it("lists with filters and tells whether a model is connected", async () => {
    const calls = stubFetch(() => ({ body: { enabled: false, jobs: [rawJob("lab_1"), { id: "junk" }] } }));
    const lab = await listLab({ status: ["done", "failed"], itemId: "itm a" });
    expect(lab.enabled).toBe(false);
    expect(lab.jobs.map((j) => j.id)).toEqual(["lab_1"]);
    expect(calls[0]!.url).toBe("/v1/media/lab?status=done%2Cfailed&item_id=itm+a");
    stubFetch(() => ({ body: { jobs: [] } }));
    expect((await listLab()).enabled).toBe(true);
  });

  it("queues an edit with the trimmed prompt and the reference", async () => {
    const calls = stubFetch(() => ({ status: 201, body: rawJob("lab_9") }));
    const job = await createLabJob("itm_a", { prompt: "  add a hat ", referenceItemId: "itm_r" });
    expect(job.id).toBe("lab_9");
    expect(calls[0]).toMatchObject({ method: "POST", url: "/v1/media/items/itm_a/lab", body: { prompt: "add a hat", reference_item_id: "itm_r" } });
    await createLabJob("itm_a", { prompt: "plain" });
    expect(calls[1]!.body).toEqual({ prompt: "plain" });
  });

  it("surfaces a full queue as an ApiError with its status", async () => {
    stubFetch(() => ({ status: 429, body: { error: "the Lab queue is full" } }));
    const err = await createLabJob("itm_a", { prompt: "x" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(429);
  });

  it("accepts, discards, retries and cancels", async () => {
    const calls = stubFetch((c) => {
      if (c.url.endsWith("/accept")) return { body: { job: rawJob("lab_1", { status: "accepted", accepted_item_id: "itm_new" }), item: { id: "itm_new", kind: "photo", title: "T" }, duplicate: false } };
      if (c.url.endsWith("/discard")) return { body: rawJob("lab_1", { status: "discarded" }) };
      if (c.url.endsWith("/retry")) return { body: rawJob("lab_1") };
      if (c.method === "DELETE") return { status: 204 };
      return { body: rawJob("lab_1") };
    });
    const accepted = await acceptLabJob("lab_1");
    expect(accepted.job?.status).toBe("accepted");
    expect(accepted.item?.id).toBe("itm_new");
    expect(accepted.duplicate).toBe(false);
    expect((await discardLabJob("lab_1"))?.status).toBe("discarded");
    expect((await retryLabJob("lab_1"))?.status).toBe("queued");
    await cancelLabJob("lab_1");
    expect((await getLabJob("lab_1"))?.id).toBe("lab_1");
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "POST /v1/media/lab/lab_1/accept",
      "POST /v1/media/lab/lab_1/discard",
      "POST /v1/media/lab/lab_1/retry",
      "DELETE /v1/media/lab/lab_1",
      "GET /v1/media/lab/lab_1",
    ]);
  });
});
