# Page system (Secretary web)

One way to build a page, so moving between sections never changes the layout under you.
It is the desktop Training page (web-live df2635eb) turned into shared parts:
`web/src/components/page/` (components) and `web/src/styles/page.css` (tokens and styles, classes `pg-*`).

Applies wherever the page's own slot is at least **880 px** wide (the Secretary Code window, a wide
browser with Lucina's stage hidden). Below that the phone layouts stay as they are unless noted.

## Page anatomy

```
┌ Page ───────────────────────────────────────────────────────────────────┐
│ Title                                   [ Sec · Sec · Sec · ⚙ ] [▶ CTA] │  PageHeader
│ context line                                                            │
│                                                                         │
│ ┌ Card ─────────────────────────────┐  ┌ Card (rail) ─────────────────┐ │  PageGrid
│ │ Card title            ghost action │  │ Card title                   │ │  main 62 / rail 38
│ │ ───────────────────────────────── │  │ key ................. value  │ │
│ │ ▶ Row title  badge    meta     ⋯  │  └──────────────────────────────┘ │
│ │   muted sub line                  │                                   │
│ └───────────────────────────────────┘                                   │
└─────────────────────────────────────────────────────────────────────────┘
```

1. **Header row** (`PageHeader`): the title (28/600) and one muted context line (a date, a count, a
   status) on the left. On the right the section nav and **at most one accent action**. The header is
   the same on every section of a page: switching sections changes only the content below it.
2. **Section nav** (`SectionNav`): a compact segmented control (4 px inset, 32 px items, 12 px
   radius). Items are links; a settings section is a gear icon at the end. No pill bars, no tab bars
   on a desktop, and the page title is never one of the tabs.
3. **Content grid** (`PageGrid`):
   - `rail`: a main column and a sticky rail, 62 / 38, 24 px gutter. The default for a page with a
     primary list plus context (stats, filters, a form).
   - `split`: a list column on the left (280-340 px, scrolls by itself) and the selected item on the
     right. For list + detail pages (notes, chats, agents, stores and a map).
   - `single`: one column up to 880 px. For one list, one form or one document.
   - `full`: the whole width. For media grids, maps, a canvas, a terminal.
4. **Cards** (`Card`): one surface for every block. 1 px border, 16 px radius, 24 px padding (16 on a
   phone), a 16/600 title on a 32 px line so titles in both columns share a baseline. Quiet actions sit
   right of the title. `list` cards run their rows edge to edge.
5. **List rows** (`ListRow` in a `RowList`): optional leading control (play button, icon, thumbnail),
   a 15/600 title (a link when the row opens something) with an optional badge, one muted 13 px sub
   line, a muted meta column, then trailing quiet actions (ghost buttons, a ⋯ menu). 64 px tall,
   hairline separators, no per-row cards.
6. **Empty states** (`EmptyState`): one friendly muted sentence (an optional title above it) and at
   most one action, a quiet button or a text link. Never a grey box, an icon tile or "No data".
7. **Sheets and dialogs** (`Sheet`, after the exercise picker): on a desktop a centred dialog 640 px
   wide (800 for `wide`), at most 80 % of the height; the frame owns the radius, border and clip, and
   only the body scrolls. A header (title, close), optional fixed tools (search, chips), the body, an
   optional footer with the actions. On a phone a bottom sheet. Escape and the backdrop close it;
   focus moves in on open and returns to the opener. A sheet may have its own one accent button
   (its confirm action) in the footer.
8. **Loading and errors**: `LoadingState` is one quiet line where the content goes (never a spinner
   that hides the header). `ErrorState` is a card with the message and a "Try again" button. A
   failed action shows a `pg-notice` above the content; a stale copy says so in the context line.
   The header and nav render before the data arrives.

## Tokens

Set on `.pg` (and the sheet), so they follow the part of the app they sit in (the Secretary Code
shell's navy surfaces, the phone's iOS greys, light mode).

| Token | Value | Use |
| --- | --- | --- |
| `--pg-surface` | `--glass` | Every card. One surface colour per page. |
| `--pg-border` / `--pg-hair` | `--border` / text 8 % | Card border / row separators. |
| `--pg-fill` / `--pg-fill-hover` | text 6 % / 10 % | Quiet buttons, the current nav item. |
| `--pg-text` / `--pg-muted` | `--text` / `--text-muted` | Primary / secondary text. |
| `--pg-accent` | `--cyan` | The one filled button (`pg-cta`). |
| `--pg-data` | `--lu-accent` | Badges, charts, focus rings, selection. |
| `--pg-success` / `-warning` / `-danger` | `--lu-positive` / `-warning` / `-negative` | Status only. |
| `--pg-radius` | 16 px | Cards, sheets. Controls 10 px, nav 12 / 8 px, chips 999 px. |

