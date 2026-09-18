# Manual UI Test Plan

Touch-driven test checklist for the app as a user sees it — no unit tests, no tooling beyond the
app itself. Derived from `docs/design/ui-spec.md` and covering **the whole spec**, not just the
screens built so far.

Each section header carries a status tag:

- **✅ implemented** — test today against the current app.
- **🚧 not yet built** — the UI doesn't exist yet (the tab is a stub or the feature is absent).
  These tests are the spec-conformance checklist to run the moment the screen lands; right now
  they document what *should* happen, and the corresponding area must fail gracefully (per §2/§10).

- [ ] = unchecked, [x] = passed. Anything failing should be reproducible from its steps alone.

## 0. Prerequisites & test assets

- The app running on a touch device (Librem 5 / PinePhone) **or** a desktop window at phone width
  (~360–500 px). Desktop is fine for everything except §11's shell/lock-screen tests and §12's
  call tests, which need a real GNOME/phosh session.
- An Audiobookshelf server you control (or the public demo `https://audiobooks.dev/audiobookshelf`,
  username/password `demo`/`demo`).
- A test server set up with:
  1. A **single-file book** (one audio file).
  2. A **multi-file book** with at least 3 audio files ("tracks"), each ≥ 2 minutes, so track
     transitions can be observed without waiting forever.
  3. A book with a **very long title and/or long author name** (for ellipsis checks).
  4. A book you have **partially listened to** on the server (for Continue Listening / Resume).
  5. A book with **many chapters** (20+) and long chapter titles (for the chapter list).
  6. **Two or more libraries** (e.g. one "Books", one "Podcasts") to trigger the library picker
     and per-library behavior.
  7. Optionally: a **reverse proxy in front of the server** (for custom headers / user-agent
     tests) and one serving a **self-signed certificate** (for the TLS tests in §16).
