#[path = "support/fake_provider.rs"]
mod fake_provider;

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use cursor_server::{
    control::{api_router, ControlService},
    network::NetworkClients,
    store::{DesktopSettings, Store, ThemeId},
};
use tower::ServiceExt;

fn router(store: Store) -> Router {
    api_router(
        ControlService::new(
            store.clone(),
            Arc::new(fake_provider::FakeProvider::default()),
            NetworkClients::new(store),
            "theme-test".into(),
        )
        .unwrap(),
    )
}

async fn read_theme(router: Router, origin: &str) -> String {
    let response = router
        .oneshot(
            Request::get("/__byok-api__/api/settings/theme")
                .header(header::ORIGIN, origin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        origin
    );
    String::from_utf8(to_bytes(response.into_body(), 4096).await.unwrap().to_vec()).unwrap()
}

#[tokio::test]
async fn theme_persists_across_new_connections_and_origins_without_touching_other_settings() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", directory.path().join("theme.db").display());
    let store = Store::connect(&url).await.unwrap();
    assert_eq!(store.theme().await.unwrap(), ThemeId::Dark);
    let desktop = DesktopSettings {
        silent_start: true,
        show_dock_icon: false,
    };
    store.set_desktop_settings(desktop).await.unwrap();
    sqlx::query("INSERT INTO service_settings(setting_key,value_json,updated_at_ms) VALUES ('unrelated_setting','true',0)")
        .execute(store.pool()).await.unwrap();

    for (theme, encoded) in [
        (ThemeId::Light, "\"default-light\""),
        (ThemeId::Dark, "\"default-dark\""),
    ] {
        let app = router(store.clone());
        let response = app
            .clone()
            .oneshot(
                Request::put("/__byok-api__/api/settings/theme")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(encoded))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(store.theme().await.unwrap(), theme);
        assert_eq!(
            read_theme(app.clone(), "http://127.0.0.1:54664").await,
            encoded
        );
        assert_eq!(read_theme(app, "http://127.0.0.1:63109").await, encoded);
        let reopened = Store::connect(&url).await.unwrap();
        assert_eq!(reopened.theme().await.unwrap(), theme);
        assert_eq!(reopened.desktop_settings().await.unwrap(), desktop);
        let retained: String = sqlx::query_scalar(
            "SELECT value_json FROM service_settings WHERE setting_key='unrelated_setting'",
        )
        .fetch_one(reopened.pool())
        .await
        .unwrap();
        assert_eq!(retained, "true");
        reopened.pool().close().await;
    }
    store.set_theme(ThemeId::Light).await.unwrap();
    store.pool().close().await;
    let restarted = Store::connect(&url).await.unwrap();
    assert_eq!(
        read_theme(router(restarted), "http://127.0.0.1:63110").await,
        "\"default-light\""
    );
}

#[tokio::test]
async fn invalid_theme_requests_do_not_overwrite_the_saved_preference() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::connect(&format!(
        "sqlite://{}",
        directory.path().join("invalid.db").display()
    ))
    .await
    .unwrap();
    store.set_theme(ThemeId::Light).await.unwrap();
    let app = router(store.clone());
    for invalid in [
        "\"light\"",
        "null",
        "{}",
        "\"\"",
        "\"default-light<script>\"",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::put("/__byok-api__/api/settings/theme")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(invalid))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        assert_eq!(store.theme().await.unwrap(), ThemeId::Light);
    }
}

#[tokio::test]
async fn unreadable_theme_is_an_error_not_an_implicit_reset() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::connect(&format!(
        "sqlite://{}",
        directory.path().join("unreadable.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("INSERT INTO service_settings(setting_key,value_json,updated_at_ms) VALUES ('theme','\"invalid\"',0)")
        .execute(store.pool()).await.unwrap();
    assert!(store.theme().await.is_err());
    let response = router(store)
        .oneshot(
            Request::get("/__byok-api__/api/settings/theme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
