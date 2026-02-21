use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !page_has_column(manager, "play_video_streams").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Page::Table)
                        .add_column(ColumnDef::new(Page::PlayVideoStreams).text().null())
                        .to_owned(),
                )
                .await?;
        }

        if !page_has_column(manager, "play_audio_streams").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Page::Table)
                        .add_column(ColumnDef::new(Page::PlayAudioStreams).text().null())
                        .to_owned(),
                )
                .await?;
        }

        if !page_has_column(manager, "play_subtitle_streams").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Page::Table)
                        .add_column(ColumnDef::new(Page::PlaySubtitleStreams).text().null())
                        .to_owned(),
                )
                .await?;
        }

        if !page_has_column(manager, "play_streams_updated_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Page::Table)
                        .add_column(ColumnDef::new(Page::PlayStreamsUpdatedAt).string().null())
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if page_has_column(manager, "play_streams_updated_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Page::Table)
                        .drop_column(Page::PlayStreamsUpdatedAt)
                        .to_owned(),
                )
                .await?;
        }

        if page_has_column(manager, "play_subtitle_streams").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Page::Table)
                        .drop_column(Page::PlaySubtitleStreams)
                        .to_owned(),
                )
                .await?;
        }

        if page_has_column(manager, "play_audio_streams").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Page::Table)
                        .drop_column(Page::PlayAudioStreams)
                        .to_owned(),
                )
                .await?;
        }

        if page_has_column(manager, "play_video_streams").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Page::Table)
                        .drop_column(Page::PlayVideoStreams)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Page {
    Table,
    PlayVideoStreams,
    PlayAudioStreams,
    PlaySubtitleStreams,
    PlayStreamsUpdatedAt,
}

async fn page_has_column(manager: &SchemaManager<'_>, column: &str) -> Result<bool, DbErr> {
    let backend = manager.get_connection().get_database_backend();
    let sql = format!(
        "SELECT COUNT(*) FROM pragma_table_info('page') WHERE name = '{}'",
        column.replace('\'', "''")
    );
    let result = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql))
        .await?;
    Ok(result.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0) >= 1)
}
