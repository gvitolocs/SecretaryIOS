import { A } from "@solidjs/router";
import { Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PrimaryAction, useWide } from "../components/page";
import { GalleryHeader } from "../components/gallery/GalleryHeader";
import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { getFaceSettings, listPeople, patchPerson, setFacesEnabled, type FaceSettings, type Person } from "../api/gallery";
import { resetFacesAvailable } from "../components/gallery/faces/facesAvailable";
import { MergeIcon } from "../components/gallery/faces/FaceIcons";
import { DeleteFaceDataDialog, MergeDialog } from "../components/gallery/people/PeopleDialogs";
import { PeopleMenu } from "../components/gallery/people/PeopleMenu";
import { FacesOffCard } from "../components/gallery/people/FacesOffCard";
import { PersonTile } from "../components/gallery/people/PersonTile";
import { ScanStatus } from "../components/gallery/people/ScanStatus";
import { UnassignedFaces } from "../components/gallery/people/UnassignedFaces";
import { BackIcon } from "../components/Icons";
import { t } from "../i18n";
import { GroupKindProvider, groupText, useGroupKind } from "../lib/galleryGroup";
import type { GroupKind } from "../api/gallery";
import { canMerge, replacePerson, stillScanning, withoutPeople } from "../lib/galleryPeople";
import "../styles/gallery.css";
import "../styles/gallery-people.css";

/** How often the page asks how the scan is going while photos are waiting. */
export const SCAN_POLL_MS = 5000;

/**
 * /gallery/people: face recognition's own page. While it is off, a card says what it does and lets the owner turn it on.
 * When it is on: the people as round faces with names (a click renames), counts, the scan's progress, hide / show hidden,
 * merging, and the faces still without a person. /gallery/pets is the same page over cats and dogs (`kind` "pets").
 */
export default function GalleryPeopleView() {
  return <GalleryGroupPage kind="people" />;
}

export function GalleryGroupPage(props: { kind: GroupKind }) {
  return (
    <GroupKindProvider value={props.kind}>
      <GroupPage />
    </GroupKindProvider>
  );
}

