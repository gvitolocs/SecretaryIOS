import "../../../styles/gallery-dedup.css";

// Icons for stacks and the duplicate review (stroke icons on a 24 px grid; they follow `currentColor`).

import type { JSX } from "solid-js";

function Svg(props: { children: JSX.Element; class?: string }) {
  return (
    <svg class={props.class ?? "icon"} viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      {props.children}
    </svg>
  );
}

/** Photos piled on each other: a stack. */
export const StackIcon = (props: { class?: string }) => (
  <Svg class={props.class}>
    <rect x="7" y="8" width="13" height="12" rx="2.2" />
    <path d="M4 15.5V6.2A2.2 2.2 0 0 1 6.2 4H15" />
  </Svg>
);

/** Two overlapping frames: the duplicate review. */
export const DuplicatesIcon = (props: { class?: string }) => (
  <Svg class={props.class}>
    <rect x="3.5" y="3.5" width="11" height="11" rx="2" />
    <rect x="9.5" y="9.5" width="11" height="11" rx="2" />
  </Svg>
);

export const CheckIcon = (props: { class?: string }) => (
  <Svg class={props.class}>
    <path d="m5 12.5 4.5 4.5L19 7.5" />
  </Svg>
);