- **Spacing**: 4 and 8 inside a control; **12 / 16 / 24** between things (16 between cards in a
  column, 24 between columns and under the header); page padding 32 top, 40 sides, 48 bottom on a
  desktop and the 16 px gutter on a phone.
- **Type**: page title 28/600 (24 on a phone), card title 16/600, row title 15/600, body 14/400,
  secondary 13, caption 12, big numbers 24 or 36/600. Two weights only. Tabular figures everywhere.
- **Material**: cards and lists are solid (`--pg-surface`). Surfaces that float over content (sheets,
  popovers and menus, the composer, the phone tab bar) are frosted glass (`styles/glass.css`, the
  `--lu-glass*` tokens from `design/tokens.json`); they keep the radii above (sheets 16 px), only the
  material changes. Never glass over the live desktop or a playing video (solid glass there).
- **Accent**: one filled accent button per page, in the header. Everything else is a quiet button
  (`pg-btn`), a ghost button (`pg-btn--ghost`), a text link (`pg-link`) or an icon button
  (`pg-icon-btn`). Destructive actions are `pg-btn--danger` behind a confirm step.

## When to use which

- A page with sections → one `PageHeader` with a `SectionNav` for all of them; each section renders
  only its content. Sub-pages (a workout, an exercise) keep the header and start with a `pg-back` link
  and a 20/600 heading.
- A list of things you open or act on → a `list` Card of `ListRow`s, grouped into one card per month
  or per category when the list is long.
- Numbers about the page → a rail Card with `pg-stats` key / value lines.
- A form → a Card (single column, or main column with help in the rail); save is a quiet `pg-btn`,
  because the header owns the accent.
- Adding or editing one thing → a `Sheet`, not an inline form that pushes the page down.
- Pictures, video, maps, a live desktop → `PageGrid layout="full"` under the same header.

## Pages

Audit of 2026-10-28 (before), at 1440×900 in Secretary Code (`?shell=1`) and 390×844 on a phone,
with the mock server's data and empty states. Before/after screenshots:
`side-session-notes/…/page-system/` (`before/`, `after/`); the after shots of converted pages are
in `docs/design/page-system/`.

| Page | Header | Nav | Surfaces | Primary action | Empty state | Deviations |
| --- | --- | --- | --- | --- | --- | --- |
| Training overview | header row, date | segmented | navy cards | Start workout | sentence + one action | reference |
| Training Journal / Progress / Settings | big h1 under the nav | huge centred pill bar with "Training" as a tab | grey `#1c1c1c` cards | none / ⊕ icon / full-width Save | grey icon tile | **switching tabs changed the whole layout** |
| Chat (Lucina) | none (shell) | channel chips | blue column + stage | composer | n/a | full-bleed by design |
| All chats | none | none | per-row cards | none | — | rows as separate cards |
| Shopping | list name + count badge | none | tiles, accordion | ⊕ in the search bar | — | own toolbar, no page title |
| Shopping map | "Offers near you", back pill | store chips | grey rows | none | "Map tiles aren't installed yet." plain text | |
| Keep | Google-Keep top bar | left label rail | coloured notes | "Take a note…" | — | app-in-app; own chrome |
| Notes | h1 + trash + New note | tag chips | navy cards | New note (cyan) | — | narrow left column, empty right pane |
| Tasks | "Today" h1 | left sidebar | grey sidebar | Add task | centred bold error text | |
| Gallery (+ albums, people, places, shared, lab, duplicates, trash) | ‹ back + small title | source and kind chips | photo grid, grey group cards | New album / Apply all (cyan pill) | centred sentence | each sub-page has its own header and width |
| Films | search only | none | grey thumbs | none | — | no title, narrow centred column |
| Drive | tiny "Drive" label + search | type chips | none | none | mock note | no real header |
| Media library | chips + search | Videos/Audio/Photos chips | grey list | none | — | |
| Instagram | search + Open | none | grey chips | Open | "Anonymous…" note | no title |
| Pixiv | wide pill bar (Ranking/Search/Bookmarks) | pill bar | thumbs | none | — | pill bar like the old Gym |
| NClient | segmented tabs | tabs | black panel | Search | — | own panel colour |
| Library | h1 | filter chips | warning boxes | none | orange error boxes | |
| Links | h1 + context | none | navy cards | Sync calendars (quiet), Add to calendar (cyan) | — | closest to the system |
| Plugins | h1 | none | grey tiles | none | — | |
| Agents | none | New run / Permissions / Schedules pills | dense list, empty right pane | New run | "Pick an agent…" | own split layout |
| Server health | ↻ only | none | grey metric tiles | none | — | no title, narrow column |
| Secrets | none | none | sheet over the chat | Add secret sheet | — | the route opens a sheet |
| Account / Settings | none | none | dialog over a blurred page | Change password (cyan) | — | the route opens a dialog |
| Notifications | none | none | panel over the chat | Enable notifications | — | the route opens a panel |
| Decisions, Approvals | h1 + context | none | none | "Back to Secretary" | sentence + Try again | narrow column |
| Computer | Desktop/Android + Live/Simple chips | chips | viewer frame | Take over | "Not found." red | |
| Weight | card title "Weight" | none | navy cards | Log weigh-in (cyan, full width) | — | phone layout stretched |
| Desk, Memos, Models, Scan | spinner only | — | — | — | — | the spinner hides the whole page while loading |
| Pokémon | none | none | small frame | Play | "Not found." | |

