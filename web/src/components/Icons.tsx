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

export function NoteIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6.5 3h8l4 4v11.5A2.5 2.5 0 0 1 16 21H6.5A2.5 2.5 0 0 1 4 18.5v-13A2.5 2.5 0 0 1 6.5 3z" />
      <path d="M14.5 3v4h4M8 11.5h8M8 15h8M8 18h5" />
    </svg>
  );
}

export function SearchIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="10.5" cy="10.5" r="6" />
      <path d="m15 15 5 5" />
    </svg>
  );
}

export function TrashIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4.5 6.5h15M9.5 6.5V4.5h5v2M6.5 6.5l1 13a1.5 1.5 0 0 0 1.5 1.5h6a1.5 1.5 0 0 0 1.5-1.5l1-13" />
      <path d="M10 10.5v6M14 10.5v6" />
    </svg>
  );
}

export function BackIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M15 5l-7 7 7 7" />
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

export function CloseIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6.5 6.5l11 11M17.5 6.5l-11 11" />
    </svg>
  );
}

/** The wardrobe: a coat hanger. */
export function HangerIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M10 6.5a2 2 0 1 1 2.8 1.84c-.5.22-.8.7-.8 1.24V10.5" />
      <path d="M12 10.5 3.6 16.2c-.9.6-.5 1.8.5 1.8h15.8c1 0 1.4-1.2.5-1.8z" />
    </svg>
  );
}

export function PinIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 4h6l-1 5 3 3v1.5H7V12l3-3z" />
      <path d="M12 13.5V20" />
    </svg>
  );
}

export function CheckIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="m5 12.5 4.5 4.5L19 7.5" />
    </svg>
  );
}

/** Wardrobe categories. */
export function GridIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4" y="4" width="6.5" height="6.5" rx="1.6" />
      <rect x="13.5" y="4" width="6.5" height="6.5" rx="1.6" />
      <rect x="4" y="13.5" width="6.5" height="6.5" rx="1.6" />
      <rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.6" />
    </svg>
  );
}

export function DressIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9.5 3.5 9 7l-2 3 1.5 1.5L5 20.5h14l-3.5-9L17 10l-2-3-.5-3.5" />
      <path d="M9 7c1.8 1.2 4.2 1.2 6 0" />
    </svg>
  );
}

export function ShirtIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 4 4 6.5l1.8 4L8 9.6V20h8V9.6l2.2.9 1.8-4L15 4c-.6 1.4-1.7 2.1-3 2.1S9.6 5.4 9 4z" />
    </svg>
  );
}

export function ChefHatIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M7 14.5c-2.2-.4-3.5-2-3.5-3.9A3.9 3.9 0 0 1 8 6.8a4.4 4.4 0 0 1 8 0 3.9 3.9 0 0 1 4.5 3.8c0 1.9-1.3 3.5-3.5 3.9V19H7z" />
      <path d="M7 16.5h10" />
    </svg>
  );
}

export function BowIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M10.5 12 4 8v8zM13.5 12 20 8v8z" />
      <rect x="10.5" y="10.3" width="3" height="3.4" rx="1" />
    </svg>
  );
}

export function SparkleIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 3.5c.6 4.2 2.3 5.9 6.5 6.5-4.2.6-5.9 2.3-6.5 6.5-.6-4.2-2.3-5.9-6.5-6.5 4.2-.6 5.9-2.3 6.5-6.5z" />
      <path d="M18.5 15.5c.3 1.6.9 2.2 2.5 2.5-1.6.3-2.2.9-2.5 2.5-.3-1.6-.9-2.2-2.5-2.5 1.6-.3 2.2-.9 2.5-2.5z" />
    </svg>
  );
}

