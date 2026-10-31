import { For, Show } from "solid-js";
import { t } from "../i18n";
import { stepText, toolSummary } from "../lib/toolSteps";
import type { ChatMessage } from "../state/chat";

/** One turn's tool calls as a single quiet line ("Worked on Drive · 4 steps") that opens to the steps. */
export function ToolSteps(props: { steps: ChatMessage[] }) {
  const summary = () => toolSummary(props.steps);
  const label = () => {
    const tools = summary().tools.join(", ");
    const count = props.steps.length;
    const base = t(count === 1 ? "chat.toolStepsOne" : "chat.toolStepsMany", { tools, count });
    const failed = summary().failed;
    return failed ? `${base} · ${t("chat.toolStepsFailed", { count: failed })}` : base;
  };
  return (
    <details class="tool-steps" classList={{ "tool-steps--failed": summary().failed > 0 }}>
      <summary>{label()}</summary>
      <ol>
        <For each={props.steps}>
          {(s) => (
            <li classList={{ "tool-step--failed": s.meta?.ok === false }}>
              <code>{stepText(s)}</code>
              <Show when={s.meta?.ok === false}>
                <span class="sr-only">{t("chat.toolStepFailedSr")}</span>
              </Show>
            </li>
          )}
        </For>
      </ol>
    </details>
  );
}
