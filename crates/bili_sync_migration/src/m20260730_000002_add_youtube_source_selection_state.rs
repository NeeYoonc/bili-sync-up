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
                    .add_column(ColumnDef::new(YouTubeSource::SelectedChannels).text().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(YouTubeSource::Table)
                    .add_column(ColumnDef::new(YouTubeSource::KnownVideoIds).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(YouTubeSource::Table)
                    .drop_column(YouTubeSource::KnownVideoIds)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(YouTubeSource::Table)
                    .drop_column(YouTubeSource::SelectedChannels)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum YouTubeSource {
    Table,
    SelectedChannels,
    KnownVideoIds,
}
