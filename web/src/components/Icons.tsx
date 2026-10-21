// Small inline SVG icons (inline SVG elements are fine under CSP; only
// inline <script>/<style> and style attributes in markup are not).

export function ChatIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H10l-4.2 3.5c-.5.4-1.3.1-1.3-.6V16A2.5 2.5 0 0 1 4 13.5z" />
    </svg>
  );
}

export function ListIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 6h11M9 12h11M9 18h11" />
      <path d="m3.5 6 1 1 2-2M3.5 12l1 1 2-2M3.5 18l1 1 2-2" />
    </svg>
  );
}

export function DumbbellIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3.5 10v4M6 8v8M18 8v8M20.5 10v4M6 12h12" />
    </svg>
  );
}

export function ScaleIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 20h16M6.5 20V9.5a5.5 5.5 0 0 1 11 0V20" />
      <path d="M12 9.5v4M12 9.5l-2 2" />
    </svg>
  );
}

export function UserIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="8.5" r="3.5" />
      <path d="M5 19.5c1.2-3.2 3.8-4.8 7-4.8s5.8 1.6 7 4.8" />
    </svg>
  );
}

export function LogoutIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M10 4H6.5A2.5 2.5 0 0 0 4 6.5v11A2.5 2.5 0 0 0 6.5 20H10" />
      <path d="M15 8l4 4-4 4M19 12H9" />
    </svg>
  );
}

export function RefreshIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M20 11a8 8 0 0 0-14.6-4.5L4 8" />
      <path d="M4 4v4h4" />
      <path d="M4 13a8 8 0 0 0 14.6 4.5L20 16" />
      <path d="M20 20v-4h-4" />
    </svg>
  );
}

export function SendIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M5 12h13M12 5l7 7-7 7" />
    </svg>
  );
}

export function PlusIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

// Source badges for the timeline. Neutral shapes, not brand marks.

export function GlobeIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12h17M12 3.5c2.4 2.3 3.6 5.1 3.6 8.5s-1.2 6.2-3.6 8.5c-2.4-2.3-3.6-5.1-3.6-8.5s1.2-6.2 3.6-8.5z" />
    </svg>
  );
}

export function PhoneIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="7" y="3" width="10" height="18" rx="2.5" />
      <path d="M11 17.5h2" />
    </svg>
  );
}

export function BubbleIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 4c4.7 0 8.5 3.1 8.5 7s-3.8 7-8.5 7c-1 0-2-.1-2.9-.4L5 20l1.1-3.6C4.5 15.1 3.5 13.1 3.5 11c0-3.9 3.8-7 8.5-7z" />
    </svg>
  );
}

export function GamepadIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M7.5 7h9a4.5 4.5 0 0 1 4.4 5.4l-.8 3.9a2.2 2.2 0 0 1-3.8 1L14.6 15H9.4l-1.7 2.3a2.2 2.2 0 0 1-3.8-1l-.8-3.9A4.5 4.5 0 0 1 7.5 7z" />
      <path d="M8 9.5v3M6.5 11h3M15.5 10.5h.01M17 12h.01" />
    </svg>
  );
}

export function SpeakerIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="6.5" y="3" width="11" height="18" rx="5.5" />
      <circle cx="12" cy="14" r="2.5" />
      <path d="M12 7.5h.01" />
    </svg>
  );
}

export function DotsIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6.5 12h.01M12 12h.01M17.5 12h.01" />
    </svg>
  );
}

export function BellIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15z" />
      <path d="M10 20.5a2.2 2.2 0 0 0 4 0" />
    </svg>
  );
}

export function VolumeIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" />
      <path d="M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11" />
    </svg>
  );
}

export function VolumeOffIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" />
      <path d="m15.5 9.5 5 5M20.5 9.5l-5 5" />
    </svg>
  );
}

export function MicIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="9" y="3.5" width="6" height="11" rx="3" />
      <path d="M6 11.5a6 6 0 0 0 12 0M12 17.5v3" />
    </svg>
  );
}

export function CollapseIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="m6 14.5 6-6 6 6" />
    </svg>
  );
}

export function AvatarIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3.5" y="4" width="17" height="16" rx="3" />
      <circle cx="12" cy="10.5" r="2.8" />
      <path d="M7.5 20c.8-2.6 2.4-3.9 4.5-3.9s3.7 1.3 4.5 3.9" />
    </svg>
  );
}
