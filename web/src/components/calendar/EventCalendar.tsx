import { createCalendar, DayGrid, destroyCalendar, Interaction, List, TimeGrid, type Calendar } from "@event-calendar/core";
import "@event-calendar/core/index.css";
import { createEffect, on, onCleanup, onMount } from "solid-js";

// The one place that touches EventCalendar (vkurko/calendar, MIT): it mounts the calendar into a div, pushes option
// changes with setOption, and hands the instance to the view for navigation (prev/next/today/gotoDate). Views and tests
// mock this module, so nothing else imports the library.

export type EcApi = Pick<Calendar, "next" | "prev" | "gotoDate" | "getView" | "unselect" | "setOption">;

export default function EventCalendar(props: {
  /** Options that stay for the calendar's life (callbacks, formats). */
  options: Calendar.Options;
  /** The events shown; replaced as a whole when it changes. */
  events: Calendar.EventInput[];
  /** The view name (dayGridMonth, timeGridWeek, …). */
  view: string;
  height: string;
  locale: string;
  onReady?: (api: EcApi) => void;
  class?: string;
}) {
  let host: HTMLDivElement | undefined;
  let ec: Calendar | undefined;

  onMount(() => {
    if (!host) return;
    ec = createCalendar(host, [DayGrid, TimeGrid, List, Interaction], {
      ...props.options,
      view: props.view,
      height: props.height,
      locale: props.locale,
      events: props.events,
    });
    props.onReady?.(ec);
  });

  createEffect(on(() => props.events, (events) => ec?.setOption("events", events), { defer: true }));
  createEffect(
    on(
      () => props.view,
      (view) => {
        if (ec && ec.getView().type !== view) ec.setOption("view", view);
      },
      { defer: true },
    ),
  );
  createEffect(on(() => props.height, (h) => ec?.setOption("height", h), { defer: true }));
  createEffect(on(() => props.locale, (l) => ec?.setOption("locale", l), { defer: true }));

  onCleanup(() => {
    if (ec) void destroyCalendar(ec);
    ec = undefined;
  });

  return <div class={props.class} ref={host} />;
}
