import type { RouteSectionProps } from "@solidjs/router";
import { createMemo, Match, onMount, Switch } from "solid-js";
import { chat, hydrateCachedChat } from "./state/chat";
import { t } from "./i18n";
import { checkSession, session } from "./state/session";
import Login from "./views/Login";
import Shell from "./views/Shell";
import { MediaMenuHost } from "./components/MediaMenu";

/** Router root: gates every route behind the session. */
export default function App(props: RouteSectionProps) {
  onMount(() => { void hydrateCachedChat(); void checkSession(); });
  const status = createMemo(() => session().status);
  const errorMessage = createMemo(() => {
    const s = session();
    return s.status === "error" ? s.message : "";
  });

  return (
    <Switch>
      <Match when={status() === "loading" && !chat.messages.length}>
        <div class="splash" role="status" aria-label={t("common.loading")}>
          <span class="brand-mark" aria-hidden="true">
            <img src="/icon-192.png" alt="" width="76" height="76" />
          </span>
        </div>
      </Match>
      <Match when={status() === "error"}>
        <main class="splash">
          <div class="empty" role="alert">
            <p class="empty-title">{t("shell.serverDown")}</p>
            <p class="empty-sub">{errorMessage()}</p>
            <button type="button" class="btn btn--primary" onClick={() => void checkSession()}>
              {t("common.tryAgain")}
            </button>
          </div>
        </main>
      </Match>
      <Match when={status() === "anon"}>
        <Login />
      </Match>
      <Match when={status() === "authed" || (status() === "loading" && chat.messages.length > 0)}>
        <Shell>{props.children}</Shell>
        <MediaMenuHost />
      </Match>
    </Switch>
  );
}
