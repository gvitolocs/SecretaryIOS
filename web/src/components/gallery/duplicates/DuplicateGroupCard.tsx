import { For, Show } from "solid-js";
import { tileUrl, type DuplicateGroup, type DuplicateMember } from "../../../api/gallery";
import { locale, t, type MessageKey } from "../../../i18n";
import { formatBytes } from "../../../lib/drive";
import { itemName, scorePercent } from "../../../lib/galleryDedup";
import { dimensionsText } from "../../../lib/galleryCompare";
import { CheckIcon } from "../stacks/StackIcons";

export interface DuplicateGroupCardProps {
  group: DuplicateGroup;
  jxl: boolean;
  /** A request is running: the buttons wait. */
  busy: boolean;
  onKeep: (group: DuplicateGroup, keepId: string) => void;
  onDismiss: (group: DuplicateGroup) => void;
  /** Open the group's photos in the viewer, at the photo `index`. */
  onOpen: (group: DuplicateGroup, index: number) => void;
}

function takenText(member: DuplicateMember): string {
  const at = member.item.takenAt;
  if (!at) return "";
  const date = new Date(at);
  return Number.isNaN(date.getTime()) ? "" : date.toLocaleDateString(locale(), { year: "numeric", month: "short", day: "numeric" });
}

/**
 * One group of the same photo found twice or more: the copies side by side, the suggested one marked with the reason,
 * "Keep this" under each (it hides the others under it, undoably) and "Not duplicates" for the whole group.
 */
export function DuplicateGroupCard(props: DuplicateGroupCardProps) {
  const reasonLabel = () => t(`galleryDedup.reason_${props.group.reason}` as MessageKey);
  return (
    <article class="dd-group" aria-label={t("galleryDedup.groupLabel", { count: props.group.members.length })}>
      <header class="dd-group__head">
        <span class="dd-class">{t(`galleryDedup.class_${props.group.class}` as MessageKey)}</span>
        <span class="dd-score" classList={{ "dd-score--weak": !props.group.applicable }}>
          {t("galleryDedup.sure", { percent: scorePercent(props.group.score) })}
        </span>
        <Show when={!props.group.applicable}>
          <span class="dd-weak">{t("galleryDedup.weak")}</span>
        </Show>
      </header>
      <ul class="dd-members">
        <For each={props.group.members}>
          {(member, index) => {
            const name = () => itemName(member.item);
            const suggested = () => member.item.id === props.group.keep;
            return (
              <li class="dd-member" classList={{ "dd-member--suggested": suggested() }}>
                <button type="button" class="dd-pic" aria-label={t("galleryDedup.open", { name: name() })} onClick={() => props.onOpen(props.group, index())}>
                  <Show when={tileUrl(member.item, props.jxl)}>
                    {(src) => <img src={src()} alt="" draggable={false} loading="lazy" style={member.item.rotation ? { transform: `rotate(${member.item.rotation}deg)` } : undefined} />}
                  </Show>
                  <Show when={suggested()}>
                    <span class="dd-tag">
                      <CheckIcon class="dd-tag__icon" />
                      {t("galleryDedup.suggested")}
                    </span>
                  </Show>
                </button>
                <div class="dd-info">
                  <p class="dd-name" title={name()}>
                    {name()}
                  </p>
                  <Show when={suggested()}>
                    <p class="dd-reason">{reasonLabel()}</p>
                  </Show>
                  <p class="dd-meta">
                    {[dimensionsText(member.item), formatBytes(member.item.size, locale())].filter(Boolean).join(" · ")}
                  </p>
                  <p class="dd-meta">{[member.item.deviceName, takenText(member)].filter(Boolean).join(" · ")}</p>
                  <p class="dd-meta">{t("galleryDedup.details", { count: member.metadata })}</p>
                </div>
                <button
                  type="button"
                  class="btn"
                  classList={{ "btn--primary": suggested() }}
                  disabled={props.busy}
                  aria-label={t("galleryDedup.keepAria", { name: name() })}
                  onClick={() => props.onKeep(props.group, member.item.id)}
                >
                  {t("galleryDedup.keepThis")}
                </button>
              </li>
            );
          }}
        </For>
      </ul>
      <footer class="dd-group__foot">
        <button type="button" class="btn" disabled={props.busy} onClick={() => props.onDismiss(props.group)}>
          {t("galleryDedup.notDuplicates")}
        </button>
      </footer>
    </article>
  );
}
