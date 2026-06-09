use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        add_split_chapters_column(manager, Collection::Table, Collection::SplitChaptersAfterDownload).await?;
        add_split_chapters_column(manager, Favorite::Table, Favorite::SplitChaptersAfterDownload).await?;
        add_split_chapters_column(manager, Submission::Table, Submission::SplitChaptersAfterDownload).await?;
        add_split_chapters_column(manager, WatchLater::Table, WatchLater::SplitChaptersAfterDownload).await?;
        add_split_chapters_column(manager, VideoSource::Table, VideoSource::SplitChaptersAfterDownload).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_split_chapters_column(manager, Collection::Table, Collection::SplitChaptersAfterDownload).await?;
        drop_split_chapters_column(manager, Favorite::Table, Favorite::SplitChaptersAfterDownload).await?;
        drop_split_chapters_column(manager, Submission::Table, Submission::SplitChaptersAfterDownload).await?;
        drop_split_chapters_column(manager, WatchLater::Table, WatchLater::SplitChaptersAfterDownload).await?;
        drop_split_chapters_column(manager, VideoSource::Table, VideoSource::SplitChaptersAfterDownload).await?;
        Ok(())
    }
}

async fn add_split_chapters_column<T, C>(manager: &SchemaManager<'_>, table: T, column: C) -> Result<(), DbErr>
where
    T: Iden + 'static,
    C: Iden + 'static,
{
    manager
        .alter_table(
            Table::alter()
                .table(table)
                .add_column(ColumnDef::new(column).boolean().not_null().default(false))
                .to_owned(),
        )
        .await
}

async fn drop_split_chapters_column<T, C>(manager: &SchemaManager<'_>, table: T, column: C) -> Result<(), DbErr>
where
    T: Iden + 'static,
    C: Iden + 'static,
{
    manager
        .alter_table(Table::alter().table(table).drop_column(column).to_owned())
        .await
}

#[derive(DeriveIden)]
enum Collection {
    Table,
    SplitChaptersAfterDownload,
}

#[derive(DeriveIden)]
enum Favorite {
    Table,
    SplitChaptersAfterDownload,
}

#[derive(DeriveIden)]
enum Submission {
    Table,
    SplitChaptersAfterDownload,
}

#[derive(DeriveIden)]
enum WatchLater {
    Table,
    SplitChaptersAfterDownload,
}

#[derive(DeriveIden)]
enum VideoSource {
    Table,
    SplitChaptersAfterDownload,
}
