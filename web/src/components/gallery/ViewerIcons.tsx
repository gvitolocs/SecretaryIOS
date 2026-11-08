// Inline icons for the Gallery viewer's toolbar and panel (stroke icons, 24 px grid; they follow `currentColor`).

import type { JSX } from "solid-js";

function Svg(props: { children: JSX.Element; filled?: boolean }) {
  return (
    <svg class="gv-icon" viewBox="0 0 24 24" aria-hidden="true" fill={props.filled ? "currentColor" : "none"} stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      {props.children}
    </svg>
  );
}

export const InfoIcon = () => (
  <Svg>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 11v5.5M12 7.8v.1" />
  </Svg>
);

export const HeartIcon = (props: { filled?: boolean }) => (
  <Svg filled={props.filled}>
    <path d="M12 20s-7.5-4.6-7.5-10.1A4.4 4.4 0 0 1 12 7.2a4.4 4.4 0 0 1 7.5 2.7C19.5 15.4 12 20 12 20z" />
  </Svg>
);

export const RotateIcon = () => (
  <Svg>
    <path d="M19.5 12a7.5 7.5 0 1 1-2.4-5.5" />
    <path d="M19.6 4.5v4.2h-4.2" />
  </Svg>
);

export const RenameIcon = () => (
  <Svg>
    <path d="M4.5 19.5h4l10-10a2.1 2.1 0 0 0-3-3l-10 10z" />
    <path d="m13.5 7.5 3 3" />
  </Svg>
);

export const AlbumIcon = () => (
  <Svg>
    <rect x="3.5" y="5" width="17" height="14" rx="2.5" />
    <path d="M12 9v6M9 12h6" />
  </Svg>
);

export const DownloadIcon = () => (
  <Svg>
    <path d="M12 4v11M7.5 10.5 12 15l4.5-4.5M5 19.5h14" />
  </Svg>
);

export const TrashIcon = () => (
  <Svg>
    <path d="M4.5 6.5h15M9.5 6.5V4.5h5v2M6.5 6.5l1 13a1.5 1.5 0 0 0 1.5 1.5h6a1.5 1.5 0 0 0 1.5-1.5l1-13" />
    <path d="M10 10.5v6M14 10.5v6" />
  </Svg>
);

export const RestoreIcon = () => (
  <Svg>
    <path d="M4.5 12a7.5 7.5 0 1 0 2.4-5.5" />
    <path d="M4.4 4.5v4.2h4.2" />
  </Svg>
);

export const PinIcon = () => (
  <Svg>
    <path d="M12 21s-6.5-5.6-6.5-11a6.5 6.5 0 0 1 13 0c0 5.4-6.5 11-6.5 11z" />
    <circle cx="12" cy="10" r="2.3" />
  </Svg>
);
