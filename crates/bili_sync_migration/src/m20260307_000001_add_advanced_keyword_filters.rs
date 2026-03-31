use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        add_filter_columns(manager, "collection", Collection::Table).await?;
        add_filter_columns(manager, "favorite", Favorite::Table).await?;
        add_filter_columns(manager, "submission", Submission::Table).await?;
        add_filter_columns(manager, "watch_later", WatchLater::Table).await?;
        add_filter_columns(manager, "video_source", VideoSource::Table).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_filter_columns(manager, "collection", Collection::Table).await?;
        drop_filter_columns(manager, "favorite", Favorite::Table).await?;
        drop_filter_columns(manager, "submission", Submission::Table).await?;
        drop_filter_columns(manager, "watch_later", WatchLater::Table).await?;
        drop_filter_columns(manager, "video_source", VideoSource::Table).await?;
        Ok(())
    }
}

async fn add_filter_columns<T>(manager: &SchemaManager<'_>, table_name: &str, table: T) -> Result<(), DbErr>
where
    T: IntoIden + Clone + 'static,
{
    add_column_if_missing(
        manager,
        table_name,
        table.clone(),
        "min_duration_seconds",
        ColumnDef::new(AdvancedFilter::MinDurationSeconds)
            .integer()
            .null()
            .to_owned(),
    )
    .await?;
    add_column_if_missing(
        manager,
        table_name,
        table.clone(),
        "max_duration_seconds",
        ColumnDef::new(AdvancedFilter::MaxDurationSeconds)
            .integer()
            .null()
            .to_owned(),
    )
    .await?;
    add_column_if_missing(
        manager,
        table_name,
        table.clone(),
        "published_after",
        ColumnDef::new(AdvancedFilter::PublishedAfter)
            .string()
            .null()
            .to_owned(),
    )
    .await?;
    add_column_if_missing(
        manager,
        table_name,
        table,
        "published_before",
        ColumnDef::new(AdvancedFilter::PublishedBefore)
            .string()
            .null()
            .to_owned(),
    )
    .await
}

async fn drop_filter_columns<T>(manager: &SchemaManager<'_>, table_name: &str, table: T) -> Result<(), DbErr>
where
    T: IntoIden + Clone + 'static,
{
    drop_column_if_exists(
        manager,
        table_name,
        table.clone(),
        "published_before",
        AdvancedFilter::PublishedBefore,
    )
    .await?;
    drop_column_if_exists(
        manager,
        table_name,
        table.clone(),
        "published_after",
        AdvancedFilter::PublishedAfter,
    )
    .await?;
    drop_column_if_exists(
        manager,
        table_name,
        table.clone(),
        "max_duration_seconds",
        AdvancedFilter::MaxDurationSeconds,
    )
    .await?;
    drop_column_if_exists(
        manager,
        table_name,
        table,
        "min_duration_seconds",
        AdvancedFilter::MinDurationSeconds,
    )
    .await
}

async fn add_column_if_missing<T>(
    manager: &SchemaManager<'_>,
    table_name: &str,
    table: T,
    column_name: &str,
    column_def: ColumnDef,
) -> Result<(), DbErr>
where
    T: IntoIden + Clone + 'static,
{
    if !table_has_column(manager, table_name, column_name).await? {
        manager
            .alter_table(Table::alter().table(table).add_column(column_def).to_owned())
            .await?;
    }

    Ok(())
}

async fn drop_column_if_exists<T, C>(
    manager: &SchemaManager<'_>,
    table_name: &str,
    table: T,
    column_name: &str,
    column: C,
) -> Result<(), DbErr>
where
    T: IntoIden + Clone + 'static,
    C: IntoIden + 'static,
{
    if table_has_column(manager, table_name, column_name).await? {
        manager
            .alter_table(Table::alter().table(table).drop_column(column).to_owned())
            .await?;
    }

    Ok(())
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

#[derive(Iden, Clone)]
enum Collection {
    Table,
}

#[derive(Iden, Clone)]
enum Favorite {
    Table,
}

#[derive(Iden, Clone)]
enum Submission {
    Table,
}

#[derive(Iden, Clone)]
enum WatchLater {
    Table,
}

#[derive(Iden, Clone)]
enum VideoSource {
    Table,
}

#[derive(Iden)]
enum AdvancedFilter {
    MinDurationSeconds,
    MaxDurationSeconds,
    PublishedAfter,
    PublishedBefore,
}