### Per page

Each entry: purpose, layout, primary action. Pages are converted one commit each; an entry without
a screenshot link is not converted yet.

- **Training** (`/training`, `/gym`, `/gym/history`, `/gym/progress`, `/gym?tab=settings`, details).
  Purpose: train, log, look back. Header: "Training", today's date, nav Overview · Journal · Progress
  · ⚙, accent **Start workout** (Continue workout during one).
  - Overview: rail. Main: this week, routines, recent workouts; rail: weight, last 30 days.
    ![](page-system/training.jpg)
  - Journal: rail. Main: one list card per month, rows open the workout and repeat it; rail: totals.
    ![](page-system/journal.jpg)
  - Progress: rail. Main: training volume chart, personal records (rows open the exercise); rail: body
    measurements, added in a sheet. ![](page-system/progress.jpg)
  - Settings: rail. Main: training preferences, your data; rail: about.
    ![](page-system/settings.jpg)
  - Workout and exercise details: same header, a back link, rail with the summary and actions.
    ![](page-system/journal-detail.jpg)
  - Adding a measurement is the reference sheet. ![](page-system/progress-sheet.jpg)
  - The live workout (`/gym/workout`) is a focused flow and keeps its own screen.

Every other page (release b). Screenshots are named after the route in `page-system/`.

