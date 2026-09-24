//! Minimal HTML → Pango-markup conversion for Audiobookshelf book descriptions.
//!
//! Audiobookshelf stores descriptions as (sanitized) HTML — its server allowlist is exactly
//! `p, ol, ul, li, a, strong, em, del, br, b, i` (`server/utils/htmlSanitizer.js`, PR #3880),
//! but descriptions synced from older servers or audio-file metadata can still carry stray
//! tags and entities. This module hand-rolls a tiny tolerant scanner (no HTML engine): the
//! ABS-allowed tags are translated to their Pango-markup equivalents, everything else is
//! discarded while keeping its text, and all text is entity-decoded and re-escaped so the
//! result is always valid Pango markup — a hostile description can at worst render as plain
//! text, never break the label or inject markup.

/// Converts a book description's HTML to Pango markup for a `gtk4::Label::set_markup` call.
///
/// Tag mapping (tags are rebuilt from scratch, so attributes never survive): `b`/`strong`
/// → `<b>`, `i`/`em` → `<i>`, `del` → `<s>`, `br` → newline, `p` → paragraph breaks,
/// `ul`/`ol`/`li` → bullet/numbered lines. Links render as plain text (per client UX
/// decision). Unknown tags (`h1`, `span`, `font`, …) are stripped but their text is kept;
/// `script`/`style` content is dropped entirely. Whitespace runs collapse per HTML rules,
/// runs of blank lines collapse to one empty line, and the result is trimmed. Returns an
/// empty string for input that renders to nothing (callers should treat that as "no
/// description").
pub fn html_to_pango(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    // Pango tags currently open, innermost last — closed in reverse at EOF or on a close tag
    // (tolerating mismatched nesting like `<b>x<i>y</b>`). Each entry also remembers where in
    // `out` its opening markup starts, so a stray close can roll the tag back.
    let mut open_tags: Vec<(&'static str, usize)> = Vec::new();
    // Open `<ul>`/`<ol>` stack for list markers; `true` = ordered.
    let mut list_stack: Vec<bool> = Vec::new();

    let mut rest = html;
    while !rest.is_empty() {
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(end) => {
                    rest = &rest[end + 3..];
                    // A comment counts as whitespace between the words around it.
                    push_separator(&mut out);
                }
                None => break, // unterminated comment swallows the rest
            }
            continue;
        }
        let Some(lt) = rest.find('<') else {
            push_text(&mut out, rest);
            break;
        };
        if lt > 0 {
            push_text(&mut out, &rest[..lt]);
            rest = &rest[lt..];
            continue; // what follows may be a comment, not a tag — re-check from the top
        }
        // `rest` now starts with '<'. A '>' hidden inside a quoted attribute value must not
        // end the tag, so scan quotes.
        let Some(tag_end) = find_tag_end(rest) else {
            // No '>' anywhere — not a tag, render the '<' as literal text.
            push_text(&mut out, rest);
            break;
        };
        let raw_tag = &rest[1..tag_end];
        rest = &rest[tag_end + 1..];

        let (is_close, inner) = match raw_tag.strip_prefix('/') {
            Some(inner) => (true, inner),
            None => (false, raw_tag),
        };
        // Only `<letter…>` opens a tag (HTML: tag names start with an ASCII letter) — `a < b`,
        // `<3`, `<2 and 3>`, `</>` are literal text, re-emitted whole.
        if !inner.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            push_text(&mut out, &format!("<{raw_tag}>"));
            continue;
        }
        let name: String = inner.chars().take_while(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();

        if is_close {
            match name.as_str() {
                // Both open and close of `<p>` break paragraphs: `<p>a</p><p>b` must not
                // double-break, but `a</p>b` (unclosed) still needs the boundary. Runs of
                // newlines collapse in the final pass.
                "p" => push_paragraph_break(&mut out),
                "b" | "strong" => close_pango_tag(&mut out, &mut open_tags, "b"),
                "i" | "em" => close_pango_tag(&mut out, &mut open_tags, "i"),
                "del" => close_pango_tag(&mut out, &mut open_tags, "s"),
                // `</li>` emits nothing: the next marker (or a paragraph break) starts the
                // next line, so unclosed `<li>`s still separate correctly.
                "li" => {}
                "ul" | "ol" => {
                    list_stack.pop();
                }
                // Unknown close tag: dropped, but still separates the words around it.
                _ => push_separator(&mut out),
            }
        } else {
            match name.as_str() {
                "br" => {
                    trim_trailing_spaces(&mut out);
                    if out.ends_with("\n\n") {
                        // `<br>` right after a paragraph break ends the paragraph's last
                        // line — a single line break, not another blank line.
                        out.truncate(out.len() - 1);
                    } else if !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
                "p" => push_paragraph_break(&mut out),
                "ul" => list_stack.push(false),
                "ol" => list_stack.push(true),
                "li" => {
                    if let Some(ordered) = list_stack.last_mut() {
                        trim_trailing_spaces(&mut out);
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                        if *ordered {
                            let n = next_ordered_number(&out);
                            out.push_str(&format!("{n}. "));
                        } else {
                            out.push_str("• ");
                        }
                    }
                }
                "b" | "strong" => {
                    open_tags.push(("b", out.len()));
                    out.push_str("<b>");
                }
                "i" | "em" => {
                    open_tags.push(("i", out.len()));
                    out.push_str("<i>");
                }
                "del" => {
                    open_tags.push(("s", out.len()));
                    out.push_str("<s>");
                }
                "script" | "style" => rest = skip_raw_element(rest, &name),
                // Unknown tag (h1, span, font, a, img, …): dropped, but it still separates
                // the words around it.
                _ => push_separator(&mut out),
            }
        }
    }
    for (tag, _) in open_tags.iter().rev() {
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    }

    // Collapse 3+ newlines to a single paragraph break and trim the edges.
    let mut collapsed = String::with_capacity(out.len());
    let mut newline_run = 0;
    for c in out.chars() {
        if c == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                collapsed.push(c);
            }
        } else {
            newline_run = 0;
            collapsed.push(c);
        }
    }
    // A trailing paragraph break is a blank line carrying no information — drop it — but a
    // trailing `<br>` line break is kept; spaces trim off both edges.
    let trimmed = collapsed.trim_matches([' ', '\t']);
    let trimmed = trimmed.strip_suffix("\n\n").unwrap_or(trimmed);
    trimmed.to_string()
}

