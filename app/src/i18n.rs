//! Translation support — thin helpers over gettext so every user-visible string in the app goes
//! through one of four functions, and `scripts/update-pot.sh` can extract them with `xgettext`.
//!
//! * [`tr`] — a plain string: `tr("Settings")`.
//! * [`tr_args`] — a string with named placeholders: `tr_args("Sign in to {server}", &[("server", name)])`.
//!   Placeholders are *named* (never `{}`) so a translation can reorder them freely.
//! * [`ntr`] / [`ntr_args`] — plural forms: `ntr_args("{count} chapter", "{count} chapters", n, &[])`.
//!   `{count}` is filled in automatically with `n`; extra named placeholders go in `args`. The
//!   *language's* plural rules decide which form is used (so don't special-case `n == 1` yourself).
//! * [`tr_noop`] — marks a literal for extraction *without* translating it yet, for `&'static str`
//!   tables (`const`s, `match` arms returning text) that are translated where they are shown:
//!   `let (headline, _) = (tr_noop("Connection lost"), …);` … `label.set_label(&tr(headline))`.
//! * [`tr_ctx`] — a string that needs disambiguating context: `tr_ctx("Playback speed", "Speed")`.
//!   The context goes to translators; it is not shown.
//!
//! **Rules for msgids** (the text passed to these functions): write each one as a plain `"…"`
//! literal directly inside the call (no `concat!`, no raw strings, and not through a variable or a
//! function parameter — `xgettext` only sees literals), and put translator notes in a
//! `// TRANSLATORS:` comment on the line(s) directly above the call. `scripts/update-pot.sh`
//! rewrites each source file into something `xgettext` can read (`scripts/rs-for-xgettext.py`),
//! which also understands Rust's `\`-newline string continuation. Strings that are *not* for
//! translation (log lines, CSS classes, icon names, action names, setting keys) don't go through
//! these functions at all.
//!
//! The text domain is bound once at startup by [`init`]. Until then (and in unit tests, which never
//! call it) gettext returns the msgid unchanged, so the English source text *is* the fallback.

use gettextrs::LocaleCategory;

/// gettext domain — the `.mo` files are `<localedir>/<lang>/LC_MESSAGES/abs-app.mo`.
pub const DOMAIN: &str = "abs-app";

/// Binds the text domain and selects the user's locale. Call once, first thing in `main`, before
/// GTK is initialised (GTK reads the same locale when it starts). Never fails: a missing locale
/// directory or `.mo` file just leaves the app in English.
pub fn init() {
    // `setlocale(LC_ALL, "")` honours LC_ALL / LC_MESSAGES / LANG / LANGUAGE. A failure here
    // (e.g. a locale the system hasn't generated) leaves the "C" locale, i.e. English.
    gettextrs::setlocale(LocaleCategory::LcAll, "");
    let dir = locale_dir();
    match gettextrs::bindtextdomain(DOMAIN, &dir) {
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(%err, dir = %dir.display(), "couldn't bind the translations directory; the UI stays in English");
            return;
        }
    }
    if let Err(err) = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8") {
        tracing::warn!(%err, "couldn't set the translations' codeset to UTF-8");
    }
    if let Err(err) = gettextrs::textdomain(DOMAIN) {
        tracing::warn!(%err, "couldn't select the translations' text domain; the UI stays in English");
    }
    tracing::info!(dir = %dir.display(), "translations bound");
}

/// Where the compiled `.mo` files live, first match wins:
/// 1. `$ABS_LOCALEDIR` — for development (`scripts/build-mo.sh` fills `target/locale`) and tests;
/// 2. `<exe dir>/../share/locale` — a relocatable layout (the AppImage's `usr/bin` → `usr/share`,
///    Flatpak's `/app/bin` → `/app/share`);
/// 3. `/usr/share/locale` — a system-wide install (the `.deb`).
fn locale_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("ABS_LOCALEDIR").filter(|d| !d.is_empty()) {
        return dir.into();
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|bin| bin.join("../share/locale")))
        .filter(|dir| dir.is_dir())
    {
        return dir;
    }
    "/usr/share/locale".into()
}

