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
