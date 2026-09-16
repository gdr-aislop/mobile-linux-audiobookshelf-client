//! The Welcome/Server login screen — shown whenever `abs_storage::repo::accounts::get_active`
//! returns `None` (first launch, or after signing out of every configured server). See
//! `docs/design/ui-spec.md`'s "Welcome / Server login" section and the published mockup for the
//! visual design this implements.

use std::rc::Rc;

use abs_core::accounts::AddedAccount;
use abs_core::error::CoreError;
use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

/// Only Password mode is wired to a real backend call right now — `abs-core`/`abs-api` have no
/// token-based login (Audiobookshelf's `/login` is username+password only; a bare API token isn't
/// a login mechanism the server exposes). The toggle is kept for visual completeness with the
/// design spec, but Connect in this mode shows an explanatory banner instead of silently failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    Password,
    ApiToken,
}

pub struct WelcomeScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
pub struct TestHooks {
    pub url_row: adw::EntryRow,
    pub username_row: adw::EntryRow,
    pub password_row: adw::PasswordEntryRow,
    pub connect_button: gtk4::Button,
    pub banner: crate::widgets::banner::ErrorBanner,
}

#[cfg(test)]
impl WelcomeScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

/// Builds the screen. `on_success` fires once, with the newly-added account, after a successful
/// connect — the caller (`application.rs`) is responsible for swapping window content.
pub fn build(pool: SqlitePool, on_success: impl Fn(AddedAccount) + 'static) -> WelcomeScreen {
    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Start)
        .margin_top(48)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .width_request(340)
        .spacing(0)
        .build();

    // Hero: icon, title, subtitle.
    let icon = gtk4::Image::builder()
        .icon_name("network-server-symbolic")
        .pixel_size(40)
        .css_classes(["welcome-icon"])
        .build();
    let title = gtk4::Label::builder()
        .label("Connect to your Audiobookshelf server")
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .css_classes(["title-2"])
        .margin_top(14)
        .build();
    let subtitle = gtk4::Label::builder()
        .label("Enter your server address and sign in to start listening.")
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .css_classes(["dim-label"])
        .margin_top(6)
        .build();
    content.append(&icon);
    content.append(&title);
    content.append(&subtitle);

    // Auth-mode toggle: two linked GtkToggleButtons — AdwToggleGroup is 1.4+ and won't compile
    // under this crate's v1_2 feature ceiling, so this is the hand-rolled equivalent.
    let mode_password = gtk4::ToggleButton::builder().label("Password").active(true).build();
    let mode_token = gtk4::ToggleButton::builder().label("API Token").build();
    mode_token.set_group(Some(&mode_password));
    let mode_box = gtk4::Box::builder()
        .css_classes(["linked"])
        .halign(gtk4::Align::Fill)
        .margin_top(22)
        .margin_bottom(16)
        .build();
    mode_password.set_hexpand(true);
    mode_token.set_hexpand(true);
    mode_box.append(&mode_password);
    mode_box.append(&mode_token);
    content.append(&mode_box);

    // Error banner — hidden until a connect attempt fails.
    let banner = crate::widgets::banner::ErrorBanner::new();
    content.append(banner.widget());

    // Fields, grouped as a libadwaita-1.2-safe ".boxed-list".
    let list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(12)
        .build();

    let url_row = adw::EntryRow::builder().title("Server URL").show_apply_button(false).build();
    let username_row = adw::EntryRow::builder().title("Username").show_apply_button(false).build();
    let password_row = adw::PasswordEntryRow::builder().title("Password").show_apply_button(false).build();
    let token_row = adw::EntryRow::builder().title("API Token").show_apply_button(false).build();
    token_row.set_visible(false);

    list.append(&url_row);
    list.append(&username_row);
    list.append(&password_row);
    list.append(&token_row);
    content.append(&list);

    let connect_button = gtk4::Button::builder()
        .label("Connect")
        .css_classes(["suggested-action", "pill"])
        .hexpand(true)
        .sensitive(false)
        .margin_top(22)
        .height_request(44)
        .build();
    content.append(&connect_button);

    // --- Mode switching: show/hide the fields each mode needs. ---
    let update_visibility: Rc<dyn Fn(AuthMode)> = Rc::new({
        let username_row = username_row.clone();
        let password_row = password_row.clone();
        let token_row = token_row.clone();
        move |mode: AuthMode| {
            let is_password = mode == AuthMode::Password;
            username_row.set_visible(is_password);
            password_row.set_visible(is_password);
            token_row.set_visible(!is_password);
        }
    });

    // --- Connect button sensitivity: enabled once the current mode's required fields are filled. ---
    let update_sensitivity: Rc<dyn Fn()> = Rc::new({
        let mode_password = mode_password.clone();
        let url_row = url_row.clone();
        let username_row = username_row.clone();
        let password_row = password_row.clone();
        let token_row = token_row.clone();
        let connect_button = connect_button.clone();
        move || {
            let url_filled = !url_row.text().trim().is_empty();
            let mode_filled = if mode_password.is_active() {
                !username_row.text().trim().is_empty() && !password_row.text().trim().is_empty()
            } else {
                !token_row.text().trim().is_empty()
            };
            connect_button.set_sensitive(url_filled && mode_filled);
        }
    });

    mode_password.connect_toggled({
        let update_visibility = update_visibility.clone();
        let update_sensitivity = update_sensitivity.clone();
        move |btn| {
            if btn.is_active() {
                update_visibility(AuthMode::Password);
                update_sensitivity();
            }
        }
    });
    mode_token.connect_toggled({
        let update_visibility = update_visibility.clone();
        let update_sensitivity = update_sensitivity.clone();
        move |btn| {
            if btn.is_active() {
                update_visibility(AuthMode::ApiToken);
                update_sensitivity();
            }
        }
    });

    for entry in [&url_row, &username_row, &token_row] {
        entry.connect_changed({
            let update_sensitivity = update_sensitivity.clone();
            move |_| update_sensitivity()
        });
    }
    password_row.connect_changed({
        let update_sensitivity = update_sensitivity.clone();
        move |_| update_sensitivity()
    });

    // --- Connect click: disable the form, spawn the login call, react to the result. ---
    connect_button.connect_clicked({
        let pool = pool.clone();
        let mode_password = mode_password.clone();
        let url_row = url_row.clone();
        let username_row = username_row.clone();
        let password_row = password_row.clone();
        let mode_box = mode_box.clone();
        let list = list.clone();
        let connect_button = connect_button.clone();
        let banner = banner.clone();
        let on_success = Rc::new(on_success);
        move |_| {
            let is_password_mode = mode_password.is_active();
            banner.set_revealed(false);
            banner.set_details(None);
            username_row.remove_css_class("error");
            password_row.remove_css_class("error");

            if !is_password_mode {
                banner.set_title("Signing in with an API token isn't supported yet — use your username and password.");
                banner.set_revealed(true);
                return;
            }

            let url = url_row.text().trim().to_string();
            let username = username_row.text().trim().to_string();
            let password = password_row.text().to_string();

            mode_box.set_sensitive(false);
            list.set_sensitive(false);
            connect_button.set_sensitive(false);
            connect_button.set_label("Connecting…");

            let pool = pool.clone();
            let mode_box = mode_box.clone();
            let list = list.clone();
            let connect_button = connect_button.clone();
            let banner = banner.clone();
            let username_row = username_row.clone();
            let password_row = password_row.clone();
            let on_success = on_success.clone();

            glib::spawn_future_local(async move {
                let result = abs_core::accounts::add_server_and_login(&pool, &url, &username, &password).await;

                mode_box.set_sensitive(true);
                list.set_sensitive(true);
                connect_button.set_label("Connect");
                connect_button.set_sensitive(true);

                match result {
                    Ok(added) => on_success(added),
                    Err(err) => {
                        // Only an authentication failure implicates the username/password
                        // fields — tinting them on a connectivity failure (unreachable host,
                        // DNS, TLS) would misdirect the user into thinking their password is
                        // wrong when the server itself couldn't be reached.
                        if matches!(err, CoreError::Login(abs_api::LoginError::InvalidCredentials)) {
                            username_row.add_css_class("error");
                            password_row.add_css_class("error");
                        }
                        banner.set_title(&error_message(&err));
                        banner.set_details(login_details(&err).as_deref());
                        banner.set_revealed(true);
                    }
                }
            });
        }
    });

    #[cfg(test)]
    let hooks = TestHooks {
        url_row: url_row.clone(),
        username_row: username_row.clone(),
        password_row: password_row.clone(),
        connect_button: connect_button.clone(),
        banner: banner.clone(),
    };

    WelcomeScreen {
        root: content.upcast(),
        #[cfg(test)]
        hooks,
    }
}

