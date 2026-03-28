use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            SourceTable::Collection,
            SourceTable::Favorite,
            SourceTable::Submission,
            SourceTable::WatchLater,
            SourceTable::VideoSource,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(table.table_name())
                        .add_column(ColumnDef::new(table.once_column()).boolean().not_null().default(false))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            SourceTable::Collection,
            SourceTable::Favorite,
            SourceTable::Submission,
            SourceTable::WatchLater,
            SourceTable::VideoSource,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(table.table_name())
                        .drop_column(table.once_column())
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

enum SourceTable {
    Collection,
    Favorite,
    Submission,
    WatchLater,
    VideoSource,
}

impl SourceTable {
    fn table_name(&self) -> DynIden {
        match self {
            SourceTable::Collection => Alias::new("collection").into_iden(),
            SourceTable::Favorite => Alias::new("favorite").into_iden(),
            SourceTable::Submission => Alias::new("submission").into_iden(),
            SourceTable::WatchLater => Alias::new("watch_later").into_iden(),
            SourceTable::VideoSource => Alias::new("video_source").into_iden(),
        }
    }

    fn once_column(&self) -> DynIden {
        Alias::new("scan_deleted_videos_once").into_iden()
    }
}