| Page (route) | Purpose | Header (context · nav · accent) | Grid | Shot |
| --- | --- | --- | --- | --- |
| Lucina (`/`) | Talk to Lucina | none: the conversation and her stage are the page | exception, full-bleed | — |
| All chats (`/chats/:key?`) | Every conversation across devices and bots | count · — · — | single, split with a conversation | ![](page-system/chats.jpg) |
| Shopping (`/shopping`) | The shopping list | list · N to buy, list switcher and ⋯ as quiet buttons · — (⊕ stays in the add field) | rail: to buy, catalog · recently used, offers | ![](page-system/shopping.jpg) |
| Offers near you (`/shopping/map`) | Stores and offers on a map | back link · — · — | split: stores list · map | ![](page-system/shopping-map.jpg) |
| Notes (`/notes/:key?`) | Briefings and documents | — · — · **New note** | split: notes list · the note | ![](page-system/notes.jpg) |
| Keep (`/keep`) | Google-Keep-style notes | current label · search, refresh, view, settings as icons · — | split: labels · notes | ![](page-system/keep.jpg) |
| Tasks (`/tasks`) | Todoist tasks | section · Inbox · Today · Upcoming · Filters & labels · Completed · **Add task** (sheet) | rail: task groups · projects, goal | ![](page-system/tasks.jpg) |
| Gallery (`/gallery`, albums, people, places, shared; lab, duplicates, trash, archive, album, person, category) | Photos and videos | count · Photos · Albums · People · Places · Shared · the section's one action (New album, Share an album, Apply all suggestions) | full; sub-pages keep the header, a back link and a heading | ![](page-system/gallery.jpg) ![](page-system/gallery-albums.jpg) |
| Films (`/films`) | Film and Dansk film | count, search · — · — | full: one card per collection | ![](page-system/films.jpg) |
| Media library (`/media/library`) | Videos, audio, photos | — · Videos · Audio · Photos, search · — | single list | ![](page-system/media-library.jpg) |
| Drive (`/drive`) | Lectures, projects and files | folder path, search · — · — | single list, type chips on top | ![](page-system/drive.jpg) |
| Instagram (`/instagram`) | Watched profiles | anonymous note · username field · **Open** | rail: recent profiles · watched accounts | ![](page-system/instagram.jpg) |
| Pixiv (`/pixiv`) | Ranking, search, bookmarks | — · Ranking · Search · Bookmarks · — | full grid, mode chips on top | ![](page-system/pixiv.jpg) |
| NClient (`/nclient`) | Private reading | — · Search · Favorites · Random · — | full | ![](page-system/nclient.jpg) |
| Library (`/library/:id?`) | Kindle books, highlights | — · — · — | rail: search, filters, books · activity | ![](page-system/library.jpg) |
| Connections (`/links`) | Calendars, chats, AI clients | subtitle · — · **Sync calendars** | rail: waiting for your OK, next 7 days · sources | ![](page-system/links.jpg) |
| Plugins (`/plugins`) | Everything else | count · — · — | full tile grid | ![](page-system/plugins.jpg) |
| Agents (`/agents/*`) | Coding agents | running / permission counts · Runs · Permissions · Schedules · Usage · Map · **New run** | split: runs · the agent | ![](page-system/agents.jpg) |
| Lucina's computer (`/computer`) | Watch and take over her desktop | connection status · Desktop · Android · **Take over** | full viewer | ![](page-system/computer.jpg) |
| Server (`/server-health`) | Hosts, services, incidents, deploys (owner ops dashboard, server-v2-web) | status line · Overview · Services · Incidents · Deploys, refresh icon · — | per section; host details in a sheet | ![](page-system/server-health.jpg) |
| Secrets (`/secrets`) | Secret names by location | — · — · **Add secret** (sheet) | single | ![](page-system/secrets.jpg) |
| Account (`/account`) | Settings of this device | device · General · Notification settings · Import · — | rail: settings cards · device, log out | ![](page-system/account.jpg) |
| Notifications (`/notifications`) | What Lucina told you | unread count, Mark all read, settings icon · — · — | single: one card per day | ![](page-system/notifications.jpg) |
| Decisions, Approvals (`/decisions`, `/approvals`, details) | Lucina's questions; requests for your OK | subtitle · — · — | single list, detail with a back link | ![](page-system/decisions.jpg) ![](page-system/approvals.jpg) |
| Weight (`/weight`) | All weigh-ins | latest weigh-in, link back to Training · — · — | rail: trend, weigh-ins · log, stats, settings | ![](page-system/weight.jpg) |
| My desk (`/desk`) | Lucina's work and tools | subtitle · — · Refresh (quiet) | single | ![](page-system/desk.jpg) |
| Voice memos (`/memos/:id?`) | Recorded, transcribed, summarised | — · — · — | split: memos · the memo | ![](page-system/memos.jpg) |
| Prompter (`/prompter/:id?`) | Talking points Lucina crosses out while he speaks | script · points · **Start take** | split: scripts · the script (points, take settings, take log); the live take is a focused full-screen flow (like the live workout) | ![](page-system/prompter.jpg) ![](page-system/prompter-live.jpg) |
| Notebooks (`/notebooks`, `/notebooks/:id`) | NotebookLM-style workspace (Open Notebook port): sources, linked Keep notes, a chat that cites passages, insights, podcasts | list: count · Active · Archived · **New notebook** (sheet). Notebook: description and counts · Search, Podcast, settings (gear) · **Add source** (sheet); on a phone the nav is Sources · Notes · Chat | list: single; notebook: full, three cards side by side (Sources · Notes · Chat), one at a time on a phone; sheets for adding a source, the source reader, the podcast and search | ![](page-system/notebooks.jpg) ![](page-system/notebook.jpg) |
| 3D models (`/models/:id?`) | Lucina's studio | — · — · — | full grid, detail with a back link | ![](page-system/models.jpg) |
| Scan QR code (`/scan`) | Sign in another device | — · — · — | single: camera card, code field | ![](page-system/scan.jpg) |
| Pokémon (`/pokemon`) | Lucina's game | — · — · **Play** | single | ![](page-system/pokemon.jpg) |

Notes:
- Inside Secretary Code, pages that show Lucina's stage beside them (All chats) have less than 880 px and
  use their narrow layout, like Training in a 1280 px browser with the stage open.
- Pages whose phone column is capped (Scan, Pokémon, Memos, Models, Desk, Computer) measure their route
  slot with `useWide().trackSlot`, or they would never switch.
