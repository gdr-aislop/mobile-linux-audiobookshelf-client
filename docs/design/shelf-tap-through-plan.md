# Shelf tap-through: Home headers → Library sort & filter

> Working plan for the in-flight feature on this branch. **Deleted in the final commit once
> executed** (branch-vs-main diff nets it out — it never lands in the merged tree).

**Status**: saved (step 0). Implementation not started — awaiting go-ahead.

**Branch**: `claude/amazing-knuth-fbmiu5` (base for this work: `6dcd4ae` per-scenario test driver;
`136df83` progress-recency fix). PR: https://github.com/gdr-aislop/mobile-linux-audiobookshelf-client/pull/new/claude/amazing-knuth-fbmiu5

## Agreed behavior

- Tap **"Recently Added"** header on Home → Library tab, sorted *Date added*, no filter.
- Tap **"Continue Listening"** header → Library tab, new **"Last listened"** sort (items with
  progress first, newest last-listen first — `progress.updated_at` now carries true listening
  times; never-played items last, stable sort) **+ transient "In progress only" filter**
  (has progress and not finished).
- Manual access (user chose combined control): the existing sort `MenuButton` popover becomes
  **"Sort & filter"** with two caption-labelled sections — **Sort** (5 buttons incl. new
  "Last listened") and **Filter** ("In progress only" `GtkCheckButton`; GTK 4.8 / libadwaita 1.2
  era widgets only).
- Active-state visibility (Nautilus pattern): while the filter is on, the header button's icon
  swaps to `funnel-symbolic` and its tooltip changes; an in-view banner
  ("Showing books in progress · Show all", mirroring the offline-banner Revealer idiom) is the
  ambient indicator + one-tap clear. Check state, icon, and banner all derive from one
  `Rc<Cell<bool>>` so they can't drift; externally-set state updates the CheckButton with its
  signal blocked (offline-toggle restore pattern).
- Sort & filter are **session-transient** (not persisted; sort already isn't) — navigation-with-
  intent, not a preference.

## Changes

1. `crates/abs-storage/src/repo/progress.rs`: add `list_for_account(pool, account_id)` (no
   `LIMIT`; doc: backs Library's last-listened sort/filter).
2. `app/src/screens/library.rs`:
   - `SortKey` gains `LastListened`; enum becomes `pub(crate)`.
   - `SortButtons.last_listened` button, wired into the existing `(button, key)` handler list.
   - Popover: section labels + "In progress only" CheckButton.
   - `LibraryData.last_listened: HashMap<String, Progress>` filled in the initial load.
   - `LibraryWidgets.in_progress_only: Rc<Cell<bool>>` + banner Revealer + "Show all" clear.
   - `render_from_current_data`: new sort arm (played before unplayed, `Reverse(updated_at)`
     within played, stable) + filter arm (`!in_progress_only || row unfinished`).
   - `pub(crate) fn apply_view(&self, key: SortKey, in_progress_only: bool)`.
   - MenuButton icon/tooltip swap on filter state.
3. `app/src/screens/home.rs`:
   - `pub(crate) enum Shelf { RecentlyAdded, ContinueListening }`.
   - `build()` gains 7th param `on_open_shelf: impl Fn(Shelf) + 'static`.
   - The two shelf headings become flat buttons (same `heading` styling; tooltips
     "Show the Library by date added" / "Show in-progress books in the Library");
     "Your Libraries" stays a label.
   - `TestHooks` gains both heading buttons.
4. `app/src/screens/main_window.rs`:
   - Build library **before** home (independent builds; `stack.append` tab order untouched).
   - `on_open_shelf` closure → `set_visible_child_name("library")` +
     `apply_view(DateAdded, false)` / `apply_view(LastListened, true)`.
5. Tests (register in the `gtk_scenarios!` registry in `main.rs`):
   - `home_shelf_headers_invoke_on_open_shelf` — recording callback asserts both kinds.
   - `library_sorts_by_last_listened` — items + `set_at`-stamped progress (one never-played);
     `flow_box_titles` order assertion.
   - `library_in_progress_filter_is_manually_toggleable` — check toggle → banner + finished
     hidden; untoggle → restored; header-tap path additionally asserts check-state sync.
   - `TestHooks` gains the check button.
6. Docs: `docs/design/ui-spec.md` (Home/Library sections) + `docs/ui-test-plan.md` lines.

## Verification

- `source /tmp/rustenv/env.sh && cargo clippy --all-targets` — only the 3 known pre-existing
  warnings allowed.
- `xvfb-run -a cargo test --workspace` (per-scenario-process driver; single-scenario rerun:
  `cargo test -p abs-app -- --exact tests::<name> --ignored`).
- Never `cargo fmt`; single-line imperative commit messages; push to the branch.

## Accepted gaps

- No main_window wiring test (the closure is 3 lines mirroring the existing
  `open-library-search` wiring; unit scenarios cover the behavior).
- Offline + in-progress filters compose (AND).
- Orphaned progress rows are harmless (map keyed by item id; render walks items).

## Research basis

- HIG header bars: keep header controls few → evolve the existing popover, no new header button.
- HIG popovers: view-option controls grouped by type, section headings when ambiguous.
- Nautilus: sort dropdown reflects the active sort; funnel indicator when a filter is active
  (usability-tested: "change sort order" = best-performing task).
- Audiobookshelf upstream clients: "Progress" filter category with "In Progress"/"Finished"
  vocabulary.
