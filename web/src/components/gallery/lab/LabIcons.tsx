// Inline icons for the AI Lab (stroke icons on a 24 px grid, `currentColor`).

import type { JSX } from "solid-js";

function Svg(props: { children: JSX.Element }) {
  return (
    <svg class="gv-icon" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      {props.children}
    </svg>
  );
}

/** A magic wand with sparkles: "Edit with Lucina…" and the Lab link. */
export const LabIcon = () => (
  <Svg>
    <path d="m5 19 9.5-9.5" />
    <path d="m13 6.5 4.5 4.5" />
    <path d="M15.5 4.2v2.2M14.4 5.3h2.2" />
    <path d="M19.2 11.8v2M18.2 12.8h2" />
    <path d="M7.5 5.2v1.8M6.6 6.1h1.8" />
  </Svg>
);

export const CheckIcon = () => (
  <Svg>
    <path d="m5 12.5 4.5 4.5L19 7.5" />
  </Svg>
);
