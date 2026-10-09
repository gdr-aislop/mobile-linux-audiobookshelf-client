#!/usr/bin/env python3
"""Rewrites a Rust source file so GNU xgettext (which has no Rust parser in gettext 0.21) can read it
as C and extract exactly the msgids the program will look up at runtime.

    rs-for-xgettext.py SRC DEST

What it changes (line numbers are preserved, so extracted locations stay right):
  * lifetimes and loop labels ('a, 'static) lose their apostrophe — C would read them as the start
    of a character constant and swallow the rest of the line, including any tr("…") on it;
  * character literals ('x', '"', '\\n') become 0;
  * raw strings (r"…", r#"…"#) become "" — they are never msgids;
  * inside ordinary strings, Rust's `\\<newline><indent>` continuation is applied the way rustc does
    (the indent is dropped), and `\\u{…}` escapes become the character itself;
  * nested block comments are flattened so C sees one comment.
Everything else, in particular `// TRANSLATORS:` comments, passes through untouched.
"""
import sys


def convert(src: str) -> str:
    out = []
    i, n = 0, len(src)
    pending_newlines = 0  # newlines swallowed inside a construct, re-emitted after it closes

    def flush():
        nonlocal pending_newlines
        if pending_newlines:
            out.append("\n" * pending_newlines)
            pending_newlines = 0

    while i < n:
        c = src[i]
        nxt = src[i + 1] if i + 1 < n else ""

        # line comment
        if c == "/" and nxt == "/":
            j = src.find("\n", i)
            j = n if j < 0 else j
            text = src[i:j]
            if text.endswith("\\"):
                text += " "  # C would splice the next line into the comment
            out.append(text)
            i = j
            continue

        # block comment (Rust nests, C does not)
        if c == "/" and nxt == "*":
            depth, j = 1, i + 2
            body = []
            while j < n and depth:
                if src.startswith("/*", j):
                    depth += 1
                    body.append("  ")
                    j += 2
                elif src.startswith("*/", j):
                    depth -= 1
                    if depth:
                        body.append("  ")
                    j += 2
                else:
                    body.append(src[j])
                    j += 1
            out.append("/*" + "".join(body) + "*/")
            i = j
            continue

        # raw string r"…" / r#"…"# / br"…"
        if c == "r" and (nxt == '"' or nxt == "#") and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            j = i + 1
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                end = '"' + "#" * hashes
                k = src.find(end, j + 1)
                k = n if k < 0 else k
                body = src[j + 1 : k]
                out.append('""')
                pending_newlines += body.count("\n")
                i = k + len(end)
                # keep line structure: re-emit swallowed newlines at the next newline
                continue

        # ordinary string
        if c == '"':
            j = i + 1
            buf = ['"']
            while j < n and src[j] != '"':
                ch = src[j]
                if ch == "\\":
                    esc = src[j + 1] if j + 1 < n else ""
                    if esc == "\n":
                        # Rust: backslash-newline skips the newline and all following whitespace
                        j += 2
                        pending_newlines += 1
                        while j < n and src[j] in " \t\r\n":
                            if src[j] == "\n":
                                pending_newlines += 1
                            j += 1
                        continue
                    if esc == "u" and j + 2 < n and src[j + 2] == "{":
                        close = src.find("}", j)
                        buf.append(chr(int(src[j + 3 : close], 16)))
                        j = close + 1
                        continue
                    buf.append(ch + esc)
                    j += 2
                    continue
                if ch == "\n":
                    # a literal newline inside a Rust string is part of the string; C forbids it
                    buf.append("\\n")
                    pending_newlines += 1
                    j += 1
                    continue
                buf.append(ch)
                j += 1
            buf.append('"')
            out.append("".join(buf))
            i = j + 1
            continue

        # character literal vs lifetime/label
        if c == "'":
            if nxt == "\\":
                j = src.find("'", i + 2)
                if j > 0:
                    out.append("0")
                    i = j + 1
                    continue
            elif i + 2 < n and src[i + 2] == "'" and nxt != "\n":
                out.append("0")
                i += 3
                continue
            # lifetime / label: drop the apostrophe, keep the identifier
            i += 1
            continue

        if c == "\n":
            out.append("\n")
            flush()
            i += 1
            continue

        out.append(c)
        i += 1

    flush()
    return "".join(out)


if __name__ == "__main__":
    with open(sys.argv[1], encoding="utf-8") as f:
        text = f.read()
    with open(sys.argv[2], "w", encoding="utf-8") as f:
        f.write(convert(text))
