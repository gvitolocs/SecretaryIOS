import { Show } from "solid-js";
import { t } from "../../i18n";
import { shiftEnd, type EventForm } from "../../lib/calendar";

// The editor's fields (views/Calendar.tsx owns the Sheet, Save and Cancel). Moving the start moves the end with it,
// keeping the event's length, as calendar apps do.

export function EventEditor(props: {
  form: EventForm;
  onChange: (next: EventForm) => void;
  error?: string;
  onSubmit: () => void;
}) {
  const set = <K extends keyof EventForm>(key: K, value: EventForm[K]) => {
    const next = { ...props.form, [key]: value };
    props.onChange(key === "startDate" || key === "startTime" ? shiftEnd(props.form, next) : next);
  };
  const id = (k: string) => `cal-ed-${k}`;

  return (
    <form
      class="cal-editor"
      novalidate
      onSubmit={(e) => {
        e.preventDefault();
        props.onSubmit();
      }}
    >
      <label class="pg-field" for={id("title")}>
        <span class="pg-field-label">{t("calendar.fieldTitle")}</span>
        <input
          id={id("title")}
          class="pg-input cal-input-title"
          type="text"
          maxLength={300}
          autocomplete="off"
          placeholder={t("calendar.fieldTitlePlaceholder")}
          value={props.form.title}
          onInput={(e) => set("title", e.currentTarget.value)}
        />
      </label>

      <label class="cal-switch">
        <input type="checkbox" role="switch" checked={props.form.allDay} onChange={(e) => set("allDay", e.currentTarget.checked)} />
        <span class="cal-switch-track" aria-hidden="true" />
        <span>{t("calendar.fieldAllDay")}</span>
      </label>

      <fieldset class="cal-when">
        <legend class="pg-field-label">{t("calendar.fieldStart")}</legend>
        <div class="cal-when-row">
          <input
            class="pg-input"
            type="date"
            aria-label={t("calendar.fieldStartDate")}
            value={props.form.startDate}
            onInput={(e) => set("startDate", e.currentTarget.value)}
          />
          <Show when={!props.form.allDay}>
            <input
              class="pg-input"
              type="time"
              step="300"
              aria-label={t("calendar.fieldStartTime")}
              value={props.form.startTime}
              onInput={(e) => set("startTime", e.currentTarget.value)}
            />
          </Show>
        </div>
      </fieldset>
      <fieldset class="cal-when">
        <legend class="pg-field-label">{t("calendar.fieldEnd")}</legend>
        <div class="cal-when-row">
          <input
            class="pg-input"
            type="date"
            aria-label={t("calendar.fieldEndDate")}
            value={props.form.endDate}
            onInput={(e) => set("endDate", e.currentTarget.value)}
          />
          <Show when={!props.form.allDay}>
            <input
              class="pg-input"
              type="time"
              step="300"
              aria-label={t("calendar.fieldEndTime")}
              value={props.form.endTime}
              onInput={(e) => set("endTime", e.currentTarget.value)}
            />
          </Show>
        </div>
      </fieldset>

      <label class="pg-field" for={id("loc")}>
        <span class="pg-field-label">{t("calendar.fieldLocation")}</span>
        <input
          id={id("loc")}
          class="pg-input"
          type="text"
          maxLength={500}
          placeholder={t("calendar.fieldLocationPlaceholder")}
          value={props.form.location}
          onInput={(e) => set("location", e.currentTarget.value)}
        />
      </label>

      <label class="pg-field" for={id("video")}>
        <span class="pg-field-label">{t("calendar.fieldVideo")}</span>
        <input
          id={id("video")}
          class="pg-input"
          type="url"
          inputmode="url"
          maxLength={1000}
          placeholder="https://meet.google.com/…"
          value={props.form.video}
          onInput={(e) => set("video", e.currentTarget.value)}
        />
      </label>

      <label class="pg-field" for={id("notes")}>
        <span class="pg-field-label">{t("calendar.fieldNotes")}</span>
        <textarea
          id={id("notes")}
          class="pg-input cal-notes-input"
          rows={4}
          maxLength={4000}
          value={props.form.notes}
          onInput={(e) => set("notes", e.currentTarget.value)}
        />
      </label>

      <Show when={props.error}>
        <p class="cal-form-error" role="alert">
          {props.error}
        </p>
      </Show>
      {/* Enter in a field saves. */}
      <button type="submit" hidden />
    </form>
  );
}
