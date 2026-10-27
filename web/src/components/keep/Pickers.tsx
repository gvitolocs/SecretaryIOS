import { createMemo, createSignal, For, Show } from "solid-js";
import { KEEP_COLORS, RECURRENCES, type KeepColor, type LabelInfo, type Recurrence, type Reminder } from "../../api/keep";
import { t, type MessageKey } from "../../i18n";
import { swatchFill, type KeepTheme } from "../../lib/keepColors";
import { fromInputs, reminderLabel, reminderPresets, toDateInput, toTimeInput } from "../../lib/keepTime";
import { cleanLabelName, normLabel } from "../../api/keep";
import { CheckIcon, ClockIcon, CloseIcon, LabelIcon, RepeatIcon } from "./icons";
import { Popover } from "./Popover";

export const colorName = (c: KeepColor): string => t(`keep.color_${c}` as MessageKey);

export function ColorPopover(props: { anchor: HTMLElement | undefined; value: KeepColor; theme: KeepTheme; onPick: (c: KeepColor) => void; onClose: () => void }) {
  return (
    <Popover anchor={props.anchor} label={t("keep.background")} onClose={props.onClose} class="kp-pop--colors">
      <div class="kp-swatches" role="radiogroup" aria-label={t("keep.background")}>
        <For each={KEEP_COLORS}>
          {(c) => (
            <button
              type="button"
              class="kp-swatch"
              classList={{ "kp-swatch--on": props.value === c, "kp-swatch--default": c === "default" }}
              role="radio"
              aria-checked={props.value === c}
              aria-label={colorName(c)}
              title={colorName(c)}
              data-color={c}
              style={{ "background-color": swatchFill(c, props.theme) }}
              onClick={() => {
                props.onPick(c);
                props.onClose();
              }}
            >
              <Show when={props.value === c}>
                <CheckIcon />
              </Show>
            </button>
          )}
        </For>
      </div>
    </Popover>
  );
}

export function ReminderPopover(props: { anchor: HTMLElement | undefined; value?: Reminder; onSave: (r: Reminder | null) => void; onClose: () => void }) {
  const base = props.value ? new Date(props.value.at) : new Date(Date.now() + 3_600_000);
  const [custom, setCustom] = createSignal(false);
  const [date, setDate] = createSignal(toDateInput(base));
  const [time, setTime] = createSignal(toTimeInput(new Date(Math.ceil(base.getTime() / 900_000) * 900_000)));
  const [repeat, setRepeat] = createSignal<Recurrence | "">(props.value?.recurrence ?? "");
  const presets = createMemo(() => reminderPresets());
  const picked = createMemo(() => fromInputs(date(), time()));

  const save = (at: Date, recurrence: Recurrence | "" = repeat()) => {
    props.onSave({ at: at.toISOString(), ...(recurrence ? { recurrence } : {}) });
    props.onClose();
  };

  return (
    <Popover anchor={props.anchor} label={t("keep.reminderTitle")} onClose={props.onClose} class="kp-pop--reminder">
      <div class="kp-pop-title">{t("keep.reminderTitle")}</div>
      <Show when={props.value}>
        {(r) => (
          <div class="kp-pop-note">
            <ClockIcon />
            <span>{reminderLabel(r().at)}</span>
            <Show when={r().recurrence}>
              <span class="kp-pop-repeat">
                <RepeatIcon />
                {t(`keep.${r().recurrence}` as MessageKey)}
              </span>
            </Show>
          </div>
        )}
      </Show>
      <Show
        when={custom()}
        fallback={
          <div class="kp-menu" role="menu">
            <For each={presets()}>
              {(p) => (
                <button type="button" class="kp-menu-item" role="menuitem" onClick={() => save(p.at, "")}>
                  <span>{t(`keep.${p.id}` as MessageKey)}</span>
                  <span class="kp-menu-hint">{reminderLabel(p.at.toISOString()).split(", ")[1]}</span>
                </button>
              )}
            </For>
            <button type="button" class="kp-menu-item" role="menuitem" onClick={() => setCustom(true)}>
              <span>
                <ClockIcon /> {t("keep.pickDateTime")}
              </span>
            </button>
            <Show when={props.value}>
              <button
                type="button"
                class="kp-menu-item"
                role="menuitem"
                onClick={() => {
                  props.onSave(null);
                  props.onClose();
                }}
              >
                <span>{t("keep.deleteReminder")}</span>
              </button>
            </Show>
          </div>
        }
      >
        <form
          class="kp-form"
          onSubmit={(e) => {
            e.preventDefault();
            const at = picked();
            if (at) save(at);
          }}
        >
          <label>
            <span>{t("keep.date")}</span>
            <input type="date" class="kp-input" value={date()} onInput={(e) => setDate(e.currentTarget.value)} required />
          </label>
          <label>
            <span>{t("keep.time")}</span>
            <input type="time" class="kp-input" value={time()} onInput={(e) => setTime(e.currentTarget.value)} required />
          </label>
          <label>
            <span>{t("keep.repeat")}</span>
            <select class="kp-input" value={repeat()} onChange={(e) => setRepeat(e.currentTarget.value as Recurrence | "")}>
              <option value="">{t("keep.noRepeat")}</option>
              <For each={RECURRENCES}>{(r) => <option value={r}>{t(`keep.${r}` as MessageKey)}</option>}</For>
            </select>
          </label>
          <p class="kp-pop-hint">{t("keep.reminderHint")}</p>
          <div class="kp-form-actions">
            <button type="button" class="kp-text-btn" onClick={() => setCustom(false)}>
              {t("keep.cancel")}
            </button>
            <button type="submit" class="kp-text-btn kp-text-btn--accent" disabled={!picked()}>
              {t("keep.save")}
            </button>
          </div>
        </form>
      </Show>
    </Popover>
  );
}

