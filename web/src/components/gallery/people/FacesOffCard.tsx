import { createSignal, Show } from "solid-js";
import { Button, Card, PrimaryAction } from "../../page";
import "../../../styles/gallery-people.css";
import { groupText, useGroupKind } from "../../../lib/galleryGroup";

/**
 * Shown while face recognition is off: what it does, that it stays on this server, that it is the owner's to switch on,
 * and (when it found something earlier) a way to delete that.
 */
export function FacesOffCard(props: { wide?: boolean; hasData: boolean; onTurnOn: () => Promise<void>; onDelete: () => void }) {
  const kind = useGroupKind();
  const pt = groupText(kind);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  async function turnOn() {
    if (busy()) return;
    setBusy(true);
    setFailed(false);
    try {
      await props.onTurnOn();
    } catch {
      setFailed(true);
      setBusy(false);
    }
  }
  const copy = () => (
    <>
      <p>{pt("offBody")}</p>
      <p class="pp-card__privacy">{pt("offPrivacy")}</p>
      <p class="pp-card__soft">{pt("offControl")}</p>
    </>
  );
  const failure = () => (
    <Show when={failed()}>
      <p class="gv-error" role="alert">
        {pt("actionFailed")}
      </p>
    </Show>
  );
  const turnOnLabel = () => (busy() ? pt("turningOn") : pt("turnOn"));
  return (
    <Show
      when={props.wide}
      fallback={
        <section class="pp-card" aria-labelledby="pp-off-title">
          <h2 id="pp-off-title" class="pp-card__title">
            {pt("offTitle")}
          </h2>
          {copy()}
          <div class="pp-card__actions">
            <button type="button" class="btn btn--primary" disabled={busy()} onClick={() => void turnOn()}>
              {turnOnLabel()}
            </button>
            <Show when={props.hasData}>
              <button type="button" class="btn" disabled={busy()} onClick={props.onDelete}>
                {pt("deleteData")}
              </button>
            </Show>
          </div>
          {failure()}
        </section>
      }
    >
      <Card title={pt("offTitle")} class="pp-off-card">
        {copy()}
        <div class="pp-card__actions">
          <PrimaryAction disabled={busy()} onClick={() => void turnOn()}>
            {turnOnLabel()}
          </PrimaryAction>
          <Show when={props.hasData}>
            <Button disabled={busy()} onClick={props.onDelete}>
              {pt("deleteData")}
            </Button>
          </Show>
        </div>
        {failure()}
      </Card>
    </Show>
  );
}
