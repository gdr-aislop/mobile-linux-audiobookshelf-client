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
| **Connection** | Per-server advanced connection settings: headers, TLS, client cert, local address, user agent. |

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
- `AdwStatusPage`-style centered layout: app icon, "Connect to your Audiobookshelf server" title,
  a one-line description. No header-bar back button — this is the first thing a user with no
  configured server sees, so there's nothing to navigate back to.
- A segmented toggle (`AdwToggleGroup`/two-button segmented control, matching the same pattern
  used for Settings' Theme picker) switches the form between two auth modes:
  - **Password** — Server URL, Username, and a masked Password field (with a show/hide eye-icon
    toggle) as one grouped card of `AdwEntryRow`/`AdwPasswordEntryRow`-equivalent rows.
  - **API Token** — Server URL and a single Token field; username/password are hidden entirely
    rather than just disabled, since they're not applicable to this mode.
- Primary `AdwButton` (suggested-action style, full width) "Connect", **disabled until the
  required fields for the current mode are filled in** — on first launch, with an empty form,
  this is the state the user actually sees.
- **Error state**: on a failed connection attempt, an inline `AdwBanner`-style strip appears above
  the form with a short explanation (e.g. "Unable to sign in — check your username and password
  and try again"), and the offending field's label switches to an error/red tint. The form
  retains whatever the user typed — a failed attempt should never clear the fields.
- This screen (via `abs_core::accounts::add_server_and_login`) is what's shown whenever
  `abs_storage::repo::accounts::get_active` returns `None` — i.e. on first launch, or after
  signing out of every configured server.

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
- This bar is pinned above the tab bar (phone) or the sidebar/content split (wide) on **every**
  tab, not just Home — see the "Home" and "Library browse" mockups, both of which show it fixed
  below their scrollable content. It reflects whatever's currently loaded regardless of which
  screen the user navigated to since starting playback.

### System media integration
Lissen's Android build posts a persistent MediaStyle notification — cover art, transport controls,
a seek bar — so playback is visible and controllable from the lock screen and notification shade
even when the app isn't focused. The direct GNOME/phosh equivalent is **not** a hand-drawn
`GNotification`, but exporting the **MPRIS2** D-Bus interfaces
(`org.mpris.MediaPlayer2`/`org.mpris.MediaPlayer2.Player`) from `abs-player`. GNOME Shell and phosh
already know how to render cover art, title/author, transport controls, and a scrub position from
those properties on their own system surfaces — the Shell's quick-settings media widget and the
lock screen's media card — with no custom UI code needed for either. See the
`SystemMediaWidget.dc.html` mockup, which depicts the phosh lock screen card this produces (marked
in the mockup itself as OS-rendered, not app UI, since there's nothing here for this app to paint).
- **Properties to keep current**, on every playback state change (play/pause/seek/position tick),
  not just at session start: `PlaybackStatus`, `Metadata` (`xesam:title`, `xesam:artist`,
  `mpris:artUrl` pointing at the cached cover file, `mpris:length`), `Position`, `Rate`.
- **Methods**: `PlayPause`/`Play`/`Pause`, `Seek`, and `Next`/`Previous` mapped to this app's
  skip-forward/back-N-seconds actions rather than a literal track change — matching what Lissen's
  own notification buttons actually do.
- A plain `GNotification` is deliberately **not** part of this design: the quick-settings/lock-screen
  media widget already gives always-on visibility and controls without user action, so a second,
  separately-dismissible notification would just duplicate it.

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
  time — plus an "Add Server" row at the end to register another Audiobookshelf instance. Tapping
  a server row's body (as opposed to its `⋯` menu) pushes that server's **Connection** page.

### Connection
- `AdwNavigationPage` pushed from a server row in Settings' Servers group; one instance per
  configured server, covering the advanced connection settings a self-hosted Audiobookshelf setup
  commonly needs (custom reverse proxies, self-signed certs, LAN-only servers).
- **Server connection** group: a single `AdwActionRow` showing the server's full URL as its
  subtitle (monospace, since it's a literal value being confirmed rather than a label) with a
  trailing info-button opening a popover/tooltip explaining what this connection is used for.
- **Advanced** group:
  - **Custom Headers** — `AdwActionRow`, chevron, pushes a page for adding request headers sent on
    every server call (e.g. for auth proxies in front of Audiobookshelf).
  - **Disable SSL verification** — `AdwSwitchRow`; off by default, since disabling verification is
    a deliberate opt-in for servers with self-signed or otherwise unverifiable certificates.
  - **Client certificate** — `AdwActionRow`, chevron, pushes a page to select/import a client
    certificate for mTLS.
  - **Local network server address** — `AdwActionRow`, chevron, pushes a page to set an alternate
    address used automatically when on the server's home Wi-Fi (avoiding a round trip through the
    public internet for LAN clients).
  - **Change User Agent** — `AdwActionRow`, chevron, pushes a page to override the `User-Agent`
    header the app sends (useful when a server or proxy filters by it).
- A destructive **"Disconnect from the Server"** plain-text action (styled like `AdwButton`'s
  `destructive-action` but as a flat/link-style button, not a filled button, since it's an
  infrequent, page-level action rather than a primary one) sits below the Advanced group,
  vertically separated rather than boxed in its own card.

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
