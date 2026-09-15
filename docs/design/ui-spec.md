# UI/UX Design Spec — Audiobookshelf client for mobile Linux (phosh) / GTK4 + libadwaita

This document defines the interface for a native Audiobookshelf client targeting mobile Linux
(phosh, e.g. Librem 5 / PinePhone) and desktop GNOME, built with GTK4 + libadwaita in Rust.

Feature scope and information architecture are informed by [Lissen](https://lissenapp.org/), an
Audiobookshelf client for Android. The visual language, navigation patterns, and widget choices
follow the GNOME HIG and adaptive-app conventions (as seen in apps like Podcasts, Decibels, and
Fractal) rather than copying Lissen's Material Design look.

## 1. Screen inventory

| Screen | Purpose |
|---|---|
| **Welcome / Server login** | Enter Audiobookshelf server URL, authenticate (username+password or API token). |
| **Library picker** | Choose which library (books / podcasts) to browse, if the server has more than one. |
| **Home** | "Continue listening" shelf, "Recently added", quick access to libraries. |
| **Library browse** | Grid/list of items in a library; filter by author, series, genre; sort; search. |
| **Item detail** | Cover, metadata, description, chapter/episode list, play/download actions. |
| **Player (mini)** | Persistent bottom bar: cover thumbnail, title, play/pause, progress line. |
| **Player (full)** | Now-playing page: large cover, scrubber, chapter list, speed, sleep timer, skip ±. |
| **Downloads** | Manage items downloaded for offline playback; storage usage; remove. |
| **Settings** | Account/server management, playback defaults (speed, skip interval), appearance, about. |

## 2. Navigation model

- **Phone width (~360–599px, single pane):** `AdwViewStack` + bottom `AdwViewSwitcher`-style tab
  bar with 4 destinations: **Home**, **Library**, **Downloads**, **Settings**. Each destination is
  an `AdwNavigationView` stack so drilling into an item (e.g. Library → Item detail) pushes a page
  with a back button, per GNOME HIG.
- **Wide width (≥600px, tablet/desktop):** `AdwNavigationSplitView` — the same 4 destinations
  become a persistent sidebar (`AdwViewStackSidebar`-like list), with content shown in the second
  pane. Item detail pushes within the content pane, not full-screen.
- The breakpoint is implemented with `AdwBreakpoint` at 600px width, matching libadwaita's own
  convention for phone-vs-wide layouts (same threshold `AdwNavigationSplitView` uses natively).
- **Player** is modeled as an overlay, not a 5th tab:
  - A **mini-player bar** is pinned above the bottom tab bar (phone) or above the sidebar/content
    split (wide) whenever something is loaded, via `AdwToolbarView`'s bottom bar slot.
  - Tapping the mini-player pushes the **full player** as a modal `AdwNavigationPage` (phone) or a
    `AdwDialog` sized to the window (wide), swipeable/dismissible back down to the mini-player.
  - This mirrors Lissen's mini-player → full-player expansion, which is a near-universal pattern
    for audio apps and translates directly to libadwaita idioms.

## 3. Screen-by-screen detail

### Welcome / Server login
- `AdwStatusPage` with app icon, "Connect to your Audiobookshelf server".
- `AdwEntryRow` for server URL, `AdwPasswordEntryRow` for password, or a toggle to switch to
  API-token entry.
- Primary `AdwButton` (suggested-action style) "Connect". Inline `AdwBanner` for connection errors.

### Library picker
- Shown only if the server exposes >1 library. Simple `AdwActionRow` list, one row per library
  with an icon (book vs. headphones for podcasts) and item count as subtitle.

### Home
- `AdwToolbarView` with `AdwHeaderBar` (title "Home", avatar/account button on the right).
- Horizontally-scrolling `GtkListView`/carousel rows: "Continue listening" (progress ring overlay
  on cover), "Recently added". Each cover is a tappable card pushing Item detail.
- Empty/offline state: `AdwStatusPage` ("No library synced yet").
- **Offline-mode toggle**: a `GtkToggleButton` in the header bar (leading side, opposite the
  avatar), iconified with an airplane/cloud-off glyph and labeled "Offline". When active, an
  `AdwBanner`-style strip appears below the header ("Showing downloaded items only") and every
  shelf (Continue listening, Recently added) and the Libraries list filter down to items that are
  downloaded fully or partially — the same downloaded-item definition used everywhere else in this
  spec (see Item detail). This is state shared with the equivalent toggle on Library browse, not a
  per-screen setting.

### Library browse
- Header bar with `GtkSearchEntry` (revealed via search button), a filter/sort `GtkMenuButton`, and
  a trailing **view-options button** (three-line "adjustments" icon) that opens the view options
  sheet described below.
- Content: `GtkGridView` of cover art (grid mode) or `GtkListView` with `AdwActionRow`s (list mode,
  useful for podcast episode-style feeds); toggle between the two via header bar button.
- Sticky section headers when sorted/grouped by author or series (`GtkListView` section headers).
- Category chips (All/Author/Series/Genre) stay in a scrollable row below the header for quick
  filtering, unchanged.
- **View options sheet**: an `AdwBottomSheet` (grip handle, no title needed — the rows are
  self-explanatory) with:
  - **Downloaded only** — `AdwSwitchRow`. When on, the grid filters to items downloaded fully or
    partially, matching Home's offline-mode behavior (see below) but scoped to this library; an
    empty result shows `AdwStatusPage` ("No downloaded items").
  - **Hide finished** — `AdwSwitchRow`, filters out fully-listened items.
  - **Grouping** — `AdwComboRow` (e.g. "By Series", "By Author", "None").
  - **Sort by** — `AdwComboRow` (e.g. "Date of creation", "Title", "Author", "Duration").
  - **Application settings** — plain `AdwActionRow` with a chevron, navigating to the Settings tab;
    included here because it's where Lissen places it and it's a reasonable one-tap shortcut from
    the library a user spends most of their time in.
  All rows use native libadwaita controls (real `GtkSwitch`-style pill toggles, not a
  platform-specific switch skin) and the sheet itself is the same `AdwBottomSheet` pattern used for
  Item detail's download sheet, so the two don't feel like different UI systems.

### Item detail
- `AdwNavigationPage` pushed from Home/Library.
- Top: large cover art, title, author/narrator, duration, progress bar if partially listened.
- Actions row: primary "Play"/"Resume" button, secondary download button.
- **Download options (Lissen-style)**: the download button opens an `AdwBottomSheet` titled
  "Download book" (libadwaita's adaptive bottom-sheet widget, matching Lissen's own sheet) instead
  of immediately downloading everything:
  - **Current chapter** — just the chapter currently playing/at the last playback position.
  - **Next chapters** — an inline numeric stepper (`−` / count / `+`) on this row lets the user
    pick exactly how many upcoming chapters to fetch, defaulting to 10 and clamped to the number
    of chapters actually remaining after the current one. Tapping the row (outside the stepper)
    starts the download for that many chapters; the other rows have no stepper and act immediately
    on tap.
  - **Remaining chapters** — from the current position to the end of the book.
  - **Entire book** — all chapters, regardless of playback position.
  - **Clear downloaded chapters** — a destructive row, separated from the four download options,
    that removes whatever has been downloaded locally for this item.

  Once a download starts, the download button itself reflects overall state for the item (idle
  download icon → in-progress spinner or progress ring → checkmark once at least the current
  chapter is fully downloaded), mirroring the badge already used on Library covers.
- `AdwExpanderRow` or plain text block for description (truncated with "more").
- Chapter/episode list as `AdwActionRow`s, each showing chapter title + duration, tap to seek.
- **Offline-availability marker**: each chapter row that has been downloaded shows a small
  filled checkmark-in-circle glyph trailing the duration, distinct from the "currently playing"
  bars icon already used on the active chapter. Chapters not yet downloaded show no glyph at all —
  presence of the glyph is the signal, so the list isn't cluttered with a "not downloaded" icon on
  every other row. A one-line legend ("● downloaded") sits in the "Chapters" section header so the
  glyph's meaning doesn't need to be inferred.

### Player — mini
- Fixed bar: 40–48px cover thumbnail, title + author (single line, ellipsized), play/pause icon
  button, thin progress line along the bottom edge of the bar.
- Swipe-up gesture or tap opens the full player.

### Player — full
- `AdwNavigationPage` with a transparent/blurred header (down-chevron to collapse, `⋯` menu for
  "Sleep timer", "Playback speed", "Add bookmark").
- Large cover art, title/author, `GtkScale`-based scrubber with elapsed/remaining time labels.
- Transport row: skip-back-N-seconds, play/pause (large), skip-forward-N-seconds.
- Secondary row: playback speed button (cycles/opens popover with 0.8×–3.0× options), sleep-timer
  button (opens popover: off / 15 / 30 / 45 min / end-of-chapter), chapters button (opens a sheet
  listing chapters, current one highlighted).

### Downloads
- `AdwPreferencesPage`-style grouped list: a summary row (storage used / device free space), then
  an `AdwActionRow` per downloaded item with a remove button. Empty state via `AdwStatusPage`.

### Settings
- `AdwPreferencesPage` with groups: **Account**, **Servers**, **Playback** (default speed,
  skip-forward/back intervals, sleep-timer default), **Appearance** (follow system / light /
  dark), **About** (version, license, links).
- **Account** group: one row showing the *active* server/account (username, server host,
  "active" subtitle) with a chevron, and one "Switch or manage servers" row that opens the
  Servers list below. There is deliberately no top-level "Sign Out" action here — with multiple
  servers supported, "sign out" is ambiguous about *which* account, so it isn't a global control.
- **Servers** group: one `AdwActionRow` per configured server (host as title, logged-in username
  and "active" marker as subtitle), each with a trailing menu button (`⋯` / `GtkMenuButton`)
  opening a small popover with **Switch to this server**, **Sign Out**, and **Remove Server**
  (destructive style). This is where sign-out actually lives — scoped to one server/account at a
  time — plus an "Add Server" row at the end to register another Audiobookshelf instance.

## 4. Adaptive/responsive behavior summary

- Breakpoint at 600px (`AdwBreakpoint`), matching libadwaita convention:
  - **< 600px (phone/phosh):** single-pane navigation, bottom tab bar, full-screen player.
  - **≥ 600px (tablet/desktop):** `AdwNavigationSplitView` sidebar + content, player as a
    window-sized dialog rather than full-screen takeover.
- All list/grid views reflow item counts per row based on available width (`GtkGridView` with a
  minimum tile width, not a fixed column count) so the same layout scales from phone to desktop.
- Touch targets sized per GNOME HIG (minimum 44×44px) throughout, since phosh is a touch shell.

## 5. State handling

- **Loading:** `AdwSpinner`/`GtkSpinner` in place of content, skeleton-less (per HIG, prefer
  spinners over skeleton screens for this style of app).
- **Empty:** `AdwStatusPage` with an icon, title, and short description (e.g. "No downloads yet").
- **Error/offline:** `AdwBanner` at the top of the affected page, plus `AdwToast` for transient
  errors (e.g. "Failed to sync progress — will retry").
- **Offline-first playback:** downloaded items remain playable and browsable without a server
  connection; the app clearly marks which items are available offline (small download badge on
  covers/rows, and per-chapter offline markers in Item detail). The Home/Library offline-mode
  toggle (see those sections) lets a user deliberately narrow either screen to only such items
  even while online, e.g. before a trip.