- A way to cut network access (airplane mode / Wi-Fi off) for the offline and adverse sections.
- Optional: a phone able to receive calls (for §12's call-interruption test).

Conventions: **playback position** always means position in the *book*, not in the current audio
file. "Mini bar" is the bottom player strip; "full player" is the Now Playing screen;
"view options sheet" is Library browse's bottom sheet.

---

## 1. Welcome / Server login (WT) — ✅ implemented

- [ ] **WT-1 — First launch shows the login screen.** Fresh app data (or after signing out of
      every server, once Settings exists). Launch the app.
      *Expected:* "Connect to your Audiobookshelf server" screen with icon, description, a
      Password / API Token toggle, Server URL + Username + Password fields, and a **Connect**
      button. No back button anywhere.

- [ ] **WT-2 — Connect starts disabled.** Look at the Connect button with an empty form.
      *Expected:* greyed out / unresponsive.

- [ ] **WT-3 — Connect enables only when the current mode's fields are filled.** Type into the
      fields one at a time: URL only → still disabled; URL + username → still disabled; then a
      password → enabled. Delete the username → disabled again.
      *Expected:* Connect flips enabled/disabled exactly as described. Whitespace-only input
      counts as empty.

- [ ] **WT-4 — Password show/hide.** Tap the eye icon in the Password field.
      *Expected:* password text toggles between dots and plain text.

- [ ] **WT-5 — Mode switch swaps fields.** Tap "API Token".
      *Expected:* Username and Password disappear entirely; a single "API Token" field appears.
      Tapping "Password" brings them back.

- [ ] **WT-6 — Successful login.** Fill in valid credentials and tap Connect.
      *Expected:* button label changes to "Connecting…" and the form locks during the attempt,
      then the main app shell (Home tab) replaces the login screen.

- [ ] **WT-7 — Wrong password error.** Connect with a wrong password.
      *Expected:* a red banner strip above the form reading "Unable to sign in — check your
      username and password and try again."; the Username **and** Password fields get a red
      error tint; **no** "Show details" disclosure (a bad password isn't a transport error).

- [ ] **WT-8 — Unreachable server error.** Connect with a valid-looking URL nothing listens on
      (e.g. `http://127.0.0.1:1`), any username/password.
      *Expected:* banner reads "Can't reach this server — check the URL and your connection.";
      the Username/Password fields are **not** tinted red (this isn't a credentials problem); a
      "Show details" disclosure is available and reveals the raw error text.

- [ ] **WT-9 — Timeout error message.** Point the URL at a host that blackholes (e.g. a
      non-routable IP) and connect.
      *Expected:* banner reads "This server took too long to respond — check your connection and
      try again."

- [ ] **WT-10 — Form survives a failed attempt.** After WT-7 or WT-8, look at the fields.
      *Expected:* everything you typed is still there, the form is re-enabled, and Connect works
      again without retyping.

- [ ] **WT-11 — API token mode is explicitly unsupported.** Switch to API Token, fill URL +
      token, tap Connect.
      *Expected:* a banner explaining that signing in with an API token isn't supported yet —
      use username and password. No crash, no fake login.

- [ ] **WT-12 — Login screen during no network.** With airplane mode on, connect with any
      credentials.
      *Expected:* the connectivity-failure banner (WT-8 wording), app stays on the login screen.

- [ ] **WT-13 — Re-login is pre-filled and abortable.** Trigger the re-login flow (HT-2a: let a
      session die, press Log in again).
      *Expected:* the login screen appears with Server URL and Username already filled in
      (password empty and focused), and a Cancel button below Connect. Cancel returns to the app
      with the old session still working; the form is otherwise the ordinary login screen (WT-1
      minus "no back button anywhere").

- [ ] **WT-14 — Replacing an account asks first.** From WT-13's pre-filled screen, change the
      username (or the URL) and connect.
      *Expected:* a confirmation dialog spelling out what will be removed on this device before
      anything happens. Cancel dismisses it with nothing changed (the form stays usable, the old
      session stays active). Replace proceeds with the login; if the login then fails, the old
      session is still intact (retryable, nothing was removed).

---

## 2. App shell & navigation (NT) — ✅ implemented (stubs for unbuilt tabs)

- [ ] **NT-1 — Four tabs exist.** After login, check the bottom tab bar.
      *Expected:* Home, Library, Downloads, Settings — each with an icon and label, Home
      selected.

- [ ] **NT-2 — Tab switching.** Tap each tab in turn and back.
      *Expected:* content switches instantly each time; selected tab stays highlighted.

- [ ] **NT-3 — Unbuilt tabs are honest stubs.** Open Library, Downloads, Settings.
      *Expected:* each shows a status page titled with the tab's name and "Coming soon" — not a
      blank screen, not a crash.

- [ ] **NT-4 — Mini bar absent before first playback.** On a fresh session, look above the tab
      bar on every tab.
      *Expected:* no mini-player bar anywhere.

---

## 3. Home (HT) — ✅ implemented (offline toggle & Sync now: 🚧)

- [ ] **HT-1 — Shelves populate.** With a synced server, check Home.
      *Expected:* "Continue Listening" (only books with progress), "Recently Added", and "Your
      Libraries" list. Covers load; items you've started show a "% listened" subtitle.

- [ ] **HT-2 — Empty server state.** Point the app at a server with no libraries / nothing
      synced (or first sync failing).
      *Expected:* while the first sync runs, a spinner with "Syncing your libraries…" instead of
      empty shelves; if the sync fails, "Couldn't sync your libraries" with the error in a
      details line and a Try again button that restarts the sync; if the server genuinely has no
      libraries, "No library synced yet" with Try again.

- [ ] **HT-2a — Dead session state.** Point the app at a server that 401s the sync (revoked
      refresh token, removed user — anything that makes the server reject the session).
      *Expected:* "Sign in again" with "Your session on this server has expired or was revoked.",
      the error in the details line, and both Try again and a **Log in again** button. Pressing
      Log in again opens the Welcome screen with URL and username pre-filled (only the password
      left to type) and a Cancel button back to the app.

- [ ] **HT-2b — Re-login replaces cleanly.** From HT-2a's pre-filled Welcome screen, sign in
      with the same credentials; then again with a different account on the same server; then
      with a different server entirely.
      *Expected:* same credentials — straight back to your libraries, no prompt, no re-sync from
      scratch. Different account on the same server — a confirmation dialog explains the old
      account's on-device progress is removed; after confirming, the libraries appear immediately
      (the cache is reused). Different server — the dialog explains the old server's cached data
      is removed; after confirming, a fresh sync runs. Cancelling the dialog or pressing Cancel
      at any point changes nothing (the old session stays intact).

- [ ] **HT-3 — Tapping a cover starts playback.** Tap any cover card in Continue Listening or
      Recently Added.
      *Expected:* playback starts (mini bar appears). Note: the spec's end state is tapping
      through to an Item detail page (§6); until that's built, the card plays directly — that's
      the current intended behavior.

- [ ] **HT-3a — Tapping a shelf heading opens the Library pre-sorted.** Tap the "Recently
      Added" heading.
      *Expected:* switches to the Library tab sorted by date added (newest first), no filter.

- [ ] **HT-3b — Continue Listening heading lands filtered.** Tap the "Continue Listening"
      heading.
      *Expected:* switches to the Library tab sorted by last listened (newest last-listen first,
      never-played books last) and filtered to in-progress books: the filter banner ("Showing
      books in progress", with a Show all button) is visible and the sort dropdown's icon is a
      funnel. Show all (or unticking the dropdown's "In progress only") restores the full library.

- [ ] **HT-3c — "Last listened" sort works from the dropdown.** Open the Library's "Sort &
      filter" dropdown, pick "Last listened"; enable "In progress only" manually.
      *Expected:* books order by when you last listened to them (per the server's own listening
      times); the filter hides finished and never-played books and shows the banner. Neither the
      sort nor the filter survives an app restart (session-transient).

- [ ] **HT-4 — Sync failure banner.** Load Home with the server unreachable (kill the server /
      turn off Wi-Fi, then switch to the Home tab so it re-syncs).
      *Expected:* banner "Couldn't sync — showing what's cached." and the previously cached
      shelves still render. With nothing cached, the retryable failure state shows instead
      ("Couldn't sync your libraries" + Try again) — the error is never hidden behind an empty
      state. If the failure is an authorization failure instead (server 401s the session), the
      banner reads "Session expired — showing what's cached." and carries a **Log in again**
      button (HT-2a). No crash, no blank page.

- [ ] **HT-5 — Cached render beats the network.** With Wi-Fi off but a previously synced Home,
      launch the app.
      *Expected:* shelves appear quickly from cache (possibly followed by the sync-failure
      banner), rather than an empty screen while a network call hangs.

- [ ] **HT-6 — Account avatar.** Check the circular avatar button (account's initial letter) at
      the header bar's right.
      *Expected:* a tooltip "Signed in as \<username\>" on long-press/hover. Tapping does nothing
      (account management will live in Settings, §15) — it must not crash.

- [ ] **HT-7 — Horizontal shelf scrolling.** With more items than fit on screen, swipe a shelf
      row sideways.
      *Expected:* the row scrolls horizontally without also scrolling the page vertically.

- [ ] **HT-8 — 🚧 Offline-mode toggle.** Once built: tap the "Offline" toggle in Home's header
      (leading side, opposite the avatar).
      *Expected:* an "Showing downloaded items only" banner appears below the header and every
      shelf — plus the Libraries list — filters down to downloaded items (fully or partially).

- [ ] **HT-9 — 🚧 Offline toggle is shared state.** With Home's offline toggle on, open Library
      browse; then toggle it there and check Home.
      *Expected:* the same state is reflected on both screens (it's one setting, not two), and
      it survives leaving the tab.

- [ ] **HT-10 — 🚧 Sync now.** Once built: Home's ⋯ header menu → "Sync now".
      *Expected:* an immediate re-sync runs and a transient toast confirms completion — or
      reports failure — without needing to leave the tab.

---

## 4. Library picker (LP) — 🚧 not yet built

Shown between login and Home only when the server exposes **more than one** library.

- [ ] **LP-1 — Multi-library server shows the picker.** Log into a server with 2+ libraries.
      *Expected:* after login (before the main shell), a simple list with one row per library.

- [ ] **LP-2 — Single-library server skips the picker.** Log into a server with exactly one
      library.
      *Expected:* no picker — straight to the main shell.

- [ ] **LP-3 — Row content.** Inspect the picker rows.
      *Expected:* each row has an icon reflecting the library type (book vs. headphones for
      podcasts), the library name, and the item count as its subtitle.

- [ ] **LP-4 — Selecting a library proceeds.** Tap a library row.
      *Expected:* the main shell opens with that library as the active one (Library browse
      §5 shows its contents).

---

## 5. Library browse (LB) — 🚧 not yet built

- [ ] **LB-1 — Header layout.** Open the Library tab.
      *Expected:* header bar with a **persistent, always-visible search field** (not hidden
      behind a search icon), a filter/sort menu button, and a trailing view-options
      (three-line "adjustments") icon button.

- [ ] **LB-2 — Grid of covers.** With content, look at the library.
      *Expected:* a grid of cover art that reflows — the number of columns per row changes as
      width changes (see §13 AL-2), not a fixed column count.

- [ ] **LB-3 — Live search.** Type a few letters of a title into the search field.
      *Expected:* the grid/list narrows live to matching items; clearing the field restores
      everything.

- [ ] **LB-4 — Category chips.** Look below the header.
      *Expected:* a horizontally scrollable row of chips: All / Author / Series / Genre. Tapping
      one filters the view; "All" restores it. The chip row scrolls horizontally without
      scrolling the page.

- [ ] **LB-5 — Grid/list toggle.** Tap the view-options button and switch between grid and list
      presentation.
      *Expected:* grid shows covers; list shows `AdwActionRow`s (cover thumbnail, title,
      subtitle) — useful for podcast-style episode feeds. Both show the same items.

- [ ] **LB-6 — View options sheet opens.** Tap the view-options (adjustments) button.
      *Expected:* a bottom sheet slides up with a grip handle (no title), containing: "Downloaded
      only" switch, "Hide finished" switch, "Grouping" combo row, "Sort by" combo row, and an
      "Application settings" row with a chevron.

- [ ] **LB-7 — Downloaded only filter.** In the sheet, turn "Downloaded only" on.
      *Expected:* the grid filters to items downloaded fully or partially. With nothing
      downloaded: a status page "No downloaded items" instead of an empty grid.

- [ ] **LB-8 — Hide finished.** With a fully-listened item in the library, turn "Hide finished"
      on.
      *Expected:* fully-listened items disappear from the grid; turning it off brings them back.

- [ ] **LB-9 — Grouping.** Set Grouping to "By Series", then "By Author", then "None".
      *Expected:* with grouping active, the list sections under sticky headers named for the
      series/author; "None" removes the sectioning. The headers stay visible while scrolling
      their section.

- [ ] **LB-10 — Sorting.** Set Sort by to each of: "Date of creation", "Title", "Author",
      "Duration".
      *Expected:* item order visibly re-sorts accordingly each time.

- [ ] **LB-11 — Application settings shortcut.** Tap the "Application settings" row in the
      sheet.
      *Expected:* the sheet closes and the app lands on the Settings tab (§15).

- [ ] **LB-12 — Sync now.** Library's ⋯ header menu → "Sync now".
      *Expected:* immediate re-sync of this library's contents and playback progress; transient
      toast on completion or failure.

- [ ] **LB-13 — Download badge on covers.** With items downloaded (§14), look at the grid.
      *Expected:* downloaded items carry a small download badge on their cover.

---

## 6. Item detail (ID) — 🚧 not yet built

Currently, tapping a cover on Home starts playback directly (HT-3); when this page lands, it
replaces that flow.

- [ ] **ID-1 — Reaching the page.** Tap a cover card on Home or in Library browse.
      *Expected:* a detail page pushes with a back button: large cover, title, author/narrator,
      duration, and a progress bar if partially listened.

- [ ] **ID-2 — Back navigation.** Tap the back button.
      *Expected:* returns to exactly where you were (same scroll position in the library).

- [ ] **ID-3 — Play vs. Resume.** Compare an unstarted book with a partially-listened one.
      *Expected:* the unstarted book's primary button reads "Play"; the partially-listened one
      reads "Resume" and tapping it continues from the listen position.

- [ ] **ID-4 — Description truncation.** Scroll to a book with a long description.
      *Expected:* truncated with a "more" affordance that expands the full text.

- [ ] **ID-5 — Chapter list.** Open a multi-chapter book's detail page.
      *Expected:* chapter rows showing chapter title + duration; the currently-playing chapter
      shows a "currently playing" bars icon; tapping any row seeks playback to that chapter.

- [ ] **ID-6 — Download sheet opens.** Tap the download button.
      *Expected:* a bottom sheet titled "Download book" with five rows: "Current chapter",
      "Next chapters" (with a − / count / + stepper), "Remaining chapters", "Entire book", and a
      visually separated destructive "Clear downloaded chapters" row.

- [ ] **ID-7 — Size estimates.** Look at each download scope row.
      *Expected:* every scope row shows an estimated size as its subtitle (e.g. "≈340 MB")
      computed from the chapter file sizes in the item's metadata.

- [ ] **ID-8 — Stepper behavior.** Tap the "Next chapters" stepper's − and + buttons.
      *Expected:* count changes by 1 per tap, **default 10**, clamped to the number of chapters
      actually remaining after the current one; both stepper buttons are ≥ 44×44 px and tappable
      independently of the row behind them.

- [ ] **ID-9 — Row activation vs. stepper.** Tap the row body (not the stepper).
      *Expected:* the download starts for the stepper's count. Tapping the other scope rows
      (no stepper) starts their download immediately.

- [ ] **ID-10 — Not enough free space.** With free disk space smaller than a scope's estimate
      (fill the device or use a huge book), open the sheet.
      *Expected:* that row's subtitle switches to a red "Not enough free space" and the download
      cannot be started from that row — instead of failing partway through.

- [ ] **ID-11 — Button state during download.** Start any download and watch the item's
      download button.
      *Expected:* idle download icon → spinner/progress ring while in progress → checkmark once
      at least the current chapter is fully downloaded.

- [ ] **ID-12 — Cancel a download.** While downloading, tap the in-progress button and cancel.
      *Expected:* the transfer aborts; whatever chapters already completed remain valid offline
      content (their rows still show the downloaded glyph).

- [ ] **ID-13 — Downloads are resumable.** Start "Entire book" on a large book, cancel or
      interrupt it (kill app / drop network), then re-tap the same scope.
      *Expected:* the download continues from the last completed chapter — already-downloaded
      chapters are not re-fetched (observable as a much faster second pass).

- [ ] **ID-14 — Clear downloaded chapters.** Tap "Clear downloaded chapters".
      *Expected:* destructive styling; all locally downloaded content for this item is removed;
      the download button returns to idle and chapter rows lose their downloaded glyphs.

- [ ] **ID-15 — Offline-availability marker.** With some chapters downloaded, inspect the
      chapter list.
      *Expected:* each downloaded chapter row shows a small filled checkmark-in-circle glyph
      trailing the duration; not-downloaded rows show **no** glyph; the "Chapters" section
      header carries a one-line legend ("● downloaded"); the active chapter additionally shows
      the playing-bars icon.

- [ ] **ID-16 — Insufficient-space row does nothing harmful.** Tap the red "Not enough free
      space" row (from ID-10).
      *Expected:* no download starts, no partial state, no crash.

---

## 7. Mini player (MP) — ✅ implemented

- [ ] **MP-1 — Appears only when playing.** Start playback from Home.
      *Expected:* the mini bar appears above the tab bar with cover thumbnail, title, author,
      play/pause button, and a thin progress line.

- [ ] **MP-2 — Visible on every tab.** While playing, cycle through Home / Library / Downloads /
      Settings.
      *Expected:* the mini bar stays put on all of them.

- [ ] **MP-3 — Play/pause from the mini bar.** Tap the mini bar's play/pause button twice.
      *Expected:* audio pauses then resumes; the icon alternates pause/play accordingly.

- [ ] **MP-4 — Progress line moves.** Let playback run.
      *Expected:* the thin line at the bar's bottom edge advances continuously.

- [ ] **MP-5 — Tap opens the full player.** Tap the mini bar itself (not the play button).
      *Expected:* the full Now Playing screen replaces the window content.

- [ ] **MP-6 — 🚧 Swipe-up gesture.** Once the gesture lands (currently tap-only): swipe up on
      the mini bar — carefully, so it doesn't collide with phosh's own bottom-edge gestures.
      *Expected:* the full player opens, same as tapping. On real Librem 5 hardware, confirm the
      hit-zone doesn't fight the shell's overview/app-switcher swipe; tap-to-expand must keep
      working regardless.

- [ ] **MP-7 — State survives open/collapse.** Open the full player, collapse back with the
      down-chevron, open it again.
      *Expected:* position, speed setting, and sleep-timer state are exactly as before — nothing
      resets, playback never stops.

- [ ] **MP-8 — Long titles ellipsize.** Play the long-title book and look at the mini bar.
      *Expected:* title/author are single-line with "…" — the bar never grows, wraps, or pushes
      the play button off screen.

---

## 8. Full player (FP) — ✅ implemented

- [ ] **FP-1 — Layout.** Open the full player.
      *Expected:* "Now Playing" header, down-chevron on the left, ⋯ menu on the right, large
      cover, title, author, scrubber with elapsed (left) and remaining (right, "-0:00" style)
      times, transport row (skip-back, big play/pause, skip-forward), secondary row (speed,
      sleep timer, chapters).

- [ ] **FP-2 — Play/pause.** Tap the big button repeatedly.
      *Expected:* audio toggles; icon alternates between pause and play states.

- [ ] **FP-3 — Skip buttons.** Note the elapsed time, tap skip-back, then skip-forward.
      *Expected:* position jumps by the configured skip interval (per Settings §15) in the right
      directions; audio continues from the new spot; labels update.

- [ ] **FP-4 — Scrubber drag.** Drag the scrubber to the middle of the book, release.
      *Expected:* audio jumps to approximately that book position; elapsed/remaining update;
      playback continues from there without stalling. Dragging while paused also works and
      playback resumes from the target when played.

- [ ] **FP-5 — Scrubber doesn't fight playback.** Don't touch anything for ~30 s.
      *Expected:* the scrubber advances smoothly on its own; it never snaps back or jitters.

- [ ] **FP-6 — Playback speed.** Tap the speed button (reads "1.0×"), pick 2.0×.
      *Expected:* audio audibly speeds up, the button now reads "2.0×". Available presets:
      0.8×, 1.0×, 1.25×, 1.5×, 1.75×, 2.0×, 2.5×, 3.0×.

- [ ] **FP-7 — Speed persists across pause and reopen.** At 2.0×, pause, collapse the player,
      reopen.
      *Expected:* still 2.0×, button label correct.

- [ ] **FP-8 — Sleep timer arms and highlights.** Sleep timer popover → "15 minutes".
      *Expected:* the popover closes and the sleep-timer button gains a visible accent/highlight.
      "Off" removes it.

- [ ] **FP-9 — Sleep timer "End of chapter".** Arm "End of chapter" shortly before the current
      chapter ends.
      *Expected:* playback pauses at (not after) the chapter boundary and the timer disarms
      (highlight gone).

- [ ] **FP-10 — Chapters popover.** Tap the chapters (list) button.
      *Expected:* a scrolling list of chapter titles with durations; the current chapter is
      visibly highlighted; the list reflects position *at the moment you opened it*.

- [ ] **FP-11 — Chapter tap seeks.** Tap a different chapter in the popover.
      *Expected:* popover closes; playback jumps to that chapter's start; reopening the popover
      shows the new chapter highlighted.

- [ ] **FP-12 — Add bookmark.** ⋯ menu → "Add bookmark".
      *Expected:* menu closes and a toast "Bookmark added" appears. (Spec'd bookmark list/UI is
      future work — nothing else is expected yet.)

- [ ] **FP-13 — Mark as finished.** ⋯ menu → "Mark as finished".
      *Expected:* toast "Marked as finished"; the item leaves "Continue Listening" on the next
      Home refresh.

- [ ] **FP-14 — Reset progress.** Note the position, ⋯ menu → "Reset progress".
      *Expected:* toast "Progress reset"; elapsed time drops to (near) zero. No confirmation
      dialog is shown by design — it must still not crash or pause unexpectedly.

- [ ] **FP-15 — Collapse chevron.** Tap the down-chevron.
      *Expected:* returns to whatever tab was open before, mini bar still visible, audio still
      playing.

---

## 9. Multi-track playback (MU) — ✅ implemented

Use the multi-file book (asset #2). Books split across several audio files must play as one
continuous book.

- [ ] **MU-1 — Continuous playback across files.** Start the multi-file book near the end of
      file 1 and let it run into file 2.
      *Expected:* playback continues into the next file automatically, with no manual action and
      at most a brief gap. The mini bar and scrubber never indicate the book has "ended".

- [ ] **MU-2 — Book-level progress across tracks.** Observe the elapsed/remaining labels and the
      mini bar's progress line before and after a track transition.
      *Expected:* elapsed time keeps counting the *book* position monotonically across the
      transition (e.g. 9:52 → 10:03), it never jumps back to 0:00; remaining time keeps
      shrinking; the mini-bar line keeps advancing.

- [ ] **MU-3 — Only the last track ends the book.** Seek into the last file's final seconds and
      let it finish.
      *Expected:* the book finishes (playback stops / marked finished); the item is treated as
      complete, not left at some mid-book position.

- [ ] **MU-4 — Finishing a middle track doesn't finish the book.** Seek near the end of file 1
      (of 3) and let it run out.
      *Expected:* file 2 starts; the book is **not** marked finished and progress is not 100 %.

- [ ] **MU-5 — Skip across a track boundary.** Position ~5 s before the end of file 1 and tap
      skip-forward (10 s).
      *Expected:* playback lands in file 2 at the correct book position (~5 s in), audio plays
      from there — not an error, not a restart of file 1.

- [ ] **MU-6 — Skip-back across a boundary.** Position ~5 s into file 2 and tap skip-back.
      *Expected:* playback lands near the end of file 1 at the correct book position.

- [ ] **MU-7 — Scrub into a later file.** From early in file 1, drag the scrubber to ~60 % of
      the book.
      *Expected:* the app loads the file containing 60 % and plays from the right spot. This may
      take a moment to load (spinner/silence acceptable), but must end up at the correct
      position and keep playing.

- [ ] **MU-8 — Resume mid-book after relaunch.** While mid-file-2, close the app completely.
      Reopen, go to Home → Continue Listening, tap the book.
      *Expected:* playback resumes at the same book position, which means loading the correct
      file (2) at the correct offset — not restarting from file 1.

- [ ] **MU-9 — Speed re-applies at track change.** Set speed to 2.0× and let a track transition
      happen (or skip across one).
      *Expected:* file 2 also plays at 2.0× — track changes must not reset speed to 1.0×.

- [ ] **MU-10 — Sleep timer across a boundary.** Arm "End of chapter" where the chapter boundary
      coincides with a file boundary.
      *Expected:* playback pauses at that boundary and does not roll on into the next file.

- [ ] **MU-11 — Chapter list stays correct.** Open the chapters popover while in file 2 or 3.
      *Expected:* all book chapters are listed, the current one highlighted, and tapping any
      chapter — including in other files — lands and plays correctly (see also FP-11).

- [ ] **MU-12 — Progress syncs to the server across tracks.** Play across a boundary, then check
      the item's progress on the server web UI (or another client).
      *Expected:* the server shows the book-level position, not a stale or per-file value.

---

## 10. Progressive enhancement / state consistency (PC) — ✅ implemented

- [ ] **PC-1 — Position survives tab switching + waiting.** Play something, switch tabs, wait a
      minute, return to the full player.
      *Expected:* scrubber and labels show the advanced position immediately — the full player
      never shows a stale position after reopening.

- [ ] **PC-2 — Rapid play/pause tapping.** Tap play/pause 8–10 times quickly.
      *Expected:* the app stays responsive; final audio state matches the final icon state.

- [ ] **PC-3 — Start playback from Home, immediately open the player.** Tap a cover and
      immediately tap the mini bar.
      *Expected:* the full player shows the right book; no crash even though the first track may
      still be loading.

- [ ] **PC-4 — Switch books mid-playback.** While book A plays, go Home and tap book B.
      *Expected:* book B replaces A cleanly (mini bar updates, position starts at B's resume
      point); no doubled audio, no crash.

- [ ] **PC-5 — Stop at the very start / very end of a book.** Tap skip-back repeatedly at 0:00,
      and skip-forward repeatedly near the end.
      *Expected:* position clamps at 0 and at the book's end; no negative times, no NaN labels,
      no crash.

---

## 11. System media integration (SI) — ✅ implemented

Needs a real GNOME/phosh session.

- [ ] **SI-1 — Shell media card appears.** While playing, open GNOME Shell's quick settings (or
      phosh's lock screen / media widget).
      *Expected:* a media card for "Audiobookshelf" showing title, artist/author, cover art and
      a live scrub position.

- [ ] **SI-2 — Shell controls work.** From the shell's media card: pause, play, and drag its
      seek bar.
      *Expected:* the app's audio follows; the mini bar and full player stay in sync with the
      shell's state.

- [ ] **SI-3 — Shell next/previous = skip.** Use next/previous on the shell's media card.
      *Expected:* the position skips forward/back by the configured skip interval — it does
      **not** jump to another book.

- [ ] **SI-4 — MPRIS metadata updates.** Switch books; check the shell's media card.
      *Expected:* title/author/cover update to the new book immediately, without reopening
      anything.

- [ ] **SI-5 — No duplicate notification.** Check the notification tray while playing.
      *Expected:* the shell media widget is the only playback surface — the app must **not**
      additionally post its own playback notification.

---

## 12. Hardware controls & interruptions (HW) — ✅ implemented (needs a phone)

- [ ] **HW-1 — Volume keys.** Press volume up/down during playback.
      *Expected:* they change the **system volume** only — they are never repurposed as
      skip/seek controls.

- [ ] **HW-2 — Call interruption pauses.** While playing, place/receive a phone call.
      *Expected:* playback pauses as the call goes active.

- [ ] **HW-3 — No auto-resume after a call.** Hang up the call and wait.
      *Expected:* playback stays paused. Resuming is manual (mini bar, player, or shell card).

- [ ] **HW-4 — Call watcher is optional.** (If testable) run the app where there is no modem /
      ModemManager (e.g. desktop).
      *Expected:* playback works normally; the app logs a warning at most — the missing watcher
      never blocks or crashes anything.

- [ ] **HW-5 — Unplugging wired headphones pauses.** While playing through wired headphones,
      unplug them.
      *Expected:* playback pauses immediately — audio never continues through the phone's
      speaker.

- [ ] **HW-6 — Bluetooth loss pauses.** While playing through Bluetooth headphones, turn the
      headphones off / walk out of range.
      *Expected:* playback pauses.

- [ ] **HW-7 — Replug stays paused by default.** With Settings → Playback → "Resume when
      headphones reconnect" off (the default), replug the headphones after HW-5.
      *Expected:* playback stays paused; resuming is manual.

- [ ] **HW-8 — Resume on reconnect (opt-in).** Turn "Resume when headphones reconnect" on,
      play, unplug, replug.
      *Expected:* playback resumes on replug. Then pause manually, replug again — playback must
      **not** resume (a replug only ever undoes an unplug pause, never a manual pause or a
      phone call). The switch is also insensitive while "Pause when headphones disconnect" is
      off.

- [ ] **HW-9 — Unplug behavior is optional.** Turn "Pause when headphones disconnect" off, play,
      unplug.
      *Expected:* playback continues (now through the phone's speaker) — unplugging changes
      nothing.

- [ ] **HW-10 — Headphone watcher is optional.** Run the app where there is no audio server
      reachable via the PulseAudio socket (e.g. a bare sandbox).
      *Expected:* playback works normally; the app logs a warning at most — the missing watcher
      never blocks or crashes anything.

---

## 13. Adaptive / responsive layout (AL) — 🚧 partially implemented

Only the phone-width single-pane layout exists today; the ≥600 px sidebar is spec'd but not
built (the tests below define the target behavior).

- [ ] **AL-1 — Phone width stays single-pane.** Below 600 px width, look at any screen.
      *Expected:* single-pane navigation with the bottom tab bar; the player takes over the full
      screen.

- [ ] **AL-2 — Grid reflow.** In Library browse (once built), slowly drag-resize the window
      across ~300–900 px.
      *Expected:* the cover grid adds/removes columns smoothly to fill the width — never a fixed
      column count, never clipped covers.

- [ ] **AL-3 — Wide-width split view.** 🚧 At ≥ 600 px (desktop window or rotated device).
      *Expected (spec):* the four destinations become a persistent sidebar list with content in
      the second pane; item detail pushes inside the content pane, not full-screen; the mini bar
      sits above the whole split.

- [ ] **AL-4 — Width, not orientation, decides.** Rotate a phone to landscape (width > 600 px)
      and back to portrait.
      *Expected:* layout follows the window width exactly — landscape gets the sidebar
      (once AL-3 lands), portrait stays single-pane; no separate orientation logic, no glitches
      on rotation.

- [ ] **AL-5 — Touch targets.** On every screen, tap each interactive element near its edge —
      especially paired/adjacent controls (a row's body vs. its trailing ⋯ menu button; the
      download sheet's stepper).
      *Expected:* every element responds within its own ≥ 44×44 px hit area; no mis-taps
      registering on a neighbor.

---

## 14. Downloads screen (DS) — 🚧 not yet built

- [ ] **DS-1 — Summary row.** Open the Downloads tab.
      *Expected:* a grouped list whose first row shows storage used by the app and the device's
      free space.

- [ ] **DS-2 — One row per downloaded item.** Download a couple of books (via §6's sheet).
      *Expected:* an `AdwActionRow` per downloaded item (cover, title), each with a remove
      button.

- [ ] **DS-3 — Remove.** Tap an item's remove button.
      *Expected:* the item's downloaded files are deleted and its row disappears; on the server
      the item itself is untouched (it just loses its offline copy — check its detail page shows
      no downloaded glyphs).

- [ ] **DS-4 — Mid-download row.** While a large download runs, look at the Downloads tab.
      *Expected:* that item's row shows a progress indicator in place of the remove button, plus
      a separate, well-spaced (≥ 44×44 px) stop action; the gray subtitle below the title shows
      live progress — "3/10 chapters · 18.2 MB · 2.1 MB/s" (chapter fraction, bytes, smoothed
      speed) — while the spinner stays as the at-a-glance cue.

- [ ] **DS-5 — Stop keeps completed chapters.** Press Stop on an in-progress download (Downloads
      tab row, or the "Stop download" row in the player's download dropdown while it's running).
      *Expected:* the transfer stops but completed chapters remain valid offline content and the
      row settles into their "N chapters, size" summary — never shown as a failure —
      matching Item detail's resumable behavior.

- [ ] **DS-6 — Empty state.** Remove every download (or fresh install).
      *Expected:* a status page ("No downloads yet"-style), not a blank screen.

- [ ] **DS-7 — Offline playback end-to-end.** Download a book, turn on airplane mode, then play
      it from Continue Listening / Library.
      *Expected:* it plays fully with no network; browsing a downloaded library works offline;
      covers and metadata render from cache. Un-downloaded/streamed items, by contrast, must
      fail gracefully (stall or clear error), never crash.

- [ ] **DS-8 — Progress syncs when back online.** While offline, listen to a downloaded book;
      then restore the network and give the app a moment (or trigger Sync now).
      *Expected:* the position you reached offline appears on the server.

---

## 15. Settings (SE) — 🚧 not yet built

- [ ] **SE-1 — Groups exist.** Open the Settings tab.
      *Expected:* `AdwPreferencesPage`-style groups: Account, Servers, Playback, Appearance,
      About.

- [ ] **SE-2 — Account group.** Inspect it.
      *Expected:* one row for the active server/account (username, server host, "active"
      subtitle) with a chevron, plus a "Switch or manage servers" row opening the Servers list.
      There is deliberately **no** top-level "Sign Out" button.

- [ ] **SE-3 — Servers group.** With 2+ configured servers, inspect each row.
      *Expected:* host as title, logged-in username and an "active" marker as subtitle; a
      trailing ⋯ menu button per row, clearly separated from the row's own tap target.

- [ ] **SE-4 — Server row body → Connection.** Tap a server row's body (not its ⋯).
      *Expected:* pushes that server's Connection page (§16).

- [ ] **SE-5 — Switch to this server.** Use a non-active server's ⋯ → "Switch to this server".
      *Expected:* it becomes the active account (its "active" marker moves); Home/Library
      re-render with that server's data; the mini bar's playback session ends or continues
      cleanly without crashing.

- [ ] **SE-6 — Sign Out (per server).** Use ⋯ → "Sign Out" on a server.
      *Expected:* that account's session is removed; if it was active, the app returns to the
      Welcome screen; other servers remain configured.

- [ ] **SE-7 — Remove Server.** Use ⋯ → "Remove Server" (destructive styling).
      *Expected:* the server and its account are removed from the list; if it was the last one,
      Welcome appears.

- [ ] **SE-8 — Add Server.** Tap the "Add Server" row at the end of the Servers group.
      *Expected:* opens the add-server flow (Welcome-style form); a successful login adds and
      activates the new server.

- [ ] **SE-9 — Playback defaults take effect.** Set default speed and skip intervals in
      Settings → Playback, then start a book.
      *Expected:* playback starts at the configured default speed; the player's skip buttons
      jump by the configured intervals (check FP-3 uses these values).

- [ ] **SE-10 — Sleep-timer default.** Set a sleep-timer default in Settings, then start a book
      and open the sleep-timer popover.
      *Expected:* the default is what the app presents/applies as configured (per spec's
      "sleep-timer default" playback setting).

- [ ] **SE-11 — Appearance.** Cycle Appearance through Follow system / Light / Dark.
      *Expected:* the whole app (all tabs, player, sheets, popovers) switches theme immediately;
      "Follow system" tracks the OS setting.

- [ ] **SE-12 — About.** Tap the About row.
      *Expected:* an about dialog with app name, version, website, and license. The version
      matches the app's `--version` output exactly.

---

## 16. Connection page (CN) — 🚧 not yet built

Reached from Settings → a server row's body. One instance per configured server.

- [ ] **CN-1 — Server connection row.** Open a server's Connection page.
      *Expected:* a "Server connection" group with one row whose subtitle is the server's full
      URL in a monospace font, plus a trailing info button whose popover/tooltip explains what
      this connection is used for.

- [ ] **CN-2 — Advanced group present.** Look below.
      *Expected:* rows for Custom Headers, Disable SSL verification (switch), Client
      certificate, Local network server address, and Change User Agent — all chevron rows except
      the switch.

- [ ] **CN-3 — SSL verification off by default.** Check the switch.
      *Expected:* off; toggling it on persists per-server and takes effect without reinstalling.

- [ ] **CN-4 — Self-signed cert flow.** Point a server at a self-signed host. With verification
      on, connect; then enable "Disable SSL verification" and connect again.
      *Expected:* fails with the TLS error message while verification is on ("Can't verify this
      server's certificate…" — see WT-8's family); succeeds once verification is disabled.

- [ ] **CN-5 — Custom headers.** Add a request header (e.g. `X-Auth: secret`) via the Custom
      Headers page, with a logging proxy in front of the server.
      *Expected:* the header appears on every request the app sends to that server; removing it
      stops it being sent.

- [ ] **CN-6 — User-Agent override.** Set a custom User-Agent and check the proxy/server logs.
      *Expected:* requests carry the overridden value; clearing it restores the default.

- [ ] **CN-7 — Client certificate (mTLS).** Import a client certificate for a server that
      requires one.
      *Expected:* connection succeeds only with the cert; the page shows the imported cert's
      identity.

- [ ] **CN-8 — Local network address.** Set a LAN address (e.g. `http://192.168.1.50:13378`) and
      join the server's home Wi-Fi.
      *Expected:* the app talks to the LAN address (visible in server logs as the LAN IP),
      avoiding the public route; off that network it falls back to the public URL automatically.

- [ ] **CN-9 — Disconnect from the Server.** Tap the destructive "Disconnect from the Server"
      action at the bottom.
      *Expected:* flat/link destructive styling (not a filled button); disconnecting removes the
      session and returns appropriately (Settings/Welcome) without crashing; other servers are
      untouched.

---

## 17. Adverse conditions (AC) — ✅ testable today

- [ ] **AC-1 — Network drops mid-stream.** While streaming a book, disable Wi-Fi/data.
      *Expected:* the app must not crash, hang, or show garbage. Playback may stall/retry;
      controls stay responsive; the last known position is kept.
- [ ] **AC-2 — Network returns.** Re-enable the network after AC-1 and interact with the app
      (switch tabs to force re-sync).
      *Expected:* the app recovers: Home re-syncs, the banner (if shown) clears, and starting
      playback works again without restarting the app.
- [ ] **AC-3 — Server dies mid-session.** Stop the Audiobookshelf server process while the app
      is playing from it.
      *Expected:* as AC-1 — graceful stall or stop, no crash, controls responsive.
- [ ] **AC-4 — Kill the app during playback.** Force-close the app mid-book, relaunch.
      *Expected:* app opens to the main shell (or login if no server); Home's Continue Listening
      shows the last synced position; tapping the book resumes near where you were (within the
      last-synced tolerance).
- [ ] **AC-5 — Screen lock / blank during playback.** Let the screen blank or lock the device
      while playing.
      *Expected:* audio keeps playing (it's an audiobook app); on unlock, the UI shows the
      advanced position.
- [ ] **AC-6 — Rotation / window resize during playback.** Rotate the device (or resize the
      desktop window) while the full player is open, across the 600 px line and back.
      *Expected:* layout reflows without losing state; playback continues. (Spec'd wide-mode
      sidebar is not built yet — see §13 — so single-pane layout at every width is currently
      correct, as long as nothing overlaps or crashes.)
- [ ] **AC-7 — Very narrow window.** Squeeze the window to ~360 px or the device's minimum.
      *Expected:* everything stays usable: labels ellipsize, buttons remain tappable, nothing
      overlaps or clips.
- [ ] **AC-8 — Popover dismissal.** Open speed, sleep-timer, chapters and ⋯ popovers; tap
      outside each one.
      *Expected:* each popover closes; nothing lingers over the transport controls.
- [ ] **AC-9 — Rapid navigation during sync.** On a slow network, launch the app and
      immediately tap through all tabs while Home is still syncing.
      *Expected:* no crash, no duplicated shelves; when sync finishes, Home renders correctly
      whenever you look at it next.
- [ ] **AC-10 — Login with weird URLs.** Try `https://server` (no path), a URL with a trailing
      slash, a URL with spaces, and a completely invalid string.
      *Expected:* no crash anywhere; a clear failure banner for the invalid ones; the valid
      variants either work or fail with the connectivity message — never a silent freeze.
- [ ] **AC-11 — Unicode / odd metadata.** Play a book whose title/author contain emoji, CJK
      characters, or very long words.
      *Expected:* labels wrap or ellipsize cleanly in the mini bar and full player; the avatar
      initial still renders.
- [ ] **AC-12 — Storage pressure.** Fill the device until free space is below the size of the
      next chapter being downloaded (see ID-10).
      *Expected:* the UI flags it before starting ("Not enough free space"); a download that
      *does* run out of space mid-way stops cleanly, keeps completed chapters, and surfaces an
      error — never corrupts existing downloads or crashes.
- [ ] **AC-13 — Offline start of a fully-downloaded book.** With the device offline (or the
      server unreachable), tap a fully-downloaded book to start playback.
      *Expected:* playback starts normally from the local files — book duration and position
      come from locally cached metadata, and no request reaches the server.
- [ ] **AC-14 — Offline start of a partially-downloaded book.** With the device offline, tap a
      book with only some chapters downloaded, positioned inside (or before) a downloaded one.
      *Expected:* playback starts from the downloaded content and stops cleanly when it crosses
      into a missing chapter (treated like a pause, not an error); a book with no cached
      metadata at all (never synced on this device) simply doesn't start.

---

## Regression quick-pass (15 minutes)

When touching the player or sync code, at minimum re-run: WT-6, NT-2, HT-3, MP-1–MP-5, FP-2,
FP-4, FP-6, FP-11, MU-1, MU-5, MU-8, PC-4, AC-1.
