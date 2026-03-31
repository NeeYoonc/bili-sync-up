use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        add_column_if_missing(
            manager,
            "page",
            Page::Table,
            "file_size_bytes",
            ColumnDef::new(Page::FileSizeBytes).big_integer().null().to_owned(),
        )
        .await?;

        add_column_if_missing(
            manager,
            "page",
            Page::Table,
            "video_stream_size_bytes",
            ColumnDef::new(Page::VideoStreamSizeBytes)
                .big_integer()
                .null()
                .to_owned(),
        )
        .await?;

        add_column_if_missing(
            manager,
            "page",
            Page::Table,
            "audio_stream_size_bytes",
            ColumnDef::new(Page::AudioStreamSizeBytes)
                .big_integer()
                .null()
                .to_owned(),
        )
        .await?;

        add_column_if_missing(
            manager,
            "video",
            Video::Table,
            "total_file_size_bytes",
            ColumnDef::new(Video::TotalFileSizeBytes)
                .big_integer()
                .null()
                .to_owned(),
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_column_if_exists(
            manager,
            "video",
            Video::Table,
            "total_file_size_bytes",
            Video::TotalFileSizeBytes,
        )
        .await?;

        drop_column_if_exists(
            manager,
            "page",
            Page::Table,
            "audio_stream_size_bytes",
            Page::AudioStreamSizeBytes,
        )
        .await?;

        drop_column_if_exists(
            manager,
            "page",
            Page::Table,
            "video_stream_size_bytes",
            Page::VideoStreamSizeBytes,
        )
        .await?;

        drop_column_if_exists(manager, "page", Page::Table, "file_size_bytes", Page::FileSizeBytes).await
    }
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
enum Page {
    Table,
    FileSizeBytes,
    VideoStreamSizeBytes,
    AudioStreamSizeBytes,
}

#[derive(Iden, Clone)]
enum Video {
    Table,
    TotalFileSizeBytes,
}
