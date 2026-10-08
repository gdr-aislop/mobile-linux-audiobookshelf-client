//! Scrubs sensitive data (server address, access tokens, usernames, the home directory) out of
//! log output — the "Anonymize sensitive data in logs" setting.
//!
//! This is deliberately a *sink* concern, living in the composition root: every other crate keeps
//! logging with plain `tracing` and knows nothing about redaction. The scrubber sits between
//! `tracing-subscriber`'s formatter and the two output sinks ([`RedactingMakeWriter`]), so it also
//! covers text produced by third-party crates (reqwest/hyper put the full request URL in their
//! error messages) that no call-site discipline could reach.
//!
//! It is GTK-free and database-free on purpose: the persisted setting is read by `main.rs::setup`
//! and pushed in through [`LogRedactor::set_enabled`]. Until that happens the redactor is on, so
//! nothing logged during startup can leak.

use std::borrow::Cow;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use tracing_subscriber::fmt::MakeWriter;

/// Matches shorter than this are not registered as sensitive literals: replacing `al` or `a`
/// everywhere would shred the log without protecting anything.
const MIN_TERM_LEN: usize = 3;

const URL_PLACEHOLDER: &str = "<url>";
const SERVER_PLACEHOLDER: &str = "<server>";
const USER_PLACEHOLDER: &str = "<user>";
const SECRET_PLACEHOLDER: &str = "REDACTED";

/// Query/assignment keys whose value is a credential.
const SECRET_KEYS: [&str; 4] = ["token", "access_token", "refresh_token", "api_key"];

#[derive(Debug, Clone)]
struct Term {
    /// ASCII-lowercased, for case-insensitive matching.
    needle: String,
    replacement: &'static str,
}

struct Shared {
    enabled: AtomicBool,
    terms: RwLock<Vec<Term>>,
}

/// Cheap to clone; all clones share state. See the module docs.
#[derive(Clone)]
pub struct LogRedactor {
    shared: Arc<Shared>,
}

impl Default for LogRedactor {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide redactor. Logging itself is process-global (one `tracing` subscriber), so the
/// switch that controls it is too; this avoids threading a handle through every screen that never
/// otherwise touches logging.
pub fn global() -> &'static LogRedactor {
    static GLOBAL: OnceLock<LogRedactor> = OnceLock::new();
    GLOBAL.get_or_init(LogRedactor::new)
}

