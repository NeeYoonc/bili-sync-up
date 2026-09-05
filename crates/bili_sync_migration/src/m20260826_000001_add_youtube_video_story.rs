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
                        ColumnDef::new(YouTubeVideo::IsStory)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(YouTubeVideo::Table)
                    .drop_column(YouTubeVideo::IsStory)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum YouTubeVideo {
    Table,
    IsStory,
}