//! The per-server Connection destination — per `docs/design/ui-spec.md`'s "Connection"
//! section. One instance per configured server, pushed from a server row's body (or the
//! Account row, for the active server) in Settings by swapping the window's content — the
//! same mechanism the full player uses, since `AdwNavigationView` is out of reach at this
//! crate's libadwaita `v1_2` ceiling (see `app/Cargo.toml`). The back button restores the
//! captured shell widget, so no shell state is lost.
//!
//! Real so far: the Server connection group (the full URL as a monospace subtitle — a literal
//! value being confirmed — with an info popover explaining what the connection is used for)
//! and the destructive Disconnect action, which signs out this server's account (its local
//! progress cascades away; the server and its cache survive) after the same kind of
//! confirmation the other destructive session changes use.
//!
//! The Advanced group (Custom Headers, Disable SSL verification, Client certificate, Local
//! network server address, Change User Agent) is deliberately not built yet: applying any of
//! them to real server traffic needs an HTTP-client factory threaded through sync, token
//! refresh, covers, downloads and streaming — a feature batch of its own, and each row is
//! only added when it can be live wiring.

use std::rc::Rc;

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_storage::models::{Account, Server};

use crate::screens::settings::{confirm, host_of};

pub struct ConnectionScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    pub hooks: ConnectionHooks,
}

#[cfg(test)]
pub struct ConnectionHooks {
    pub url_row: adw::ActionRow,
    pub info_button: gtk4::MenuButton,
    pub back_button: gtk4::Button,
    pub disconnect_button: gtk4::Button,
}

pub fn build(
    pool: SqlitePool,
    server: Server,
    account: Option<Account>,
    window: &adw::ApplicationWindow,
    shell_root: &gtk4::Widget,
    on_session_changed: Rc<dyn Fn()>,
) -> ConnectionScreen {
    ensure_mono_css();

    let back_button = gtk4::Button::builder()
        .icon_name("go-previous-symbolic")
        .css_classes(["flat"])
        .build();
    back_button.connect_clicked({
        let window = window.clone();
        let shell_root = shell_root.clone();
        move |_| window.set_content(Some(&shell_root))
    });

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Connection", "")));
    header.pack_start(&back_button);

    let page = adw::PreferencesPage::new();

    let connection_group = adw::PreferencesGroup::new();
    connection_group.set_title("Server connection");
    let url_row = adw::ActionRow::builder().title(host_of(&server.url)).subtitle(&server.url).build();
    url_row.add_css_class("mono-subtitle");

    let info_button = gtk4::MenuButton::builder()
        .icon_name("dialog-information-symbolic")
        .css_classes(["flat"])
        .valign(gtk4::Align::Center)
        .build();
    let info_label = gtk4::Label::builder()
        .label("This connection is used for syncing, streaming and downloads — every call the app makes to the server goes through it.")
        .wrap(true)
        .max_width_chars(32)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    info_button.set_popover(Some(&gtk4::Popover::builder().child(&info_label).build()));
    url_row.add_suffix(&info_button);
    connection_group.add(&url_row);
    page.add(&connection_group);

    // A flat, destructive plain-text action below the groups — vertically separated rather
    // than boxed in a card (the spec's styling note for infrequent page-level actions).
    let disconnect_button = gtk4::Button::builder()
        .label("Disconnect from the Server")
        .css_classes(["flat", "destructive-action"])
        .halign(gtk4::Align::Center)
        .margin_top(28)
        .sensitive(account.is_some())
        .build();
    disconnect_button.connect_clicked({
        let window = window.clone();
        let account = account.clone();
        let on_session_changed = on_session_changed.clone();
        move |_| {
            let Some(account) = account.clone() else { return };
            let window = window.clone();
            let on_session_changed = on_session_changed.clone();
            let pool = pool.clone();
            confirm(
                &window,
                &format!("Disconnect from {}?", host_of(&server.url)),
                &format!(
                    "{}'s listening progress and bookmarks stored on this device will be removed — \
                     everything on the server stays where it is.",
                    account.username
                ),
                "Disconnect",
                Rc::new(move || {
                    let pool = pool.clone();
                    let account_id = account.id.clone();
                    let on_session_changed = on_session_changed.clone();
                    glib::spawn_future_local(async move {
                        if let Err(err) = abs_core::accounts::sign_out(&pool, &account_id).await {
                            tracing::warn!(%err, "couldn't sign out");
                            return;
                        }
                        on_session_changed();
                    });
                }),
            );
        }
    });

    let root = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    root.append(&header);
    root.append(&page);
    root.append(&disconnect_button);

    ConnectionScreen {
        root: root.upcast(),
        #[cfg(test)]
        hooks: ConnectionHooks {
            url_row,
            info_button,
            back_button,
            disconnect_button,
        },
    }
}

