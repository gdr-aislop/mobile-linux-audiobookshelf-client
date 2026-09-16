//! Resolves Item Detail's four download-scope options (see `docs/design/ui-spec.md`) into a
//! concrete list of chapter indices to fetch. Pure logic, deliberately kept free of any I/O —
//! `abs-storage`'s `downloads` repo does the actual file/row bookkeeping once a caller has this
//! list.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadScope {
    CurrentChapter,
    /// The "Next chapters" stepper — how many upcoming chapters to fetch, always >= 1.
    NextChapters(u32),
    RemainingChapters,
    EntireBook,
}

/// `current_chapter_index` and `total_chapters` are both required context: "current" and
/// "remaining" are meaningless without knowing where playback is, and "entire book" needs the
/// total to enumerate. Chapters already downloaded are not filtered out here — that's a
/// presentation concern (skip already-downloaded chapters when actually fetching), not part of
/// resolving what the scope *means*.
pub fn resolve_scope(
    scope: DownloadScope,
    current_chapter_index: usize,
    total_chapters: usize,
) -> Vec<usize> {
    if total_chapters == 0 {
        return Vec::new();
    }
    let last_index = total_chapters - 1;
    let current = current_chapter_index.min(last_index);

    match scope {
        DownloadScope::CurrentChapter => vec![current],
        DownloadScope::NextChapters(n) => {
            let start = current + 1;
            if start > last_index {
                return Vec::new(); // already at (or past) the last chapter
            }
            let end = (start + n as usize - 1).min(last_index);
            (start..=end).collect()
        }
        DownloadScope::RemainingChapters => (current..=last_index).collect(),
        DownloadScope::EntireBook => (0..=last_index).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_chapter_returns_just_that_index() {
        assert_eq!(resolve_scope(DownloadScope::CurrentChapter, 5, 32), vec![5]);
    }

    #[test]
    fn current_chapter_at_the_last_index_is_still_just_that_one() {
        assert_eq!(resolve_scope(DownloadScope::CurrentChapter, 31, 32), vec![31]);
    }

    #[test]
    fn next_chapters_returns_the_requested_count_after_current() {
        assert_eq!(
            resolve_scope(DownloadScope::NextChapters(3), 5, 32),
            vec![6, 7, 8]
        );
    }

    #[test]
    fn next_chapters_matches_the_lissen_style_default_of_ten() {
        assert_eq!(
            resolve_scope(DownloadScope::NextChapters(10), 13, 32),
            (14..=23).collect::<Vec<_>>()
        );
    }

    #[test]
    fn next_chapters_clamps_to_the_end_of_the_book() {
        assert_eq!(
            resolve_scope(DownloadScope::NextChapters(10), 28, 32),
            vec![29, 30, 31]
        );
    }

    #[test]
    fn next_chapters_at_the_last_chapter_is_empty() {
        assert_eq!(resolve_scope(DownloadScope::NextChapters(10), 31, 32), Vec::<usize>::new());
    }

    #[test]
    fn next_chapters_of_one_is_the_single_next_chapter() {
        assert_eq!(resolve_scope(DownloadScope::NextChapters(1), 5, 32), vec![6]);
    }

    #[test]
    fn remaining_chapters_includes_current_through_the_end() {
        assert_eq!(
            resolve_scope(DownloadScope::RemainingChapters, 29, 32),
            vec![29, 30, 31]
        );
    }

    #[test]
    fn remaining_chapters_at_the_start_is_the_whole_book() {
        assert_eq!(
            resolve_scope(DownloadScope::RemainingChapters, 0, 3),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn remaining_chapters_at_the_last_chapter_is_just_that_one() {
        assert_eq!(resolve_scope(DownloadScope::RemainingChapters, 31, 32), vec![31]);
    }

    #[test]
    fn entire_book_ignores_current_position() {
        let expected: Vec<usize> = (0..32).collect();
        assert_eq!(resolve_scope(DownloadScope::EntireBook, 0, 32), expected);
        assert_eq!(resolve_scope(DownloadScope::EntireBook, 31, 32), expected);
    }

    #[test]
    fn single_chapter_book_behaves_consistently_across_scopes() {
        assert_eq!(resolve_scope(DownloadScope::CurrentChapter, 0, 1), vec![0]);
        assert_eq!(resolve_scope(DownloadScope::RemainingChapters, 0, 1), vec![0]);
        assert_eq!(resolve_scope(DownloadScope::EntireBook, 0, 1), vec![0]);
        assert_eq!(resolve_scope(DownloadScope::NextChapters(5), 0, 1), Vec::<usize>::new());
    }

    #[test]
    fn zero_chapters_returns_empty_for_every_scope() {
        assert_eq!(resolve_scope(DownloadScope::CurrentChapter, 0, 0), Vec::<usize>::new());
        assert_eq!(resolve_scope(DownloadScope::NextChapters(5), 0, 0), Vec::<usize>::new());
        assert_eq!(resolve_scope(DownloadScope::RemainingChapters, 0, 0), Vec::<usize>::new());
        assert_eq!(resolve_scope(DownloadScope::EntireBook, 0, 0), Vec::<usize>::new());
    }

    #[test]
    fn current_index_past_the_end_is_clamped_rather_than_panicking() {
        // Defensive: a caller passing a stale/out-of-range position should not panic.
        assert_eq!(resolve_scope(DownloadScope::CurrentChapter, 999, 32), vec![31]);
        assert_eq!(
            resolve_scope(DownloadScope::RemainingChapters, 999, 32),
            vec![31]
        );
    }
}
