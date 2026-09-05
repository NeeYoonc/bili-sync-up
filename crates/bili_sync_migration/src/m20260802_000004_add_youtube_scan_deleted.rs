use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            ColumnDef::new(YouTubeSource::ScanDeletedVideos)
                .boolean()
                .not_null()
                .default(false)
                .to_owned(),
            ColumnDef::new(YouTubeSource::ScanDeletedVideosOnce)
                .boolean()
                .not_null()
                .default(false)
                .to_owned(),
            ColumnDef::new(YouTubeSource::DeletedVideoIds).text().null().to_owned(),
        ] {
            manager
                .alter_table(Table::alter().table(YouTubeSource::Table).add_column(column).to_owned())
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            YouTubeSource::DeletedVideoIds,
            YouTubeSource::ScanDeletedVideosOnce,
            YouTubeSource::ScanDeletedVideos,
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

#[derive(DeriveIden)]
enum YouTubeSource {
    Table,
    ScanDeletedVideos,
    ScanDeletedVideosOnce,
    DeletedVideoIds,
}
