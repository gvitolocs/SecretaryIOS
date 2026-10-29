// The global approval pop-up (signed in only): polls the pending requests and shows them one at a time over any
// page, in the browser, on Android and inside Secretary Code (where the top and tab bars are not rendered; this
// sits in the shell root and does not depend on them).

import { useLocation } from "@solidjs/router";
import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { t } from "../i18n";
import { queuedApprovals, skipApproval, startApprovals, type Outcome } from "../state/approvals";
import { ApprovalCard, ApprovalOutcome } from "./ApprovalCard";

/** How long the "Approved." state stays before the next request comes up. */
export const FLASH_MS = 1800;

export function ApprovalPopup() {
  const location = useLocation();
  const [flash, setFlash] = createSignal<Outcome | null>(null);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => onCleanup(startApprovals()));
  onCleanup(() => flashTimer !== undefined && clearTimeout(flashTimer));

  // The /approvals pages show requests themselves.
  const onApprovalsPage = () => location.pathname === "/approvals" || location.pathname.startsWith("/approvals/");
  const queue = () => queuedApprovals();
  const current = () => queue()[0];
  const shown = () => !onApprovalsPage() && (flash() !== null || current() !== undefined);

  function closed(outcome: Outcome) {
    setFlash(outcome);
    if (flashTimer !== undefined) clearTimeout(flashTimer);
    flashTimer = setTimeout(() => setFlash(null), FLASH_MS);
  }

  // Escape puts the request off; the pop-up never traps the page behind it for good.
  createEffect(() => {
    if (!shown()) return;
    const onKey = (e: KeyboardEvent) => {
      const c = current();
      if (e.key === "Escape" && c && flash() === null) skipApproval(c.id);
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });

  return (
    <Show when={shown()}>
      <div class="apv-overlay">
        <div class="apv-dialog" role="alertdialog" aria-modal="true" aria-label={t("approvals.popupLabel")}>
          <Show
            when={flash()}
            fallback={
              <Show when={current()} keyed>
                {(item) => (
                  <ApprovalCard
                    item={item}
                    position={{ index: 1, total: queue().length }}
                    onDone={closed}
                    onLater={() => skipApproval(item.id)}
                  />
                )}
              </Show>
            }
          >
            {(o) => <ApprovalOutcome outcome={o()} />}
          </Show>
        </div>
      </div>
    </Show>
  );
}
