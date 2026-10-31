import { createMemo, For, Show } from "solid-js";
import type { CalEvent, CalProposal } from "../../api/calendar";
import { locale, t } from "../../i18n";
import {
  calendarColor,
  calendarName,
  durationParts,
  meetingUrl,
  plainNotes,
  whenLabel,
} from "../../lib/calendar";
import { placeRoute } from "../../lib/mapKit";
import { InAppLink } from "../InAppLink";
import { LinkifiedText } from "../LinkifiedText";
import { ClockIcon, ExternalIcon, RepeatIcon, UserIcon } from "../Icons";

// The body of the event sheet (views/Calendar.tsx owns the Sheet and its buttons): when, where (with a map link),
// the video call, notes, guests, who added it and where it lives.

const MAX_GUESTS = 10;

export function durationLabel(e: CalEvent): string {
  const d = durationParts(e);
  if ("days" in d) return d.days > 1 ? t("calendar.durationDays", { count: d.days }) : "";
  if (d.h && d.m) return t("calendar.durationHMin", { h: d.h, m: d.m });
  if (d.h) return t("calendar.durationH", { h: d.h });
  return t("calendar.durationMin", { m: d.m });
}

export function sourceLine(e: CalEvent): string {
  if (e.source === "lucina") return t("calendar.sourceLucina");
  if (e.source === "google") return t("calendar.sourceGoogle");
  if (e.source === "iphone") return t("calendar.sourceIphone");
  return t("calendar.sourceIcs", { name: calendarName(e) });
}

function PlaceIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 21s-6.5-5.6-6.5-11a6.5 6.5 0 0 1 13 0c0 5.4-6.5 11-6.5 11z" />
      <circle cx="12" cy="10" r="2.4" />
    </svg>
  );
}

function VideoIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3" y="6.5" width="12.5" height="11" rx="2.5" />
      <path d="m15.5 10.5 5-3v9l-5-3z" />
    </svg>
  );
}

export function EventDetail(props: { event: CalEvent }) {
  const e = () => props.event;
  const color = createMemo(() => calendarColor(calendarName(e()), e().source));
  const notes = createMemo(() => plainNotes(e().description));
  const video = createMemo(() => meetingUrl(e()));
  const guests = createMemo(() => e().attendees ?? []);
  const mapHref = createMemo(() => {
    const loc = e().location?.trim();
    if (!loc) return null;
    return placeRoute({ title: e().title, address: loc, when: e().allDay ? undefined : new Date(e().startMs).toISOString() });
  });
  const gmaps = createMemo(() => {
    const loc = e().location?.trim();
    return loc ? `https://www.google.com/maps/search/?api=1&query=${encodeURIComponent(loc)}` : null;
  });

  return (
    <div class="cal-detail">
      <p class="cal-detail-cal">
        <span class="cal-dot" style={{ "--cal-color": color() }} aria-hidden="true" />
        <span>{calendarName(e())}</span>
        <Show when={e().recurring}>
          <span class="pg-badge cal-detail-badge">
            <RepeatIcon />
            {t("calendar.repeats")}
          </span>
        </Show>
      </p>

      <div class="cal-detail-row">
        <ClockIcon />
        <div>
          <p class="cal-detail-strong">{whenLabel(e(), locale())}</p>
          <p class="pg-muted">{e().allDay ? [t("calendar.allDay"), durationLabel(e())].filter(Boolean).join(" · ") : durationLabel(e())}</p>
        </div>
      </div>

      <Show when={e().location}>
        {(loc) => (
          <div class="cal-detail-row">
            <PlaceIcon />
            <div class="cal-detail-grow">
              <p class="cal-detail-strong cal-detail-wrap">{loc()}</p>
              <p class="cal-detail-links">
                <Show when={mapHref()}>{(href) => <InAppLink href={href()} class="pg-link cal-link">{t("calendar.showOnMap")}</InAppLink>}</Show>
                <Show when={gmaps()}>
                  {(href) => (
                    <a href={href()} target="_blank" rel="noopener noreferrer" class="pg-link cal-link">
                      {t("calendar.googleMaps")}
                    </a>
                  )}
                </Show>
              </p>
            </div>
          </div>
        )}
      </Show>

      <Show when={video()}>
        {(href) => (
          <a class="pg-btn cal-join" href={href()} target="_blank" rel="noopener noreferrer">
            <VideoIcon />
            {t("calendar.joinCall")}
          </a>
        )}
      </Show>

      <Show when={notes()}>
        <section class="cal-detail-section" aria-label={t("calendar.notes")}>
          <h3 class="cal-detail-h">{t("calendar.notes")}</h3>
          <p class="cal-detail-notes">
            <LinkifiedText text={notes()} />
          </p>
        </section>
      </Show>

      <Show when={guests().length}>
        <section class="cal-detail-section" aria-label={t("calendar.guests")}>
          <h3 class="cal-detail-h">{t("calendar.guests")}</h3>
          <ul class="cal-guests">
            <For each={guests().slice(0, MAX_GUESTS)}>{(g) => <li>{g}</li>}</For>
          </ul>
          <Show when={guests().length > MAX_GUESTS}>
            <p class="pg-muted">{t("calendar.moreGuests", { count: guests().length - MAX_GUESTS })}</p>
          </Show>
        </section>
      </Show>

      <div class="cal-detail-meta">
        <Show when={e().addedBy}>
          {(who) => (
            <p class="cal-detail-row cal-detail-row--quiet">
              <UserIcon />
              <span>{t("calendar.addedBy", { name: who() })}</span>
            </p>
          )}
        </Show>
        <p class="pg-muted">{sourceLine(e())}</p>
        <Show when={e().url && e().source === "google"}>
          <a href={e().url} target="_blank" rel="noopener noreferrer" class="pg-link cal-link">
            {t("calendar.openInGoogle")}
            <ExternalIcon />
          </a>
        </Show>
      </div>
    </div>
  );
}

/** The body of a pending proposal's sheet. */
export function ProposalDetail(props: { proposal: CalProposal; when: string }) {
  const p = () => props.proposal;
  return (
    <div class="cal-detail">
      <p class="cal-detail-cal">
        <span class="cal-dot cal-dot--proposal" aria-hidden="true" />
        <span>{t("calendar.proposed")}</span>
      </p>
      <div class="cal-detail-row">
        <ClockIcon />
        <p class="cal-detail-strong">{props.when}</p>
      </div>
      <Show when={p().event.location}>
        {(loc) => (
          <div class="cal-detail-row">
            <PlaceIcon />
            <p class="cal-detail-strong cal-detail-wrap">{loc()}</p>
          </div>
        )}
      </Show>
      <Show when={plainNotes(p().event.description)}>
        {(n) => (
          <p class="cal-detail-notes">
            <LinkifiedText text={n()} />
          </p>
        )}
      </Show>
      <div class="cal-detail-meta">
        <p class="pg-muted">{t("calendar.proposedBy", { from: p().from })}</p>
        <Show when={p().reason}>{(r) => <p class="pg-muted">{t("calendar.proposalReason", { reason: r() })}</p>}</Show>
      </div>
    </div>
  );
}
