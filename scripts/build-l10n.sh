#!/bin/sh
# Compiles the translations for packaging.
#
#   scripts/build-l10n.sh OUTDIR
#
# Produces, under OUTDIR:
#   locale/<lang>/LC_MESSAGES/abs-app.mo     for every language in po/LINGUAS
#   io.github.gdr_aislop.abs-app.desktop     the .desktop file with translated Name/Comment/…
#   io.github.gdr_aislop.abs-app.metainfo.xml  the metainfo with translated summary/description
# With an empty po/LINGUAS the last two are plain copies of the sources, so packaging can always
# install from OUTDIR. Needs GNU gettext (msgfmt).
#
# For development, run the app against the result:  ABS_LOCALEDIR=OUTDIR/locale LANGUAGE=de abs-app
# (see docs/i18n.md).
set -eu
cd "$(dirname "$0")/.."

OUT="${1:?usage: scripts/build-l10n.sh OUTDIR}"
APP_ID="io.github.gdr_aislop.abs-app"

rm -rf "$OUT"
mkdir -p "$OUT/locale"

languages="$(grep -v '^[[:space:]]*#' po/LINGUAS | grep -v '^[[:space:]]*$' || true)"

for lang in $languages; do
    mkdir -p "$OUT/locale/$lang/LC_MESSAGES"
    # -c: check the header and that plural forms and {placeholders}-bearing strings are well formed
    # (`--check-format` also catches a translation that dropped a %-style directive).
    msgfmt -c --check-format -o "$OUT/locale/$lang/LC_MESSAGES/abs-app.mo" "po/$lang.po"
done

if [ -n "$languages" ]; then
    msgfmt --desktop -d po --template "app/assets/$APP_ID.desktop" -o "$OUT/$APP_ID.desktop"
    GETTEXTDATADIRS="$PWD/po/gettext-data" msgfmt --xml -d po --template "app/assets/$APP_ID.metainfo.xml" -o "$OUT/$APP_ID.metainfo.xml"
else
    cp "app/assets/$APP_ID.desktop" "$OUT/$APP_ID.desktop"
    cp "app/assets/$APP_ID.metainfo.xml" "$OUT/$APP_ID.metainfo.xml"
fi

echo "translations built in $OUT ($(echo "$languages" | wc -w) language(s))"