/// Finds the `>` ending the tag that starts at `tag_start` (which begins with `<`),
/// skipping over quoted attribute values; `None` when the tag never ends.
fn find_tag_end(tag_start: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in tag_start.char_indices().skip(1) {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == '>' {
            return Some(i);
        }
    }
    None
}

/// Skips a raw-text element's contents (`<script>`/`<style>` — never rendered), returning
/// the input from after the element's closing tag. Unterminated input drops everything.
fn skip_raw_element<'a>(rest: &'a str, name: &str) -> &'a str {
    let mut search = rest;
    while let Some(lt) = search.find('<') {
        let after = &search[lt + 1..];
        let after_bytes = after.as_bytes();
        let close = format!("/{name}");
        if after_bytes.len() > close.len()
            && after_bytes[..close.len()].eq_ignore_ascii_case(close.as_bytes())
            && matches!(after_bytes[close.len()], b'>' | b' ' | b'/')
        {
            return match find_tag_end(after) {
                Some(end) => &after[end + 1..],
                None => "",
            };
        }
        search = after;
    }
    ""
}

/// Closes `tag`, along with any tags opened inside it that were never closed (mismatched
/// nesting like `<b>x<i>y</b>` — innermost first so the markup stays balanced). A close with
/// no matching open means the nesting is broken beyond recovery: every pending open tag is
/// rolled back (its opening markup removed, the text it wrapped kept) so no unbalanced or
/// knowingly-mismatched markup is ever emitted.
fn close_pango_tag(out: &mut String, open_tags: &mut Vec<(&'static str, usize)>, tag: &'static str) {
    if let Some(pos) = open_tags.iter().rposition(|(t, _)| *t == tag) {
        for (t, _) in open_tags.drain(pos..).rev() {
            out.push_str("</");
            out.push_str(t);
            out.push('>');
        }
    } else {
        // Highest offset first, so the stored positions of the remaining tags stay valid.
        while let Some((t, pos)) = open_tags.pop() {
            out.replace_range(pos..pos + t.len() + 2, "");
        }
    }
}

fn push_paragraph_break(out: &mut String) {
    trim_trailing_spaces(out);
    if out.is_empty() || out.ends_with("\n\n") {
        return; // nothing to break, or the previous break already made the blank line
    }
    out.push_str("\n\n");
}

/// The number for the next `<ol>` marker: the count of `N. ` markers already on the current
/// line-separated block of output. (Ordered lists in descriptions are flat and short; this
/// avoids threading counters through nested-list bookkeeping.)
fn next_ordered_number(out: &str) -> u32 {
    let line = out.lines().last().unwrap_or(out);
    match line.split_once(". ").and_then(|(prefix, _)| prefix.trim().parse::<u32>().ok()) {
        Some(n) => n + 1,
        None => 1,
    }
}

fn trim_trailing_spaces(out: &mut String) {
    let trimmed = out.trim_end_matches([' ', '\t']).len();
    out.truncate(trimmed);
}

/// Appends a text node: HTML whitespace runs collapse to single spaces, entities decode,
/// and `&`/`<`/`>` re-escape so Pango never sees raw markup characters.
fn push_text(out: &mut String, text: &str) {
    let mut collapsed = String::with_capacity(text.len());
    let mut in_ws = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\r' | '\n' | '\u{b}' | '\u{c}') {
            if !in_ws {
                collapsed.push(' ');
            }
            in_ws = true;
        } else {
            collapsed.push(c);
            in_ws = false;
        }
    }
    // A dropped tag or comment already left a separator at the end of `out` — don't double it.
    if out.ends_with([' ', '\t', '\n']) && collapsed.starts_with(' ') {
        collapsed.remove(0);
    }

    let mut rest = collapsed.as_str();
    while let Some(amp) = rest.find('&') {
        escape_and_push(out, &rest[..amp]);
        rest = &rest[amp..];
        match decode_entity(rest) {
            Some((c, consumed)) => {
                escape_char(out, c);
                rest = &rest[consumed..];
            }
            None => {
                // No decodable entity: keep the `&…` run literally rather than splitting it
                // mid-name. A known name that's only missing its `;` (`&amp`) normalizes to
                // the entity's source form (`&amp;`); anything else is escaped as-is.
                let mut end = 1;
                let bytes = rest.as_bytes();
                while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'#') {
                    end += 1;
                }
                let had_semicolon = end < bytes.len() && bytes[end] == b';';
                if had_semicolon {
                    end += 1;
                }
                let run = &rest[..end];
                if !had_semicolon && is_known_entity(run) {
                    escape_and_push(out, &format!("{run};"));
                } else {
                    escape_and_push(out, run);
                }
                rest = &rest[end..];
            }
        }
    }
    escape_and_push(out, rest);
}

