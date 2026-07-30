use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let columns = [
            ColumnDef::new(YouTubeSource::AudioOnlyM4aOnly)
                .boolean()
                .not_null()
                .default(false)
                .to_owned(),
            ColumnDef::new(YouTubeSource::FlatFolder)
                .boolean()
                .not_null()
                .default(false)
                .to_owned(),
            ColumnDef::new(YouTubeSource::DownloadDanmaku)
                .boolean()
                .not_null()
                .default(true)
                .to_owned(),
            ColumnDef::new(YouTubeSource::AiSubtitleLanguage)
                .string()
                .not_null()
                .default("zh-CN")
                .to_owned(),
            ColumnDef::new(YouTubeSource::FilterOption).json().null().to_owned(),
            ColumnDef::new(YouTubeSource::BlacklistKeywords)
                .text()
                .null()
                .to_owned(),
            ColumnDef::new(YouTubeSource::WhitelistKeywords)
                .text()
                .null()
                .to_owned(),
            ColumnDef::new(YouTubeSource::KeywordCaseSensitive)
                .boolean()
                .not_null()
                .default(true)
                .to_owned(),
            ColumnDef::new(YouTubeSource::MinDurationSeconds)
                .integer()
                .null()
                .to_owned(),
            ColumnDef::new(YouTubeSource::MaxDurationSeconds)
                .integer()
                .null()
                .to_owned(),
            ColumnDef::new(YouTubeSource::PublishedAfter).string().null().to_owned(),
            ColumnDef::new(YouTubeSource::PublishedBefore)
                .string()
                .null()
                .to_owned(),
        ];
        for column in columns {
            manager
                .alter_table(Table::alter().table(YouTubeSource::Table).add_column(column).to_owned())
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            YouTubeSource::PublishedBefore,
            YouTubeSource::PublishedAfter,
            YouTubeSource::MaxDurationSeconds,
            YouTubeSource::MinDurationSeconds,
            YouTubeSource::KeywordCaseSensitive,
            YouTubeSource::WhitelistKeywords,
            YouTubeSource::BlacklistKeywords,
            YouTubeSource::FilterOption,
            YouTubeSource::AiSubtitleLanguage,
            YouTubeSource::DownloadDanmaku,
            YouTubeSource::FlatFolder,
            YouTubeSource::AudioOnlyM4aOnly,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(YouTubeSource::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden, Clone, Copy)]
enum YouTubeSource {
    Table,
    AudioOnlyM4aOnly,
    FlatFolder,
    DownloadDanmaku,
    AiSubtitleLanguage,
    FilterOption,
    BlacklistKeywords,
    WhitelistKeywords,
    KeywordCaseSensitive,
    MinDurationSeconds,
    MaxDurationSeconds,
    PublishedAfter,
    PublishedBefore,
}