impl LogRedactor {
    /// Starts **enabled** with no registered terms (safe default).
    pub fn new() -> Self {
        let redactor = Self { shared: Arc::new(Shared { enabled: AtomicBool::new(true), terms: RwLock::new(Vec::new()) }) };
        if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()) {
            redactor.set_home_dir(&home);
        }
        redactor
    }

    pub fn is_enabled(&self) -> bool {
        self.shared.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::Relaxed);
    }

    fn add_term(&self, literal: &str, replacement: &'static str) {
        let literal = literal.trim();
        if literal.len() < MIN_TERM_LEN {
            return;
        }
        let needle = literal.to_ascii_lowercase();
        let mut terms = self.shared.terms.write().expect("log redactor terms");
        if terms.iter().any(|t| t.needle == needle) {
            return;
        }
        terms.push(Term { needle, replacement });
        // Longest first, so `audio.example.com` is consumed before `example.com` could match
        // inside it.
        terms.sort_by_key(|t| std::cmp::Reverse(t.needle.len()));
    }

    fn set_home_dir(&self, home: &str) {
        self.add_term(home.trim_end_matches('/'), "~");
    }

    /// Registers a server's address: the full URL, its host, and `host:port`.
    pub fn register_server_url(&self, url: &str) {
        let url = url.trim().trim_end_matches('/');
        self.add_term(url, SERVER_PLACEHOLDER);
        let authority = url.split_once("://").map_or(url, |(_, rest)| rest);
        let authority = authority.split(['/', '?', '#']).next().unwrap_or("");
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        self.add_term(host_port, SERVER_PLACEHOLDER);
        self.add_term(strip_port(host_port), SERVER_PLACEHOLDER);
    }

    /// Registers a bare host / `host:port` (e.g. a server's local-network address).
    pub fn register_address(&self, address: &str) {
        let address = address.trim();
        let address = address.split_once("://").map_or(address, |(_, rest)| rest);
        let host_port = address.split(['/', '?', '#']).next().unwrap_or("");
        self.add_term(host_port, SERVER_PLACEHOLDER);
        self.add_term(strip_port(host_port), SERVER_PLACEHOLDER);
    }

    pub fn register_username(&self, username: &str) {
        self.add_term(username, USER_PLACEHOLDER);
    }

    /// Drops every registered server/user term (keeping the home directory), so a removed
    /// server's address stops being tracked. Called before re-registering the current set.
    pub fn clear_registered_terms(&self) {
        self.shared.terms.write().expect("log redactor terms").retain(|t| t.replacement == "~");
    }

    /// Returns `text` untouched (borrowed) when disabled.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if !self.is_enabled() {
            return Cow::Borrowed(text);
        }
        let mut current: Option<String> = None;
        if let Some(next) = scrub_urls(text) {
            current = Some(next);
        }
        if let Some(next) = scrub_secrets(current.as_deref().unwrap_or(text)) {
            current = Some(next);
        }
        if let Some(next) = self.scrub_terms(current.as_deref().unwrap_or(text)) {
            current = Some(next);
        }
        match current {
            Some(redacted) if redacted != text => Cow::Owned(redacted),
            _ => Cow::Borrowed(text),
        }
    }

    /// `None` when nothing matched.
    fn scrub_terms(&self, text: &str) -> Option<String> {
        let terms = self.shared.terms.read().expect("log redactor terms");
        let mut current: Option<String> = None;
        for term in terms.iter() {
            if let Some(next) = replace_ignore_ascii_case(current.as_deref().unwrap_or(text), &term.needle, term.replacement) {
                current = Some(next);
            }
        }
        current
    }
}

/// `host:port` -> `host` (IPv6 literals in brackets keep their brackets).
fn strip_port(host_port: &str) -> &str {
    if host_port.starts_with('[') {
        return host_port.rsplit_once("]:").map_or(host_port, |(h, _)| &host_port[..h.len() + 1]);
    }
    match host_port.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => host_port,
    }
}

/// Case-insensitive (ASCII) literal replace. Skips matches that are already inside a `<...>`
/// placeholder, which keeps [`LogRedactor::redact`] idempotent (a username `user` must not turn
/// `<user>` into `<<user>>`).
fn replace_ignore_ascii_case(text: &str, needle_lower: &str, replacement: &str) -> Option<String> {
    let haystack = text.to_ascii_lowercase(); // ASCII lowercasing never changes byte offsets.
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut search_from = 0;
    while let Some(rel) = haystack[search_from..].find(needle_lower) {
        let start = search_from + rel;
        let end = start + needle_lower.len();
        search_from = end;
        let bytes = text.as_bytes();
        if start > 0 && bytes[start - 1] == b'<' && bytes.get(end) == Some(&b'>') {
            continue;
        }
        let buf = out.get_or_insert_with(|| String::with_capacity(text.len()));
        buf.push_str(&text[copied..start]);
        buf.push_str(replacement);
        copied = end;
    }
    out.map(|mut buf| {
        buf.push_str(&text[copied..]);
        buf
    })
}

fn is_url_scheme_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')
}

fn is_url_terminator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | '}' | '|' | '\\' | '`')
}