/// The URL row's monospace subtitle — a one-off stylesheet, added once per process. The
/// selector targets the `AdwActionRow`'s internal subtitle label (CSS node `row`, style class
/// `subtitle`), which can't be styled from the widget API.
static MONO_CSS: std::sync::Once = std::sync::Once::new();
fn ensure_mono_css() {
    MONO_CSS.call_once(|| {
        let provider = gtk4::CssProvider::new();
        provider.load_from_data("row.mono-subtitle label.subtitle { font-family: monospace; }");
        gtk4::style_context_add_provider_for_display(
            &gtk4::gdk::Display::default().expect("a display for the app's css"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    use crate::test_support::pump_until;

    fn find_message_dialog() -> Option<gtk4::MessageDialog> {
        gtk4::Window::list_toplevels()
            .into_iter()
            .find_map(|window| window.downcast::<gtk4::MessageDialog>().ok())
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run_url_info_disconnect_and_back(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(crate::test_support::pool());
        let server_id = runtime.block_on(abs_storage::repo::servers::add(&pool, "https://library.example/abs")).unwrap();
        let account_id = runtime
            .block_on(abs_storage::repo::accounts::add(&pool, &server_id, "jane", "token", None))
            .unwrap();
        runtime.block_on(abs_storage::repo::accounts::set_active(&pool, &account_id)).unwrap();
        let server = runtime.block_on(abs_storage::repo::servers::get(&pool, &server_id)).unwrap();
        let account = runtime.block_on(abs_storage::repo::accounts::get(&pool, &account_id)).unwrap();

        let app_window = adw::ApplicationWindow::builder().build();
        let shell_root: gtk4::Widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0).upcast();
        app_window.set_content(Some(&shell_root));

        let session_changed = Rc::new(Cell::new(false));
        let on_session_changed: Rc<dyn Fn()> = {
            let session_changed = session_changed.clone();
            let pool = pool.clone();
            let window = app_window.clone();
            Rc::new(move || {
                session_changed.set(true);
                crate::application::show_main_or_welcome(
                    &window,
                    pool.clone(),
                    crate::test_support::test_paths(),
                    abs_core::settings::PlaybackSettings::default(),
                )
            })
        };

        let screen = build(pool.clone(), server, Some(account), &app_window, &shell_root, on_session_changed);
        app_window.set_content(Some(&screen.root));
        // Popovers only open in a mapped window (same as the shell scenario's focus check).
        app_window.present();
        pump_until(|| app_window.is_mapped(), Duration::from_secs(5));

        // The URL row: host as title, the full URL as the subtitle (a literal value being
        // confirmed), flagged for the monospace stylesheet.
        assert_eq!(screen.hooks.url_row.title(), "library.example");
        assert_eq!(screen.hooks.url_row.subtitle().as_deref(), Some("https://library.example/abs"));
        assert!(screen.hooks.url_row.has_css_class("mono-subtitle"));

        // The info button opens the popover explaining what this connection is used for.
        screen.hooks.info_button.popup();
        pump_until(
            || screen.hooks.info_button.popover().is_some_and(|p| p.is_visible()),
            Duration::from_secs(5),
        );

        // Back restores the shell exactly as it was — the captured widget, not a rebuild.
        screen.hooks.back_button.emit_clicked();
        pump_until(
            {
                let shell_root = shell_root.clone();
                let app_window = app_window.clone();
                move || app_window.content().as_ref() == Some(&shell_root)
            },
            Duration::from_secs(5),
        );

        // Disconnect: cancelling the confirmation changes nothing; confirming signs the
        // server's account out (the only one — so the shell hands over to Welcome).
        app_window.set_content(Some(&screen.root));
        screen.hooks.disconnect_button.emit_clicked();
        pump_until(|| find_message_dialog().is_some(), Duration::from_secs(5));
        find_message_dialog().unwrap().response(gtk4::ResponseType::Cancel);
        pump_until(|| find_message_dialog().is_none(), Duration::from_secs(5));
        assert!(
            runtime.block_on(abs_storage::repo::accounts::get_active(&pool)).unwrap().is_some(),
            "a cancelled disconnect must leave the session intact"
        );

        screen.hooks.disconnect_button.emit_clicked();
        pump_until(|| find_message_dialog().is_some(), Duration::from_secs(5));
        find_message_dialog().unwrap().response(gtk4::ResponseType::Ok);
        pump_until(|| session_changed.get(), Duration::from_secs(10));
        pump_until(|| find_message_dialog().is_none(), Duration::from_secs(5));
        assert!(
            runtime.block_on(abs_storage::repo::accounts::get_active(&pool)).unwrap().is_none(),
            "a confirmed disconnect must remove the account"
        );
    }
}
