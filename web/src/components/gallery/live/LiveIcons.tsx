// The Live Photo mark: concentric circles, the dashed outer ring and the dot of the system's own badge.

import "../../../styles/gallery-live.css";

export function LiveIcon(props: { class?: string }) {
  return (
    <svg class={props.class ?? "gl-icon"} viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
      <circle cx="12" cy="12" r="9.2" stroke-dasharray="1.6 3.1" />
      <circle cx="12" cy="12" r="5.6" />
      <circle cx="12" cy="12" r="1.6" fill="currentColor" stroke="none" />
    </svg>
  );
}
