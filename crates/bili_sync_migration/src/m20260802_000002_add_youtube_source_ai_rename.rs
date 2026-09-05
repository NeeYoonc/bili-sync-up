use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let columns = [
            ColumnDef::new(YouTubeSource::AiRename)
                .boolean()
                .not_null()
                .default(false)
                .to_owned(),
            ColumnDef::new(YouTubeSource::AiRenameVideoPrompt)
                .text()
                .not_null()
                .default("")
                .to_owned(),
            ColumnDef::new(YouTubeSource::AiRenameAudioPrompt)
                .text()
                .not_null()
                .default("")
                .to_owned(),
            ColumnDef::new(YouTubeSource::AiRenameEnableMultiPage)
                .boolean()
                .not_null()
                .default(false)
                .to_owned(),
            ColumnDef::new(YouTubeSource::AiRenameEnableCollection)
                .boolean()
                .not_null()
                .default(false)
                .to_owned(),
            ColumnDef::new(YouTubeSource::AiRenameEnableBangumi)
                .boolean()
                .not_null()
                .default(false)
                .to_owned(),
            ColumnDef::new(YouTubeSource::AiRenameRenameParentDir)
                .boolean()
                .not_null()
                .default(false)
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
            YouTubeSource::AiRenameRenameParentDir,
            YouTubeSource::AiRenameEnableBangumi,
            YouTubeSource::AiRenameEnableCollection,
            YouTubeSource::AiRenameEnableMultiPage,
            YouTubeSource::AiRenameAudioPrompt,
            YouTubeSource::AiRenameVideoPrompt,
            YouTubeSource::AiRename,
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
    AiRename,
    AiRenameVideoPrompt,
    AiRenameAudioPrompt,
    AiRenameEnableMultiPage,
    AiRenameEnableCollection,
    AiRenameEnableBangumi,
    AiRenameRenameParentDir,
}
