#!/bin/sh
# Regenerates po/audiobooklet.pot from the sources, then (unless --pot-only) merges the new template
# into every po/<lang>.po listed in po/LINGUAS so translators see new/changed strings.
#
#   scripts/update-pot.sh [--pot-only]
#
# Needs GNU gettext (xgettext, msgmerge, msgcat). Sources scanned:
#   * app/src/**/*.rs   — strings passed to i18n::tr / tr_args / ntr / ntr_args / tr_noop / tr_ctx
#   * app/assets/*.desktop   — Name, GenericName, Comment, Keywords
#   * app/assets/*.metainfo.xml — name, summary, description (rules in po/metainfo.its)
# gettext 0.21 has no Rust parser, so each .rs file is first rewritten into something xgettext's C
# lexer reads correctly (scripts/rs-for-xgettext.py: lifetimes, char/raw strings, `\u{…}` escapes,
# string continuations). Msgids must be plain "…" literals (see the rules in app/src/i18n.rs).
# `tr_ctx(context, msgid)` is mapped with the `1c,2` keyword spec.
set -eu
cd "$(dirname "$0")/.."

APP_ID="io.github.gdr_aislop.audiobooklet"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/version *= *"([^"]+)"/\1/')"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Sorted for a stable template; the paths have no spaces. The converted copies live under
# $TMP/src with the same relative paths, and xgettext runs from there, so the "#:" location
# comments in the template read app/src/….
RS_FILES="$(find app/src -name '*.rs' ! -name i18n.rs ! -name test_support.rs | LC_ALL=C sort)"
for f in $RS_FILES; do
    mkdir -p "$TMP/src/$(dirname "$f")"
    python3 scripts/rs-for-xgettext.py "$f" "$TMP/src/$f"
done

# shellcheck disable=SC2086 # RS_FILES is a deliberate word-split list
(cd "$TMP/src" && xgettext --language=C --from-code=UTF-8 \
    --keyword= \
    --keyword=tr:1 --keyword=tr_args:1 --keyword=tr_noop:1 \
    --keyword=ntr:1,2 --keyword=ntr_args:1,2 \
    --keyword=tr_ctx:1c,2 \
    --add-comments=TRANSLATORS \
    --add-location=file \
    --sort-by-file \
    --package-name=audiobooklet --package-version="$VERSION" \
    --msgid-bugs-address="https://github.com/gdr-aislop/audiobooklet/issues" \
    --copyright-holder="the audiobooklet authors" \
    -o "$TMP/rs.pot" $RS_FILES)

xgettext --language=Desktop --from-code=UTF-8 --add-location=file \
    -o "$TMP/desktop.pot" "app/assets/$APP_ID.desktop"
GETTEXTDATADIRS="$PWD/po/gettext-data" xgettext --add-location=file \
    -o "$TMP/metainfo.pot" "app/assets/$APP_ID.metainfo.xml"

# `--use-first` keeps the first occurrence's header (the Rust one, with the project metadata);
# files appear in the order given, so the template reads: UI strings, then desktop, then metainfo.
msgcat --use-first --add-location=file -o po/audiobooklet.pot "$TMP/rs.pot" "$TMP/desktop.pot" "$TMP/metainfo.pot"

# xgettext writes the creation date; drop it so an unchanged tree gives an unchanged template.
sed -i '/^"POT-Creation-Date:/d' po/audiobooklet.pot

entries="$(grep -c '^msgid ' po/audiobooklet.pot || true)"
echo "po/audiobooklet.pot: $((entries - 1)) messages"

[ "${1:-}" = "--pot-only" ] && exit 0

grep -v '^[[:space:]]*#' po/LINGUAS | grep -v '^[[:space:]]*$' | while read -r lang; do
    if [ -f "po/$lang.po" ]; then
        msgmerge --update --backup=none --add-location=file "po/$lang.po" po/audiobooklet.pot 2>&1 | sed "s|^|po/$lang.po: |"
    else
        echo "po/$lang.po is listed in po/LINGUAS but missing" >&2
        exit 1
    fi
done
