import type { Parcel } from "../api/parcels";
import { t } from "../i18n";
import { parseTs } from "../api/timeline";

// Display helpers for the Parcels plugin: carrier names, stage names and colours, and the list order.

/** The common carriers by the scraper's id (a prefix before `-`, as in `gls-dk`, counts); an id not listed falls back below. */
const CARRIERS: { match: RegExp; name: string }[] = [
  { match: /^postnord$/, name: "PostNord" },
  { match: /^gls(-|$)/, name: "GLS" },
  { match: /^dao$/, name: "DAO" },
  { match: /^bring-posten$/, name: "Bring" },
  { match: /^dhl(-|$)/, name: "DHL" },
  { match: /^ups$/, name: "UPS" },
  { match: /^poste-italiane$/, name: "Poste Italiane" },
  { match: /^dpd(-|$)/, name: "DPD" },
  { match: /^budbee$/, name: "Budbee" },
  { match: /^instabox$/, name: "Instabox" },
];

/** "postnord" → PostNord, "gls-dk" → GLS, "some-carrier" → Some Carrier. */
export function carrierName(id: string | null | undefined): string {
  const key = (id ?? "").trim().toLowerCase();
  if (!key) return "";
  const known = CARRIERS.find((c) => c.match.test(key));
  if (known) return known.name;
  return key
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

const STAGE_KEYS = {
  pending: "parcels.stagePending",
  registered: "parcels.stageRegistered",
  accepted: "parcels.stageAccepted",
  in_transit: "parcels.stageInTransit",
  out_for_delivery: "parcels.stageOutForDelivery",
  delivered: "parcels.stageDelivered",
  customs: "parcels.stageCustoms",
  failed_attempt: "parcels.stageFailedAttempt",
  ready_for_pickup: "parcels.stageReadyForPickup",
  returned: "parcels.stageReturned",
  exception: "parcels.stageException",
  unknown: "parcels.stageUnknown",
} as const;

/** The readable name of a stage; a stage this app does not know shows as its id with spaces ("held_at_depot" → "Held at depot"). */
export function stageName(stage: string | null | undefined): string {
  const key = (stage ?? "").trim();
  if (Object.hasOwn(STAGE_KEYS, key)) return t(STAGE_KEYS[key as keyof typeof STAGE_KEYS]);
  const words = key.replace(/[-_]+/g, " ").trim();
  return words ? words.charAt(0).toUpperCase() + words.slice(1) : t(STAGE_KEYS.unknown);
}

export type StageTone = "ok" | "info" | "warn" | "bad" | "muted";

/** The colour family of a stage's chip and timeline dot (the app's --pg-success / --pg-data / --pg-warning / --pg-danger). */
export function stageTone(stage: string | null | undefined): StageTone {
  switch (stage) {
    case "delivered":
      return "ok";
    case "in_transit":
    case "out_for_delivery":
    case "accepted":
    case "registered":
    case "customs":
      return "info";
    case "ready_for_pickup":
    case "returned":
      return "warn";
    case "failed_attempt":
    case "exception":
      return "bad";
    default:
      return "muted";
  }
}

/** The stage a parcel is in now: its status, else its newest event. */
export function parcelStage(p: Parcel): string {
  return p.status?.stage ?? p.events[0]?.stage ?? "unknown";
}

/** The row title: its label, else the carrier's name and the number. */
export function parcelTitle(p: Parcel): string {
  if (p.label) return p.label;
  const carrier = carrierName(p.carrier);
  return carrier ? `${carrier} ${p.number}` : p.number;
}

/** When the parcel last changed: the status update, the newest event, the last check, else when it was added (ms). */
export function lastChange(p: Parcel): number {
  const candidates = [p.status?.lastUpdate, p.events[0]?.time, p.lastChecked, p.addedAt];
  for (const c of candidates) {
    const ms = parseTs(c);
    if (ms !== null) return ms;
  }
  return 0;
}

/** Active parcels, newest change first; then the delivered and stopped ones, newest change first. */
export function splitParcels(parcels: Parcel[]): { active: Parcel[]; done: Parcel[] } {
  const newest = (a: Parcel, b: Parcel) => lastChange(b) - lastChange(a);
  return {
    active: parcels.filter((p) => p.state === "active").sort(newest),
    done: parcels.filter((p) => p.state !== "active").sort(newest),
  };
}

/** The same list in display order. */
export function sortParcels(parcels: Parcel[]): Parcel[] {
  const { active, done } = splitParcels(parcels);
  return [...active, ...done];
}

/** A date-only string (`2026-09-28`) as a local date; null when it is not one. */
export function dateOnly(value: string | null | undefined): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(value ?? "");
  if (!m) return null;
  return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
}
