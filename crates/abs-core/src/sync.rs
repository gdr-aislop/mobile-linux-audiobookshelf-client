//! Pulls server state into the local cache. Only covers what `abs-api` actually exposes
//! (libraries) — item/chapter sync needs the hand-written calls described in
//! `docs/api/README.md`'s "Handling endpoints the spec doesn't cover" and aren't implemented yet.

use abs_storage::repo::libraries::{self, UpsertLibrary};
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

#[cfg(test)]
mod tests {
    use super::*;
    use abs_storage::connect_and_migrate;
    use abs_storage::repo::servers;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn pool_with_server(server_url: &str) -> (SqlitePool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        let server_id = servers::add(&pool, server_url).await.unwrap();
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
}
