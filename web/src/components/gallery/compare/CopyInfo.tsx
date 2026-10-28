import { createEffect, createResource, createSignal, For, on, Show, type JSX } from "solid-js";
import { getRenditions, type GalleryItem, type ItemRenditions } from "../../../api/gallery";
import { locale, t, type MessageKey } from "../../../i18n";
import { formatBytes } from "../../../lib/drive";
import { compareSources, defaultPair, deviceVerdict, dimensionsText, formatName, shortHash } from "../../../lib/galleryCompare";
import { supportsJxl } from "../../../lib/jxl";
import { CompareView } from "./CompareView";
import "../../../styles/gallery-compare.css";

type Tab = "server" | "device";

function Row(props: { label: string; children: JSX.Element }) {
  return (
    <>
      <dt>{props.label}</dt>
      <dd>{props.children}</dd>
    </>
  );
}

function when(iso: string | null | undefined, lang: string): string {
  if (!iso) return "";
  const at = Date.parse(iso);
  return Number.isNaN(at) ? "" : new Intl.DateTimeFormat(lang, { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(at);
}

function Checksum(props: { hash: string | null | undefined }) {
  return (
    <Show when={props.hash} fallback={<span class="gv-sub">{t("galleryCompare.notReported")}</span>}>
      {(hash) => (
        <code class="cc-hash" title={hash()}>
          {shortHash(hash())}
        </code>
      )}
    </Show>
  );
}

/**
 * "Server copy / Device copy" in the info panel: the format, size and checksum of what Lucina stores against what the
 * device said it sent, the other stored forms, and a Compare button that opens them side by side. The device copy is
 * what the app reported when it uploaded; the original on the phone itself can't be read from here.
 */
export function CopyInfo(props: { item: GalleryItem }) {
  const [data] = createResource(
    () => props.item.id,
    async (id): Promise<ItemRenditions | null> => {
      try {
        return await getRenditions(id);
      } catch {
        return null;
      }
    },
  );
  const [jxl] = createResource(supportsJxl);
  const [tab, setTab] = createSignal<Tab>("server");
  const [comparing, setComparing] = createSignal(false);
  const lang = () => locale();
  // Another picture: back to the server copy, and the compare view (if open) is for the old one.
  createEffect(
    on(
      () => props.item.id,
      () => {
        setTab("server");
        setComparing(false);
      },
      { defer: true },
    ),
  );

  const device = () => data()?.device ?? null;
  // The device tab is only for pictures that came from a device; if the picture changes under it, go back to the server.
  const shown = (): Tab => (tab() === "device" && device() ? "device" : "server");
  const pair = () => {
    const d = data();
    return d && props.item.kind === "photo" ? defaultPair(compareSources(d, jxl() ?? false)) : null;
  };

  const sizeText = (bytes: number | null | undefined) => (bytes !== null && bytes !== undefined ? formatBytes(bytes, lang()) : "");

  const Server = (p: { d: ItemRenditions }) => (
    <dl class="gv-rows cc-rows">
      <Row label={t("galleryCompare.format")}>
        {formatName(p.d.original.format || p.d.original.mime)}
        <Show when={p.d.original.mime}>
          <span class="gv-sub">{p.d.original.mime}</span>
        </Show>
      </Row>
      <Row label={t("galleryCompare.size")}>
        <span title={p.d.original.bytes !== null ? `${p.d.original.bytes} B` : undefined}>{sizeText(p.d.original.bytes) || t("galleryCompare.notReported")}</span>
      </Row>
      <Show when={dimensionsText(p.d.original)}>
        <Row label={t("galleryCompare.dimensions")}>{dimensionsText(p.d.original)}</Row>
      </Show>
      <Row label={t("galleryCompare.checksum")}>
        <Checksum hash={p.d.original.blake3} />
      </Row>
      <Show when={p.d.original.stored}>
        {(where) => <Row label={t("galleryCompare.stored")}>{t(`galleryCompare.stored_${where()}` as MessageKey)}</Row>}
      </Show>
      <Show when={!p.d.original.reachable}>
        <Row label={t("galleryCompare.stored")}>
          <span class="gv-sub">{t("galleryCompare.unreachable")}</span>
        </Row>
      </Show>
      <Show when={p.d.versions.length}>
        <Row label={t("galleryCompare.versions")}>{t("galleryCompare.olderVersions", { count: p.d.versions.length })}</Row>
      </Show>
    </dl>
  );

  const Device = (p: { d: ItemRenditions }) => {
    const verdict = () => deviceVerdict(p.d);
    return (
      <Show when={p.d.device}>
        {(dev) => (
          <>
            <dl class="gv-rows cc-rows">
              <Row label={t("galleryCompare.device")}>
                {dev().name ?? dev().id}
                <Show when={dev().kind}>
                  <span class="gv-sub">{dev().kind}</span>
                </Show>
              </Row>
              <Show when={dev().latest}>
                {(up) => (
                  <>
                    <Show when={up().filename}>
                      <Row label={t("galleryCompare.fileName")}>{up().filename}</Row>
                    </Show>
                    <Row label={t("galleryCompare.format")}>
                      {up().mime ?? t("galleryCompare.notReported")}
                      <Show when={up().uti}>
                        <span class="gv-sub">{up().uti}</span>
                      </Show>
                    </Row>
                    <Row label={t("galleryCompare.size")}>{sizeText(up().bytes) || t("galleryCompare.notReported")}</Row>
                    <Show when={dimensionsText(up())}>
                      <Row label={t("galleryCompare.dimensions")}>{dimensionsText(up())}</Row>
                    </Show>
                    <Row label={t("galleryCompare.checksum")}>
                      <Checksum hash={up().blake3} />
                    </Row>
                    <Show when={up().uploadedAt}>
                      <Row label={t("galleryCompare.uploaded")}>{when(up().uploadedAt, lang())}</Row>
                    </Show>
                  </>
                )}
              </Show>
              <Show when={dev().uploads.length > 1}>
                <Row label={t("galleryCompare.uploads")}>{t("galleryCompare.uploadCount", { count: dev().uploads.length })}</Row>
              </Show>
            </dl>
            <p class="cc-verdict" classList={{ "cc-verdict--same": verdict() === "same", "cc-verdict--differs": verdict() === "differs" }} role="status">
              {t(`galleryCompare.verdict_${verdict()}` as MessageKey)}
            </p>
            <p class="gv-sub cc-note">{t("galleryCompare.deviceNote")}</p>
          </>
        )}
      </Show>
    );
  };

  return (
    <Show when={!data.loading && data()}>
      {(d) => (
        <section class="gv-section cc" aria-label={t("galleryCompare.copies")}>
          <div class="cc-tabs" role="tablist" aria-label={t("galleryCompare.copies")}>
            <button type="button" role="tab" class="cc-tab" classList={{ "cc-tab--on": shown() === "server" }} aria-selected={shown() === "server"} onClick={() => setTab("server")}>
              {t("galleryCompare.tabServer")}
            </button>
            <button
              type="button"
              role="tab"
              class="cc-tab"
              classList={{ "cc-tab--on": shown() === "device" }}
              aria-selected={shown() === "device"}
              disabled={!device()}
              title={device() ? undefined : t("galleryCompare.noDevice")}
              onClick={() => setTab("device")}
            >
              {t("galleryCompare.tabDevice")}
            </button>
          </div>
          <Show when={shown() === "server"} fallback={<Device d={d()} />}>
            <Server d={d()} />
            <Show when={d().renditions.length}>
              <details class="cc-more">
                <summary>{t("galleryCompare.displayCopies", { count: d().renditions.length })}</summary>
                <ul class="cc-list">
                  <For each={d().renditions}>
                    {(r) => (
                      <li>
                        <span>{t(`galleryCompare.rn_${r.name}` as MessageKey)}</span>
                        <span class="gv-sub">
                          {[formatName(r.format || r.mime), sizeText(r.bytes), dimensionsText(r)].filter(Boolean).join(" · ")}
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
              </details>
            </Show>
          </Show>
          <Show when={props.item.kind === "photo"}>
            <button type="button" class="gv-small-btn cc-compare" disabled={!pair()} title={pair() ? undefined : t("galleryCompare.compareUnavailable")} onClick={() => setComparing(true)}>
              {t("galleryCompare.compare")}
            </button>
          </Show>
          <Show when={comparing()}>
            <CompareView name={props.item.title || props.item.origin.filename || props.item.id} data={d()} onClose={() => setComparing(false)} />
          </Show>
        </section>
      )}
    </Show>
  );
}
