use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CollectionSeasonMapping::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CollectionSeasonMapping::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CollectionSeasonMapping::CollectionId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(CollectionSeasonMapping::UpMid).big_integer().not_null())
                    .col(ColumnDef::new(CollectionSeasonMapping::BasePath).string().not_null())
                    .col(ColumnDef::new(CollectionSeasonMapping::PubYear).integer().not_null())
                    .col(ColumnDef::new(CollectionSeasonMapping::PubQuarter).integer().not_null())
                    .col(ColumnDef::new(CollectionSeasonMapping::SeasonId).integer().not_null())
                    .col(
                        ColumnDef::new(CollectionSeasonMapping::ReferencePubtime)
                            .timestamp()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CollectionSeasonMapping::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(CollectionSeasonMapping::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_collection_season_map_collection_id")
                    .table(CollectionSeasonMapping::Table)
                    .col(CollectionSeasonMapping::CollectionId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_collection_season_map_group_quarter")
                    .table(CollectionSeasonMapping::Table)
                    .col(CollectionSeasonMapping::UpMid)
                    .col(CollectionSeasonMapping::BasePath)
                    .col(CollectionSeasonMapping::PubYear)
                    .col(CollectionSeasonMapping::PubQuarter)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_collection_season_map_group_season")
                    .table(CollectionSeasonMapping::Table)
                    .col(CollectionSeasonMapping::UpMid)
                    .col(CollectionSeasonMapping::BasePath)
                    .col(CollectionSeasonMapping::SeasonId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_collection_season_map_group_season")
                    .table(CollectionSeasonMapping::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_collection_season_map_group_quarter")
                    .table(CollectionSeasonMapping::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_collection_season_map_collection_id")
                    .table(CollectionSeasonMapping::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(CollectionSeasonMapping::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CollectionSeasonMapping {
    Table,
    Id,
    CollectionId,
    UpMid,
    BasePath,
    PubYear,
    PubQuarter,
    SeasonId,
    ReferencePubtime,
    CreatedAt,
    UpdatedAt,
}
