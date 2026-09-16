//! SQLite-backed local storage: the on-disk schema, XDG path resolution, and one repository
//! module per aggregate (servers, accounts, libraries, items, chapters, progress, downloads,
//! settings). This crate owns every filesystem/database decision the app makes — `abs-core` and
//! the `app` UI never touch a path or write SQL directly.

pub mod db;
pub mod error;
pub mod models;
pub mod paths;
pub mod repo;

pub use db::connect_and_migrate;
pub use error::{Result, StorageError};
pub use paths::AppPaths;
