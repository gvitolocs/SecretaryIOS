// Icons for smart search (stroke icons on a 24 px grid; they follow `currentColor`).

import type { JSX } from "solid-js";

function Svg(props: { children: JSX.Element }) {
  return (
    <svg class="gv-icon" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      {props.children}
    </svg>
  );
}

/** Two overlapping frames: pictures like this one. */
export const SimilarIcon = () => (
  <Svg>
    <rect x="3.5" y="7" width="12" height="12" rx="2" />
    <path d="M8.5 4.5h10a2 2 0 0 1 2 2v10" />
    <path d="m4.5 16.5 3.2-3.2 2.6 2.6 1.6-1.6 3.6 3.6" />
  </Svg>
);