/** Hairstyles: a head with a flowing ponytail. */
export function HairIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M8 13.5c-1.3-.7-2-2.2-2-4A6 6 0 0 1 12 3.5a6 6 0 0 1 6 6c0 1.6-.6 3-1.7 3.8" />
      <path d="M8.5 9.5c1.6.2 4.2-.6 5.6-2.4.6 1.2 1.6 2 2.9 2.4" />
      <path d="M9 13.5c.6 1.6 1.7 2.5 3 2.5s2.4-.9 3-2.5" />
      <path d="M17.5 11.5c1.8 1.4 2.4 4 1.6 6.3-.6 1.7-2 2.7-3.6 2.7" />
    </svg>
  );
}

export function SparkIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 3.5c.6 3.9 2.6 5.9 6.5 6.5-3.9.6-5.9 2.6-6.5 6.5-.6-3.9-2.6-5.9-6.5-6.5 3.9-.6 5.9-2.6 6.5-6.5z" />
      <path d="M18.5 15.5c.2 1.5 1 2.3 2.5 2.5-1.5.2-2.3 1-2.5 2.5-.2-1.5-1-2.3-2.5-2.5 1.5-.2 2.3-1 2.5-2.5z" />
    </svg>
  );
}

/** Hair colour: a drop. */
export function DropIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 3.5c3 3.6 5.5 6.9 5.5 10a5.5 5.5 0 0 1-11 0c0-3.1 2.5-6.4 5.5-10z" />
      <path d="M9.3 14.2a2.8 2.8 0 0 0 2.4 2.6" />
    </svg>
  );
}

export function ClockIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </svg>
  );
}

export function CalendarIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3.5" y="5" width="17" height="15" rx="2.5" />
      <path d="M3.5 10h17M8 3v4M16 3v4" />
    </svg>
  );
}

export function ShieldCheckIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 3.5 5 6v5.5c0 4.2 2.9 7.6 7 9 4.1-1.4 7-4.8 7-9V6z" />
      <path d="m9 12 2 2 4-4" />
    </svg>
  );
}

export function PulseIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3 12h4l2-5 4 10 2-5h6" />
    </svg>
  );
}

export function GearIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="3" />
      <path d="M12 3v2.5M12 18.5V21M3 12h2.5M18.5 12H21M5.6 5.6l1.8 1.8M16.6 16.6l1.8 1.8M5.6 18.4l1.8-1.8M16.6 7.4l1.8-1.8" />
    </svg>
  );
}

// Links tab.

export function LinkIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1.2 1.2" />
      <path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1.2-1.2" />
    </svg>
  );
}


export function MailIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3.5" y="5.5" width="17" height="13" rx="2.5" />
      <path d="m4.5 7 7.5 6 7.5-6" />
    </svg>
  );
}

export function DriveIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 4h6l6 10.5-3 5.5H6l-3-5.5z" />
      <path d="M9 4l6 10.5H3M15 4 9 14.5l-3 5.5" />
    </svg>
  );
}


export function PlugIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 3.5v4M15 3.5v4M7 7.5h10v3a5 5 0 0 1-10 0z" />
      <path d="M12 15.5v5" />
    </svg>
  );
}

// ---- Tasks ----

export function CheckCircleIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" />
      <path d="m8.5 12.2 2.4 2.4 4.6-5" />
    </svg>
  );
}

export function InboxIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 13.5 6.3 5.6A1.5 1.5 0 0 1 7.7 4.5h8.6a1.5 1.5 0 0 1 1.4 1.1L20 13.5v4A2.5 2.5 0 0 1 17.5 20h-11A2.5 2.5 0 0 1 4 17.5z" />
      <path d="M4 13.5h4.5l1.2 2h4.6l1.2-2H20" />
    </svg>
  );
}

export function TodayIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4" y="5" width="16" height="15" rx="2.5" />
      <path d="M4 9.5h16M8.5 3v4M15.5 3v4" />
      <path d="M10 13.2h2.2v4.3" />
    </svg>
  );
}

export function UpcomingIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4" y="5" width="16" height="15" rx="2.5" />
      <path d="M4 9.5h16M8.5 3v4M15.5 3v4M8 13h2M12 13h2M16 13h0M8 16.5h2M12 16.5h2" />
    </svg>
  );
}

