//! Pulls server state into the local cache.

use abs_storage::repo::items::{self, UpsertItem};
use abs_storage::repo::libraries::{self, UpsertLibrary};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::error::{CoreError, Result};

/// Fetch the server's libraries and upsert them into local storage, returning how many were
/// synced. Libraries the server no longer reports are left in place rather than deleted — a
/// transient fetch failure or a server-side hiccup shouldn't nuke the local cache of a library a
/// user has downloaded content from.
pub async fn sync_libraries(pool: &SqlitePool, api: &abs_api::Client, server_id: &str) -> Result<usize> {
    // `progenitor`'s generated `Error<E>` type differs per operation (its error-body type
    // parameter), so there's no single `From` impl to lean on here — stringify instead.
    let response = api
        .get_libraries()
        .await
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;
    let libraries = response.into_inner().libraries;

    let mut synced = 0;
    for library in libraries {
        let Some(id) = library.id else { continue };
        let Some(name) = library.name else { continue };
        let Some(media_type) = library.media_type else { continue };

        libraries::upsert(
            pool,
            UpsertLibrary {
                id: &id.to_string(),
                server_id,
                name: &name,
                media_type: &media_type,
                icon: library.icon.as_deref(),
                display_order: library.display_order.unwrap_or(1),
            },
        )
        .await?;
        synced += 1;
    }

    Ok(synced)
}

/// Fetch one library's items (with media metadata) and upsert them into local storage, returning
/// how many were synced. Uses `abs_api::Client::get_library_items_with_media` — a hand-written
/// extension, not the generated `get_library_items` — because the generated response type's
/// `results` element (`LibraryItemBase`) has no `media` field at all, a real gap in the vendored
/// spec's schema for this endpoint (see `third_party/audiobookshelf-openapi/README.md`'s "Known
/// gaps"). Items the server no longer reports are left in place, same rationale as
/// `sync_libraries`: a transient failure shouldn't nuke a library a user has downloads from.
pub async fn sync_items_for_library(
    pool: &SqlitePool,
    api: &abs_api::Client,
    server_id: &str,
    library_id: &str,
) -> Result<usize> {
    let remote_items = api
        .get_library_items_with_media(library_id)
        .await
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;

    let mut synced = 0;
    for item in remote_items {
        let added_at = DateTime::<Utc>::from_timestamp_millis(item.added_at_ms).unwrap_or_else(Utc::now);

        items::upsert(
            pool,
            UpsertItem {
                id: &item.id,
                server_id,
                library_id,
                title: &item.title,
                author: item.author.as_deref(),
                narrator: item.narrator.as_deref(),
                description: item.description.as_deref(),
                duration_seconds: item.duration_seconds,
                added_at,
            },
        )
        .await?;
        synced += 1;
    }

    Ok(synced)
}

