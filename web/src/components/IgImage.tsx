import { Show } from "solid-js";
import { t } from "../i18n";
import { createFreshSrc } from "../lib/instagramImages";
import { RefreshIcon } from "./Icons";

// A picture behind a signed link: never a broken image. An expired link is re-signed first; a failure is asked again
// (lib/instagramImages), blank at first and "Can't load, trying again" on the tile after a couple of tries.

export function IgImage(props: {
  url: string;
  lazy?: boolean;
  /** The small places (avatars) stay blank instead of carrying the sentence. */
  quiet?: boolean;
  onGiveUp?: () => void;
}) {
  const fresh = createFreshSrc(() => props.url, () => props.onGiveUp?.());
  return (
    <>
      <Show when={fresh.src()}>
        {(src) => (
          <img
            src={src()}
            alt=""
            loading={props.lazy ? "lazy" : undefined}
            decoding="async"
            referrerpolicy="no-referrer"
            classList={{ "is-retrying": fresh.retrying() }}
            onLoad={(e) => {
              e.currentTarget.classList.add("is-loaded");
              fresh.onLoad();
            }}
            onError={fresh.onError}
          />
        )}
      </Show>
      <Show when={fresh.failed() && !props.quiet}>
        <span class="ig-img-failed" role="status">
          <RefreshIcon />
          <small>{t("instagram.mediaRetrying")}</small>
        </span>
      </Show>
    </>
  );
}