/// A dropped tag or comment still separates the words around it: emit one space, unless the
/// output already ends in whitespace (runs collapse; the final pass trims the edges).
fn push_separator(out: &mut String) {
    if !out.is_empty() && !out.ends_with([' ', '\t', '\n']) {
        out.push(' ');
    }
}

/// `rest` starts with `&`. Returns the decoded character and total bytes consumed
/// (including the `&` and `;`), or `None` for unknown/malformed entities — those render
/// literally, same as Audiobookshelf's own decoder.
fn decode_entity(rest: &str) -> Option<(char, usize)> {
    let rest = rest.strip_prefix('&')?;
    if let Some(hex) = rest.strip_prefix("#x").or_else(|| rest.strip_prefix("#X")) {
        let end = hex.find(';')?;
        let cp = u32::from_str_radix(&hex[..end], 16).ok()?;
        // Consumed: `&` + `#x` + digits + `;`. Control characters render literally too —
        // they are invisible in Pango markup, same as invalid code points.
        return char::from_u32(cp).filter(|c| !c.is_control()).map(|c| (c, end + 4));
    }
    if let Some(dec) = rest.strip_prefix('#') {
        let end = dec.find(';')?;
        let cp = dec[..end].parse::<u32>().ok()?;
        return char::from_u32(cp).filter(|c| !c.is_control()).map(|c| (c, end + 3));
    }
    let end = rest.find(';')?;
    let name = &rest[..end];
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => return None,
    };
    Some((c, end + 2))
}

