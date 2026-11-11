import type { Parcel } from "../api/parcels";

// Fixtures for the Parcels tests only.

export function parcel(over: Partial<Parcel> = {}): Parcel {
  return {
    id: "pc-1a2b3c4d",
    number: "00357059930440074440",
    carrier: "postnord",
    label: "Headphones",
    state: "active",
    schedule: "hourly",
    near: true,
    addedAt: "2026-11-11T15:58:00Z",
    lastChecked: "2026-11-11T16:00:00Z",
    nextCheck: new Date(Date.now() + 3_600_000).toISOString(),
    lastError: null,
    status: {
      stage: "out_for_delivery",
      text: "The delivery of the shipment item is in progress.",
      lastUpdate: "2026-09-28T05:33:46Z",
      expectedDelivery: "2026-09-28",
      location: "AARHUS DEPOT",
      destinationCountry: "DK",
    },
    events: [
      { time: "2026-09-28T05:33:46Z", description: "Out for delivery", location: "AARHUS DEPOT", stage: "out_for_delivery" },
      { time: "2026-09-27T20:10:00Z", description: "Arrived at the terminal", location: "TAULOV", stage: "in_transit" },
      { time: "2026-09-26T09:00:00Z", description: "The item has been registered", location: null, stage: "registered" },
    ],
    ...over,
  };
}

export const delivered = (): Parcel =>
  parcel({
    id: "pc-99999999",
    number: "JD014600003SE",
    carrier: "dhl-express",
    label: null,
    state: "delivered",
    schedule: "stopped",
    near: false,
    nextCheck: null,
    status: { stage: "delivered", text: "Delivered", lastUpdate: "2026-09-20T10:00:00Z", expectedDelivery: null, location: "ODENSE", destinationCountry: "DK" },
    events: [{ time: "2026-09-20T10:00:00Z", description: "Delivered", location: "ODENSE", stage: "delivered" }],
  });

export const daily = (): Parcel =>
  parcel({
    id: "pc-22222222",
    number: "RR123456789DK",
    carrier: null,
    label: null,
    schedule: "daily",
    near: false,
    status: null,
    events: [],
    lastChecked: null,
    lastError: "the carrier has no record of it yet",
    nextCheck: new Date(Date.now() + 5 * 3_600_000).toISOString(),
  });