function GroupPage() {
  const kind = useGroupKind();
  const pt = groupText(kind);
  // The viewer's face boxes only care about faces.
  const facesChanged = () => {
    if (kind === "people") resetFacesAvailable();
  };
  const { wide, track } = useWide();
  const [settings, { mutate: setSettings, refetch: refetchSettings }] = createResource(() => getFaceSettings(kind));
  const [showHidden, setShowHidden] = createSignal(false);
  const current = (): FaceSettings | undefined => (settings.state === "errored" ? undefined : settings.latest);
  const enabled = () => current()?.enabled === true;
  const [people, { mutate: setPeople, refetch: refetchPeople }] = createResource(
    () => (enabled() ? { hidden: showHidden() } : false),
    (key) => listPeople({ includeHidden: key.hidden }, kind),
  );
  const list = createMemo((): Person[] => (people.state === "errored" ? [] : (people.latest ?? [])));

  const [selecting, setSelecting] = createSignal(false);
  const [selected, setSelected] = createSignal<ReadonlySet<string>>(new Set());
  const [merging, setMerging] = createSignal(false);
  const [deleting, setDeleting] = createSignal(false);
  const [status, setStatus] = createSignal<{ text: string; error: boolean } | null>(null);
  let statusTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(statusTimer));
  function say(text: string, error = false) {
    clearTimeout(statusTimer);
    setStatus({ text, error });
    statusTimer = setTimeout(() => setStatus(null), error ? 8000 : 4000);
  }

  // While photos are waiting, ask how the scan is going; new people appear as the scan finds them.
  createEffect(() => {
    const s = current();
    if (!s || !stillScanning(s)) return;
    const timer = setTimeout(async () => {
      try {
        const next = await getFaceSettings(kind);
        if (next.people !== s.people || next.faces !== s.faces) void refetchPeople();
        setSettings(next);
      } catch {
        // Try again at the next tick.
        setSettings({ ...s });
      }
    }, SCAN_POLL_MS);
    onCleanup(() => clearTimeout(timer));
  });

  const selectedPeople = createMemo(() => list().filter((p) => selected().has(p.id)));
  const toggle = (id: string) =>
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  const stopSelecting = () => {
    setSelecting(false);
    setSelected(new Set<string>());
  };

  async function turnOn() {
    const next = await setFacesEnabled(true, kind);
    facesChanged();
    setSettings(next);
  }

  async function turnOff() {
    try {
      const next = await setFacesEnabled(false, kind);
      facesChanged();
      stopSelecting();
      setSettings(next);
      say(pt("turnedOff"));
    } catch {
      say(pt("actionFailed"), true);
    }
  }

  function dataDeleted() {
    facesChanged();
    setDeleting(false);
    stopSelecting();
    setPeople([]);
    void refetchSettings();
    say(pt("deleted"));
  }

  async function renameSaved(person: Person, name: string | null): Promise<void> {
    const saved = await patchPerson(person.id, { name }, kind);
    setPeople((now) => replacePerson(now ?? [], saved));
  }

  async function hide(person: Person) {
    try {
      const saved = await patchPerson(person.id, { hidden: !person.hidden }, kind);
      // Hidden people leave the list unless "Show hidden" is on.
      setPeople((now) => (saved.hidden && !showHidden() ? withoutPeople(now ?? [], [saved.id]) : replacePerson(now ?? [], saved)));
    } catch {
      say(pt("actionFailed"), true);
    }
  }

  function merged(keep: Person, from: string[]) {
    setPeople((now) => replacePerson(withoutPeople(now ?? [], from), keep));
    setMerging(false);
    stopSelecting();
    say(pt("merged", { count: from.length + 1 }));
    void refetchSettings();
  }

  const showHiddenChip = () => (
    <button type="button" class="g-chip" aria-pressed={showHidden()} onClick={() => setShowHidden((v) => !v)}>
      {pt("showHidden")}
    </button>
  );
  const selectChip = () => (
    <button type="button" class="g-chip" onClick={() => setSelecting(true)}>
      {pt("select")}
    </button>
  );
  const doneChip = () => (
    <button type="button" class="g-chip" onClick={stopSelecting}>
      {pt("doneSelecting")}
    </button>
  );
  const selCount = () => (
    <span class="pp-selcount" role="status">
      {pt("selected", { count: selected().size })}
    </span>
  );
  const retryBox = (retry: () => void) => (
    <Show
      when={wide()}
      fallback={
        <p class="pp-empty" role="alert">
          {pt("loadError")}{" "}
          <button type="button" class="btn" onClick={retry}>
            {pt("retry")}
          </button>
        </p>
      }
    >
      <ErrorState message={pt("loadError")} retryLabel={pt("retry")} onRetry={retry} />
    </Show>
  );
  const loadingBox = () => (
    <Show when={wide()} fallback={<p class="pp-empty" role="status">{t("gallery.loading")}</p>}>
      <LoadingState label={t("gallery.loading")} />
    </Show>
  );

  const body = () => (
    <Show when={settings.state !== "pending" && settings.state !== "unresolved"} fallback={loadingBox()}>
      <Show when={settings.state !== "errored"} fallback={retryBox(() => void refetchSettings())}>
        <Show
          when={enabled()}
          fallback={<FacesOffCard wide={wide()} hasData={(current()?.faces ?? 0) > 0} onTurnOn={turnOn} onDelete={() => setDeleting(true)} />}
        >
          <Show when={current()}>{(s) => <ScanStatus settings={s()} />}</Show>
          <Show when={!people.loading || list().length > 0} fallback={loadingBox()}>
            <Show when={people.state !== "errored"} fallback={retryBox(() => void refetchPeople())}>
              <Show
                when={list().length > 0}
                fallback={
                  <Show when={wide()} fallback={<p class="pp-empty">{pt("empty")}</p>}>
                    <EmptyState text={pt("empty")} />
                  </Show>
                }
              >
                <ul class="pp-grid" aria-label={pt("title")}>
                  <For each={list()}>
                    {(person) => (
                      <li>
                        <PersonTile
                          person={person}
                          selecting={selecting()}
                          selected={selected().has(person.id)}
                          onToggle={() => toggle(person.id)}
                          onRename={(name) => renameSaved(person, name)}
                          onHide={() => void hide(person)}
                        />
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </Show>
          </Show>
        </Show>
      </Show>
    </Show>
  );
  const unassigned = () => (
    <Show when={settings.state === "ready" || settings.state === "refreshing"}>
      <Show when={enabled()}>
        <UnassignedFaces wide={wide()} refreshKey={(current()?.people ?? 0) * 100000 + (current()?.faces ?? 0)} onAssigned={() => { void refetchPeople(); void refetchSettings(); }} />
      </Show>
    </Show>
  );

  return (
    <div class={wide() ? "g-page-root" : "pp-page"} ref={track}>
      <Show
        when={wide()}
        fallback={
          <>
            <header class="pp-head">
              <A class="g-back" href="/gallery" aria-label={t("gallery.backToGallery")}>
                <BackIcon />
              </A>
              <h1 class="pp-h1">{pt("title")}</h1>
              <Show when={enabled()}>
                <Show
                  when={selecting()}
                  fallback={
                    <>
                      {showHiddenChip()}
                      {selectChip()}
                      <PeopleMenu onTurnOff={() => void turnOff()} onDelete={() => setDeleting(true)} />
                    </>
                  }
                >
                  {selCount()}
                  <button type="button" class="btn btn--primary" disabled={!canMerge(selected())} onClick={() => setMerging(true)}>
                    <MergeIcon /> {pt("merge")}
                  </button>
                  {doneChip()}
                </Show>
              </Show>
            </header>
            {body()}
            {unassigned()}
          </>
        }
      >
        <Page class="g-lib">
          <GalleryHeader
            count={enabled() && !people.loading && people.state !== "errored" ? list().length : undefined}
            action={
              <Show when={enabled() && selecting()}>
                <PrimaryAction icon={<MergeIcon />} disabled={!canMerge(selected())} onClick={() => setMerging(true)}>
                  {pt("merge")}
                </PrimaryAction>
              </Show>
            }
          />
          <PageGrid layout="full">
            <Show when={enabled()}>
              <div class="g-desktop-tools">
                <Show
                  when={selecting()}
                  fallback={
                    <>
                      {showHiddenChip()}
                      {selectChip()}
                      <PeopleMenu wide onTurnOff={() => void turnOff()} onDelete={() => setDeleting(true)} />
                    </>
                  }
                >
                  {selCount()}
                  {doneChip()}
                </Show>
              </div>
            </Show>
            <Show when={enabled()} fallback={body()}>
              <Card label={pt("title")}>{body()}</Card>
              {unassigned()}
            </Show>
          </PageGrid>
        </Page>
      </Show>

      <Show when={status()}>
        {(s) => (
          <div class="pp-status" classList={{ "pp-status--error": s().error }} role={s().error ? "alert" : "status"}>
            <span>{s().text}</span>
            <button type="button" onClick={() => setStatus(null)} aria-label={t("gallery.dismiss")}>
              ×
            </button>
          </div>
        )}
      </Show>
      <Show when={merging()}>
        <MergeDialog people={selectedPeople()} onMerged={merged} onCancel={() => setMerging(false)} />
      </Show>
      <Show when={deleting()}>
        <DeleteFaceDataDialog onDeleted={dataDeleted} onCancel={() => setDeleting(false)} />
      </Show>
    </div>
  );
}
