import { useNavigate, useSearchParams } from "@solidjs/router";
import { createSignal } from "solid-js";
import { Portal } from "solid-js/web";
import { AccountDialog } from "../components/AccountDialog";
import { NotificationPanel } from "../components/NotificationBell";
import { signOut } from "../state/session";
import ChatView from "./Chat";

// The sheets the web app's top bar used to open, as routes for Secretary Code's own titlebar bell (/notifications)
// and rail owner button (/account). Each shows the chat with the sheet over it; closing goes back to the chat.
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
        section={params.section === "notifications" ? "notifications" : null}
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
