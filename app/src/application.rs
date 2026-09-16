//! The GTK application shell. Kept deliberately minimal at this stage — the real screens
//! (`docs/design/ui-spec.md`'s Home, Library browse, Item detail, Player, Downloads, Settings,
//! Connection) are a later pass. What's here proves the wiring: the app actually opens the local
//! database, loads settings through `abs-core`, and shows a window built from `AdwApplicationWindow`
//! and `AdwHeaderBar` — the libadwaita-1.2-compatible composition described in the architecture
//! plan (no `AdwToolbarView`, which is 1.4+).

use abs_storage::paths::APP_ID;
use adw::prelude::*;
use sqlx::SqlitePool;

/// Everything the UI needs a handle to. Constructed once in `main` after the async setup step
/// (DB connect + migrate) completes, then moved into the `connect_activate` closure.
///
/// `pool` isn't read yet — no screen exists to query it — but it's already threaded through so
/// the next pass (Home/Library/etc.) doesn't need to change this plumbing, only use it.
#[allow(dead_code)]
pub struct AppState {
    pub pool: SqlitePool,
    pub paths: abs_storage::AppPaths,
}

pub fn build_application(state: AppState) -> adw::Application {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        build_window(app, &state);
    });

    app
}

fn build_window(app: &adw::Application, state: &AppState) {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Audiobookshelf", "")));

    let status_text = format!(
        "Local database ready at:\n{}",
        state.paths.db_path().display()
    );
    let status = gtk4::Label::builder()
        .label(&status_text)
        .wrap(true)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    // Pre-1.4 composition: a plain vertical GtkBox holding the header bar and content, rather
    // than AdwToolbarView (which doesn't exist in libadwaita 1.2).
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&status);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Audiobookshelf")
        .default_width(390)
        .default_height(760)
        .content(&content)
        .build();

    window.present();
}
