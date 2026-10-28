import { useNavigate, useSearchParams } from "@solidjs/router";
import { createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { AccountDialog } from "../components/AccountDialog";
import { NotificationPanel } from "../components/NotificationBell";
import { closeSecretSheet, openSecretSheet, secretSheet } from "../state/secretSheet";
import { signOut } from "../state/session";
import ChatView from "./Chat";

// The sheets the web app's top bar used to open, as routes for Secretary Code's own titlebar bell (/notifications),
// rail owner button (/account) and Secrets item (/secrets). Each shows the chat with the sheet over it; closing goes back to the chat.
// They work in a browser too.

export function NotificationsView() {
  const navigate = useNavigate();
  return (
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
  );
}

export function AccountView() {
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const [signingOut, setSigningOut] = createSignal(false);
  return (
    <>
      <ChatView />
      <AccountDialog
        open
        section={params.section === "notifications" || params.section === "approvals" ? params.section : null}
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
    </>
  );
}

/** The "Add secret" sheet (rendered by the Shell) over the chat; closing it goes back to the chat. */
export function SecretsView() {
  const navigate = useNavigate();
  let opened = false;
  onMount(() => openSecretSheet());
  createEffect(() => {
    if (secretSheet()) opened = true;
    else if (opened) navigate("/", { replace: true });
  });
  onCleanup(() => {
    if (secretSheet()) closeSecretSheet();
  });
  return <ChatView />;
}
