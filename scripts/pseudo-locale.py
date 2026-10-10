#!/usr/bin/env python3
"""Builds a throwaway "pseudo" translation of po/audiobooklet.pot, to find UI text that was never
wrapped in a translation call.

    scripts/pseudo-locale.py OUTDIR            # needs msgfmt (GNU gettext)
    ABS_LOCALEDIR=OUTDIR LC_ALL=en_US.UTF-8 LANGUAGE=en audiobooklet

gettext ignores catalogs under the plain "C"/"C.UTF-8" locales, so the app must run under a real
locale; if `locale -a` has no en_US.UTF-8, generate one without root:
    localedef -i en_US -f UTF-8 ~/locales/en_US.UTF-8   # then add LOCPATH=~/locales to the line above

Every translated string reads "[!! original text !!]" (placeholders such as {name} are kept). Any
user-visible *English* text without the brackets was missed by the string conversion — or is
user data (a book title, a server name), which is expected to stay as it is.
Plural forms use the English rule; the catalog is installed as language "en".
"""
import os
import re
import subprocess
import sys

POT = os.path.join(os.path.dirname(__file__), "..", "po", "audiobooklet.pot")


def parse(text):
    """Yields dicts {ctx, id, plural} for each non-header entry of a .pot."""
    entry, field = {}, None
    for raw in text.splitlines() + [""]:
        line = raw.strip()
        if not line:
            if entry.get("id"):
                yield entry
            entry, field = {}, None
            continue
        if line.startswith("#"):
            continue
        m = re.match(r'(msgctxt|msgid_plural|msgid|msgstr(?:\[\d+\])?)\s+"(.*)"$', line)
        if m:
            key = {"msgctxt": "ctx", "msgid": "id", "msgid_plural": "plural"}.get(m.group(1))
            field = key
            if key:
                entry[key] = m.group(2)
            continue
        m = re.match(r'"(.*)"$', line)
        if m and field:
            entry[field] += m.group(1)


def pseudo(s):
    return "[!! " + s + " !!]"


def main():
    out_dir = sys.argv[1]
    with open(POT, encoding="utf-8") as f:
        entries = list(parse(f.read()))
    po = [
        'msgid ""',
        'msgstr ""',
        '"Content-Type: text/plain; charset=UTF-8\\n"',
        '"Plural-Forms: nplurals=2; plural=(n != 1);\\n"',
        "",
    ]
    for e in entries:
        if "ctx" in e:
            po.append(f'msgctxt "{e["ctx"]}"')
        po.append(f'msgid "{e["id"]}"')
        if "plural" in e:
            po.append(f'msgid_plural "{e["plural"]}"')
            po.append(f'msgstr[0] "{pseudo(e["id"])}"')
            po.append(f'msgstr[1] "{pseudo(e["plural"])}"')
        else:
            po.append(f'msgstr "{pseudo(e["id"])}"')
        po.append("")
    lc = os.path.join(out_dir, "en", "LC_MESSAGES")
    os.makedirs(lc, exist_ok=True)
    po_path = os.path.join(lc, "audiobooklet.po")
    with open(po_path, "w", encoding="utf-8") as f:
        f.write("\n".join(po))
    subprocess.run(["msgfmt", "-o", os.path.join(lc, "audiobooklet.mo"), po_path], check=True)
    os.remove(po_path)
    print(f"pseudo-locale with {len(entries)} messages in {out_dir}")


if __name__ == "__main__":
    main()