/// One network round-trip's worth of work for a freshly-opened Home screen: sync this server's
/// libraries, then every library's items. Builds its own authenticated `abs_api::Client` from
/// `server_url`/`access_token` so callers (the `app` crate) never construct one themselves — the
/// same boundary `accounts::add_server_and_login` already draws. `access_token` is the account's
/// stored token; every one of these calls is authenticated (unlike `login`), and
/// `Client::with_bearer_token` is what actually attaches it — a plain `Client::new` sends no auth
/// at all and every call comes back `401` (caught live against the real demo server, not just in
/// theory).
pub async fn sync_all(pool: &SqlitePool, server_url: &str, server_id: &str, access_token: &str) -> Result<()> {
    let api = abs_api::Client::with_bearer_token(server_url, access_token)
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;
    sync_libraries(pool, &api, server_id).await?;
    for library in libraries::list_for_server(pool, server_id).await? {
        sync_items_for_library(pool, &api, server_id, &library.id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use abs_storage::connect_and_migrate;
    use abs_storage::repo::servers;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn pool_with_server(server_url: &str) -> (SqlitePool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        let server_id = servers::add(&pool, server_url).await.unwrap();
        (pool, server_id)
    }

    /// Item rows have a foreign key on `library_id`, so item-sync tests need a real library row
    /// to attach to first — `sync_items_for_library` itself never creates one (that's
    /// `sync_libraries`'/`sync_all`'s job).
    async fn pool_with_server_and_library(server_url: &str, library_id: &str) -> (SqlitePool, String) {
        let (pool, server_id) = pool_with_server(server_url).await;
        libraries::upsert(
            &pool,
            UpsertLibrary {
                id: library_id,
                server_id: &server_id,
                name: "Audiobooks",
                media_type: "book",
                icon: None,
                display_order: 1,
            },
        )
        .await
        .unwrap();
        (pool, server_id)
    }

    #[tokio::test]
    async fn sync_libraries_upserts_every_returned_library() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraries": [
                    {
                        "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b",
                        "name": "Audiobooks",
                        "mediaType": "book",
                        "icon": "audiobookshelf",
                        "displayOrder": 1,
                    },
                    {
                        "id": "b2a1e9c0-4a4f-4dd6-8be0-e615d233185c",
                        "name": "Podcasts",
                        "mediaType": "podcast",
                        "icon": "podcast",
                        "displayOrder": 2,
                    },
                ]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server(&mock_server.uri()).await;
        let api = abs_api::Client::new(&mock_server.uri());

        let synced = sync_libraries(&pool, &api, &server_id).await.unwrap();
        assert_eq!(synced, 2);

        let stored = libraries::list_for_server(&pool, &server_id).await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].name, "Audiobooks");
        assert_eq!(stored[1].name, "Podcasts");
    }

    #[tokio::test]
    async fn sync_libraries_running_twice_does_not_duplicate() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraries": [{
                    "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b",
                    "name": "Audiobooks",
                    "mediaType": "book",
                    "displayOrder": 1,
                }]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server(&mock_server.uri()).await;
        let api = abs_api::Client::new(&mock_server.uri());

        sync_libraries(&pool, &api, &server_id).await.unwrap();
        sync_libraries(&pool, &api, &server_id).await.unwrap();

        assert_eq!(libraries::list_for_server(&pool, &server_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sync_libraries_skips_entries_missing_required_fields() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraries": [
                    { "name": "No ID", "mediaType": "book" },
                    {
                        "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b",
                        "name": "Valid Library",
                        "mediaType": "book",
                    },
                ]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server(&mock_server.uri()).await;
        let api = abs_api::Client::new(&mock_server.uri());

        let synced = sync_libraries(&pool, &api, &server_id).await.unwrap();
        assert_eq!(synced, 1, "only the well-formed library should be synced");
    }

    #[tokio::test]
    async fn sync_libraries_propagates_server_errors() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server(&mock_server.uri()).await;
        let api = abs_api::Client::new(&mock_server.uri());

        let result = sync_libraries(&pool, &api, &server_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sync_libraries_defaults_missing_display_order_to_one() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraries": [{
                    "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b",
                    "name": "Audiobooks",
                    "mediaType": "book",
                }]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server(&mock_server.uri()).await;
        let api = abs_api::Client::new(&mock_server.uri());

        sync_libraries(&pool, &api, &server_id).await.unwrap();

        let stored = libraries::list_for_server(&pool, &server_id).await.unwrap();
        assert_eq!(stored[0].display_order, 1);
    }

    fn item_json(id: &str, title: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "addedAt": 1_700_000_000_000i64,
            "media": {
                "duration": 3600.0,
                "metadata": { "title": title, "authorName": "Andy Weir" }
            }
        })
    }

    #[tokio::test]
    async fn sync_items_for_library_upserts_every_returned_item() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/lib-1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [item_json("item-1", "Project Hail Mary"), item_json("item-2", "Dune")]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server_and_library(&mock_server.uri(), "lib-1").await;
        let api = abs_api::Client::new(&mock_server.uri());

        let synced = sync_items_for_library(&pool, &api, &server_id, "lib-1").await.unwrap();
        assert_eq!(synced, 2);

        let stored = items::list_for_library(&pool, &server_id, "lib-1").await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].author.as_deref(), Some("Andy Weir"));
    }

    #[tokio::test]
    async fn sync_items_for_library_running_twice_does_not_duplicate() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/lib-1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [item_json("item-1", "Project Hail Mary")]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server_and_library(&mock_server.uri(), "lib-1").await;
        let api = abs_api::Client::new(&mock_server.uri());

        sync_items_for_library(&pool, &api, &server_id, "lib-1").await.unwrap();
        sync_items_for_library(&pool, &api, &server_id, "lib-1").await.unwrap();

        let stored = items::list_for_library(&pool, &server_id, "lib-1").await.unwrap();
        assert_eq!(stored.len(), 1);
    }

    #[tokio::test]
    async fn sync_items_for_library_skips_entries_missing_media_or_title() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/lib-1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "id": "no-media" },
                    item_json("item-1", "Valid Item"),
                ]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server_and_library(&mock_server.uri(), "lib-1").await;
        let api = abs_api::Client::new(&mock_server.uri());

        let synced = sync_items_for_library(&pool, &api, &server_id, "lib-1").await.unwrap();
        assert_eq!(synced, 1, "only the well-formed item should be synced");
    }

    #[tokio::test]
    async fn sync_items_for_library_propagates_server_errors() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/lib-1/items"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server_and_library(&mock_server.uri(), "lib-1").await;
        let api = abs_api::Client::new(&mock_server.uri());

        let result = sync_items_for_library(&pool, &api, &server_id, "lib-1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sync_items_for_library_defaults_missing_duration_to_zero() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/lib-1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{
                    "id": "item-1",
                    "media": { "metadata": { "title": "No Duration" } }
                }]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server_and_library(&mock_server.uri(), "lib-1").await;
        let api = abs_api::Client::new(&mock_server.uri());

        sync_items_for_library(&pool, &api, &server_id, "lib-1").await.unwrap();

        let stored = items::list_for_library(&pool, &server_id, "lib-1").await.unwrap();
        assert_eq!(stored[0].duration_seconds, 0.0);
    }

    #[tokio::test]
    async fn sync_all_syncs_libraries_then_every_librarys_items() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraries": [
                    { "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b", "name": "Audiobooks", "mediaType": "book" },
                    { "id": "b2a1e9c0-4a4f-4dd6-8be0-e615d233185c", "name": "Podcasts", "mediaType": "podcast" },
                ]
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/e4bb1afb-4a4f-4dd6-8be0-e615d233185b/items"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [item_json("item-1", "Project Hail Mary")]
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/b2a1e9c0-4a4f-4dd6-8be0-e615d233185c/items"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [item_json("item-2", "Some Podcast Episode")]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id) = pool_with_server(&mock_server.uri()).await;

        // Regression coverage: `sync_all` used to build an unauthenticated `abs_api::Client`,
        // which every one of these mocks (each requiring the Authorization header) would reject —
        // caught live against the real demo server as a blanket 401, not just here.
        sync_all(&pool, &mock_server.uri(), &server_id, "test-token").await.unwrap();

        assert_eq!(libraries::list_for_server(&pool, &server_id).await.unwrap().len(), 2);
        assert_eq!(
            items::list_for_library(&pool, &server_id, "e4bb1afb-4a4f-4dd6-8be0-e615d233185b")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            items::list_for_library(&pool, &server_id, "b2a1e9c0-4a4f-4dd6-8be0-e615d233185c")
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
