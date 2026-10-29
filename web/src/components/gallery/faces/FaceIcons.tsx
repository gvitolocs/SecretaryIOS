// Icons for people and faces (stroke icons on a 24 px grid; they follow `currentColor`).

/** Two heads: the People page's link in the Gallery header (the page icon style). */
export function PeopleIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="9" cy="8.5" r="3.2" />
      <path d="M3.5 19.5v-1a4.5 4.5 0 0 1 4.5-4.5h2a4.5 4.5 0 0 1 4.5 4.5v1" />
      <path d="M15.5 5.5a3.2 3.2 0 0 1 0 6M17.5 14.4a4.5 4.5 0 0 1 3 4.1v1" />
    </svg>
  );
}

/** A face in a frame's corners: the viewer's "Faces" toggle. */
export function FacesIcon() {
  return (
    <svg class="gv-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M4 8.5V6a2 2 0 0 1 2-2h2.5M15.5 4H18a2 2 0 0 1 2 2v2.5M20 15.5V18a2 2 0 0 1-2 2h-2.5M8.5 20H6a2 2 0 0 1-2-2v-2.5" />
      <circle cx="12" cy="10" r="2.4" />
      <path d="M7.8 17c.5-2.2 2.2-3.4 4.2-3.4s3.7 1.2 4.2 3.4" />
    </svg>
  );
}

export function HideEyeIcon(props: { off?: boolean }) {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z" />
      <circle cx="12" cy="12" r="2.8" />
      {props.off ? <path d="M4 4l16 16" /> : null}
    </svg>
  );
}

export function MergeIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 4v4a4 4 0 0 0 4 4h4a4 4 0 0 1 4 4v4M18 4v4a4 4 0 0 1-1.2 2.8" />
      <path d="m15 17 3 3 3-3" />
    </svg>
  );
}
