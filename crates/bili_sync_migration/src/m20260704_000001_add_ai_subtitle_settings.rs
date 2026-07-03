use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in SOURCE_TABLES {
            add_column_if_missing(
                manager,
                table.name,
                table.iden,
                "download_ai_subtitle",
                ColumnDef::new(AiSubtitleColumn::DownloadAiSubtitle)
                    .boolean()
                    .not_null()
                    .default(true)
                    .to_owned(),
            )
            .await?;

            add_column_if_missing(
                manager,
                table.name,
                table.iden,
                "ai_subtitle_language",
                ColumnDef::new(AiSubtitleColumn::AiSubtitleLanguage)
                    .string()
                    .not_null()
                    .default("zh-CN")
                    .to_owned(),
            )
            .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in SOURCE_TABLES {
            drop_column_if_exists(
                manager,
                table.name,
                table.iden,
                "ai_subtitle_language",
                AiSubtitleColumn::AiSubtitleLanguage,
            )
            .await?;

            drop_column_if_exists(
                manager,
                table.name,
                table.iden,
                "download_ai_subtitle",
                AiSubtitleColumn::DownloadAiSubtitle,
            )
            .await?;
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
struct SourceTable {
    name: &'static str,
    iden: SourceTableIden,
}

const SOURCE_TABLES: [SourceTable; 5] = [
    SourceTable {
        name: "collection",
        iden: SourceTableIden::Collection,
    },
    SourceTable {
        name: "favorite",
        iden: SourceTableIden::Favorite,
    },
    SourceTable {
        name: "submission",
        iden: SourceTableIden::Submission,
    },
    SourceTable {
        name: "watch_later",
        iden: SourceTableIden::WatchLater,
    },
    SourceTable {
        name: "video_source",
        iden: SourceTableIden::VideoSource,
    },
];

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

#[derive(Iden, Clone, Copy)]
enum SourceTableIden {
    Collection,
    Favorite,
    Submission,
    WatchLater,
    VideoSource,
}

#[derive(Iden)]
enum AiSubtitleColumn {
    DownloadAiSubtitle,
    AiSubtitleLanguage,
}
