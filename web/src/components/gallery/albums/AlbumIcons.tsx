// Icons for the albums page and an album's toolbar (stroke icons on a 24 px grid; they follow `currentColor`).

export function ShareIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 15V4M8 7.5 12 3.5l4 4" />
      <path d="M6 11.5H5.5A1.5 1.5 0 0 0 4 13v6a1.5 1.5 0 0 0 1.5 1.5h13A1.5 1.5 0 0 0 20 19v-6a1.5 1.5 0 0 0-1.5-1.5H18" />
    </svg>
  );
}

/** An album with a minus: take the chosen photos out of this album (they stay in the Gallery). */
export function RemoveFromAlbumIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3.5" y="6" width="14" height="14" rx="2.2" />
      <path d="M7 3.5h11a2.5 2.5 0 0 1 2.5 2.5v11M7.5 13h6" />
    </svg>
  );
}

export function RenamePenIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4.5 19.5h4l10-10a2.1 2.1 0 0 0-3-3l-10 10z" />
      <path d="m13.5 7.5 3 3" />
    </svg>
  );
}
