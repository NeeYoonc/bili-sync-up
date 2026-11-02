use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(YouTubeSource::Table)
                    .add_column(
                        ColumnDef::new(YouTubeSource::GenerateImagePostVideo)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(YouTubeSource::Table)
                    .drop_column(YouTubeSource::GenerateImagePostVideo)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum YouTubeSource {
    Table,
    GenerateImagePostVideo,
}