/// Replaces every `scheme://...` with `<url>`: userinfo, host, port, path and query go together,
/// so the server address, any `?token=` and any path that names an item are all covered at once.
fn scrub_urls(text: &str) -> Option<String> {
    if !text.contains("://") {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut pos = 0;
    while let Some(rel) = text[pos..].find("://") {
        let sep = pos + rel;
        // Walk back over the scheme; it must start with a letter.
        let mut start = sep;
        while start > copied && is_url_scheme_byte(bytes[start - 1]) {
            start -= 1;
        }
        while start < sep && !bytes[start].is_ascii_alphabetic() {
            start += 1;
        }
        let after = sep + 3;
        let mut end = text[after..].find(is_url_terminator).map_or(text.len(), |i| after + i);
        // Trailing sentence punctuation is not part of the URL.
        while end > after && matches!(bytes[end - 1], b'.' | b',' | b';' | b':' | b'!' | b'?' | b']') {
            end -= 1;
        }
        if start == sep || end == after {
            // `://` without a scheme or without anything after it: not a URL.
            pos = after;
            continue;
        }
        out.push_str(&text[copied..start]);
        out.push_str(URL_PLACEHOLDER);
        copied = end;
        pos = end.max(after);
    }
    out.push_str(&text[copied..]);
    Some(out)
}

/// Redacts `token=...` style assignments (query strings, `key="value"`, JSON `"token":"..."`)
/// and `Bearer ...` credentials.
fn scrub_secrets(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if !SECRET_KEYS.iter().any(|k| lower.contains(k)) && !lower.contains("bearer ") {
        return None;
    }
    let bytes = text.as_bytes();
    // (value_start, value_end) spans to blank out, found against the lowercase copy.
    let mut spans: Vec<(usize, usize)> = Vec::new();

    for key in SECRET_KEYS {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(key) {
            let key_start = from + rel;
            let key_end = key_start + key.len();
            from = key_end;
            // Must be a whole identifier: `access_token` is its own key, and must not be matched
            // again as `token` inside it, nor a word like `tokenizer`.
            if key_start > 0 && (bytes[key_start - 1].is_ascii_alphanumeric() || bytes[key_start - 1] == b'_') {
                continue;
            }
            if bytes.get(key_end).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
                continue;
            }
            let mut i = key_end;
            if matches!(bytes.get(i), Some(b'"' | b'\'')) {
                i += 1; // closing quote of a JSON/debug key
            }
            let assignment = match bytes.get(i) {
                Some(b'=') => {
                    i += 1;
                    true
                }
                Some(b':') => {
                    // `token: "x"` / `"token":"x"` — a colon only counts when a quoted value
                    // follows, so prose like "token: expired" is left alone.
                    let mut j = i + 1;
                    while bytes.get(j) == Some(&b' ') {
                        j += 1;
                    }
                    if matches!(bytes.get(j), Some(b'"' | b'\'')) {
                        i = j;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if !assignment {
                continue;
            }
            let quote = bytes.get(i).copied().filter(|b| matches!(b, b'"' | b'\''));
            let value_start = if quote.is_some() { i + 1 } else { i };
            let value_end = match quote {
                Some(q) => text[value_start..].find(q as char).map_or(text.len(), |p| value_start + p),
                None => text[value_start..]
                    .find(|c: char| c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ',' | ')' | '}' | ']' | ';'))
                    .map_or(text.len(), |p| value_start + p),
            };
            if value_end > value_start {
                spans.push((value_start, value_end));
            }
        }
    }

    let mut from = 0;
    while let Some(rel) = lower[from..].find("bearer ") {
        let value_start = from + rel + "bearer ".len();
        from = value_start;
        let value_end = text[value_start..].find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',')).map_or(text.len(), |p| value_start + p);
        if value_end > value_start {
            spans.push((value_start, value_end));
        }
    }

    if spans.is_empty() {
        return None;
    }
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (start, end) in spans {
        if start < copied {
            continue; // overlapping span already handled
        }
        out.push_str(&text[copied..start]);
        out.push_str(SECRET_PLACEHOLDER);
        copied = end;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

/// Re-reads every server and account from the database and registers their addresses and
/// usernames as sensitive. Runs at startup and after a successful sign-in; a new address typed
/// into the sign-in form is also registered synchronously, before the first request, by the
/// screen that owns the form. Failure is logged and otherwise ignored: the generic URL/token
/// scrubbing still applies.
pub async fn refresh_from_db(pool: &sqlx::SqlitePool) {
    let redactor = global();
    let servers = match abs_storage::repo::servers::list(pool).await {
        Ok(servers) => servers,
        Err(err) => {
            tracing::warn!(%err, "couldn't read servers to register them for log anonymization");
            return;
        }
    };
    redactor.clear_registered_terms();
    for server in servers {
        redactor.register_server_url(&server.url);
        if let Some(address) = &server.local_network_address {
            redactor.register_address(address);
        }
        if let Ok(accounts) = abs_storage::repo::accounts::list_for_server(pool, &server.id).await {
            for account in accounts {
                redactor.register_username(&account.username);
            }
        }
    }
}

/// A [`MakeWriter`] that wraps another and scrubs everything written through it.
#[derive(Clone)]
pub struct RedactingMakeWriter<M> {
    inner: M,
    redactor: LogRedactor,
}

impl<M> RedactingMakeWriter<M> {
    pub fn new(inner: M, redactor: LogRedactor) -> Self {
        Self { inner, redactor }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactingMakeWriter<M> {
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(self.inner.make_writer(), self.redactor.clone())
    }
}

/// Redacts whole lines. `tracing-subscriber` hands over one complete event per `write`, but this
/// does not rely on that: bytes are held until a newline arrives so a URL split across two
/// writes is still seen whole. Any unterminated tail is scrubbed and written on flush/drop.
pub struct RedactingWriter<W: Write> {
    inner: W,
    redactor: LogRedactor,
    pending: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    fn new(inner: W, redactor: LogRedactor) -> Self {
        Self { inner, redactor, pending: Vec::new() }
    }

    fn emit(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        let text = String::from_utf8_lossy(chunk);
        self.inner.write_all(self.redactor.redact(&text).as_bytes())
    }

    fn emit_pending(&mut self) -> std::io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending);
        self.emit(&pending)
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        if let Some(last_newline) = self.pending.iter().rposition(|b| *b == b'\n') {
            let rest = self.pending.split_off(last_newline + 1);
            let complete = std::mem::replace(&mut self.pending, rest);
            self.emit(&complete)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.emit_pending()?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        let _ = self.emit_pending();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> LogRedactor {
        let r = LogRedactor::new();
        r.shared.terms.write().unwrap().clear(); // ignore the developer's real $HOME
        r
    }

    #[test]
    fn urls_are_replaced_whole() {
        let r = on();
        for (input, expected) in [
            ("GET https://abs.example.com/api/items/x?token=abc failed", "GET <url> failed"),
            ("error sending request for url (http://192.168.1.5:13378/ping)", "error sending request for url (<url>)"),
            ("see https://user:pw@host.example:8443/a/b, then", "see <url>, then"),
            ("ws://[::1]:3000/socket.", "<url>."),
            ("a https://one.example/x and https://two.example/y", "a <url> and <url>"),
            ("quoted \"https://h.example/p\" end", "quoted \"<url>\" end"),
            ("no url here", "no url here"),
            ("odd :// separator", "odd :// separator"),
        ] {
            assert_eq!(r.redact(input), expected, "input: {input}");
        }
    }

    #[test]
    fn tokens_and_bearer_credentials_are_replaced() {
        let r = on();
        for (input, expected) in [
            ("cover?size=1&token=abc123&x=1", "cover?size=1&token=REDACTED&x=1"),
            ("access_token=eyJ.a.b done", "access_token=REDACTED done"),
            ("refresh_token=\"secret value\" next", "refresh_token=\"REDACTED\" next"),
            (r#"{"token":"abc","a":1}"#, r#"{"token":"REDACTED","a":1}"#),
            ("token: \"abc\"", "token: \"REDACTED\""),
            ("Authorization: Bearer abc.def.ghi", "Authorization: Bearer REDACTED"),
            ("the token: expired", "the token: expired"),
            ("tokenizer=fast", "tokenizer=fast"),
            ("my_token=keep", "my_token=keep"),
        ] {
            assert_eq!(r.redact(input), expected, "input: {input}");
        }
    }

    #[test]
    fn registered_terms_are_replaced_case_insensitively_longest_first() {
        let r = on();
        r.register_server_url("https://Audio.Example.com:8443/");
        r.register_server_url("https://example.com");
        r.register_username("Alice");
        assert_eq!(r.redact("connect audio.example.com failed"), "connect <server> failed");
        assert_eq!(r.redact("host Audio.Example.com:8443 down"), "host <server> down");
        assert_eq!(r.redact("only example.com"), "only <server>");
        assert_eq!(r.redact("user ALICE signed in"), "user <user> signed in");
    }

    #[test]
    fn short_and_empty_terms_are_ignored() {
        let r = on();
        r.register_username("al");
        r.register_username("");
        r.register_address("  ");
        assert_eq!(r.redact("al and everything else"), "al and everything else");
    }

    #[test]
    fn terms_are_literal_not_patterns() {
        let r = on();
        r.register_username("a.b+c");
        assert_eq!(r.redact("axb+c vs a.b+c"), "axb+c vs <user>");
    }

    #[test]
    fn local_network_address_registers_host_and_port() {
        let r = on();
        r.register_address("http://192.168.1.50:13378");
        assert_eq!(r.redact("probe 192.168.1.50 refused"), "probe <server> refused");
    }

    #[test]
    fn home_directory_becomes_tilde() {
        let r = on();
        r.set_home_dir("/home/alice/");
        assert_eq!(r.redact("state_dir=/home/alice/.local/state"), "state_dir=~/.local/state");
    }

    #[test]
    fn clearing_terms_forgets_servers_but_keeps_home() {
        let r = on();
        r.set_home_dir("/home/alice");
        r.register_server_url("https://abs.example.com");
        r.clear_registered_terms();
        assert_eq!(r.redact("abs.example.com /home/alice"), "abs.example.com ~");
    }

    #[test]
    fn redaction_is_idempotent() {
        let r = on();
        r.register_username("user");
        r.register_server_url("https://abs.example.com");
        let input = "user at abs.example.com: https://abs.example.com/x?token=t Bearer zzz token=q";
        let once = r.redact(input).into_owned();
        assert_eq!(r.redact(&once), once);
        assert!(once.contains("<user>"), "{once}");
    }

    #[test]
    fn disabled_redactor_passes_text_through_untouched() {
        let r = on();
        r.register_server_url("https://abs.example.com");
        r.set_enabled(false);
        let input = "https://abs.example.com/x?token=abc";
        assert!(matches!(r.redact(input), Cow::Borrowed(s) if s == input));
        r.set_enabled(true);
        assert_eq!(r.redact(input), "<url>");
    }

    #[test]
    fn a_new_redactor_starts_enabled() {
        assert!(LogRedactor::new().is_enabled());
    }

    #[test]
    fn non_ascii_text_survives() {
        let r = on();
        r.register_username("Zoë");
        assert_eq!(r.redact("héllo Zoë — https://ü.example/ß"), "héllo <user> — <url>");
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Sink {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    #[test]
    fn writer_redacts_a_url_split_across_writes() {
        let sink = Sink::default();
        let mut w = RedactingWriter::new(sink.clone(), on());
        w.write_all(b"failed: https://abs.exa").unwrap();
        w.write_all(b"mple.com/api?token=abc\nnext line\n").unwrap();
        assert_eq!(sink.text(), "failed: <url>\nnext line\n");
    }

    #[test]
    fn writer_handles_multi_line_events_and_unterminated_tail() {
        let sink = Sink::default();
        let mut w = RedactingWriter::new(sink.clone(), on());
        w.write_all(b"trace\n  at https://h.example/x\ntail https://h.example/y").unwrap();
        assert_eq!(sink.text(), "trace\n  at <url>\n", "the unterminated tail is held back");
        w.flush().unwrap();
        assert_eq!(sink.text(), "trace\n  at <url>\ntail <url>");
    }

    #[test]
    fn writer_flushes_its_tail_on_drop_and_tolerates_invalid_utf8() {
        let sink = Sink::default();
        {
            let mut w = RedactingWriter::new(sink.clone(), on());
            w.write_all(b"bad \xff byte https://h.example/z").unwrap();
        }
        assert_eq!(sink.text(), "bad \u{fffd} byte <url>");
    }

    #[test]
    fn disabling_applies_to_writers_that_already_exist() {
        let sink = Sink::default();
        let r = on();
        let mut w = RedactingWriter::new(sink.clone(), r.clone());
        r.set_enabled(false);
        w.write_all(b"https://h.example/z\n").unwrap();
        assert_eq!(sink.text(), "https://h.example/z\n");
    }
}
