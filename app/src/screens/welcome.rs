//! The Welcome/Server login screen — shown whenever there's no usable session: first launch
//! (`abs_storage::repo::accounts::get_active` returns `None`), or as the re-login flow when a
//! dead session's "Log in again" action hands control back from the shell. See
//! `docs/design/ui-spec.md`'s "Welcome / Server login" section and the published mockup for the
//! visual design this implements.

use std::rc::Rc;

use abs_core::accounts::{AddedAccount, ReloginSeed, ReplacementKind};
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
    pub cancel_button: gtk4::Button,
    pub banner: crate::widgets::banner::ErrorBanner,
}

#[cfg(test)]
impl WelcomeScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

/// Builds the screen. `on_success` fires once, with the (possibly rotated) account, after a
/// successful connect — the caller (`application.rs`) is responsible for swapping window content.
///
/// `previous` turns this into the re-login flow behind Home's "Log in again": the form comes
/// pre-filled with the previous session's URL and username (only the password left to type), a
/// Cancel button (`on_cancel`) returns to the main window, and credentials that would *replace*
/// the previous session's local data — a different account, or a different server — ask for
/// confirmation before `abs_core::accounts::relogin` touches anything.
pub fn build(
    pool: SqlitePool,
    paths: abs_storage::AppPaths,
    previous: Option<ReloginSeed>,
    on_success: impl Fn(AddedAccount) + 'static,
    on_cancel: Option<Rc<dyn Fn()>>,
) -> WelcomeScreen {
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
    crate::widgets::entry_row_input_purpose(&url_row, gtk4::InputPurpose::Url);
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

    // A way back to the (cached) app — the re-login flow ("Log in again") and the Add-Server
    // flow from Settings both provide one; only the true first run has nothing to return to.
    // Without it, "Log in again" would be a trap — a typo'd URL or a changed mind would leave
    // the user stuck on this screen with no route back to their libraries.
    let cancel_button = gtk4::Button::builder()
        .label("Cancel")
        .css_classes(["pill"])
        .hexpand(true)
        .visible(on_cancel.is_some())
        .margin_top(8)
        .height_request(44)
        .build();
    if let Some(on_cancel) = on_cancel.as_ref() {
        let on_cancel = on_cancel.clone();
        cancel_button.connect_clicked(move |_| on_cancel());
    }
    content.append(&cancel_button);

    // Re-login: pre-fill what the user shouldn't have to retype (URL, username — the session died,
    // not the server address) and put the cursor on the one field that's genuinely empty. The
    // set_text calls fire `changed`, which runs the sensitivity update, so Connect correctly
    // stays disabled until a password is typed.
    if let Some(previous) = &previous {
        url_row.set_text(&previous.url);
        username_row.set_text(&previous.username);
        password_row.grab_focus();
    }

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

    // --- Connect: validate, confirm any replacement, then run the login. ---
    //
    // The actual login run is shared by the direct Connect click and the confirm dialog's
    // "Replace" response: disable the form, call `relogin` (re-login flow) or
    // `add_server_and_login` (first run), re-enable, then fire `on_success` or show the error.
    let run_connect: Rc<dyn Fn()> = Rc::new({
        let pool = pool.clone();
        let paths = paths.clone();
        let previous = previous.clone();
        let url_row = url_row.clone();
        let username_row = username_row.clone();
        let password_row = password_row.clone();
        let mode_box = mode_box.clone();
        let list = list.clone();
        let connect_button = connect_button.clone();
        let banner = banner.clone();
        let on_success = Rc::new(on_success);
        move || {
            let url = url_row.text().trim().to_string();
            let username = username_row.text().trim().to_string();
            let password = password_row.text().to_string();

            mode_box.set_sensitive(false);
            list.set_sensitive(false);
            connect_button.set_sensitive(false);
            connect_button.set_label("Connecting…");

            // Cloned per call: this closure fires once per Connect press (and once per confirmed
            // replacement), so nothing may be moved out of it — widget handles included.
            let pool = pool.clone();
            let paths = paths.clone();
            let previous = previous.clone();
            let on_success = on_success.clone();
            let username_row = username_row.clone();
            let password_row = password_row.clone();
            let mode_box = mode_box.clone();
            let list = list.clone();
            let connect_button = connect_button.clone();
            let banner = banner.clone();

            glib::spawn_future_local(async move {
                let result = match &previous {
                    Some(seed) => abs_core::accounts::relogin(&pool, &paths, seed, &url, &username, &password).await,
                    None => abs_core::accounts::add_server_and_login(&pool, &url, &username, &password).await,
                };

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

    connect_button.connect_clicked({
        let url_row = url_row.clone();
        let username_row = username_row.clone();
        let password_row = password_row.clone();
        let mode_password = mode_password.clone();
        let banner = banner.clone();
        let previous = previous.clone();
        let run_connect = run_connect.clone();
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

            // Re-login flow: credentials that would replace the previous session's local data are
            // confirmed before anything runs. The dialog only *previews* `replacement_kind`'s
            // verdict — nothing is persisted until the new login actually succeeds (a failed
            // login leaves the old session intact, so even a confirmed replacement that then
            // fails changes nothing).
            let replacement = previous
                .as_ref()
                .and_then(|seed| abs_core::accounts::replacement_kind(seed, &url, &username));
            if let Some(kind) = replacement {
                let (heading, body) = replacement_warning(&kind, previous.as_ref().unwrap(), &url, &username);
                let dialog = gtk4::MessageDialog::builder()
                    .message_type(gtk4::MessageType::Warning)
                    .text(heading)
                    .secondary_text(body)
                    .modal(true)
                    .build();
                dialog.add_button("Cancel", gtk4::ResponseType::Cancel);
                let replace = dialog.add_button("Replace", gtk4::ResponseType::Ok);
                replace.add_css_class("destructive-action");
                dialog.connect_response({
                    let run_connect = run_connect.clone();
                    move |dialog, response| {
                        dialog.destroy();
                        if response == gtk4::ResponseType::Ok {
                            run_connect();
                        }
                    }
                });
                dialog.present();
                return;
            }

            run_connect();
        }
    });

    // --- Keyboard flow: Enter in the password/token field submits the form, Enter in the other
    // fields advances focus to the next one — the Connect button must not be reachable by pointer
    // only. `entry-activated` fires when Enter is pressed inside an entry row; submission routes
    // through the same production handler as a click (and only when the form is complete, i.e.
    // the button is sensitive — the handler reads the fields itself and has no empty-form guard). ---
    let try_connect: Rc<dyn Fn()> = Rc::new({
        let connect_button = connect_button.clone();
        move || {
            if connect_button.is_sensitive() {
                connect_button.emit_clicked();
            }
        }
    });

    url_row.connect_entry_activated({
        let mode_password = mode_password.clone();
        let username_row = username_row.clone();
        let token_row = token_row.clone();
        move |_| {
            if mode_password.is_active() {
                username_row.grab_focus();
            } else {
                token_row.grab_focus();
            }
        }
    });
    username_row.connect_entry_activated({
        let password_row = password_row.clone();
        move |_| {
            password_row.grab_focus();
        }
    });
    password_row.connect_entry_activated({
        let try_connect = try_connect.clone();
        move |_| try_connect()
    });
    token_row.connect_entry_activated({
        let try_connect = try_connect.clone();
        move |_| try_connect()
    });

    #[cfg(test)]
    let hooks = TestHooks {
        url_row: url_row.clone(),
        username_row: username_row.clone(),
        password_row: password_row.clone(),
        connect_button: connect_button.clone(),
        cancel_button: cancel_button.clone(),
        banner: banner.clone(),
    };

    WelcomeScreen {
        root: content.upcast(),
        #[cfg(test)]
        hooks,
    }
}

/// The confirm dialog's copy for a re-login that would replace the previous session's local data,
/// per kind: an account switch on the same server only costs the old account's on-device progress
/// (the server's cache stays valid), while a server switch costs everything cached for the old
/// server. Both stress what the user's *server* keeps — the local data is only a cache.
fn replacement_warning(kind: &ReplacementKind, previous: &ReloginSeed, url: &str, username: &str) -> (String, String) {
    match kind {
        ReplacementKind::SameServerNewAccount => (
            format!("Replace {} on this server?", previous.username),
            format!(
                "Signing in as {username} will replace {}'s account on this device. Their listening progress \
                 stored here will be removed — everything on the server stays where it is.",
                previous.username
            ),
        ),
        ReplacementKind::NewServer => (
            "Switch server?".to_string(),
            format!(
                "Signing in to {url} will remove {}'s cached libraries, downloads, and progress from this \
                 device. Everything on {} itself stays untouched.",
                previous.username, previous.url
            ),
        ),
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

            let screen = build(
                pool,
                crate::test_support::test_paths(),
                None,
                {
                    let result = result.clone();
                    move |added| *result.borrow_mut() = Some(added)
                },
                None,
            );
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

            let screen = build(
                pool,
                crate::test_support::test_paths(),
                None,
                {
                    let succeeded = succeeded.clone();
                    move |_| *succeeded.borrow_mut() = true
                },
                None,
            );
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

    /// The re-login flow end to end: pre-fill, the Cancel affordance, and confirm-before-replace.
    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run_relogin_flow(runtime: &tokio::runtime::Runtime) {
        use abs_core::accounts::ReloginSeed;
        use std::cell::Cell;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn find_message_dialog() -> Option<gtk4::MessageDialog> {
            gtk4::Window::list_toplevels()
                .into_iter()
                .find_map(|window| window.downcast::<gtk4::MessageDialog>().ok())
        }

        fn seed_session(
            runtime: &tokio::runtime::Runtime,
            url: &str,
            username: &str,
        ) -> (sqlx::SqlitePool, ReloginSeed) {
            let pool = runtime.block_on(pool());
            let server_id = runtime.block_on(abs_storage::repo::servers::add(&pool, url)).unwrap();
            let account_id = runtime
                .block_on(abs_storage::repo::accounts::add(&pool, &server_id, username, "old-token", None))
                .unwrap();
            (
                pool,
                ReloginSeed {
                    server_id,
                    account_id,
                    url: url.to_string(),
                    username: username.to_string(),
                },
            )
        }

        // --- Pre-fill and cancel: the session died, not the server address — the user should
        // only have to type a password, and changing their mind must lead back to the app. ---
        {
            let (pool, previous) = seed_session(runtime, "http://127.0.0.1:1", "jane");
            let cancelled = Rc::new(Cell::new(false));
            let on_cancel = {
                let cancelled = cancelled.clone();
                Rc::new(move || cancelled.set(true)) as Rc<dyn Fn()>
            };
            let screen = build(pool, crate::test_support::test_paths(), Some(previous), |_| {}, Some(on_cancel));
            let hooks = screen.test_hooks();

            assert_eq!(hooks.url_row.text(), "http://127.0.0.1:1", "the URL must come pre-filled");
            assert_eq!(hooks.username_row.text(), "jane", "the username must come pre-filled");
            assert_eq!(hooks.password_row.text(), "", "only the password is left to type");
            assert!(hooks.cancel_button.is_visible(), "a re-login must be abortable");

            hooks.cancel_button.emit_clicked();
            assert!(cancelled.get(), "cancel must hand control back to the shell via on_cancel");
        }

        // --- Same credentials: no confirmation, straight to a token-rotating login. ---
        {
            let mock_server = runtime.block_on(MockServer::start());
            runtime.block_on(
                Mock::given(method("POST"))
                    .and(path("/login"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "user": { "id": "user-1", "username": "jane", "accessToken": "new-access", "refreshToken": "new-refresh" }
                    })))
                    .mount(&mock_server),
            );
            let (pool, previous) = seed_session(runtime, &mock_server.uri(), "jane");

            let succeeded = Rc::new(Cell::new(false));
            let screen = build(
                pool.clone(),
                crate::test_support::test_paths(),
                Some(previous.clone()),
                {
                    let succeeded = succeeded.clone();
                    move |_| succeeded.set(true)
                },
                None,
            );
            let hooks = screen.test_hooks();

            hooks.password_row.set_text("hunter2");
            hooks.connect_button.emit_clicked();

            pump_until(|| succeeded.get(), Duration::from_secs(15));
            let account = runtime
                .block_on(abs_storage::repo::accounts::get(&pool, &previous.account_id));
            assert_eq!(account.unwrap().token, "new-access", "same credentials must rotate the stored tokens in place");
        }

        // --- Different username, dialog cancelled: nothing happens at all. ---
        {
            let mock_server = runtime.block_on(MockServer::start());
            runtime.block_on(
                Mock::given(method("POST"))
                    .and(path("/login"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "user": { "id": "user-2", "username": "bob", "accessToken": "bob-token", "refreshToken": None::<String> }
                    })))
                    .mount(&mock_server),
            );
            let (pool, previous) = seed_session(runtime, &mock_server.uri(), "jane");

            let succeeded = Rc::new(Cell::new(false));
            let screen = build(
                pool.clone(),
                crate::test_support::test_paths(),
                Some(previous.clone()),
                {
                    let succeeded = succeeded.clone();
                    move |_| succeeded.set(true)
                },
                None,
            );
            let hooks = screen.test_hooks();

            hooks.username_row.set_text("bob");
            hooks.password_row.set_text("hunter2");
            hooks.connect_button.emit_clicked();

            pump_until(|| find_message_dialog().is_some(), Duration::from_secs(5));
            find_message_dialog().unwrap().response(gtk4::ResponseType::Cancel);

            pump_until(|| hooks.connect_button.is_sensitive(), Duration::from_secs(5));
            assert!(!succeeded.get(), "a cancelled replacement must not log in");
            let account = runtime.block_on(abs_storage::repo::accounts::get(&pool, &previous.account_id));
            assert_eq!(
                account.unwrap().token,
                "old-token",
                "a cancelled replacement must leave the old session untouched"
            );
        }

        // --- Different username, dialog confirmed: the replacement runs. ---
        {
            let mock_server = runtime.block_on(MockServer::start());
            runtime.block_on(
                Mock::given(method("POST"))
                    .and(path("/login"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "user": { "id": "user-2", "username": "bob", "accessToken": "bob-token", "refreshToken": None::<String> }
                    })))
                    .mount(&mock_server),
            );
            let (pool, previous) = seed_session(runtime, &mock_server.uri(), "jane");

            let succeeded = Rc::new(Cell::new(false));
            let screen = build(
                pool.clone(),
                crate::test_support::test_paths(),
                Some(previous.clone()),
                {
                    let succeeded = succeeded.clone();
                    move |_| succeeded.set(true)
                },
                None,
            );
            let hooks = screen.test_hooks();

            hooks.username_row.set_text("bob");
            hooks.password_row.set_text("hunter2");
            hooks.connect_button.emit_clicked();

            pump_until(|| find_message_dialog().is_some(), Duration::from_secs(5));
            find_message_dialog().unwrap().response(gtk4::ResponseType::Ok);

            pump_until(|| succeeded.get(), Duration::from_secs(15));
            assert!(
                runtime.block_on(abs_storage::repo::accounts::get(&pool, &previous.account_id)).is_err(),
                "the confirmed replacement must remove the old account"
            );
            let active = runtime.block_on(abs_storage::repo::accounts::get_active(&pool)).unwrap().unwrap();
            assert_eq!(active.username, "bob", "the replacement account must be the active one");
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
            let screen = build(pool, crate::test_support::test_paths(), None, |_| {}, None);
            let hooks = screen.test_hooks();

            assert!(!hooks.connect_button.is_sensitive(), "empty form should start disabled");
            let url_text = hooks
                .url_row
                .delegate()
                .unwrap()
                .downcast::<gtk4::Text>()
                .expect("EntryRow delegate should be a GtkText");
            assert_eq!(
                url_text.input_purpose(),
                gtk4::InputPurpose::Url,
                "the Server URL field must ask the on-screen keyboard for its URL layout"
            );

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
            let screen = build(pool, crate::test_support::test_paths(), None, |_| {}, None);
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

        // Pressing Enter in the password field must submit the form — exactly the same production
        // handler as a Connect click (see the `try_connect` wiring), simulated here by emitting
        // the row's `entry-activated` signal, which is what the Enter key triggers. Points at the
        // same nothing-listening address, so it stays fast and network-free.
        {
            let pool = runtime.block_on(pool());
            let screen = build(pool, crate::test_support::test_paths(), None, |_| {}, None);
            let hooks = screen.test_hooks();

            hooks.url_row.set_text("http://127.0.0.1:1");
            hooks.username_row.set_text("demo");
            hooks.password_row.set_text("demo");
            assert!(
                hooks.connect_button.is_sensitive(),
                "the form must be complete before Enter-submit can fire"
            );
            hooks
                .password_row
                .emit_by_name::<()>("entry-activated", &[]);

            pump_until(
                || hooks.banner.widget().reveals_child(),
                Duration::from_secs(15),
            );
            assert!(
                hooks.banner.widget().reveals_child(),
                "pressing Enter in the password field should submit the form"
            );
            assert_eq!(
                hooks.banner.title(),
                "Can't reach this server — check the URL and your connection."
            );
        }
    }
}
