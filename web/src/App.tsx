import type { RouteSectionProps } from "@solidjs/router";
import { createMemo, Match, onMount, Switch } from "solid-js";
import { t } from "./i18n";
import { checkSession, session } from "./state/session";
import Login from "./views/Login";
import Shell from "./views/Shell";

/** Router root: gates every route behind the session. */
export default function App(props: RouteSectionProps) {
  onMount(() => void checkSession());
  const status = createMemo(() => session().status);
  const errorMessage = createMemo(() => {
    const s = session();
    return s.status === "error" ? s.message : "";
  });

  return (
    <Switch>
      <Match when={status() === "loading"}>
        <div class="splash" role="status" aria-label={t("common.loading")}>
          <span class="brand-mark" aria-hidden="true">
            <img src="/icon-192.png" alt="" width="32" height="32" />
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
      <Match when={status() === "authed"}>
        <Shell>{props.children}</Shell>
      </Match>
    </Switch>
  );
}
