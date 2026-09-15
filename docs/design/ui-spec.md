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

### Library browse
- Header bar with `GtkSearchEntry` (revealed via search button) and a filter/sort `GtkMenuButton`.
- Content: `GtkGridView` of cover art (grid mode) or `GtkListView` with `AdwActionRow`s (list mode,
  useful for podcast episode-style feeds); toggle between the two via header bar button.
- Sticky section headers when sorted/grouped by author or series (`GtkListView` section headers).

### Item detail
- `AdwNavigationPage` pushed from Home/Library.
- Top: large cover art, title, author/narrator, duration, progress bar if partially listened.
- Actions row: primary "Play"/"Resume" button, secondary download button (`GtkButton` with a
  download/checkmark/spinner icon reflecting download state).
- `AdwExpanderRow` or plain text block for description (truncated with "more").
- Chapter/episode list as `AdwActionRow`s, each showing chapter title + duration, tap to seek.

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
- `AdwPreferencesPage` with groups: **Account** (server URL, logged-in user, sign out),
  **Playback** (default speed, skip-forward/back intervals, sleep-timer default),
  **Appearance** (follow system / light / dark), **About** (version, license, links).

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
  covers/rows).
