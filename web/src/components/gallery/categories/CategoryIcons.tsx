import type { JSX } from "solid-js";
import type { GalleryCategory } from "../../../api/gallery";

// One stroke icon per automatic album (24 px grid, `currentColor`), for the Things section and the info panel.

const PATHS: Record<GalleryCategory, () => JSX.Element> = {
  screenshot: () => (
    <>
      <rect x="7" y="3" width="10" height="18" rx="2.2" />
      <path d="M10.5 18h3M4 8V6.5A2.5 2.5 0 0 1 6.5 4M20 8V6.5A2.5 2.5 0 0 0 17.5 4" />
    </>
  ),
  receipt: () => (
    <>
      <path d="M6 3.5h12v17l-2-1.4-2 1.4-2-1.4-2 1.4-2-1.4-2 1.4z" />
      <path d="M9 8h6M9 11.5h6M9 15h3.5" />
    </>
  ),
  document: () => (
    <>
      <path d="M7 3.5h7l4 4v13H7z" />
      <path d="M14 3.5v4h4M9.5 12h5M9.5 15.5h5" />
    </>
  ),
  note: () => (
    <>
      <path d="M5 5.5A1.5 1.5 0 0 1 6.5 4h11A1.5 1.5 0 0 1 19 5.5v9L14 20H6.5A1.5 1.5 0 0 1 5 18.5z" />
      <path d="M14 20v-4a1 1 0 0 1 1-1h4M8.5 9h7M8.5 12.5h4" />
    </>
  ),
  ticket: () => (
    <>
      <path d="M4 8.5A1.5 1.5 0 0 1 5.5 7h13A1.5 1.5 0 0 1 20 8.5V10a2 2 0 0 0 0 4v1.5a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 15.5V14a2 2 0 0 0 0-4z" />
      <path d="M14.5 7.5v9" stroke-dasharray="1.6 2" />
    </>
  ),
  map: () => (
    <>
      <path d="m4 6.5 5-2 6 2 5-2v13l-5 2-6-2-5 2z" />
      <path d="M9 4.5v13M15 6.5v13" />
    </>
  ),
  meme: () => (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M8.5 14.5c1 1.4 2.2 2 3.5 2s2.5-.6 3.5-2M9 9.5h.01M15 9.5h.01" />
    </>
  ),
  food: () => (
    <>
      <path d="M7 3.5v6a2 2 0 0 0 2 2v9M9 3.5v6M5 3.5v6a2 2 0 0 0 2 2" />
      <path d="M16.5 20.5v-17c-2 1.2-3 3.7-3 6.5 0 1.5 1 2.5 3 2.5" />
    </>
  ),
  pet: () => (
    <>
      <circle cx="8" cy="9" r="1.7" />
      <circle cx="16" cy="9" r="1.7" />
      <circle cx="5.5" cy="13.5" r="1.5" />
      <circle cx="18.5" cy="13.5" r="1.5" />
      <path d="M12 12.5c-2.5 0-4.5 3-4.5 5 0 1.5 1.3 2 2.5 2 .8 0 1.3-.3 2-.3s1.2.3 2 .3c1.2 0 2.5-.5 2.5-2 0-2-2-5-4.5-5z" />
    </>
  ),
  photo: () => (
    <>
      <rect x="4" y="5" width="16" height="14" rx="2.2" />
      <circle cx="9" cy="10" r="1.5" />
      <path d="m5 17 4.5-4 3 2.5 3-3.500L19 17" />
    </>
  ),
};

export function CategoryIcon(props: { category: GalleryCategory }) {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      {PATHS[props.category]()}
    </svg>
  );
}
