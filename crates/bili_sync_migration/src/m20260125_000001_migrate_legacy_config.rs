use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;
use serde_json::Value;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // 仅在存在旧版 config 表且新配置表为空时执行迁移
        if !table_exists(db, "config").await? || !table_exists(db, "config_items").await? {
            return Ok(());
        }

        let existing_count = count_rows(db, "config_items").await?;
        if existing_count > 0 {
            return Ok(());
        }

        let config_json = match load_legacy_config_json(db).await? {
            Some(value) => value,
            None => return Ok(()),
        };

        let Value::Object(map) = config_json else {
            return Ok(());
        };

        for (key, value) in map {
            let value_json =
                serde_json::to_string(&value).map_err(|e| DbErr::Custom(format!("序列化旧配置失败: {}", e)))?;
            upsert_config_item(db, &key, &value_json).await?;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // 仅迁移旧配置到新表，不做回滚
        Ok(())
    }
}

async fn load_legacy_config_json<C: ConnectionTrait>(db: &C) -> Result<Option<Value>, DbErr> {
    use sea_orm::{DatabaseBackend, Statement};

    let stmt = Statement::from_string(
        DatabaseBackend::Sqlite,
        "SELECT data FROM config WHERE id = 1 LIMIT 1".to_string(),
    );
    let row = db.query_one(stmt).await?;
    let Some(row) = row else {
        return Ok(None);
    };

    let data: String = row.try_get("", "data")?;
    let value: Value = serde_json::from_str(&data).map_err(|e| DbErr::Custom(format!("解析旧配置失败: {}", e)))?;
    Ok(Some(value))
}

async fn upsert_config_item<C: ConnectionTrait>(db: &C, key: &str, value_json: &str) -> Result<(), DbErr> {
    use sea_orm::{DatabaseBackend, Statement};

    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "INSERT OR REPLACE INTO config_items (key_name, value_json, updated_at) VALUES (?, ?, CURRENT_TIMESTAMP)",
        vec![key.into(), value_json.into()],
    );
    db.execute(stmt).await?;
    Ok(())
}

async fn count_rows<C: ConnectionTrait>(db: &C, table_name: &str) -> Result<i64, DbErr> {
    use sea_orm::{DatabaseBackend, Statement};

    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT COUNT(*) as count FROM sqlite_master WHERE type='table' AND name=?",
        vec![table_name.into()],
    );
    let exists_row = db.query_one(stmt).await?;
    if exists_row.is_none() {
        return Ok(0);
    }

    let stmt = Statement::from_string(
        DatabaseBackend::Sqlite,
        format!("SELECT COUNT(*) as count FROM {}", table_name),
    );
    let row = db.query_one(stmt).await?;
    let Some(row) = row else {
        return Ok(0);
    };
    let count: i64 = row.try_get("", "count")?;
    Ok(count)
}

async fn table_exists<C: ConnectionTrait>(db: &C, table_name: &str) -> Result<bool, DbErr> {
    use sea_orm::{DatabaseBackend, Statement};

    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=? LIMIT 1",
        vec![table_name.into()],
    );
    let result = db.query_one(stmt).await?;
    Ok(result.is_some())
}
