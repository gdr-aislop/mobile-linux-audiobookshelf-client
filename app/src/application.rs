//! The GTK application shell. Kept deliberately minimal at this stage — the real screens
//! (`docs/design/ui-spec.md`'s Home, Library browse, Item detail, Player, Downloads, Settings,
//! Connection) are a later pass, beyond the Welcome/login screen implemented here. What's here
//! proves the wiring: the app opens the local database, decides whether to show the login screen
//! or a signed-in placeholder based on whether an account is active, and shows a window built
//! from `AdwApplicationWindow` and `AdwHeaderBar` — the libadwaita-1.2-compatible composition
//! described in the architecture plan (no `AdwToolbarView`, which is 1.4+).

use abs_storage::paths::APP_ID;
use adw::prelude::*;
use sqlx::SqlitePool;

use crate::screens;

/// Everything the UI needs a handle to. Constructed once in `main` after the async setup step
/// (DB connect + migrate, active-account lookup) completes, then moved into the
/// `connect_activate` closure.
pub struct AppState {
    pub pool: SqlitePool,
    pub paths: abs_storage::AppPaths,
    pub active_account: Option<abs_storage::models::Account>,
}

pub fn build_application(state: AppState) -> adw::Application {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        build_window(app, &state);
    });

    app
}

fn build_window(app: &adw::Application, state: &AppState) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Audiobookshelf")
        .default_width(390)
        .default_height(760)
        .build();

    match &state.active_account {
        None => {
            let pool = state.pool.clone();
            let window_for_callback = window.clone();
            let screen = screens::welcome::build(pool, move |added| {
                window_for_callback.set_content(Some(&build_signed_in_placeholder(&added)));
            });
            window.set_content(Some(&screen.root));
        }
        Some(account) => {
            window.set_content(Some(&build_signed_in_content(state, &account.username)));
        }
    }

    window.present();
}

/// Shown once an account is active on startup. No real Home screen yet — see the module doc.
fn build_signed_in_content(state: &AppState, username: &str) -> gtk4::Widget {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Audiobookshelf", "")));

    let status_text = format!(
        "Signed in as {username}.\nLocal database at:\n{}",
        state.paths.db_path().display()
    );
    build_placeholder_content(&header, &status_text)
}

/// Shown right after a successful login, before the window has any other state to reference —
/// same placeholder shape as `build_signed_in_content`, just without needing `&AppState`.
fn build_signed_in_placeholder(added: &abs_core::accounts::AddedAccount) -> gtk4::Widget {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Audiobookshelf", "")));

    let status_text = format!(
        "Signed in.\nserver_id: {}\naccount_id: {}",
        added.server_id, added.account_id
    );
    build_placeholder_content(&header, &status_text)
}

fn build_placeholder_content(header: &adw::HeaderBar, status_text: &str) -> gtk4::Widget {
    let status = gtk4::Label::builder()
        .label(status_text)
        .wrap(true)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    // Pre-1.4 composition: a plain vertical GtkBox holding the header bar and content, rather
    // than AdwToolbarView (which doesn't exist in libadwaita 1.2).
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content.append(header);
    content.append(&status);
    content.upcast()
}
