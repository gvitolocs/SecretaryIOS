// Inline stroke icons for the photo editor (24 px grid, they follow `currentColor`).

import type { JSX } from "solid-js";

function Svg(props: { children: JSX.Element }) {
  return (
    <svg class="ed-icon" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      {props.children}
    </svg>
  );
}

/** Sliders: the Edit button and the Adjust tab. */
export const EditIcon = () => (
  <Svg>
    <path d="M4 7h9M17 7h3M4 17h3M11 17h9" />
    <circle cx="15" cy="7" r="2" />
    <circle cx="9" cy="17" r="2" />
  </Svg>
);

export const CropIcon = () => (
  <Svg>
    <path d="M7 3v13a1 1 0 0 0 1 1h13M3 7h13a1 1 0 0 1 1 1v13" />
  </Svg>
);

export const FilterIcon = () => (
  <Svg>
    <circle cx="9" cy="9" r="5.5" />
    <circle cx="15" cy="9" r="5.5" />
    <circle cx="12" cy="15" r="5.5" />
  </Svg>
);

export const MarkupIcon = () => (
  <Svg>
    <path d="M4.5 19.5h4l10-10a2.1 2.1 0 0 0-3-3l-10 10z" />
    <path d="m13.5 7.5 3 3" />
  </Svg>
);

export const UndoIcon = () => (
  <Svg>
    <path d="M9 14 4 9l5-5" />
    <path d="M4 9h10a6 6 0 0 1 0 12h-3" />
  </Svg>
);

export const RedoIcon = () => (
  <Svg>
    <path d="m15 14 5-5-5-5" />
    <path d="M20 9H10a6 6 0 0 0 0 12h3" />
  </Svg>
);

export const CompareIcon = () => (
  <Svg>
    <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
    <path d="M12 4.5v15" />
    <path d="M7 10h2M7 14h2" />
  </Svg>
);

export const RotateLeftIcon = () => (
  <Svg>
    <path d="M4.5 12a7.5 7.5 0 1 0 2.4-5.5" />
    <path d="M4.4 4.5v4.2h4.2" />
  </Svg>
);

export const RotateRightIcon = () => (
  <Svg>
    <path d="M19.5 12a7.5 7.5 0 1 1-2.4-5.5" />
    <path d="M19.6 4.5v4.2h-4.2" />
  </Svg>
);

export const ResetIcon = () => (
  <Svg>
    <path d="M4 12a8 8 0 1 0 2.5-5.8" />
    <path d="M4 4.5v4.5h4.5" />
  </Svg>
);

export const AutoIcon = () => (
  <Svg>
    <path d="m12 3 1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z" />
    <path d="m18.5 16 .7 2 2 .7-2 .7-.7 2-.7-2-2-.7 2-.7z" />
  </Svg>
);

export const PenIcon = MarkupIcon;

export const HighlighterIcon = () => (
  <Svg>
    <path d="m9 15 7.5-7.5 3 3L12 18z" />
    <path d="M9 15l-1.5 4.5L12 18" />
    <path d="M4 21h6" />
  </Svg>
);

export const ArrowIcon = () => (
  <Svg>
    <path d="M5 19 19 5" />
    <path d="M9 5h10v10" />
  </Svg>
);

export const TextIcon = () => (
  <Svg>
    <path d="M5 6.5V5h14v1.5M12 5v14M9 19h6" />
  </Svg>
);

export const TrashIcon = () => (
  <Svg>
    <path d="M5 7h14M10 7V4.5h4V7M7 7l.8 12.5h8.4L17 7" />
  </Svg>
);