/// Never clears the fields on failure — the user shouldn't have to retype everything after a
/// typo. Message text is keyed off the error variant, per `docs/design/ui-spec.md`'s error state.
/// The username/password field tint (see the Connect click handler) is keyed off the same
/// `InvalidCredentials` variant this function branches on, but deliberately not applied for
/// `Network`/other variants — a connectivity failure isn't a credentials problem.
fn error_message(err: &CoreError) -> String {
    match err {
        CoreError::Login(abs_api::LoginError::InvalidCredentials) => {
            "Unable to sign in — check your username and password and try again.".to_string()
        }
        CoreError::Login(abs_api::LoginError::Tls(_)) => {
            "Can't verify this server's certificate — if you trust it, allow it in that server's connection settings.".to_string()
        }
        CoreError::Login(abs_api::LoginError::Connect(_) | abs_api::LoginError::Network(_)) => {
            "Can't reach this server — check the URL and your connection.".to_string()
        }
        CoreError::Login(abs_api::LoginError::Timeout(_)) => {
            "This server took too long to respond — check your connection and try again.".to_string()
        }
        _ => "Something went wrong — please try again.".to_string(),
    }
}

/// The raw underlying transport-error text for the banner's "Show details" disclosure — `None`
/// for anything that isn't a low-level transport failure (a bad password or an unexpected server
/// response aren't the kind of opaque library text this disclosure exists for).
fn login_details(err: &CoreError) -> Option<String> {
    match err {
        CoreError::Login(login_err) => login_err.details(),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{pool, pump_until};
    use std::cell::RefCell;
    use std::time::Duration;

    const DEMO_SERVER_URL: &str = "https://audiobooks.dev/audiobookshelf";

    /// Exercises the real Connect button — `emit_clicked()` invokes the actual production
    /// signal handler, not a simulated input event — against the live public demo server
    /// (confirmed reachable and running server 2.36.0 before this test was written; see
    /// `crates/abs-core/tests/live_demo_server.rs`). This is also what catches a real
    /// GTK-main-loop-vs-Tokio-runtime bridging bug: `add_server_and_login` uses `sqlx`/
    /// `reqwest`, both of which need an entered Tokio runtime context to do any I/O at all —
    /// if `main.rs` didn't keep one entered for the GTK main loop's lifetime, this test would
    /// hang or panic with "no reactor running" instead of completing.
    ///
    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every GTK-touching live-server
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run_live(runtime: &tokio::runtime::Runtime) {
        // Success case: correct demo credentials.
        {
            let pool = runtime.block_on(pool());
            let result: Rc<RefCell<Option<AddedAccount>>> = Rc::new(RefCell::new(None));

            let screen = build(pool, {
                let result = result.clone();
                move |added| *result.borrow_mut() = Some(added)
            });
            let hooks = screen.test_hooks();

            hooks.url_row.set_text(DEMO_SERVER_URL);
            hooks.username_row.set_text("demo");
            hooks.password_row.set_text("demo");

            assert!(
                hooks.connect_button.is_sensitive(),
                "Connect should be enabled once every required field is filled"
            );
            hooks.connect_button.emit_clicked();

            pump_until(|| result.borrow().is_some(), Duration::from_secs(15));

            let added = result.borrow_mut().take().expect("on_success should have fired");
            assert!(!added.server_id.is_empty());
            assert!(!added.account_id.is_empty());
            assert!(
                !hooks.banner.widget().reveals_child(),
                "the error banner must not be showing after a successful login"
            );
        }

        // Failure case: wrong password against the same live server.
        {
            let pool = runtime.block_on(pool());
            let succeeded = Rc::new(RefCell::new(false));

            let screen = build(pool, {
                let succeeded = succeeded.clone();
                move |_| *succeeded.borrow_mut() = true
            });
            let hooks = screen.test_hooks();

            hooks.url_row.set_text(DEMO_SERVER_URL);
            hooks.username_row.set_text("demo");
            hooks.password_row.set_text("definitely-wrong-password");
            hooks.connect_button.emit_clicked();

            pump_until(
                || hooks.banner.widget().reveals_child(),
                Duration::from_secs(15),
            );

            assert!(!*succeeded.borrow(), "on_success must not fire on a failed login");
            assert!(
                hooks.banner.widget().reveals_child(),
                "the error banner should now be visible"
            );
            assert!(
                hooks.username_row.has_css_class("error") && hooks.password_row.has_css_class("error"),
                "a credentials failure should tint the username/password fields"
            );
            assert!(
                !hooks.banner.details_visible(),
                "a bad password isn't a low-level transport error — there's no raw detail to show"
            );
        }
    }

    /// Covers both the disabled/toggling behavior and the connectivity-failure error state. Not a
    /// `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching scenario in
    /// this binary has to run from one single entry point (`gtk4::init()` can only succeed once
    /// per process/thread, and libtest gives every `#[test]` fn its own fresh OS thread even under
    /// `--test-threads=1`, which only limits how many run *concurrently*, not which thread each
    /// runs on).
    pub(crate) fn run(runtime: &tokio::runtime::Runtime) {
        // Starts disabled; filling every required Password-mode field enables Connect.
        {
            let pool = runtime.block_on(pool());
            let screen = build(pool, |_| {});
            let hooks = screen.test_hooks();

            assert!(!hooks.connect_button.is_sensitive(), "empty form should start disabled");

            hooks.url_row.set_text(DEMO_SERVER_URL);
            hooks.username_row.set_text("demo");
            hooks.password_row.set_text("demo");
            assert!(
                hooks.connect_button.is_sensitive(),
                "filling every required Password-mode field should enable Connect"
            );
        }

        // Connectivity failures (nothing listening at the given address) must show a different
        // banner message than a credentials failure, must NOT tint the username/password fields
        // — see docs/design/ui-spec.md's Welcome/Server login error-state section — and, since
        // this is a low-level transport failure, must expose the raw error via the banner's
        // "Show details" disclosure. Uses a real TCP connection attempt to an address nothing
        // listens on rather than wiremock, so this exercises abs_api::LoginError::Connect for
        // real; it's fast (immediate connection refused) and needs no external network, so it
        // isn't #[ignore]d.
        {
            let pool = runtime.block_on(pool());
            let screen = build(pool, |_| {});
            let hooks = screen.test_hooks();

            hooks.url_row.set_text("http://127.0.0.1:1");
            hooks.username_row.set_text("demo");
            hooks.password_row.set_text("demo");
            hooks.connect_button.emit_clicked();

            pump_until(
                || hooks.banner.widget().reveals_child(),
                Duration::from_secs(15),
            );

            assert!(hooks.banner.widget().reveals_child(), "the error banner should now be visible");
            assert_eq!(
                hooks.banner.title(),
                "Can't reach this server — check the URL and your connection."
            );
            assert!(
                !hooks.username_row.has_css_class("error") && !hooks.password_row.has_css_class("error"),
                "a connectivity failure must not tint the username/password fields"
            );
            assert!(
                hooks.banner.details_visible(),
                "a connectivity failure should expose the 'Show details' disclosure"
            );
            assert!(
                !hooks.banner.details_text().is_empty(),
                "the details disclosure should carry the raw transport error text"
            );
        }
    }
}