const EMAIL = /^[^\s@,]+@[^\s@,]+\.[^\s@,]+$/;

export function CollaboratorPopover(props: { anchor: HTMLElement | undefined; value: string[]; onChange: (emails: string[]) => void; onClose: () => void }) {
  const [text, setText] = createSignal("");
  const valid = () => EMAIL.test(text().trim());
  const add = () => {
    const e = text().trim().toLowerCase();
    if (!EMAIL.test(e) || props.value.includes(e)) return;
    props.onChange([...props.value, e]);
    setText("");
  };
  return (
    <Popover anchor={props.anchor} label={t("keep.collabTitle")} onClose={props.onClose} class="kp-pop--collab">
      <div class="kp-pop-title">{t("keep.collabTitle")}</div>
      <ul class="kp-people">
        <For each={props.value}>
          {(email) => (
            <li>
              <span class="kp-avatar" aria-hidden="true">
                {email.charAt(0).toUpperCase()}
              </span>
              <span class="kp-people-email">{email}</span>
              <button type="button" class="kp-icon-btn kp-icon-btn--sm" aria-label={t("keep.collabRemove", { email })} onClick={() => props.onChange(props.value.filter((x) => x !== email))}>
                <CloseIcon />
              </button>
            </li>
          )}
        </For>
      </ul>
      <form
        class="kp-people-add"
        onSubmit={(e) => {
          e.preventDefault();
          add();
        }}
      >
        <input class="kp-input" type="email" placeholder={t("keep.collabAdd")} aria-label={t("keep.collabAdd")} value={text()} onInput={(e) => setText(e.currentTarget.value)} />
        <button type="submit" class="kp-text-btn kp-text-btn--accent" disabled={!valid()}>
          {t("keep.save")}
        </button>
      </form>
      <p class="kp-pop-hint">{t("keep.collabHint")}</p>
    </Popover>
  );
}

/** "Label note": the labels with a check each, a filter and "Create …" for a new name. */
export function LabelPopover(props: {
  anchor: HTMLElement | undefined;
  all: LabelInfo[];
  selected: string[];
  onToggle: (label: LabelInfo, on: boolean) => void;
  onCreate: (name: string) => void;
  onClose: () => void;
}) {
  const [q, setQ] = createSignal("");
  const shown = createMemo(() => {
    const f = normLabel(q());
    return props.all.filter((l) => !f || l.tag.includes(f) || l.name.toLowerCase().includes(f));
  });
  const canCreate = createMemo(() => {
    const tag = normLabel(q());
    return !!tag && !props.all.some((l) => l.tag === tag);
  });
  return (
    <Popover anchor={props.anchor} label={t("keep.labelNote")} onClose={props.onClose} class="kp-pop--labels">
      <div class="kp-pop-title">{t("keep.labelNote")}</div>
      <input
        class="kp-input kp-input--flat"
        placeholder={t("keep.labelEnter")}
        aria-label={t("keep.labelEnter")}
        value={q()}
        onInput={(e) => setQ(e.currentTarget.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && canCreate()) {
            e.preventDefault();
            props.onCreate(cleanLabelName(q()));
            setQ("");
          }
        }}
      />
      <div class="kp-label-list" role="group" aria-label={t("keep.labels")}>
        <For each={shown()}>
          {(l) => {
            const on = () => props.selected.includes(l.tag);
            return (
              <label class="kp-label-row">
                <input type="checkbox" checked={on()} onChange={(e) => props.onToggle(l, e.currentTarget.checked)} />
                <span>{l.name}</span>
              </label>
            );
          }}
        </For>
        <Show when={shown().length === 0 && !canCreate()}>
          <p class="kp-pop-hint">{t("keep.labelNone")}</p>
        </Show>
      </div>
      <Show when={canCreate()}>
        <button
          type="button"
          class="kp-menu-item"
          onClick={() => {
            props.onCreate(cleanLabelName(q()));
            setQ("");
          }}
        >
          <span>
            <LabelIcon /> {t("keep.labelCreate", { name: cleanLabelName(q()) })}
          </span>
        </button>
      </Show>
    </Popover>
  );
}

export interface MenuEntry {
  id: string;
  label: string;
  run: () => void;
}

export function MenuPopover(props: { anchor: HTMLElement | undefined; label: string; entries: MenuEntry[]; onClose: () => void }) {
  return (
    <Popover anchor={props.anchor} label={props.label} onClose={props.onClose} class="kp-pop--menu">
      <div class="kp-menu" role="menu">
        <For each={props.entries}>
          {(e) => (
            <button
              type="button"
              class="kp-menu-item"
              role="menuitem"
              data-action={e.id}
              onClick={() => {
                props.onClose();
                e.run();
              }}
            >
              {e.label}
            </button>
          )}
        </For>
      </div>
    </Popover>
  );
}