/// Translates `msgid`.
pub fn tr(msgid: &str) -> String {
    let text = gettextrs::gettext(msgid);
    #[cfg(test)]
    audit::record(&text);
    text
}

/// Marks `msgid` for extraction and returns it unchanged; the caller passes it through [`tr`] at
/// the point it is shown. Only for text that has to live in a `&'static str`.
pub const fn tr_noop(msgid: &'static str) -> &'static str {
    msgid
}

/// Translates `msgid` as a string of the given disambiguating `context`.
pub fn tr_ctx(context: &str, msgid: &str) -> String {
    let text = gettextrs::pgettext(context, msgid);
    #[cfg(test)]
    audit::record(&text);
    text
}

/// Translates `msgid`, then substitutes each `{name}` with its value from `args`.
pub fn tr_args(msgid: &str, args: &[(&str, &str)]) -> String {
    let text = substitute(&tr(msgid), args);
    #[cfg(test)]
    audit::record(&text);
    text
}

/// Plural-aware translation: picks the form `n` calls for in the user's language. `{count}` in the
/// result is replaced with `n`.
pub fn ntr(singular: &str, plural: &str, n: u32) -> String {
    ntr_args(singular, plural, n, &[])
}

/// [`ntr`] plus extra named placeholders.
pub fn ntr_args(singular: &str, plural: &str, n: u32, args: &[(&str, &str)]) -> String {
    let count = n.to_string();
    let mut all = Vec::with_capacity(args.len() + 1);
    all.push(("count", count.as_str()));
    all.extend_from_slice(args);
    let text = substitute(&gettextrs::ngettext(singular, plural, n), &all);
    #[cfg(test)]
    audit::record(&text);
    text
}

/// Replaces every `{name}` in `text` with the matching value, in one left-to-right pass — so a
/// value that itself contains `{…}` (a book titled "{count}") is never expanded a second time. A
/// placeholder with no value is left as written, so a bad translation shows `{typo}` rather than
/// panicking.
fn substitute(text: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        let hit = tail
            .find('}')
            .and_then(|close| args.iter().find(|(name, _)| *name == &tail[1..close]).map(|(_, v)| (close, *v)));
        match hit {
            Some((close, value)) => {
                out.push_str(value);
                rest = &tail[close + 1..];
            }
            None => {
                out.push('{');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Test-only audit for finding UI text that never went through `tr*()` — see "Finding missed
/// strings" in `docs/i18n.md`. Every string the helpers return is remembered; while a UI scenario
/// runs, [`audit::start`] samples all on-screen label/button/tooltip text and, at [`audit::finish`],
/// appends the texts that were *not* produced by a helper to the file named by
/// `ABS_TEST_I18N_AUDIT`. What is left is user data (titles, server names) or a missed string.
#[cfg(test)]
pub(crate) mod audit {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use gtk4::prelude::*;

    static PSEUDO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static TRANSLATED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    static ON_SCREEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

    pub(super) fn record(text: &str) {
        TRANSLATED.lock().unwrap().get_or_insert_with(HashSet::new).insert(text.to_string());
        // Pseudo-locale mode: every helper result must come out bracketed `[!! … !!]`; one that
        // doesn't was not found in the catalog, i.e. the extracted msgid differs from the literal
        // the program looks up (or the .pot is stale).
        if PSEUDO.load(std::sync::atomic::Ordering::Relaxed) && !text.contains("[!!") {
            append_line(&format!("MISS\t{}", text.replace('\n', "\\n")));
        }
    }

    fn append_line(line: &str) {
        use std::io::Write;
        if let Some(path) = std::env::var_os("ABS_TEST_I18N_AUDIT") {
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn collect(widget: &gtk4::Widget, into: &mut HashSet<String>) {
        let mut add = |text: String| {
            if !text.trim().is_empty() {
                into.insert(text);
            }
        };
        if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
            add(label.text().to_string());
        }
        if let Some(button) = widget.downcast_ref::<gtk4::Button>() {
            if let Some(text) = button.label() {
                add(text.to_string());
            }
        }
        if let Some(entry) = widget.downcast_ref::<gtk4::Entry>() {
            if let Some(text) = entry.placeholder_text() {
                add(text.to_string());
            }
        }
        if let Some(text) = widget.tooltip_text() {
            add(text.to_string());
        }
        if let Some(window) = widget.downcast_ref::<gtk4::Window>() {
            if let Some(title) = window.title() {
                add(title.to_string());
            }
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            collect(&c, into);
            child = c.next_sibling();
        }
    }

    fn sample() {
        let mut found = HashSet::new();
        for window in gtk4::Window::list_toplevels() {
            collect(&window, &mut found);
        }
        ON_SCREEN.lock().unwrap().get_or_insert_with(HashSet::new).extend(found);
    }

    /// Starts sampling the open windows every 20 ms when `ABS_TEST_I18N_AUDIT` is set. With
    /// `ABS_TEST_I18N_PSEUDO` also set, the scenario runs under the catalog in `ABS_LOCALEDIR`
    /// (see `scripts/pseudo-locale.py`) and `MISS` lines report helper results absent from it.
    pub(crate) fn start() {
        if std::env::var_os("ABS_TEST_I18N_AUDIT").is_some() {
            if std::env::var_os("ABS_TEST_I18N_PSEUDO").is_some() {
                super::init();
                PSEUDO.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            gtk4::glib::timeout_add_local(std::time::Duration::from_millis(20), || {
                sample();
                gtk4::glib::ControlFlow::Continue
            });
        }
    }

    /// Appends this scenario's untranslated on-screen texts (one per line) to the audit file.
    pub(crate) fn finish(scenario: &str) {
        let Some(path) = std::env::var_os("ABS_TEST_I18N_AUDIT") else { return };
        sample();
        let translated = TRANSLATED.lock().unwrap().clone().unwrap_or_default();
        let on_screen = ON_SCREEN.lock().unwrap().clone().unwrap_or_default();
        let mut lines: Vec<_> = on_screen.difference(&translated).map(|t| format!("{}\t{scenario}", t.replace('\n', "\\n"))).collect();
        lines.sort();
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", lines.join("\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untranslated_text_passes_through() {
        assert_eq!(tr("Settings"), "Settings");
        assert_eq!(tr_ctx("menu", "Settings"), "Settings");
    }

    #[test]
    fn named_placeholders_are_filled_in_any_order() {
        assert_eq!(
            tr_args("{b} follows {a}", &[("a", "one"), ("b", "two")]),
            "two follows one"
        );
    }

    #[test]
    fn unknown_placeholders_are_left_alone() {
        assert_eq!(tr_args("Hello {who}", &[("other", "x")]), "Hello {who}");
    }

    #[test]
    fn values_containing_braces_are_not_reinterpreted() {
        assert_eq!(
            tr_args("{a} and {b}", &[("a", "{b}"), ("b", "B")]),
            "{b} and B",
            "a substituted value is never expanded again"
        );
    }

    #[test]
    fn plurals_use_the_english_rule_without_a_catalog() {
        assert_eq!(ntr("{count} chapter", "{count} chapters", 1), "1 chapter");
        assert_eq!(ntr("{count} chapter", "{count} chapters", 0), "0 chapters");
        assert_eq!(ntr("{count} chapter", "{count} chapters", 2), "2 chapters");
        assert_eq!(
            ntr_args("{count} book in {series}", "{count} books in {series}", 3, &[("series", "Dune")]),
            "3 books in Dune"
        );
    }
}