/// Whether `run` (starting with `&`, no trailing `;`) names an entity `decode_entity` knows.
fn is_known_entity(run: &str) -> bool {
    matches!(
        run.strip_prefix('&'),
        Some("amp" | "lt" | "gt" | "quot" | "apos" | "nbsp")
    )
}

fn escape_and_push(out: &mut String, text: &str) {
    for c in text.chars() {
        escape_char(out, c);
    }
}

fn escape_char(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(html_to_pango("Hello world"), "Hello world");
        assert_eq!(html_to_pango(&"A".repeat(400)), "A".repeat(400));
    }

    #[test]
    fn text_is_pango_escaped() {
        assert_eq!(html_to_pango("Tom & Jerry <3"), "Tom &amp; Jerry &lt;3");
    }

    #[test]
    fn allowed_tags_map_to_pango() {
        assert_eq!(html_to_pango("<b>B</b> <strong>S</strong> <i>I</i> <em>E</em> <del>D</del>"), "<b>B</b> <b>S</b> <i>I</i> <i>E</i> <s>D</s>");
        assert_eq!(html_to_pango("a<br>b<br/>c"), "a\nb\nc");
        assert_eq!(html_to_pango("<p>a</p><p>b</p>"), "a\n\nb");
        assert_eq!(html_to_pango("a<p>b"), "a\n\nb");
        assert_eq!(html_to_pango("a</p>b"), "a\n\nb");
    }

    #[test]
    fn lists_get_markers() {
        assert_eq!(html_to_pango("<ul><li>x</li><li>y</li></ul>"), "• x\n• y");
        assert_eq!(html_to_pango("<ol><li>a</li><li>b</li><li>c</li></ol>"), "1. a\n2. b\n3. c");
        // Unclosed `<li>`s still break lines.
        assert_eq!(html_to_pango("<ul><li>a<li>b"), "• a\n• b");
    }

    #[test]
    fn unknown_tags_strip_keeping_text() {
        assert_eq!(html_to_pango("<h1>Head</h1> body"), "Head body");
        assert_eq!(html_to_pango("<font color=\"red\">red</font>"), "red");
        assert_eq!(html_to_pango("<span>a</span>div<div>b</div>"), "a div b");
    }

    #[test]
    fn attributes_never_survive() {
        assert_eq!(html_to_pango("<b style=\"color:red\" onmouseover=\"evil()\">x</b>"), "<b>x</b>");
        assert_eq!(html_to_pango("<p class=\"a\">y</p>"), "y");
        // A '>' inside a quoted attribute value doesn't end the tag.
        assert_eq!(html_to_pango("<b title=\"a>b\">x</b>"), "<b>x</b>");
    }

    #[test]
    fn links_render_as_plain_text() {
        assert_eq!(html_to_pango("<a href=\"https://example.com\">site</a>"), "site");
        assert_eq!(html_to_pango("<a href=\"javascript:alert(1)\">x</a>"), "x");
    }

    #[test]
    fn script_and_style_content_drops() {
        assert_eq!(html_to_pango("<script>alert(1)</script>ok"), "ok");
        assert_eq!(html_to_pango("<script type=\"text/javascript\">alert(1)</script>y"), "y");
        assert_eq!(html_to_pango("<style>p { color: red }</style>ok"), "ok");
        assert_eq!(html_to_pango("before<script>bad"), "before");
        assert_eq!(html_to_pango("<SCRIPT>x</SCRIPT>ok"), "ok");
    }

    #[test]
    fn entities_decode_then_re_escape() {
        assert_eq!(html_to_pango("&amp; &lt; &gt; &quot; &apos;"), "&amp; &lt; &gt; \" '");
        assert_eq!(html_to_pango("a&nbsp;b"), "a\u{a0}b");
        assert_eq!(html_to_pango("&#65;&#x42;&#X48;&#39;"), "ABH'");
        // Unknown entities render literally.
        assert_eq!(html_to_pango("&nosuch;"), "&amp;nosuch;");
        assert_eq!(html_to_pango("&amp"), "&amp;amp;");
        // Invalid numeric code points render literally.
        assert_eq!(html_to_pango("&#0;"), "&amp;#0;");
    }

    #[test]
    fn html_whitespace_collapses() {
        assert_eq!(html_to_pango("line1\n   line2\t\tline3"), "line1 line2 line3");
        assert_eq!(html_to_pango("  padded  "), "padded");
    }

    // --- malformed HTML ---

    #[test]
    fn unclosed_tags_auto_close() {
        assert_eq!(html_to_pango("<b>bold text"), "<b>bold text</b>");
        assert_eq!(html_to_pango("<i>a<b>c"), "<i>a<b>c</b></i>");
        assert_eq!(html_to_pango("<p>never closed"), "never closed");
    }

    #[test]
    fn mismatched_nesting_stays_balanced() {
        assert_eq!(html_to_pango("<b>x<i>y</b>z</i>"), "<b>x<i>y</i></b>z");
        assert_eq!(html_to_pango("<b>x</i>y"), "xy");
    }

    #[test]
    fn stray_angle_brackets_render_literally() {
        assert_eq!(html_to_pango("a < b"), "a &lt; b");
        assert_eq!(html_to_pango("1<2 and 3>2"), "1&lt;2 and 3&gt;2");
        assert_eq!(html_to_pango("trailing <"), "trailing &lt;");
        assert_eq!(html_to_pango("<>"), "&lt;&gt;");
        assert_eq!(html_to_pango("< 3"), "&lt; 3");
    }

    #[test]
    fn unterminated_tag_renders_as_text() {
        assert_eq!(html_to_pango("text <b"), "text &lt;b");
        assert_eq!(html_to_pango("text <b attr=\"unterminated"), "text &lt;b attr=\"unterminated");
    }

    #[test]
    fn unterminated_comment_drops_rest() {
        assert_eq!(html_to_pango("ok<!-- oops"), "ok");
        assert_eq!(html_to_pango("ok<!-- comment -->more"), "ok more");
    }

    #[test]
    fn case_insensitive_tags() {
        assert_eq!(html_to_pango("<BR>"), "\n");
        assert_eq!(html_to_pango("<P>x</P>"), "x");
        assert_eq!(html_to_pango("<B>bold</B>"), "<b>bold</b>");
        assert_eq!(html_to_pango("<Del>s</DEl>"), "<s>s</s>");
    }

    #[test]
    fn renders_to_nothing_for_empty_input() {
        assert_eq!(html_to_pango(""), "");
        assert_eq!(html_to_pango("   "), "");
        assert_eq!(html_to_pango("<script>x</script>"), "");
        assert_eq!(html_to_pango("<p></p>"), "");
    }

    #[test]
    fn blank_line_runs_collapse() {
        assert_eq!(html_to_pango("<p>a</p><p></p><p></p><p>b</p>"), "a\n\nb");
    }

    #[test]
    fn realistic_audible_description() {
        // Shape of a description as synced from an ABS server (sanitized HTML from Audible).
        let html = "<p>In space, <b>no one</b> can hear you scream &mdash; except the crew.</p><p>Rated <i>excellent</i> by &quot;readers&quot;.</p><br>Enjoy!";
        assert_eq!(
            html_to_pango(html),
            "In space, <b>no one</b> can hear you scream &amp;mdash; except the crew.\n\nRated <i>excellent</i> by \"readers\".\nEnjoy!"
        );
    }
}
