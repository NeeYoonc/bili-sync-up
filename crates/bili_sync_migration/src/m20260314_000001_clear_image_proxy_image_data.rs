use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !table_has_column(manager, "image_proxy_cache", "image_data").await? {
            return Ok(());
        }

        let backend = manager.get_connection().get_database_backend();
        let conn = manager.get_connection();

        // SQLite 删除列兼容方案：重建表。
        conn.execute(Statement::from_string(
            backend,
            r#"
            CREATE TABLE IF NOT EXISTS image_proxy_cache_new (
                cache_key TEXT NOT NULL PRIMARY KEY,
                url TEXT NOT NULL,
                content_type TEXT NOT NULL,
                etag TEXT NOT NULL,
                cached_at_unix BIGINT NOT NULL,
                expires_at_unix BIGINT NOT NULL,
                updated_at_unix BIGINT NOT NULL
            )
            "#
            .to_string(),
        ))
        .await?;

        conn.execute(Statement::from_string(
            backend,
            r#"
            INSERT INTO image_proxy_cache_new (
                cache_key, url, content_type, etag, cached_at_unix, expires_at_unix, updated_at_unix
            )
            SELECT
                cache_key, url, content_type, etag, cached_at_unix, expires_at_unix, updated_at_unix
            FROM image_proxy_cache
            "#
            .to_string(),
        ))
        .await?;

        conn.execute(Statement::from_string(
            backend,
            "DROP TABLE image_proxy_cache".to_string(),
        ))
        .await?;
        conn.execute(Statement::from_string(
            backend,
            "ALTER TABLE image_proxy_cache_new RENAME TO image_proxy_cache".to_string(),
        ))
        .await?;

        conn.execute(Statement::from_string(
            backend,
            "CREATE INDEX IF NOT EXISTS idx_image_proxy_cache_expires_at ON image_proxy_cache(expires_at_unix)"
                .to_string(),
        ))
        .await?;
        conn.execute(Statement::from_string(
            backend,
            "CREATE INDEX IF NOT EXISTS idx_image_proxy_cache_updated_at ON image_proxy_cache(updated_at_unix)"
                .to_string(),
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if table_has_column(manager, "image_proxy_cache", "image_data").await? {
            return Ok(());
        }

        let backend = manager.get_connection().get_database_backend();
        manager
            .get_connection()
            .execute(Statement::from_string(
                backend,
                "ALTER TABLE image_proxy_cache ADD COLUMN image_data BLOB NOT NULL DEFAULT X''".to_string(),
            ))
            .await?;
        Ok(())
    }
}

async fn table_has_column(manager: &SchemaManager<'_>, table_name: &str, column_name: &str) -> Result<bool, DbErr> {
    let backend = manager.get_connection().get_database_backend();
    let sql = format!(
        "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
        table_name.replace('\'', "''"),
        column_name.replace('\'', "''")
    );
    let result = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql))
        .await?;
    Ok(result.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0) >= 1)
}
