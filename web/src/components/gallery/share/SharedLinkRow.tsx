import { createSignal, Show } from "solid-js";
import type { GalleryShare } from "../../../api/gallery";
import { locale, t } from "../../../i18n";
import { Button, ListRow } from "../../page";
import { expiryState, shareUrl } from "../../../lib/galleryShare";

export interface SharedLinkRowProps {
  /** The page-system row (desktop); the phone keeps its own markup. */
  wide?: boolean;
  share: GalleryShare;
  /** Copies the link; resolves true when it was copied. */
  onCopy: (share: GalleryShare) => Promise<boolean>;
  /** Turns the link off; rejects when the server refuses. */
  onRevoke: (share: GalleryShare) => Promise<void>;
}

function when(iso: string | null, lang: string): string {
  if (!iso) return "";
  const at = Date.parse(iso);
  return Number.isNaN(at) ? "" : new Intl.DateTimeFormat(lang, { year: "numeric", month: "short", day: "numeric" }).format(at);
}

/** One link in the list: what it shares, how it is doing, and Copy / Open / Turn off. */
export function SharedLinkRow(props: SharedLinkRowProps) {
  const [confirming, setConfirming] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [copied, setCopied] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const share = () => props.share;
  const state = () => expiryState(share(), Date.now());

  const life = () => {
    const s = state();
    if (s.kind === "off") return t("galleryShare.stateOff", { date: when(share().revokedAt, locale()) });
    if (s.kind === "expired") return t("galleryShare.stateExpired", { date: when(share().expiresAt, locale()) });
    if (s.kind === "never") return t("galleryShare.neverExpires");
    return t("galleryShare.expiresIn", { count: s.days });
  };
  const seen = () =>
    share().views === 0
      ? t("galleryShare.noViews")
      : `${t("galleryShare.views", { count: share().views })}${share().lastViewAt ? ` · ${t("galleryShare.lastView", { date: when(share().lastViewAt, locale()) })}` : ""}`;

  const copy = async () => {
    setFailed(false);
    const ok = await props.onCopy(share());
    setCopied(ok);
    setFailed(!ok);
    if (ok) window.setTimeout(() => setCopied(false), 2000);
  };
  const revoke = async () => {
    if (busy()) return;
    setBusy(true);
    setFailed(false);
    try {
      await props.onRevoke(share());
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
      setConfirming(false);
    }
  };

  const phoneRow = () => (
    <li class="sl-row" classList={{ "sl-row--over": !share().live }}>
      <div class="sl-cover" aria-hidden="true">
        <Show when={share().coverThumb}>{(src) => <img src={src()} alt="" loading="lazy" draggable={false} />}</Show>
      </div>
      <div class="sl-main">
        <div class="sl-title">
          <span class="sl-name">{share().title || t("galleryShare.untitled")}</span>
          <span class="sl-badge">{share().kind === "album" ? t("galleryShare.kindAlbum") : t("galleryShare.kindPhotos")}</span>
        </div>
        <p class="sl-meta">
          {t("galleryShare.itemCount", { count: share().count })} · {seen()}
        </p>
        <p class="sl-meta">
          {life()} · {share().allowOriginal ? t("galleryShare.originalsAllowed") : t("galleryShare.originalsNot")}
        </p>
        <Show when={failed()}>
          <p class="gv-error" role="alert">
            {t("galleryShare.rowFailed")}
          </p>
        </Show>
      </div>
      <Show when={share().live}>
        <div class="sl-actions">
          <Show
            when={!confirming()}
            fallback={
              <>
                <span class="sl-ask">{t("galleryShare.turnOffAsk")}</span>
                <button type="button" class="btn btn--danger" disabled={busy()} onClick={() => void revoke()}>
                  {busy() ? t("galleryShare.turningOff") : t("galleryShare.turnOffYes")}
                </button>
                <button type="button" class="btn" disabled={busy()} onClick={() => setConfirming(false)}>
                  {t("common.cancel")}
                </button>
              </>
            }
          >
            <button type="button" class="btn" onClick={() => void copy()} aria-label={`${t("galleryShare.copy")}: ${share().title}`}>
              {copied() ? t("galleryShare.copiedShort") : t("galleryShare.copy")}
            </button>
            <a class="btn" href={shareUrl(share(), window.location)} target="_blank" rel="noopener noreferrer">
              {t("galleryShare.open")}
            </a>
            <button type="button" class="btn" onClick={() => setConfirming(true)} aria-label={`${t("galleryShare.turnOff")}: ${share().title}`}>
              {t("galleryShare.turnOff")}
            </button>
          </Show>
        </div>
      </Show>
    </li>
  );

  const wideRow = () => (
    <ListRow
      class={share().live ? "" : "sl-row--over"}
      leading={
        <div class="sl-cover" aria-hidden="true">
          <Show when={share().coverThumb}>{(src) => <img src={src()} alt="" loading="lazy" draggable={false} />}</Show>
        </div>
      }
      title={share().title || t("galleryShare.untitled")}
      badge={share().kind === "album" ? t("galleryShare.kindAlbum") : t("galleryShare.kindPhotos")}
      sub={
        <>
          <span class="sl-line">
            {t("galleryShare.itemCount", { count: share().count })} · {seen()}
          </span>
          <span class="sl-line">
            {life()} · {share().allowOriginal ? t("galleryShare.originalsAllowed") : t("galleryShare.originalsNot")}
          </span>
          <Show when={failed()}>
            <span class="sl-line gv-error" role="alert">
              {t("galleryShare.rowFailed")}
            </span>
          </Show>
        </>
      }
      trailing={
        <Show when={share().live}>
          <div class="sl-actions">
            <Show
              when={!confirming()}
              fallback={
                <>
                  <span class="sl-ask">{t("galleryShare.turnOffAsk")}</span>
                  <Button variant="danger" disabled={busy()} onClick={() => void revoke()}>
                    {busy() ? t("galleryShare.turningOff") : t("galleryShare.turnOffYes")}
                  </Button>
                  <Button variant="ghost" disabled={busy()} onClick={() => setConfirming(false)}>
                    {t("common.cancel")}
                  </Button>
                </>
              }
            >
              <Button onClick={() => void copy()} aria-label={`${t("galleryShare.copy")}: ${share().title}`}>
                {copied() ? t("galleryShare.copiedShort") : t("galleryShare.copy")}
              </Button>
              <a class="pg-btn" href={shareUrl(share(), window.location)} target="_blank" rel="noopener noreferrer">
                {t("galleryShare.open")}
              </a>
              <Button variant="ghost" onClick={() => setConfirming(true)} aria-label={`${t("galleryShare.turnOff")}: ${share().title}`}>
                {t("galleryShare.turnOff")}
              </Button>
            </Show>
          </div>
        </Show>
      }
    />
  );

  return <Show when={props.wide} fallback={phoneRow()}>{wideRow()}</Show>;
}
