//! Removing the footer must also work for a database already opened by 1.0.9.
use std::borrow::Cow;

use cursor_server::store::Store;
use sqlx::{
    migrate::{Migration, Migrator},
    sqlite::SqlitePoolOptions,
};

async fn assert_footer_removed(store: &Store) {
    let tables: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name='cursor_footer_runs'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    let settings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM service_settings WHERE setting_key IN ('cursor_reply_footer','cursor_reply_footer_token')")
        .fetch_one(store.pool()).await.unwrap();
    assert_eq!(tables, 0);
    assert_eq!(settings, 0);
}

#[tokio::test]
async fn removing_footer_cleans_109_state_and_preserves_other_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("upgrade.db");
    std::fs::File::create(&path).unwrap();
    let url = format!("sqlite://{}", path.display());
    let pool = SqlitePoolOptions::new().connect(&url).await.unwrap();
    let all = sqlx::migrate!("./migrations");
    let old = Migrator {
        migrations: Cow::Owned(
            all.iter()
                .filter(|m| m.version <= 10)
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
    };
    old.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO cursor_footer_runs VALUES ('old-run',NULL)")
        .execute(&pool)
        .await
        .unwrap();
    for (key, value) in [
        ("cursor_reply_footer", "true"),
        ("cursor_reply_footer_token", "\"old-test-token\""),
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
    assert_footer_removed(&store).await;
    let retained: String = sqlx::query_scalar(
        "SELECT value_json FROM service_settings WHERE setting_key='unrelated_setting'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(retained, "true");
}

#[tokio::test]
async fn fresh_database_does_not_keep_footer_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::connect(&format!(
        "sqlite://{}",
        dir.path().join("fresh.db").display()
    ))
    .await
    .unwrap();
    assert_footer_removed(&store).await;
}
