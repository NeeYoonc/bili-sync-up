use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(YouTubeVideo::Table)
                    .add_column(
                        ColumnDef::new(YouTubeVideo::EpisodeNumber)
                            .integer()
                            .null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(YouTubeVideo::Table)
                    .drop_column(YouTubeVideo::EpisodeNumber)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum YouTubeVideo {
    Table,
    EpisodeNumber,
}
