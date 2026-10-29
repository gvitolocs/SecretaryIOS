import { useNavigate, useSearchParams } from "@solidjs/router";
import { createSignal, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { useWide } from "../components/page";
import { AccountDialog } from "../components/AccountDialog";
import { NotificationPanel } from "../components/NotificationBell";
import { NotificationsPage } from "../components/NotificationsPage";
import { signOut } from "../state/session";
import ChatView from "./Chat";

// The sheets the web app's top bar used to open, as routes for Secretary Code's own titlebar bell (/notifications),
// rail owner button (/account) and Secrets item (/secrets). Each shows the chat with the sheet over it; closing goes back to the chat.
// They work in a browser too.

export function NotificationsView() {
  const { wide, track } = useWide();
  const navigate = useNavigate();
  return (
    <div ref={track}>
      <Show
        when={wide()}
        fallback={
          <>
            <ChatView />
            <Portal>
              <NotificationPanel
                open
                onClose={() => navigate("/", { replace: true })}
                onSettings={() => navigate("/account?section=notifications", { replace: true })}
              />
            </Portal>
          </>
        }
      >
        <NotificationsPage />
      </Show>
    </div>
  );
}

export function AccountView() {
  const { wide, track } = useWide();
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const [signingOut, setSigningOut] = createSignal(false);
  return (
    <div ref={track}>
      <Show when={!wide()}><ChatView /></Show>
      <AccountDialog
        desktop={wide()}
        open
        section={params.section === "notifications" || params.section === "approvals" || params.section === "import" || params.section === "plugins" ? params.section : null}
        onClose={() => navigate("/", { replace: true })}
        signingOut={signingOut()}
        onSignOut={async () => {
          if (signingOut()) return;
          setSigningOut(true);
          try {
            await signOut();
          } finally {
            setSigningOut(false);
          }
        }}
      />
    </div>
  );
}

export { SecretsView } from "./Secrets";