export function FiltersIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4" y="4" width="7" height="7" rx="1.8" />
      <rect x="13" y="4" width="7" height="7" rx="1.8" />
      <rect x="4" y="13" width="7" height="7" rx="1.8" />
      <path d="M13 16.5h7M16.5 13v7" />
    </svg>
  );
}

export function TagIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3.5 12.6V5a1.5 1.5 0 0 1 1.5-1.5h7.6l8 8a1.5 1.5 0 0 1 0 2.1l-7.1 7.1a1.5 1.5 0 0 1-2.1 0z" />
      <circle cx="8" cy="8" r="1.3" />
    </svg>
  );
}

export function FlagIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M5.5 21V4.5M5.5 4.5h11l-2.2 4 2.2 4h-11" />
    </svg>
  );
}

export function RepeatIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M17 3.5 20 6.5l-3 3M4 11.5V10a3.5 3.5 0 0 1 3.5-3.5H20M7 20.5 4 17.5l3-3M20 12.5V14a3.5 3.5 0 0 1-3.5 3.5H4" />
    </svg>
  );
}

export function HashIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9.5 4 7.5 20M16.5 4l-2 16M5 9h15M4 15h15" />
    </svg>
  );
}

export function MenuIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 7h16M4 12h16M4 17h16" />
    </svg>
  );
}

export function SubtaskIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 4v10.5A2.5 2.5 0 0 0 8.5 17H18M15 14l3 3-3 3" />
    </svg>
  );
}

export function EditIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4.5 19.5 5.3 15.8 15.6 5.5a2 2 0 0 1 2.9 0l0 0a2 2 0 0 1 0 2.9L8.2 18.7z" />
      <path d="M13.8 7.3l2.9 2.9" />
    </svg>
  );
}

/** Editor rail: the figure. */
export function BodyIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="4.8" r="2.3" />
      <path d="M7 9.5h10M12 8.5v6M12 14.5l-3 6M12 14.5l3 6M7 9.5l-1.5 5M17 9.5l1.5 5" />
    </svg>
  );
}

/** Editor rail: the face. */
export function FaceIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 3.5c4.2 0 7 3.2 7 7.6 0 4.9-3.4 9.4-7 9.4s-7-4.5-7-9.4c0-4.4 2.8-7.6 7-7.6z" />
      <path d="M9 11.2h.01M15 11.2h.01M9.8 15.6c1.3.9 3.1.9 4.4 0" />
    </svg>
  );
}

/** Editor rail: the eyes. */
export function EyeIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z" />
      <circle cx="12" cy="12" r="3.2" />
    </svg>
  );
}

/** Editor rail: skin and makeup (lips). */
export function LipsIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3 12c2.2-3.6 4.6-5 6.4-5 1.1 0 1.9.6 2.6 1.3.7-.7 1.5-1.3 2.6-1.3 1.8 0 4.2 1.4 6.4 5-2.6 3.4-5.6 5-9 5s-6.4-1.6-9-5z" />
      <path d="M3 12h18" />
    </svg>
  );
}

/** Editor rail: accessories (a gem). */
export function GemIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M7 4.5h10l3.5 5L12 20 3.5 9.5z" />
      <path d="M3.5 9.5h17M9.5 4.5 8 9.5l4 10.5 4-10.5-1.5-5" />
    </svg>
  );
}

export function UndoIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 7 4.5 11.5 9 16" />
      <path d="M4.5 11.5h10a5 5 0 0 1 0 10H12" />
    </svg>
  );
}

export function RedoIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M15 7l4.5 4.5L15 16" />
      <path d="M19.5 11.5h-10a5 5 0 0 0 0 10H12" />
    </svg>
  );
}

/** Randomise: a die. */
export function DiceIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4" y="4" width="16" height="16" rx="3.5" />
      <path d="M8.5 8.5h.01M15.5 8.5h.01M12 12h.01M8.5 15.5h.01M15.5 15.5h.01" />
    </svg>
  );
}
