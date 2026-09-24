//! Converts a (sanitized) HTML fragment into Pango markup, for rendering server-supplied item
//! descriptions in a plain `GtkLabel` via `set_markup` — GTK has no HTML-rendering widget at this
//! crate's libadwaita ceiling, but Pango's own small markup language covers everything
//! Audiobookshelf's server-side sanitizer allows through: `p, ol, ul, li, a, strong, em, del, br,
//! b, i`. Anything outside that allowlist is dropped (its inner text is kept, just unwrapped);
//! entities already present in the source (`&amp;`, `&#39;`, …) are passed through rather than
//! double-escaped, and any other `&`/`<`/`>` in plain text is escaped so the result is always
//! valid markup even if the source is malformed (unclosed tags are auto-closed at the end, same
//! as a browser would).

/// Converts `html` to Pango markup. Never fails — malformed input just renders as best-effort
/// text; the caller (`screens::item_detail`) treats an empty result as "nothing to show".
pub fn html_to_pango(html: &str) -> String {
    let mut out = String::new();
    let mut stack: Vec<StackTag> = Vec::new();
    let mut has_content = false;
    let mut chars = html.chars();
    let mut pending: Option<char> = None;

    while let Some(c) = pending.take().or_else(|| chars.next()) {
        if c == '<' {
            let mut tag_src = String::new();
            let mut closed = false;
            for c2 in chars.by_ref() {
                if c2 == '>' {
                    closed = true;
                    break;
                }
                tag_src.push(c2);
            }
            if closed {
                handle_tag(&tag_src, &mut out, &mut stack, &mut has_content);
            }
            // An unterminated `<...` with no closing `>` at all is dropped silently — sanitized
            // server HTML shouldn't produce this, and there's nothing sensible to render for it.
        } else {
            let mut text = String::new();
            text.push(c);
            for c2 in chars.by_ref() {
                if c2 == '<' {
                    pending = Some(c2);
                    break;
                }
                text.push(c2);
            }
            if !text.is_empty() {
                escape_text_into(&text, &mut out);
                has_content = true;
            }
        }
    }

    // Auto-close whatever's left open, innermost first — same as a browser's error recovery for
    // the truncated-HTML case (e.g. a description that got cut off mid-tag somewhere upstream).
    while let Some(tag) = stack.pop() {
        close_tag(&tag, &mut out);
    }

    out
}

enum StackTag {
    /// A paragraph boundary — never itself emits markup; only its *opening* triggers the blank
    /// line separating it from whatever came before.
    Paragraph,
    /// `ul`/`ol` — a pure grouping construct, Pango has no list markup so this only exists to
    /// make its `li` children's close tags find the right container to stop at.
    Container,
    List,
    Link,
    /// An inline tag mapped to its Pango equivalent (`b`, `i`, `s`) — several source tag names
    /// can map to the same one (`strong`/`b`, `em`/`i`), so this stores the *mapped* name; a
    /// close tag matches by re-mapping its own name and comparing.
    Inline(&'static str),
}

impl StackTag {
    fn matches(&self, name: &str) -> bool {
        match self {
            StackTag::Paragraph => name == "p",
            StackTag::Container => matches!(name, "ul" | "ol"),
            StackTag::List => name == "li",
            StackTag::Link => name == "a",
            StackTag::Inline(mapped) => map_inline_tag(name) == Some(*mapped),
        }
    }
}

fn map_inline_tag(name: &str) -> Option<&'static str> {
    match name {
        "strong" | "b" => Some("b"),
        "em" | "i" => Some("i"),
        "del" => Some("s"),
        _ => None,
    }
}

fn close_tag(tag: &StackTag, out: &mut String) {
    match tag {
        StackTag::Paragraph | StackTag::Container | StackTag::List => {}
        StackTag::Link => out.push_str("</a>"),
        StackTag::Inline(mapped) => {
            out.push_str("</");
            out.push_str(mapped);
            out.push('>');
        }
    }
}

fn handle_tag(tag_src: &str, out: &mut String, stack: &mut Vec<StackTag>, has_content: &mut bool) {
    let trimmed = tag_src.trim();
    if trimmed.is_empty() || trimmed.starts_with('!') || trimmed.starts_with('?') {
        return;
    }
    let is_close = trimmed.starts_with('/');
    let body = if is_close { trimmed[1..].trim() } else { trimmed };
    let body = body.trim_end_matches('/').trim();
    let mut parts = body.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_lowercase();
    let attrs = parts.next().unwrap_or("");

    if is_close {
        if let Some(pos) = stack.iter().rposition(|t| t.matches(&name)) {
            while stack.len() > pos {
                let t = stack.pop().expect("just checked len() > pos, so pop() cannot be empty");
                close_tag(&t, out);
            }
        }
        return;
    }

    match name.as_str() {
        "p" => {
            if *has_content {
                out.push_str("\n\n");
            }
            stack.push(StackTag::Paragraph);
        }
        "br" => {
            out.push('\n');
            *has_content = true;
        }
        "ul" | "ol" => stack.push(StackTag::Container),
        "li" => {
            if *has_content {
                out.push('\n');
            }
            out.push_str("\u{2022} ");
            *has_content = true;
            stack.push(StackTag::List);
        }
        "a" => {
            let href = extract_attr(attrs, "href").unwrap_or_default();
            out.push_str("<a href=\"");
            escape_attr_into(&href, out);
            out.push_str("\">");
            stack.push(StackTag::Link);
        }
        "strong" | "b" => {
            out.push_str("<b>");
            *has_content = true;
            stack.push(StackTag::Inline("b"));
        }
        "em" | "i" => {
            out.push_str("<i>");
            *has_content = true;
            stack.push(StackTag::Inline("i"));
        }
        "del" => {
            out.push_str("<s>");
            *has_content = true;
            stack.push(StackTag::Inline("s"));
        }
        // Anything outside the server's own sanitizer allowlist: drop the tag, keep processing
        // its children as if it were never there.
        _ => {}
    }
}

