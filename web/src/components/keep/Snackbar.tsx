import { Show } from "solid-js";
import { dismissSnackbar, snackbar } from "../../state/keep";

/** Keep's bottom-left snackbar: one message, an optional action ("Undo"). */
export function Snackbar() {
  return (
    <div class="kp-snack-host" aria-live="polite" role="status">
      <Show when={snackbar()} keyed>
        {(m) => (
          <div class="kp-snack" data-snack={m.id}>
            <span>{m.text}</span>
            <Show when={m.action}>
              {(a) => (
                <button
                  type="button"
                  class="kp-snack-action"
                  onClick={() => {
                    dismissSnackbar();
                    a().run();
                  }}
                >
                  {a().label}
                </button>
              )}
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}
