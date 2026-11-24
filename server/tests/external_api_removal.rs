//! Removing the external API must not affect the Cursor inference or management API.
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{borrow::Cow, sync::Arc};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use cursor_server::{
    api,
    control::{api_router, ControlService},
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        transport::TransportRegistry,
    },
    network::NetworkClients,
    store::Store,
};
use sqlx::{
    migrate::{Migration, Migrator},
    sqlite::SqlitePoolOptions,
};
use tower::ServiceExt;

#[tokio::test]
async fn removed_endpoints_return_not_found_while_cursor_and_model_management_remain() {
    let (_dir, store) = fixtures::temp_store().await;
    let provider = Arc::new(fake_provider::FakeProvider::default());
    let clients = NetworkClients::new(store.clone());
    let registry = TransportRegistry::new(
        store.clone(),
        provider.clone(),
        PromptCompiler::new(PromptAssets::embedded().unwrap()),
    );
    let service =
        ControlService::new(store, provider, clients.clone(), "removal-test".into()).unwrap();
    let router = api::router(registry, clients)
        .unwrap()
        .merge(api_router(service));
    for (method, path) in [
        (Method::GET, "/byok/v1/models"),
        (Method::POST, "/byok/v1/chat/completions"),
        (Method::POST, "/byok/v1/responses"),
        (Method::POST, "/byok/v1/messages"),
        (Method::GET, "/__byok-api__/api/settings/external-api"),
        (Method::PUT, "/__byok-api__/api/settings/external-api"),
        (
            Method::GET,
            "/__byok-api__/api/settings/external-api/runtime",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    for (path, status) in [
        ("/__byok-api__/healthz", StatusCode::NO_CONTENT),
        ("/__byok-api__/api/models", StatusCode::OK),
        ("/__byok-api__/api/settings/ports", StatusCode::OK),
    ] {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
    }
}

#[tokio::test]
async fn upgrading_removes_external_api_credentials_and_preserves_other_settings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("upgrade.db");
    std::fs::File::create(&path).unwrap();
    let url = format!("sqlite://{}", path.display());
    let pool = SqlitePoolOptions::new().connect(&url).await.unwrap();
    let all = sqlx::migrate!("./migrations");
    Migrator {
        migrations: Cow::Owned(
            all.iter()
                .filter(|m| m.version <= 11)
                .map(|m| {
                    Migration::new(
                        m.version,
                        m.description.clone(),
                        m.migration_type,
                        Cow::Owned(m.sql.replace("\r\n", "\n")),
                        m.no_tx,
                    )
                })
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
    .run(&pool)
    .await
    .unwrap();
    for (key, value) in [
        (
            "external_api",
            r#"{"enabled":true,"api_key":"obsolete-test-key","allowed_endpoints":["models"]}"#,
        ),
        ("theme", r#""default-light""#),
        ("unrelated_setting", "true"),
    ] {
        sqlx::query(
            "INSERT INTO service_settings(setting_key,value_json,updated_at_ms) VALUES (?,?,0)",
        )
        .bind(key)
        .bind(value)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;
    let store = Store::connect(&url).await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_settings WHERE setting_key='external_api'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        store.theme().await.unwrap(),
        cursor_server::store::ThemeId::Light
    );
    let retained: String = sqlx::query_scalar(
        "SELECT value_json FROM service_settings WHERE setting_key='unrelated_setting'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(retained, "true");
}
