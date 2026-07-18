use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in VideoSourceTable::tables() {
            manager
                .alter_table(
                    Table::alter()
                        .table(table)
                        .add_column(ColumnDef::new(VideoSourceTable::FilterOption).json().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in VideoSourceTable::tables() {
            manager
                .alter_table(
                    Table::alter()
                        .table(table)
                        .drop_column(VideoSourceTable::FilterOption)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum VideoSourceTable {
    Collection,
    Favorite,
    Submission,
    WatchLater,
    VideoSource,
    FilterOption,
}

impl VideoSourceTable {
    fn tables() -> [Self; 5] {
        [
            Self::Collection,
            Self::Favorite,
            Self::Submission,
            Self::WatchLater,
            Self::VideoSource,
        ]
    }
}
