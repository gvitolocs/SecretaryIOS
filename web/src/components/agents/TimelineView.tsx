import { For, Match, Show, Switch } from "solid-js";
import type { ToolCallDetail } from "../../api/agents";
import { t } from "../../i18n";
import { toolLine, type Entry } from "../../lib/agentsTimeline";
import { Markdown } from "../Markdown";

function s(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function json(v: unknown): string {
  try {
    return typeof v === "string" ? v : JSON.stringify(v, null, 2);
  } catch {
    return "";
  }
}

/** The body of a tool call, by its detail kind (shown when the row is opened). */
function ToolBody(props: { detail: ToolCallDetail | undefined; error: unknown }) {
  const d = () => props.detail;
  return (
    <div class="ag-tool__body">
      <Switch>
        <Match when={d()?.type === "shell"}>
          <pre class="ag-code">{`$ ${s(d()!.command)}`}</pre>
          <Show when={s(d()!.output)}>
            <pre class="ag-code ag-code--out">{s(d()!.output)}</pre>
          </Show>
          <Show when={typeof d()!.exitCode === "number"}>
            <p class="ag-muted">{t("agents.exitCode", { code: d()!.exitCode as number })}</p>
          </Show>
        </Match>
        <Match when={d()?.type === "read"}>
          <p class="ag-muted">{s(d()!.filePath)}</p>
          <Show when={s(d()!.content)}>
            <pre class="ag-code ag-code--out">{s(d()!.content)}</pre>
          </Show>
        </Match>
        <Match when={d()?.type === "edit"}>
          <p class="ag-muted">{s(d()!.filePath)}</p>
          <Show
            when={s(d()!.unifiedDiff)}
            fallback={
              <>
                <Show when={s(d()!.oldString)}>
                  <pre class="ag-code ag-code--del">{s(d()!.oldString)}</pre>
                </Show>
                <Show when={s(d()!.newString)}>
                  <pre class="ag-code ag-code--add">{s(d()!.newString)}</pre>
                </Show>
              </>
            }
          >
            <pre class="ag-code">{s(d()!.unifiedDiff)}</pre>
          </Show>
        </Match>
        <Match when={d()?.type === "write"}>
          <p class="ag-muted">{s(d()!.filePath)}</p>
          <Show when={s(d()!.content)}>
            <pre class="ag-code ag-code--out">{s(d()!.content)}</pre>
          </Show>
        </Match>
        <Match when={d()?.type === "search"}>
          <p class="ag-muted">{s(d()!.query)}</p>
          <Show when={s(d()!.content)}>
            <pre class="ag-code ag-code--out">{s(d()!.content)}</pre>
          </Show>
          <Show when={Array.isArray(d()!.filePaths)}>
            <pre class="ag-code ag-code--out">{(d()!.filePaths as string[]).join("\n")}</pre>
          </Show>
        </Match>
        <Match when={d()?.type === "fetch"}>
          <p class="ag-muted">{s(d()!.url)}</p>
          <Show when={s(d()!.result)}>
            <pre class="ag-code ag-code--out">{s(d()!.result)}</pre>
          </Show>
        </Match>
        <Match when={d()?.type === "sub_agent"}>
          <p class="ag-muted">{s(d()!.description)}</p>
          <Show when={s(d()!.log)}>
            <pre class="ag-code ag-code--out">{s(d()!.log)}</pre>
          </Show>
        </Match>
        <Match when={d()?.type === "worktree_setup"}>
          <pre class="ag-code ag-code--out">{s(d()!.log)}</pre>
        </Match>
        <Match when={d()?.type === "plan"}>
          <Markdown text={s(d()!.text)} />
        </Match>
        <Match when={d()?.type === "plain_text"}>
          <pre class="ag-code ag-code--out">{s(d()!.text)}</pre>
        </Match>
        <Match when={true}>
          <Show when={d()?.input !== undefined}>
            <pre class="ag-code">{json(d()!.input)}</pre>
          </Show>
          <Show when={d()?.output !== undefined && d()?.output !== null}>
            <pre class="ag-code ag-code--out">{json(d()!.output)}</pre>
          </Show>
        </Match>
      </Switch>
      <Show when={props.error}>
        <pre class="ag-code ag-code--err">{json(props.error)}</pre>
      </Show>
    </div>
  );
}

function ToolRow(props: { entry: Entry }) {
  const item = () => props.entry.item;
  const line = () => toolLine(item());
  const status = () => item().status ?? "completed";
  return (
    <details class="ag-tool" data-status={status()} data-kind={line().kind}>
      <summary>
        <span class="ag-tool__dot" aria-hidden="true" />
        <span class="ag-tool__title">{line().title}</span>
        <span class="ag-tool__sum">{line().summary}</span>
        <span class="ag-tool__state">{status() === "running" ? t("agents.toolRunning") : status() === "failed" ? t("agents.toolFailed") : status() === "canceled" ? t("agents.toolCanceled") : ""}</span>
      </summary>
      <ToolBody detail={item().detail} error={item().error} />
    </details>
  );
}

/** The chat: one block per projected timeline entry. */
export function TimelineView(props: { entries: Entry[] }) {
  return (
    <For each={props.entries}>
      {(e) => (
        <div class="ag-row" data-seq={e.seqStart}>
        <Switch>
          <Match when={e.item.type === "user_message"}>
            <div class="ag-msg ag-msg--user">
              <p>{e.item.text}</p>
            </div>
          </Match>
          <Match when={e.item.type === "assistant_message"}>
            <div class="ag-msg ag-msg--assistant">
              <Markdown text={e.item.text ?? ""} />
            </div>
          </Match>
          <Match when={e.item.type === "reasoning"}>
            <details class="ag-reason">
              <summary>{t("agents.thinking")}</summary>
              <p>{e.item.text}</p>
            </details>
          </Match>
          <Match when={e.item.type === "tool_call"}>
            <ToolRow entry={e} />
          </Match>
          <Match when={e.item.type === "todo"}>
            <ul class="ag-todo" aria-label={t("agents.todo")}>
              <For each={e.item.items ?? []}>
                {(x) => (
                  <li data-done={x.completed || x.status === "completed" ? "yes" : "no"}>
                    <span aria-hidden="true">{x.completed || x.status === "completed" ? "✓" : x.status === "in_progress" ? "…" : "○"}</span> {x.text}
                  </li>
                )}
              </For>
            </ul>
          </Match>
          <Match when={e.item.type === "error"}>
            <p class="ag-error" role="alert">{e.item.message}</p>
          </Match>
          <Match when={e.item.type === "notification"}>
            <p class="ag-info" data-level={e.item.level}>{e.item.message}</p>
          </Match>
          <Match when={e.item.type === "compaction"}>
            <p class="ag-divider"><span>{t("agents.compaction")}</span></p>
          </Match>
        </Switch>
        </div>
      )}
    </For>
  );
}
