// The owner's queue of secret requests, as the chat's cards see it: names, status and fingerprint,
// never a value (crates/lucina-server `secret_requests`). One card is active at a time; the server
// posts the next one as a new timeline item when this one is saved or dismissed.

import { createSignal } from "solid-js";
import { secretsApi, type SecretQueue } from "../api/secrets";

const [queue, setQueue] = createSignal<SecretQueue | null>(null);
const [failed, setFailed] = createSignal(false);
let inFlight: Promise<void> | null = null;

export { queue as secretQueue, failed as secretQueueFailed };

/** Ask for the queue; calls made while one is running share it. */
export function refreshSecretQueue(): Promise<void> {
  if (inFlight) return inFlight;
  const run: Promise<void> = secretsApi
    .requests()
    .then(
      (q) => {
        setQueue(q);
        setFailed(false);
      },
      () => {
        setFailed(true);
      },
    )
    .finally(() => {
      inFlight = null;
    });
  inFlight = run;
  return run;
}

export function resetSecretQueue(): void {
  setQueue(null);
  setFailed(false);
}