/// `attr="value"` or `attr='value'` — audiobookshelf's sanitized `<a>` tags only ever carry
/// `href` (and occasionally `target`, which this ignores; Pango markup has no notion of it).
fn extract_attr(attrs: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=");
    let idx = attrs.find(&needle)?;
    let after = &attrs[idx + needle.len()..];
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let after = &after[1..];
    let end = after.find(quote)?;
    Some(after[..end].to_string())
}

/// Escapes a plain-text run for inclusion in Pango markup, without double-escaping entities the
/// source already carries (`&amp;`, `&#39;`, …) — those are valid as-is and re-escaping the `&`
/// would turn `&amp;` into the literal text `&amp;amp;`.
fn escape_text_into(text: &str, out: &mut String) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '&' => {
                let rest: String = chars.clone().collect();
                if let Some(len) = known_entity_len(&rest) {
                    out.push('&');
                    for _ in 0..len {
                        out.push(chars.next().expect("known_entity_len only returns a length within `rest`"));
                    }
                } else {
                    out.push_str("&amp;");
                }
            }
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// Like [`escape_text_into`], but for an attribute value (`href`) rather than text content —
/// always fully escapes, including `"`, since the caller always wraps the result in double
/// quotes regardless of which quote character the source used.
fn escape_attr_into(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

/// Given the text immediately after an `&`, the length (in chars, not counting the `&` itself)
/// of a recognized entity if `rest` starts with one — `amp;`, `lt;`, `gt;`, `quot;`, `apos;`, or
/// a numeric reference (`#39;`, `#x27;`).
fn known_entity_len(rest: &str) -> Option<usize> {
    for name in ["amp;", "lt;", "gt;", "quot;", "apos;"] {
        if rest.starts_with(name) {
            return Some(name.len());
        }
    }
    if let Some(digits_part) = rest.strip_prefix('#') {
        let (is_hex, digits_part) = match digits_part.strip_prefix('x').or_else(|| digits_part.strip_prefix('X')) {
            Some(rest) => (true, rest),
            None => (false, digits_part),
        };
        let digit_count = digits_part.chars().take_while(|c| if is_hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() }).count();
        if digit_count > 0 && digits_part.chars().nth(digit_count) == Some(';') {
            return Some(1 + usize::from(is_hex) + digit_count + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(html_to_pango("just some text"), "just some text");
    }

    #[test]
    fn paragraphs_become_blank_line_separated() {
        assert_eq!(html_to_pango("<p>First</p><p>Second</p>"), "First\n\nSecond");
    }

    #[test]
    fn br_becomes_a_newline() {
        assert_eq!(html_to_pango("one<br>two"), "one\ntwo");
    }

    #[test]
    fn inline_tags_map_to_their_pango_equivalents() {
        assert_eq!(html_to_pango("<strong>bold</strong> and <em>italic</em> and <del>gone</del>"), "<b>bold</b> and <i>italic</i> and <s>gone</s>");
    }

    #[test]
    fn unclosed_tags_are_auto_closed_at_the_end() {
        assert_eq!(html_to_pango("<b>bold <i>and italic"), "<b>bold <i>and italic</i></b>");
    }

    #[test]
    fn existing_entities_are_preserved_not_double_escaped() {
        assert_eq!(html_to_pango("Ben &amp; Jerry&#39;s"), "Ben &amp; Jerry&#39;s");
    }

    #[test]
    fn a_stray_ampersand_is_escaped() {
        assert_eq!(html_to_pango("Ben & Jerry's"), "Ben &amp; Jerry's");
    }

    #[test]
    fn links_carry_their_href_and_escape_it() {
        assert_eq!(html_to_pango(r#"<a href="https://a.example?x=1&y=2">click</a>"#), "<a href=\"https://a.example?x=1&amp;y=2\">click</a>");
    }

    #[test]
    fn list_items_get_a_bullet_and_a_line_each() {
        assert_eq!(html_to_pango("<ul><li>one</li><li>two</li></ul>"), "\u{2022} one\n\u{2022} two");
    }

    #[test]
    fn unrecognized_tags_are_dropped_but_their_text_is_kept() {
        assert_eq!(html_to_pango("<script>evil</script>plain <unknown>text</unknown>"), "evilplain text");
    }
}
